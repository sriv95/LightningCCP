//! Port of `src/matches.py`: every match state transition. Each transition is a guarded store call, so
//! concurrent button presses cannot start, cancel or score a match twice.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand::seq::IndexedRandom;

use crate::cache::problems::ProblemCache;
use crate::cf::client::CodeforcesClient;
use crate::cf::models::{Problem, Submission};
use crate::errors::BotError;
use crate::logic::base::{
    CUSTOM_SCALE, LOBBY_TTL_SECONDS, MATCH_TTL_SECONDS, ProblemFilter, earliest_solve, filter_problems, match_deltas,
    match_rating, normalize_tags, parse_tags, rank_groups, solo_delta, solved_keys, utc_day,
};
use crate::store::base::Store;
use crate::store::matches::{MatchPlayer, MatchRow, NewMatch};

pub use crate::ui::embeds::a::MatchView;

/// What the service needs from the Codeforces client (a fake in tests).
pub trait CfApi: Send + Sync {
    fn user_status(
        &self,
        handle: &str,
        count: Option<i64>,
        since: Option<i64>,
    ) -> impl Future<Output = Result<Vec<Submission>>> + Send;
}

impl CfApi for CodeforcesClient {
    fn user_status(
        &self,
        handle: &str,
        count: Option<i64>,
        since: Option<i64>,
    ) -> impl Future<Output = Result<Vec<Submission>>> + Send {
        CodeforcesClient::user_status(self, handle, count, since)
    }
}

/// What the service needs from the problem cache (a fake in tests).
pub trait ProblemSource: Send + Sync {
    /// Python `.problems`: the cached problems, empty while the cache has none.
    fn problems(&self) -> Arc<Vec<Problem>>;
    /// `CacheNotReady` while empty.
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError>;
}

impl ProblemSource for ProblemCache {
    fn problems(&self) -> Arc<Vec<Problem>> {
        self.require().unwrap_or_default()
    }

    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        ProblemCache::require(self)
    }
}

/// Seconds since the epoch, like Python's `time.time`; the service truncates it with `int()`.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
    })
}

/// Keyword arguments of Python `MatchService.create`.
#[derive(Debug, Clone, Default)]
pub struct CreateMatch {
    pub guild_id: i64,
    pub channel_id: i64,
    pub host_id: i64,
    pub mode: String,
    pub max_players: i64,
    pub invited_id: Option<i64>,
    pub rating_arg: Option<i64>,
    pub tags: Option<String>,
    pub exclude_tags: Option<String>,
}

pub struct MatchService<C, P> {
    store: Arc<Store>,
    cf: Arc<C>,
    problems: Arc<P>,
    clock: Clock,
    rng: Mutex<StdRng>,
}

fn bot(e: BotError) -> anyhow::Error {
    e.into()
}

fn plain(msg: &str) -> anyhow::Error {
    BotError::Plain(msg.into()).into()
}

fn closed(msg: &str) -> anyhow::Error {
    BotError::MatchClosed(msg.into()).into()
}

impl<C: CfApi, P: ProblemSource> MatchService<C, P> {
    /// Real clock and an OS-seeded rng.
    pub fn new(store: Arc<Store>, cf: Arc<C>, problems: Arc<P>) -> Self {
        Self::with(store, cf, problems, system_clock(), StdRng::from_rng(&mut rand::rng()))
    }

    /// Injected clock and rng (tests, and anywhere time or picks must be controlled).
    pub fn with(store: Arc<Store>, cf: Arc<C>, problems: Arc<P>, clock: Clock, rng: StdRng) -> Self {
        Self { store, cf, problems, clock, rng: Mutex::new(rng) }
    }

    /// Python `int(self._clock())`: truncates toward zero.
    fn now(&self) -> i64 {
        (self.clock)() as i64
    }

    fn pick(&self, pool: &[Problem]) -> Option<Problem> {
        pool.choose(&mut *self.rng.lock().unwrap()).cloned()
    }

    async fn match_row(&self, match_id: i64) -> Result<MatchRow> {
        match self.store.get_match(match_id).await? {
            Some(m) => Ok(m),
            None => Err(closed("That match no longer exists.")),
        }
    }

    async fn view(&self, match_id: i64, note: Option<String>) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        let players = self.store.get_players(match_id).await?;
        let problem = match &m.problem_key {
            Some(key) if !key.is_empty() => self.problems.problems().iter().find(|p| &p.key() == key).cloned(),
            _ => None,
        };
        let ratings = if m.status == "finished" {
            let mut r = HashMap::new();
            for p in &players {
                r.insert(p.user_id, self.store.rating(m.guild_id, p.user_id).await?);
            }
            Some(r)
        } else {
            None
        };
        Ok(MatchView { r#match: m, players, problem, ratings, note })
    }

    async fn linked_handle(&self, guild_id: i64, user_id: i64) -> Result<String> {
        match self.store.get_handle(guild_id, user_id).await? {
            Some(l) => Ok(l.handle),
            None => Err(bot(BotError::not_linked())),
        }
    }

    pub async fn set_message(&self, match_id: i64, channel_id: i64, message_id: i64) -> Result<()> {
        self.store.set_message(match_id, channel_id, message_id).await
    }

    pub async fn current(&self, guild_id: i64, user_id: i64) -> Result<Option<MatchView>> {
        match self.store.active_match_for(guild_id, user_id).await? {
            Some(id) => Ok(Some(self.view(id, None).await?)),
            None => Ok(None),
        }
    }

    pub async fn create(&self, c: CreateMatch) -> Result<MatchView> {
        let handle = self.linked_handle(c.guild_id, c.host_id).await?;
        if c.invited_id == Some(c.host_id) {
            return Err(plain("You cannot duel yourself."));
        }
        let mode = c.mode.clone();
        let match_id = self
            .store
            .create_match(NewMatch {
                guild_id: c.guild_id,
                channel_id: c.channel_id,
                host_id: c.host_id,
                host_handle: handle,
                mode: c.mode,
                max_players: c.max_players,
                invited_id: c.invited_id,
                rating_arg: c.rating_arg,
                now: self.now(),
                tags: normalize_tags(c.tags.as_deref()),
                exclude_tags: normalize_tags(c.exclude_tags.as_deref()),
            })
            .await?;
        if mode != "solo" {
            return self.view(match_id, None).await;
        }
        match self.start_inner(match_id).await {
            Ok(v) => Ok(v),
            Err(e) => {
                self.store.cancel_match(match_id, self.now()).await?;
                Err(e)
            }
        }
    }

    /// New duel lobby against the same opponent, hosted by whoever pressed Rematch.
    pub async fn rematch(&self, match_id: i64, user_id: i64, channel_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        if m.status != "finished" || m.mode != "duel" {
            return Err(closed("Only a finished duel can be rematched."));
        }
        let ids: Vec<i64> = self.store.get_players(match_id).await?.iter().map(|p| p.user_id).collect();
        if !ids.contains(&user_id) {
            return Err(plain("Only the players of this duel can ask for a rematch."));
        }
        let opponent = ids.iter().copied().find(|&u| u != user_id).ok_or_else(|| anyhow!("no opponent to rematch"))?;
        self.create(CreateMatch {
            guild_id: m.guild_id,
            channel_id,
            host_id: user_id,
            mode: "duel".into(),
            max_players: 2,
            invited_id: Some(opponent),
            rating_arg: m.rating_arg,
            tags: Some(m.tags),
            exclude_tags: Some(m.exclude_tags),
        })
        .await
    }

    pub async fn join(&self, match_id: i64, user_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        if m.status != "lobby" {
            return Err(bot(BotError::match_closed()));
        }
        if let Some(invited) = m.invited_id
            && user_id != invited
        {
            return Err(BotError::Plain(format!("This duel is reserved for <@{invited}>.")).into());
        }
        let handle = self.linked_handle(m.guild_id, user_id).await?;
        self.store.add_player(match_id, m.guild_id, user_id, &handle, self.now()).await?;
        self.view(match_id, None).await
    }

    pub async fn leave(&self, match_id: i64, user_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        let players = self.store.get_players(match_id).await?;
        if !players.iter().any(|p| p.user_id == user_id) {
            return Err(plain("You are not in this match."));
        }
        if m.status == "active" {
            return Err(closed("The match already started. Use Give Up instead."));
        }
        if m.status != "lobby" {
            return Err(bot(BotError::match_closed()));
        }
        if user_id == m.host_id {
            if !self.store.cancel_match(match_id, self.now()).await? {
                return Err(bot(BotError::match_closed()));
            }
            return self.view(match_id, Some("The host left, so the match was cancelled.".into())).await;
        }
        if !self.store.remove_player(match_id, user_id).await? {
            return Err(bot(BotError::match_closed()));
        }
        self.view(match_id, None).await
    }

    pub async fn start(&self, match_id: i64, user_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        if user_id != m.host_id {
            return Err(plain("Only the host can start the match."));
        }
        if m.status != "lobby" {
            return Err(bot(BotError::match_closed()));
        }
        if self.store.get_players(match_id).await?.len() < 2 {
            return Err(plain("You need at least 2 players to start."));
        }
        self.start_inner(match_id).await
    }

    async fn start_inner(&self, match_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        let players = self.store.get_players(match_id).await?;
        if m.mode != "solo" && players.len() < 2 {
            return Err(plain("You need at least 2 players to start."));
        }
        let problems = self.problems.require().map_err(bot)?;
        let mut ratings = Vec::new();
        for p in &players {
            ratings.push(self.store.rating(m.guild_id, p.user_id).await?);
        }
        let mut solved: HashSet<String> = HashSet::new();
        for p in &players {
            solved.extend(solved_keys(&self.cf.user_status(&p.handle, None, None).await?));
        }
        let rating = match_rating(m.rating_arg, &ratings)?;
        let pool = filter_problems(
            &problems,
            &ProblemFilter {
                min_rating: Some(rating),
                max_rating: Some(rating),
                tags: parse_tags(Some(&m.tags)),
                exclude_tags: parse_tags(Some(&m.exclude_tags)),
                solved,
                contest_id: None,
            },
        );
        if pool.is_empty() {
            let with_tags = if !m.tags.is_empty() || !m.exclude_tags.is_empty() { " with those tags" } else { "" };
            return Err(BotError::Plain(format!("No problem rated {rating}{with_tags} is unsolved by every player."))
                .into());
        }
        let problem = self.pick(&pool).ok_or_else(|| anyhow!("empty pool"))?;
        let now = self.now();
        let ids: Vec<i64> = players.iter().map(|p| p.user_id).collect();
        if !self.store.start_match(match_id, rating, &problem.key(), now, now + MATCH_TTL_SECONDS, &ids).await? {
            if self.match_row(match_id).await?.status == "lobby" {
                return Err(plain("The players changed while starting. Press Start again."));
            }
            return Err(bot(BotError::match_closed()));
        }
        self.view(match_id, None).await
    }

    pub async fn check(&self, match_id: i64, user_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        if m.status != "active" {
            return Err(closed("This match is not running."));
        }
        let players = self.store.get_players(match_id).await?;
        match players.iter().find(|p| p.user_id == user_id) {
            Some(me) if !me.gave_up => {}
            _ => return Err(plain("You are not playing in this match.")),
        }
        let players = self.refresh_solves(&m, &players, true, Some(user_id)).await?;
        self.maybe_finish(&m, &players, false, None).await
    }

    pub async fn give_up(&self, match_id: i64, user_id: i64) -> Result<MatchView> {
        let m = self.match_row(match_id).await?;
        if m.status != "active" {
            return Err(closed("This match is not running."));
        }
        let players = self.store.get_players(match_id).await?;
        let me = match players.iter().find(|p| p.user_id == user_id) {
            Some(me) if !me.gave_up => me,
            _ => return Err(plain("You are not playing in this match.")),
        };
        if me.solved_at.is_some() {
            return Err(plain("You already solved it, so there is nothing to give up."));
        }
        if !self.store.mark_gave_up(match_id, user_id).await? {
            return Err(bot(BotError::match_closed()));
        }
        let players = self.store.get_players(match_id).await?;
        match m.mode.as_str() {
            "solo" => self.finish(&m, &players, None, None).await,
            "duel" => {
                let opponent = players
                    .iter()
                    .find(|p| p.user_id != user_id)
                    .ok_or_else(|| anyhow!("no opponent in this duel"))?
                    .user_id;
                self.finish(&m, &players, Some(vec![vec![opponent], vec![user_id]]), None).await
            }
            _ => self.maybe_finish(&m, &players, false, None).await,
        }
    }

    /// Cancel expired lobbies and finish matches past their time.
    pub async fn expire_due(&self) -> Result<Vec<MatchView>> {
        let now = self.now();
        let due = self.store.matches_due(now - LOBBY_TTL_SECONDS, now).await?;
        Ok(self.expire_rows(due).await)
    }

    /// The body of `expire_due` for an already fetched list (also lets a test hand it a stale list).
    pub async fn expire_rows(&self, due: Vec<MatchRow>) -> Vec<MatchView> {
        let now = self.now();
        let mut views = Vec::new();
        for m in due {
            match self.expire_one(&m, now).await {
                Ok(Some(v)) => views.push(v),
                Ok(None) => {}
                Err(e) => tracing::error!("Could not expire match {}: {e:?}", m.id),
            }
        }
        views
    }

    async fn expire_one(&self, m: &MatchRow, now: i64) -> Result<Option<MatchView>> {
        if m.status == "lobby" {
            if self.store.cancel_match(m.id, now).await? {
                return Ok(Some(self.view(m.id, Some("The lobby expired.".into())).await?));
            }
            return Ok(None);
        }
        let players = self.store.get_players(m.id).await?;
        let players = self.refresh_solves(m, &players, true, None).await?;
        Ok(Some(self.maybe_finish(m, &players, true, Some("Time is up.".into())).await?))
    }

    async fn refresh_solves(
        &self,
        m: &MatchRow,
        players: &[MatchPlayer],
        tolerant: bool,
        strict_user: Option<i64>,
    ) -> Result<Vec<MatchPlayer>> {
        let mut found: Vec<(i64, i64)> = Vec::new();
        for p in players {
            if p.solved_at.is_some() || p.gave_up {
                continue;
            }
            let subs = match self.cf.user_status(&p.handle, Some(100), m.started_at).await {
                Ok(s) => s,
                Err(e) => {
                    if !tolerant || Some(p.user_id) == strict_user {
                        return Err(e);
                    }
                    // One unreachable handle (e.g. renamed) must not block expiry or other players' Check.
                    tracing::warn!("Skipping {} while expiring match {}: {e:?}", p.handle, m.id);
                    continue;
                }
            };
            let Some(key) = m.problem_key.as_deref() else { continue };
            let since = m.started_at.ok_or_else(|| anyhow!("match {} has no start time", m.id))?;
            if let Some(t) = earliest_solve(&subs, key, since) {
                found.push((p.user_id, t));
            }
        }
        if !found.is_empty() {
            self.store.record_solves(m.id, &found).await?;
        }
        self.store.get_players(m.id).await
    }

    async fn maybe_finish(
        &self,
        m: &MatchRow,
        players: &[MatchPlayer],
        force: bool,
        note: Option<String>,
    ) -> Result<MatchView> {
        // the first solve ends every match; a custom match also ends when everyone gave up
        let mut done = players.iter().any(|p| p.solved_at.is_some());
        if m.mode == "custom" {
            done = done || players.iter().all(|p| p.gave_up);
        }
        if !(done || force) {
            return self.view(m.id, None).await;
        }
        self.finish(m, players, None, note).await
    }

    async fn finish(
        &self,
        m: &MatchRow,
        players: &[MatchPlayer],
        groups: Option<Vec<Vec<i64>>>,
        note: Option<String>,
    ) -> Result<MatchView> {
        let mut ratings = HashMap::new();
        for p in players {
            ratings.insert(p.user_id, self.store.rating(m.guild_id, p.user_id).await?);
        }
        let any_solved = players.iter().any(|p| p.solved_at.is_some());
        let deltas: Vec<(i64, i64)> = if m.mode == "solo" {
            let [p] = players else { return Err(anyhow!("a solo match needs exactly 1 player")) };
            let problem_rating = m.rating.ok_or_else(|| anyhow!("match {} has no rating", m.id))?;
            vec![(p.user_id, solo_delta(ratings[&p.user_id], problem_rating, p.solved_at.is_some()))]
        } else if let Some(groups) = groups {
            match_deltas(&groups, &ratings, 1.0)?
        } else if !any_solved {
            players.iter().map(|p| (p.user_id, 0)).collect()
        } else if m.mode == "duel" {
            let entries: Vec<(i64, Option<i64>)> = players.iter().map(|p| (p.user_id, p.solved_at)).collect();
            match_deltas(&rank_groups(&entries), &ratings, 1.0)?
        } else {
            // custom: the first solver beats everyone else, who tie
            let first = players.iter().filter_map(|p| p.solved_at).min().ok_or_else(|| anyhow!("no solve"))?;
            let winners: Vec<i64> = players.iter().filter(|p| p.solved_at == Some(first)).map(|p| p.user_id).collect();
            let losers: Vec<i64> = players.iter().filter(|p| p.solved_at != Some(first)).map(|p| p.user_id).collect();
            let mut groups = vec![winners];
            if !losers.is_empty() {
                groups.push(losers);
            }
            match_deltas(&groups, &ratings, CUSTOM_SCALE)?
        };
        // False means another request already finished this match; show its result.
        let now = self.now();
        let bonus_day = (m.mode == "solo").then(|| utc_day(now));
        if !self.store.finish_match(m.id, now, &deltas, bonus_day).await? {
            return self.view(m.id, None).await; // its own result, not this caller's note
        }
        let mut note = note;
        if let Some(day) = bonus_day {
            let daily = self.store.get_daily(m.guild_id, players[0].user_id, day).await?;
            if daily.is_some_and(|d| d.match_id == Some(m.id)) {
                note = note
                    .filter(|n| !n.is_empty())
                    .or_else(|| Some("Daily bonus: your first solo win of the day counts double.".into()));
            }
        }
        self.view(m.id, note).await
    }
}
