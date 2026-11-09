//! Port of `src/mashup.py` lines 125-503: every contest and bingo state transition up to the read of one
//! player (`tick`, the read budget, the interval and the ratings stay in `tick.rs`). Each
//! transition is a guarded store call, so concurrent button presses cannot start or cancel twice.
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::AtomicI64;

use anyhow::{Result, anyhow};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use std::sync::Arc;

use crate::cf::models::Problem;
use crate::errors::BotError;
use crate::logic::base::{
    BINGO_MAX_SIZE, BINGO_MIN_SIZE, MASHUP_MAX_INTERVAL, MASHUP_MAX_PLAYERS, MASHUP_MAX_PROBLEMS, MASHUP_MAX_TEAM_SIZE,
    MASHUP_MAX_TEAMS, MASHUP_READS_PER_SECOND, first_solve, normalize_tags, parse_tags, solved_keys,
};
use crate::logic::mashup::{pick_mashup, pick_replacement, team_solves};
use crate::matches::{CfApi, Clock, ProblemSource, system_clock};
use crate::store::base::Store;
use crate::store::mashup::{MashupPlayer, MashupRow, NewMashup};

pub use crate::matches::{CfApi as MashupCf, ProblemSource as MashupProblems};
pub use crate::ui::embeds::a::MashupRecord;
pub use crate::ui::embeds::b::{LogEntry, MashupView};

/// Keyword arguments of Python `MashupService.create`; `Default` is Python's defaults for the optional ones.
#[derive(Debug, Clone)]
pub struct CreateMashup {
    pub guild_id: i64,
    pub channel_id: i64,
    pub host_id: i64,
    pub min_rating: i64,
    pub max_rating: i64,
    pub count: i64,
    pub duration_seconds: i64,
    pub penalty_minutes: i64,
    pub teams: i64,
    pub team_size: i64,
    pub rated: bool,
    pub tags: Option<String>,
    pub exclude_tags: Option<String>,
    pub contest_id: Option<i64>,
    pub mode: String,
    pub size: i64,
    pub win: String,
}

impl Default for CreateMashup {
    fn default() -> Self {
        CreateMashup {
            guild_id: 0,
            channel_id: 0,
            host_id: 0,
            min_rating: 0,
            max_rating: 0,
            count: 0,
            duration_seconds: 0,
            penalty_minutes: 0,
            teams: MASHUP_MAX_TEAMS as i64,
            team_size: 1,
            rated: true,
            tags: None,
            exclude_tags: None,
            contest_id: None,
            mode: "contest".into(),
            size: 0,
            win: "line".into(),
        }
    }
}

pub struct MashupService<C, P> {
    pub(crate) store: Arc<Store>,
    pub(crate) cf: Arc<C>,
    pub(crate) problems: Arc<P>,
    clock: Clock,
    rng: Mutex<StdRng>,
    /// Python `interval`: seconds until the next tick, set by every tick.
    pub interval: AtomicI64,
    /// Python `_read_at`: (contest, user) -> last read, in memory.
    pub(crate) read_at: Mutex<HashMap<(i64, i64), i64>>,
    /// Python `MASHUP_READS_PER_SECOND`; a field only so a test can lower it (Python monkeypatches it).
    pub(crate) reads_per_second: f64,
}

fn bot(e: BotError) -> anyhow::Error {
    e.into()
}

fn plain(msg: impl Into<String>) -> anyhow::Error {
    BotError::Plain(msg.into()).into()
}

fn closed(msg: &str) -> anyhow::Error {
    BotError::MatchClosed(msg.into()).into()
}

/// Python `_check_team`: a team number from 1 to `teams`.
fn check_team(teams: i64, team: Option<i64>) -> Result<i64> {
    match team {
        Some(t) if (1..=teams).contains(&t) => Ok(t),
        _ => Err(plain(format!("Pick a team number from 1 to {teams}."))),
    }
}

/// A player's team; `None` cannot happen (every contest is a team contest, see PARITY notes) and is an error.
fn team_of(p: &MashupPlayer) -> Result<i64> {
    p.team.ok_or_else(|| anyhow!("player {} has no team", p.user_id))
}

fn members(players: &[MashupPlayer]) -> Result<Vec<(i64, i64)>> {
    players.iter().map(|p| Ok((p.user_id, team_of(p)?))).collect()
}

impl<C: CfApi, P: ProblemSource> MashupService<C, P> {
    /// Real clock and an OS-seeded rng.
    pub fn new(store: Arc<Store>, cf: Arc<C>, problems: Arc<P>) -> Self {
        Self::with(store, cf, problems, system_clock(), StdRng::from_rng(&mut rand::rng()))
    }

    /// Injected clock and rng (tests, and anywhere time or picks must be controlled).
    pub fn with(store: Arc<Store>, cf: Arc<C>, problems: Arc<P>, clock: Clock, rng: StdRng) -> Self {
        Self {
            store,
            cf,
            problems,
            clock,
            rng: Mutex::new(rng),
            interval: AtomicI64::new(MASHUP_MAX_INTERVAL),
            read_at: Mutex::new(HashMap::new()),
            reads_per_second: MASHUP_READS_PER_SECOND,
        }
    }

    /// Replaces the read budget (reads per second); the default is `MASHUP_READS_PER_SECOND`.
    pub fn with_reads_per_second(mut self, reads_per_second: f64) -> Self {
        self.reads_per_second = reads_per_second;
        self
    }

    /// Python `int(self._clock())`: truncates toward zero.
    pub fn now(&self) -> i64 {
        (self.clock)() as i64
    }

    pub async fn view(&self, mashup_id: i64, note: Option<String>) -> Result<MashupView> {
        let mashup = self.mashup(mashup_id).await?;
        let problems = self.store.mashup_problems(mashup_id).await?;
        let players = self.store.mashup_players(mashup_id).await?;
        let solves = self.store.mashup_solves(mashup_id).await?;
        let cells = if mashup.mode != "contest" { self.store.mashup_cells(mashup_id).await? } else { Vec::new() };
        Ok(MashupView { mashup, problems, players, solves, note, cells })
    }

    /// The user's newest finished contests with their place, newest first.
    pub async fn history(&self, guild_id: i64, user_id: i64, limit: i64) -> Result<Vec<MashupRecord>> {
        let mut records = Vec::new();
        for mashup in self.store.finished_mashups(guild_id, user_id, limit).await? {
            let view = self.view(mashup.id, None).await?;
            let me = view.players.iter().find(|p| p.user_id == user_id).ok_or_else(|| anyhow!("no player {user_id}"))?;
            let ranks = view.standings()?;
            let mine = ranks
                .iter()
                .find(|r| Some(r.user_id) == me.team)
                .ok_or_else(|| anyhow!("no standing for team {:?}", me.team))?;
            records.push(MashupRecord {
                mashup,
                rank: mine.rank,
                entrants: ranks.len() as i64,
                solved: mine.solved,
                team: me.team,
                delta: me.delta,
            });
        }
        Ok(records)
    }

    /// The solves not yet posted to the thread (each is returned once). A bingo problem is claimed once; a
    /// contest problem is solved once per team.
    pub async fn claims_to_log(&self, view: &MashupView) -> Result<Vec<LogEntry>> {
        if view.mashup.thread_id.is_none() {
            return Ok(Vec::new());
        }
        let bingo = view.mashup.mode != "contest";
        let mut out = Vec::new();
        for e in view.log()? {
            let key = if bingo { e.key.clone() } else { format!("{}:{}", e.team, e.key) };
            if self.store.mark_logged(view.mashup.id, &key).await? {
                out.push(e);
            }
        }
        Ok(out)
    }

    /// True once, when a contest with a thread has ended or was cancelled.
    pub async fn final_due(&self, view: &MashupView) -> Result<bool> {
        Ok(view.mashup.thread_id.is_some()
            && matches!(view.mashup.status.as_str(), "finished" | "cancelled")
            && self.store.mark_logged(view.mashup.id, "#final").await?)
    }

    pub async fn set_log_board(&self, mashup_id: i64, message_id: i64) -> Result<()> {
        self.store.set_log_board(mashup_id, message_id).await
    }

    pub async fn current(&self, guild_id: i64, user_id: i64) -> Result<Option<MashupView>> {
        match self.store.active_mashup_for(guild_id, user_id).await? {
            Some(id) => Ok(Some(self.view(id, None).await?)),
            None => Ok(None),
        }
    }

    pub async fn set_thread(&self, mashup_id: i64, thread_id: i64) -> Result<()> {
        self.store.set_mashup_thread(mashup_id, thread_id).await
    }

    /// Finished contests (the newest `limit`) the user's team won or tied for first, with something solved and
    /// another team to beat.
    pub async fn wins(&self, guild_id: i64, user_id: i64, limit: i64) -> Result<i64> {
        let records = self.history(guild_id, user_id, limit).await?;
        Ok(records.iter().filter(|r| r.rank == 1 && r.solved > 0 && r.entrants > 1).count() as i64)
    }

    pub async fn set_message(&self, mashup_id: i64, channel_id: i64, message_id: i64) -> Result<()> {
        self.store.set_mashup_message(mashup_id, channel_id, message_id).await
    }

    async fn handle(&self, guild_id: i64, user_id: i64) -> Result<String> {
        match self.store.get_handle(guild_id, user_id).await? {
            Some(l) => Ok(l.handle),
            None => Err(bot(BotError::not_linked())),
        }
    }

    /// Python `_mashup`: the row, or `MatchClosed` when it is gone.
    pub async fn mashup(&self, mashup_id: i64) -> Result<MashupRow> {
        match self.store.get_mashup(mashup_id).await? {
            Some(m) => Ok(m),
            None => Err(closed("That contest no longer exists.")),
        }
    }

    /// The rated problems of a Codeforces contest in contest order.
    fn contest_problems(&self, contest_id: i64) -> Result<Vec<Problem>> {
        let all = self.problems.require().map_err(bot)?;
        let mut found: Vec<Problem> =
            all.iter().filter(|p| p.contest_id == contest_id && p.rating.is_some_and(|r| r != 0)).cloned().collect();
        found.sort_by(|a, b| a.index.cmp(&b.index));
        if found.is_empty() {
            return Err(plain(format!("I know no rated problems of contest {contest_id} yet.")));
        }
        if found.len() > MASHUP_MAX_PROBLEMS {
            return Err(plain(format!("That contest has more than {MASHUP_MAX_PROBLEMS} rated problems.")));
        }
        Ok(found)
    }

    pub async fn create(&self, c: CreateMashup) -> Result<MashupView> {
        let CreateMashup { mut min_rating, mut max_rating, mut count, mut penalty_minutes, .. } = c;
        let (mut tags, mut exclude_tags) = (c.tags, c.exclude_tags);
        if c.mode != "contest" {
            // bingo: the board is size x size problems
            if !(BINGO_MIN_SIZE as i64..=BINGO_MAX_SIZE as i64).contains(&c.size) {
                return Err(plain(format!("The board size must be from {BINGO_MIN_SIZE} to {BINGO_MAX_SIZE}.")));
            }
            (count, penalty_minutes) = (c.size * c.size, 0);
        }
        if let Some(contest_id) = c.contest_id {
            // a virtual contest: the problems are the contest's own
            let problems = self.contest_problems(contest_id)?;
            let ratings = problems.iter().filter_map(|p| p.rating);
            min_rating = ratings.clone().min().ok_or_else(|| anyhow!("no ratings"))?;
            max_rating = ratings.max().ok_or_else(|| anyhow!("no ratings"))?;
            count = problems.len() as i64;
            tags = None;
            exclude_tags = None;
        }
        if min_rating > max_rating {
            return Err(plain("min_rating cannot be higher than max_rating."));
        }
        if !(1..=MASHUP_MAX_TEAMS as i64).contains(&c.teams) {
            return Err(plain(format!("The number of teams must be from 1 to {MASHUP_MAX_TEAMS}.")));
        }
        if !(1..=MASHUP_MAX_TEAM_SIZE as i64).contains(&c.team_size) {
            return Err(plain(format!("Players per team must be from 1 to {MASHUP_MAX_TEAM_SIZE}.")));
        }
        if c.duration_seconds <= 0 {
            return Err(plain("The contest must last at least one minute."));
        }
        let host_handle = self.handle(c.guild_id, c.host_id).await?;
        let mashup_id = self
            .store
            .create_mashup(NewMashup {
                guild_id: c.guild_id,
                channel_id: c.channel_id,
                host_id: c.host_id,
                host_handle,
                min_rating,
                max_rating,
                count,
                duration_seconds: c.duration_seconds,
                penalty_minutes,
                now: self.now(),
                teams: c.teams,
                host_team: Some(1), // the host always starts in team 1
                max_players: (MASHUP_MAX_PLAYERS as i64).min(c.teams * c.team_size),
                rated: c.rated,
                team_size: c.team_size,
                tags: normalize_tags(tags.as_deref()),
                exclude_tags: normalize_tags(exclude_tags.as_deref()),
                contest_id: c.contest_id,
                mode: c.mode,
                size: c.size,
                win: c.win,
            })
            .await?;
        self.view(mashup_id, None).await
    }

    /// New lobby with the same settings, hosted by whoever pressed Rematch.
    pub async fn rematch(&self, mashup_id: i64, user_id: i64, channel_id: i64) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        if m.status != "finished" {
            return Err(closed("Only a finished contest can be rematched."));
        }
        if !self.store.mashup_players(mashup_id).await?.iter().any(|p| p.user_id == user_id) {
            return Err(plain("Only the players of this contest can ask for a rematch."));
        }
        self.create(CreateMashup {
            guild_id: m.guild_id,
            channel_id,
            host_id: user_id,
            min_rating: m.min_rating,
            max_rating: m.max_rating,
            count: m.count,
            duration_seconds: m.duration_seconds,
            penalty_minutes: m.penalty_minutes,
            teams: m.teams,
            team_size: m.team_size,
            rated: m.rated != 0,
            tags: Some(m.tags),
            exclude_tags: Some(m.exclude_tags),
            contest_id: m.contest_id,
            mode: m.mode,
            size: m.size,
            win: m.win,
        })
        .await
    }

    pub async fn join(&self, mashup_id: i64, user_id: i64, team: Option<i64>) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        if m.status != "lobby" {
            return Err(closed("This contest has already started, so nobody can join."));
        }
        let players = self.store.mashup_players(mashup_id).await?;
        let team = if m.team_size == 1 {
            // every player is a team: take the first free number
            let taken: HashSet<Option<i64>> = players.iter().map(|p| p.team).collect();
            (1..=m.teams).find(|t| !taken.contains(&Some(*t))).ok_or_else(|| bot(BotError::match_full()))?
        } else {
            let team = check_team(m.teams, team)?;
            if players.iter().any(|p| p.user_id == user_id) {
                if user_id == m.host_id {
                    return Err(plain("The host stays in team 1."));
                }
                self.store.move_mashup_player(mashup_id, user_id, team, m.team_size).await?;
                return self.view(mashup_id, None).await;
            }
            team
        };
        let handle = self.handle(m.guild_id, user_id).await?;
        self.store
            .add_mashup_player(mashup_id, m.guild_id, user_id, &handle, self.now(), m.max_players, Some(team), m.team_size)
            .await?;
        self.view(mashup_id, None).await
    }

    pub async fn leave(&self, mashup_id: i64, user_id: i64) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        let players = self.store.mashup_players(mashup_id).await?;
        if !players.iter().any(|p| p.user_id == user_id) {
            return Err(plain("You are not in this contest."));
        }
        if m.status != "lobby" {
            return Err(closed("The contest already started. Use Give Up instead."));
        }
        if user_id == m.host_id {
            if !self.store.cancel_mashup(mashup_id).await? {
                return Err(bot(BotError::match_closed()));
            }
            return self.view(mashup_id, Some("The host left, so the contest was cancelled.".into())).await;
        }
        if !self.store.remove_mashup_player(mashup_id, user_id).await? {
            return Err(bot(BotError::match_closed()));
        }
        self.view(mashup_id, None).await
    }

    pub async fn start(&self, mashup_id: i64, user_id: i64) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        if user_id != m.host_id {
            return Err(plain("Only the host can start the contest."));
        }
        if m.status != "lobby" {
            return Err(bot(BotError::match_closed()));
        }
        let players = self.store.mashup_players(mashup_id).await?;
        // Python binds `solved` only on the random branch, so a replace bingo with a contest id fails later
        let mut solved: Option<HashSet<String>> = None;
        let picked = if let Some(contest_id) = m.contest_id {
            Some(self.contest_problems(contest_id)?) // earlier solves do not matter
        } else {
            let mut all_solved: HashSet<String> = HashSet::new();
            for p in &players {
                all_solved.extend(solved_keys(&self.cf.user_status(&p.handle, None, None).await?));
            }
            let problems = self.problems.require().map_err(bot)?;
            let picked = pick_mashup(
                &problems,
                m.min_rating,
                m.max_rating,
                m.count,
                &all_solved,
                &mut *self.rng.lock().unwrap(),
                &parse_tags(Some(&m.tags)),
                &parse_tags(Some(&m.exclude_tags)),
            )?;
            solved = Some(all_solved);
            picked
        };
        let Some(mut picked) = picked else {
            let with_tags = if !m.tags.is_empty() || !m.exclude_tags.is_empty() { " with those tags" } else { "" };
            return Err(plain(format!(
                "Not enough problems rated {}-{} {with_tags} that nobody here has solved, for {} problems.",
                m.min_rating, m.max_rating, m.count
            )));
        };
        if m.mode != "contest" {
            picked.shuffle(&mut *self.rng.lock().unwrap()); // pick_mashup lists easy to hard; a board is mixed
        }
        let now = self.now();
        let excluded: Vec<String> = if m.mode == "replace" {
            solved.ok_or_else(|| anyhow!("cannot access local variable 'solved' where it is not associated with a value"))?.into_iter().collect()
        } else {
            Vec::new()
        };
        let problems: Vec<(String, i64)> =
            picked.iter().map(|p| Ok((p.key(), p.rating.ok_or_else(|| anyhow!("{} has no rating", p.key()))?))).collect::<Result<_>>()?;
        let ids: Vec<i64> = players.iter().map(|p| p.user_id).collect();
        if !self.store.start_mashup(mashup_id, &problems, now, now + m.duration_seconds, &ids, &excluded).await? {
            if self.mashup(mashup_id).await?.status == "lobby" {
                return Err(plain("The players changed while starting. Press Start again."));
            }
            return Err(bot(BotError::match_closed()));
        }
        self.view(mashup_id, None).await
    }

    /// Python `self._now() >= mashup.ends_at`, for a contest that is active (its end is then set).
    fn out_of_time(&self, m: &MashupRow) -> Result<bool> {
        let ends_at = m.ends_at.ok_or_else(|| anyhow!("active contest {} has no end", m.id))?;
        Ok(self.now() >= ends_at)
    }

    pub async fn check(&self, mashup_id: i64, user_id: i64) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        if m.status != "active" || self.out_of_time(&m)? {
            return Err(closed("This contest is not running."));
        }
        let players = self.store.mashup_players(mashup_id).await?;
        let player = players.iter().find(|p| p.user_id == user_id).ok_or_else(|| plain("You are not in this contest."))?;
        if !player.active {
            return Err(plain("You gave up this contest."));
        }
        self.refresh(&m, player).await?;
        self.view(mashup_id, None).await
    }

    /// Leave a running contest: the place and rating change stay (from the solves so far, no more are read)
    /// but the player is free to join another match or contest.
    pub async fn give_up(&self, mashup_id: i64, user_id: i64) -> Result<MashupView> {
        let m = self.mashup(mashup_id).await?;
        if m.status != "active" || self.out_of_time(&m)? {
            return Err(closed("This contest is not running."));
        }
        let players = self.store.mashup_players(mashup_id).await?;
        if !players.iter().any(|p| p.user_id == user_id && p.active) {
            return Err(plain("You are not playing in this contest."));
        }
        if !self.store.mark_mashup_gave_up(mashup_id, user_id).await? {
            return Err(bot(BotError::match_closed()));
        }
        self.view(mashup_id, None).await
    }

    /// Every player still in has a team that solved every problem (true when all gave up). A bingo is over
    /// when it has a winner (or a full classic board) or all gave up.
    pub async fn everyone_done(&self, m: &MashupRow) -> Result<bool> {
        if m.mode != "contest" {
            let view = self.view(m.id, None).await?;
            return Ok(view.board()?.over || !view.players.iter().any(|p| p.active));
        }
        let total = self.store.mashup_problems(m.id).await?.len();
        let players = self.store.mashup_players(m.id).await?;
        let solves = team_solves(&members(&players)?, &self.store.mashup_solves(m.id).await?);
        for p in players.iter().filter(|p| p.active) {
            let team = team_of(p)?;
            let n = solves.iter().find(|(t, _)| *t == team).map_or(0, |(_, s)| s.len());
            if n < total {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Reads the player's recent submissions; returns how many new solves it stored.
    pub async fn refresh(&self, m: &MashupRow, player: &MashupPlayer) -> Result<usize> {
        let cells = self.store.mashup_cells(m.id).await?;
        let subs = self.cf.user_status(&player.handle, Some(100), m.started_at).await?;
        let ends_at = m.ends_at.ok_or_else(|| anyhow!("contest {} has no end", m.id))?;
        let mut solves: Vec<(String, (i64, i64))> = Vec::new();
        for c in &cells {
            if let Some(found) = first_solve(&subs, &c.key, c.since, ends_at) {
                // Python's dict: a repeated key keeps its first position and takes the last value
                match solves.iter_mut().find(|(k, _)| *k == c.key) {
                    Some(slot) => slot.1 = found,
                    None => solves.push((c.key.clone(), found)),
                }
            }
        }
        let new = self.store.record_mashup_solves(m.id, player.user_id, &solves).await?;
        if new > 0 && m.mode == "replace" {
            self.replace_claimed(m).await?;
        }
        Ok(new)
    }

    /// Gives the newest problem of every claimed cell of a replace bingo a successor rated 100 higher, which
    /// counts from the claim and lets another team take the cell over. It is a problem nobody in the lobby had
    /// solved before the start (attempts are not checked).
    pub async fn replace_claimed(&self, m: &MashupRow) -> Result<()> {
        let view = self.view(m.id, None).await?;
        let claims = view.board()?.claims;
        let mut used: HashSet<String> = view.cells.iter().map(|c| c.key.clone()).collect();
        used.extend(self.store.mashup_excluded(m.id).await?);
        // the cells come in the order they were added: the last of each cell is its newest
        let mut newest: Vec<(Option<i64>, &crate::store::mashup::MashupCell)> = Vec::new();
        for c in &view.cells {
            match newest.iter_mut().find(|(cell, _)| *cell == c.cell) {
                Some(slot) => slot.1 = c,
                None => newest.push((c.cell, c)),
            }
        }
        for (cell, current) in newest {
            let Some(&(_, claimed_at)) = claims.get(&current.key) else { continue };
            let problems = self.problems.require().map_err(bot)?;
            let pick = pick_replacement(
                &problems,
                current.rating,
                &used,
                m.max_rating,
                &mut *self.rng.lock().unwrap(),
                &parse_tags(Some(&m.tags)),
                &parse_tags(Some(&m.exclude_tags)),
            );
            let Some(pick) = pick else {
                tracing::warn!("No replacement for {} in mashup {}", current.key, m.id);
                continue;
            };
            used.insert(pick.key());
            let cell = cell.ok_or_else(|| anyhow!("{} has no board cell", current.key))?;
            let rating = pick.rating.ok_or_else(|| anyhow!("{} has no rating", pick.key()))?;
            self.store.replace_cell(m.id, cell, current.idx, &pick.key(), rating, claimed_at).await?;
        }
        Ok(())
    }
}
