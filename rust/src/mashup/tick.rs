//! Port of `src/mashup.py` lines 503-606: the tick body, the read of the stalest players within the budget,
//! the dynamic interval and the match-rating changes. The cog-side loop that posts the returned views is s18.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::Ordering::SeqCst;

use anyhow::{Result, anyhow};

use super::service::{MashupService, MashupView};
use crate::logic::base::{MASHUP_LOBBY_TTL_SECONDS, MASHUP_MAX_INTERVAL, MASHUP_MIN_INTERVAL, match_deltas};
use crate::logic::mashup::team_solves;
use crate::matches::{CfApi, ProblemSource};
use crate::store::mashup::{MashupPlayer, MashupRow};

/// A player to read: its staleness (-1 = out of time, read first and without budget), then the contest.
type Due = (i64, usize, MashupPlayer);

impl<C: CfApi, P: ProblemSource> MashupService<C, P> {
    /// Cancels expired lobbies, reads the stalest players of running contests (the reads the last interval
    /// pays for, everyone in a contest that is out of time) and ends the contests out of time. Returns the
    /// contests whose message needs an update. Sets `interval`: the fewer players are waiting for a read, the
    /// sooner the next tick. A contest that fails to update is logged and skipped, as in Python.
    pub async fn tick(&self) -> Result<Vec<MashupView>> {
        let now = self.now();
        let mut views = Vec::new();
        let mut running: Vec<MashupRow> = Vec::new();
        for m in self.store.mashups_in(&["lobby", "active"]).await? {
            let id = m.id;
            let step: Result<()> = async {
                if m.status == "active" {
                    running.push(m);
                } else if now - m.created_at >= MASHUP_LOBBY_TTL_SECONDS && self.store.cancel_mashup(m.id).await? {
                    views.push(self.view(m.id, Some("The lobby expired.".into())).await?);
                }
                Ok(())
            }
            .await;
            if let Err(e) = step {
                tracing::error!("Could not update mashup {id}: {e:#}");
            }
        }
        let (mut changed, waiting) = self.read_players(&running, now, &HashSet::new()).await?;
        // A bingo that is won gets one full read first: a team may have claimed a square earlier than the
        // solves read so far show.
        let mut won: Vec<MashupRow> = Vec::new();
        for m in running.iter().filter(|m| m.mode != "contest") {
            if now < ends_at(m)? && self.everyone_done(m).await? {
                won.push(m.clone());
            }
        }
        if !won.is_empty() {
            let full: HashSet<i64> = won.iter().map(|m| m.id).collect();
            for (id, n) in self.read_players(&won, now, &full).await?.0 {
                *changed.entry(id).or_insert(0) += n;
            }
        }
        // ceil(waiting / reads per second), clamped; `as i64` saturates, never panics
        let wanted = (waiting as f64 / self.reads_per_second).ceil() as i64;
        self.interval.store(wanted.max(MASHUP_MIN_INTERVAL).min(MASHUP_MAX_INTERVAL), SeqCst);
        for m in &running {
            let step: Result<()> = async {
                let ended = now >= ends_at(m)? || won.iter().any(|w| w.id == m.id) || self.everyone_done(m).await?;
                if ended {
                    let deltas = self.rating_changes(&self.view(m.id, None).await?).await?;
                    self.store.finish_mashup(m.id, &deltas, Some(now)).await?;
                    self.read_at.lock().unwrap().retain(|k, _| k.0 != m.id);
                }
                if changed.get(&m.id).is_some_and(|n| *n != 0) || ended {
                    views.push(self.view(m.id, None).await?);
                }
                Ok(())
            }
            .await;
            if let Err(e) = step {
                tracing::error!("Could not update mashup {}: {e:#}", m.id);
            }
        }
        Ok(views)
    }

    /// Match rating change per player: every pair of teams (rated at their members' average, a team of one at
    /// its player's rating) is a duel decided by the standings, as in a custom match. Empty for a single
    /// entrant or an unrated contest. Pairs are `(user, change)` in player order.
    pub async fn rating_changes(&self, view: &MashupView) -> Result<Vec<(i64, i64)>> {
        let ranks = view.standings()?;
        if ranks.len() < 2 || view.mashup.rated == 0 {
            return Ok(Vec::new());
        }
        let mut ratings: HashMap<i64, i64> = HashMap::new();
        for p in &view.players {
            ratings.insert(p.user_id, self.store.rating(view.mashup.guild_id, p.user_id).await?);
        }
        let mut members: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for p in &view.players {
            members.entry(team_of(p)?).or_default().push(ratings[&p.user_id]);
        }
        // Python `sum(r) // len(r)` floors; len > 0 so div_euclid is the same
        let entrant: HashMap<i64, i64> =
            members.iter().map(|(t, r)| (*t, r.iter().sum::<i64>().div_euclid(r.len() as i64))).collect();
        let mut groups: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for r in &ranks {
            groups.entry(r.rank).or_default().push(r.user_id);
        }
        let groups: Vec<Vec<i64>> = groups.into_values().collect();
        let changes: HashMap<i64, i64> = match_deltas(&groups, &entrant, 1.0)?.into_iter().collect();
        view.players
            .iter()
            .map(|p| {
                let t = team_of(p)?;
                Ok((p.user_id, *changes.get(&t).ok_or_else(|| anyhow!("no rating change for team {t}"))?))
            })
            .collect()
    }

    /// Reads submissions; returns the new solves per contest and how many players of contests still running
    /// are due the next time. A contest out of time (or in `full`) is read in full (its last solves count);
    /// the others share the budget of the last interval, the player read longest ago first. A team that
    /// solved everything is not read.
    pub(crate) async fn read_players(
        &self,
        running: &[MashupRow],
        now: i64,
        full: &HashSet<i64>,
    ) -> Result<(HashMap<i64, usize>, usize)> {
        let mut due: Vec<Due> = Vec::new();
        for (i, m) in running.iter().enumerate() {
            let listed: Result<_> = async {
                let total = self.store.mashup_problems(m.id).await?.len(); // a board is never solved by one team
                let players = self.store.mashup_players(m.id).await?;
                let members: Vec<(i64, i64)> = players.iter().map(|p| Ok((p.user_id, team_of(p)?))).collect::<Result<_>>()?;
                let solves = team_solves(&members, &self.store.mashup_solves(m.id).await?);
                Ok((total, players, solves))
            }
            .await;
            let (total, players, solves) = match listed {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("Could not list the players of mashup {}: {e:#}", m.id);
                    continue;
                }
            };
            let bingo = m.mode != "contest";
            // out of time: before everyone, no budget
            let last = now >= ends_at(m)? || full.contains(&m.id);
            for p in players {
                let solved = solves.iter().find(|(t, _)| Some(*t) == p.team).map_or(0, |(_, s)| s.len());
                if p.active && (bingo || solved < total) {
                    let stale = if last { -1 } else { self.read_at.lock().unwrap().get(&(m.id, p.user_id)).copied().unwrap_or(0) };
                    due.push((stale, i, p));
                }
            }
        }
        due.sort_by_key(|d| d.0); // stable: equal staleness keeps contest and joining order
        let interval = self.interval.load(SeqCst);
        // Python `round` is half-to-even
        let mut budget = ((self.reads_per_second * interval as f64).round_ties_even() as i64).max(1);
        let mut changed: HashMap<i64, usize> = HashMap::new();
        for (stale, i, player) in &due {
            if *stale >= 0 && budget <= 0 {
                break;
            }
            budget -= i64::from(*stale >= 0);
            let m = &running[*i];
            self.read_at.lock().unwrap().insert((m.id, player.user_id), now);
            match self.refresh(m, player).await {
                Ok(n) => *changed.entry(m.id).or_insert(0) += n,
                // One unreachable handle must not stall the others or the ending.
                Err(e) => tracing::warn!("Skipping {} in mashup {}: {e:#}", player.handle, m.id),
            }
        }
        Ok((changed, due.iter().filter(|d| d.0 >= 0).count()))
    }
}

fn ends_at(m: &MashupRow) -> Result<i64> {
    m.ends_at.ok_or_else(|| anyhow!("active contest {} has no end", m.id))
}

fn team_of(p: &MashupPlayer) -> Result<i64> {
    p.team.ok_or_else(|| anyhow!("player {} has no team", p.user_id))
}
