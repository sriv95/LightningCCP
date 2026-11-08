//! Matches, the daily problem, ratings and stats (port of `src/db/store.py` lines 592-980).
use std::collections::HashSet;

use rusqlite::{OptionalExtension, Row, Transaction, params, params_from_iter};

use super::base::{Store, is_constraint_violation};
use crate::errors::BotError;
use crate::logic::base::{DAILY_MULTIPLIER, DAY_SECONDS, START_RATING};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchRow {
    pub id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub message_id: Option<i64>,
    pub host_id: i64,
    pub mode: String,
    pub max_players: i64,
    pub invited_id: Option<i64>,
    pub rating_arg: Option<i64>,
    pub rating: Option<i64>,
    pub problem_key: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub tags: String,
    pub exclude_tags: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchPlayer {
    pub match_id: i64,
    pub guild_id: i64,
    pub user_id: i64,
    pub handle: String,
    pub joined_at: i64,
    pub solved_at: Option<i64>,
    pub gave_up: bool,
    pub delta: Option<i64>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyRow {
    pub guild_id: i64,
    pub user_id: i64,
    pub day: i64,
    pub problem_key: Option<String>,
    pub rating: Option<i64>,
    pub issued_at: Option<i64>,
    pub solved_at: Option<i64>,
    pub match_id: Option<i64>,
}

/// Arguments of [`Store::create_match`] (Python keyword-only parameters; `tags` and `exclude_tags` default to `""`).
#[derive(Debug, Clone, Default)]
pub struct NewMatch {
    pub guild_id: i64,
    pub channel_id: i64,
    pub host_id: i64,
    pub host_handle: String,
    pub mode: String,
    pub max_players: i64,
    pub invited_id: Option<i64>,
    pub rating_arg: Option<i64>,
    pub now: i64,
    pub tags: String,
    pub exclude_tags: String,
}

/// Arguments of [`Store::record_daily_solve`].
#[derive(Debug, Clone, Default)]
pub struct DailySolve {
    pub guild_id: i64,
    pub channel_id: i64,
    pub user_id: i64,
    pub handle: String,
    pub day: i64,
    pub solved_at: i64,
    pub delta: i64,
    pub now: i64,
}

fn match_row(r: &Row) -> rusqlite::Result<MatchRow> {
    Ok(MatchRow {
        id: r.get("id")?,
        guild_id: r.get("guild_id")?,
        channel_id: r.get("channel_id")?,
        message_id: r.get("message_id")?,
        host_id: r.get("host_id")?,
        mode: r.get("mode")?,
        max_players: r.get("max_players")?,
        invited_id: r.get("invited_id")?,
        rating_arg: r.get("rating_arg")?,
        rating: r.get("rating")?,
        problem_key: r.get("problem_key")?,
        status: r.get("status")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        expires_at: r.get("expires_at")?,
        finished_at: r.get("finished_at")?,
        tags: r.get("tags")?,
        exclude_tags: r.get("exclude_tags")?,
    })
}

fn player_row(r: &Row) -> rusqlite::Result<MatchPlayer> {
    Ok(MatchPlayer {
        match_id: r.get("match_id")?,
        guild_id: r.get("guild_id")?,
        user_id: r.get("user_id")?,
        handle: r.get("handle")?,
        joined_at: r.get("joined_at")?,
        solved_at: r.get("solved_at")?,
        gave_up: r.get("gave_up")?,
        delta: r.get("delta")?,
        active: r.get("active")?,
    })
}

/// Python `_in_mashup`: the user is in an active mashup contest of the guild.
pub(crate) fn in_mashup(tx: &Transaction, guild_id: i64, user_id: i64) -> anyhow::Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM mashup_players WHERE guild_id = ? AND user_id = ? AND active = 1",
            (guild_id, user_id),
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Python catches `sqlite3.IntegrityError` around the match writes and raises `AlreadyInMatch`.
fn already_in_match(error: anyhow::Error) -> anyhow::Error {
    if is_constraint_violation(&error) { BotError::AlreadyInMatch.into() } else { error }
}

/// Python `_claim_daily`: true when the user had no solved daily for that day yet.
fn claim_daily(tx: &Transaction, guild_id: i64, user_id: i64, day: i64, now: i64, match_id: i64) -> anyhow::Result<bool> {
    Ok(tx.execute(
        "INSERT INTO daily (guild_id, user_id, day, solved_at, match_id) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT (guild_id, user_id, day) DO UPDATE SET \
         solved_at = excluded.solved_at, match_id = excluded.match_id \
         WHERE daily.solved_at IS NULL",
        params![guild_id, user_id, day, now, match_id],
    )? > 0)
}

/// A player's rating changes come from finished matches and finished mashups.
const CHANGES: &str = "SELECT guild_id, user_id, handle, delta FROM match_players WHERE delta IS NOT NULL \
                       UNION ALL \
                       SELECT guild_id, user_id, handle, delta FROM mashup_players WHERE delta IS NOT NULL";

// --- matches ---
impl Store {
    pub async fn create_match(&self, m: NewMatch) -> anyhow::Result<i64> {
        self.tx(move |tx| {
            if in_mashup(tx, m.guild_id, m.host_id)? {
                return Err(BotError::AlreadyInMatch.into());
            }
            tx.execute(
                "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, \
                 invited_id, rating_arg, status, created_at, tags, exclude_tags) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'lobby', ?, ?, ?)",
                params![m.guild_id, m.channel_id, m.host_id, m.mode, m.max_players, m.invited_id, m.rating_arg, m.now, m.tags, m.exclude_tags],
            )?;
            let match_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at) VALUES (?, ?, ?, ?, ?)",
                params![match_id, m.guild_id, m.host_id, m.host_handle, m.now],
            )?;
            Ok(match_id)
        })
        .await
        .map_err(already_in_match)
    }

    pub async fn get_match(&self, match_id: i64) -> anyhow::Result<Option<MatchRow>> {
        self.run(move |c| Ok(c.query_row("SELECT * FROM matches WHERE id = ?", [match_id], match_row).optional()?)).await
    }

    pub async fn get_players(&self, match_id: i64) -> anyhow::Result<Vec<MatchPlayer>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT * FROM match_players WHERE match_id = ? ORDER BY joined_at, rowid")?;
            Ok(stmt.query_map([match_id], player_row)?.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn active_match_for(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Option<i64>> {
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT mp.match_id FROM match_players mp JOIN matches m ON m.id = mp.match_id \
                 WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.active = 1 \
                 AND m.status IN ('lobby', 'active')",
                (guild_id, user_id),
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
    }

    pub async fn set_message(&self, match_id: i64, channel_id: i64, message_id: i64) -> anyhow::Result<()> {
        self.write("UPDATE matches SET channel_id = ?, message_id = ? WHERE id = ?", (channel_id, message_id, match_id)).await?;
        Ok(())
    }

    pub async fn add_player(&self, match_id: i64, guild_id: i64, user_id: i64, handle: &str, now: i64) -> anyhow::Result<()> {
        let handle = handle.to_string();
        self.tx(move |tx| {
            let found: Option<(String, i64)> = tx
                .query_row("SELECT status, max_players FROM matches WHERE id = ?", [match_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?;
            let Some((_, max_players)) = found.filter(|(status, _)| status == "lobby") else {
                return Err(BotError::match_closed().into());
            };
            let count: i64 = tx.query_row("SELECT COUNT(*) FROM match_players WHERE match_id = ?", [match_id], |r| r.get(0))?;
            if count >= max_players {
                return Err(BotError::match_full().into());
            }
            if in_mashup(tx, guild_id, user_id)? {
                return Err(BotError::AlreadyInMatch.into());
            }
            tx.execute(
                "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at) VALUES (?, ?, ?, ?, ?)",
                params![match_id, guild_id, user_id, handle, now],
            )?;
            Ok(())
        })
        .await
        .map_err(already_in_match)
    }

    pub async fn remove_player(&self, match_id: i64, user_id: i64) -> anyhow::Result<bool> {
        let n = self
            .write(
                "DELETE FROM match_players WHERE match_id = ? AND user_id = ? \
                 AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'lobby')",
                (match_id, user_id, match_id),
            )
            .await?;
        Ok(n > 0)
    }

    pub async fn cancel_match(&self, match_id: i64, now: i64) -> anyhow::Result<bool> {
        self.tx(move |tx| {
            let n = tx.execute(
                "UPDATE matches SET status = 'cancelled', finished_at = ? WHERE id = ? AND status = 'lobby'",
                (now, match_id),
            )?;
            if n == 0 {
                return Ok(false);
            }
            tx.execute("UPDATE match_players SET active = 0 WHERE match_id = ?", [match_id])?;
            Ok(true)
        })
        .await
    }

    /// Activate a lobby only if its players are still exactly `player_ids`, the set the problem was chosen for.
    pub async fn start_match(&self, match_id: i64, rating: i64, problem_key: &str, now: i64, expires_at: i64, player_ids: &[i64]) -> anyhow::Result<bool> {
        let problem_key = problem_key.to_string();
        let wanted: HashSet<i64> = player_ids.iter().copied().collect();
        self.tx(move |tx| {
            let status: Option<String> = tx.query_row("SELECT status FROM matches WHERE id = ?", [match_id], |r| r.get(0)).optional()?;
            if status.as_deref() != Some("lobby") {
                return Ok(false);
            }
            let mut stmt = tx.prepare("SELECT user_id FROM match_players WHERE match_id = ?")?;
            let present: HashSet<i64> = stmt.query_map([match_id], |r| r.get(0))?.collect::<Result<_, _>>()?;
            drop(stmt);
            if present != wanted {
                return Ok(false);
            }
            tx.execute(
                "UPDATE matches SET status = 'active', rating = ?, problem_key = ?, started_at = ?, expires_at = ? WHERE id = ?",
                params![rating, problem_key, now, expires_at, match_id],
            )?;
            Ok(true)
        })
        .await
    }

    pub async fn mark_gave_up(&self, match_id: i64, user_id: i64) -> anyhow::Result<bool> {
        let n = self
            .write(
                "UPDATE match_players SET gave_up = 1, active = 0 \
                 WHERE match_id = ? AND user_id = ? AND gave_up = 0 AND solved_at IS NULL \
                 AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'active')",
                (match_id, user_id, match_id),
            )
            .await?;
        Ok(n > 0)
    }

    /// `solves` is `(user_id, solved_at)` in the caller's order (Python takes a dict, so ids are unique).
    pub async fn record_solves(&self, match_id: i64, solves: &[(i64, i64)]) -> anyhow::Result<()> {
        let solves = solves.to_vec();
        self.tx(move |tx| {
            let mut stmt = tx.prepare(
                "UPDATE match_players SET solved_at = ? WHERE match_id = ? AND user_id = ? \
                 AND solved_at IS NULL \
                 AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'active')",
            )?;
            for (uid, t) in solves {
                stmt.execute((t, match_id, uid, match_id))?;
            }
            Ok(())
        })
        .await
    }

    /// `deltas` is `(user_id, change)`. `bonus_day` (solo only, then `deltas` has exactly one entry): the UTC day;
    /// a solve that is the user's first claim of that day doubles the rating change.
    pub async fn finish_match(&self, match_id: i64, now: i64, deltas: &[(i64, i64)], bonus_day: Option<i64>) -> anyhow::Result<bool> {
        let mut deltas = deltas.to_vec();
        self.tx(move |tx| {
            let n = tx.execute("UPDATE matches SET status = 'finished', finished_at = ? WHERE id = ? AND status = 'active'", (now, match_id))?;
            if n == 0 {
                return Ok(false);
            }
            if let Some(day) = bonus_day {
                // Python unpacks `((uid, gain),) = deltas.items()`: anything but one entry raises ValueError
                let [(uid, gain)] = deltas[..] else {
                    anyhow::bail!("bonus_day needs exactly one delta, got {}", deltas.len());
                };
                let guild_id: i64 = tx.query_row("SELECT guild_id FROM matches WHERE id = ?", [match_id], |r| r.get(0))?;
                if gain > 0 && claim_daily(tx, guild_id, uid, day, now, match_id)? {
                    deltas = vec![(uid, gain * DAILY_MULTIPLIER)];
                }
            }
            let mut stmt = tx.prepare("UPDATE match_players SET delta = ? WHERE match_id = ? AND user_id = ?")?;
            for (uid, d) in deltas {
                stmt.execute((d, match_id, uid))?;
            }
            drop(stmt);
            tx.execute("UPDATE match_players SET active = 0 WHERE match_id = ?", [match_id])?;
            Ok(true)
        })
        .await
    }

    pub async fn matches_due(&self, lobby_before: i64, now: i64) -> anyhow::Result<Vec<MatchRow>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT * FROM matches WHERE (status = 'lobby' AND created_at <= ?) OR (status = 'active' AND expires_at <= ?)",
            )?;
            Ok(stmt.query_map((lobby_before, now), match_row)?.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// The player's newest finished matches (Python default `limit` is 100) with all their players.
    pub async fn finished_matches(&self, guild_id: i64, user_id: i64, limit: i64) -> anyhow::Result<Vec<(MatchRow, Vec<MatchPlayer>)>> {
        self.run(move |c| {
            let rows: Vec<MatchRow> = c
                .prepare(
                    "SELECT m.* FROM matches m JOIN match_players mp ON mp.match_id = m.id \
                     WHERE m.guild_id = ? AND mp.user_id = ? AND m.status = 'finished' \
                     ORDER BY m.finished_at DESC, m.id DESC LIMIT ?",
                )?
                .query_map((guild_id, user_id, limit), match_row)?
                .collect::<Result<_, _>>()?;
            if rows.is_empty() {
                return Ok(vec![]);
            }
            let marks = vec!["?"; rows.len()].join(",");
            let mut stmt = c.prepare(&format!("SELECT * FROM match_players WHERE match_id IN ({marks}) ORDER BY joined_at, rowid"))?;
            let players = stmt.query_map(params_from_iter(rows.iter().map(|m| m.id)), player_row)?;
            let mut by_match: std::collections::HashMap<i64, Vec<MatchPlayer>> = std::collections::HashMap::new();
            for p in players {
                let p = p?;
                by_match.entry(p.match_id).or_default().push(p);
            }
            Ok(rows
                .into_iter()
                .map(|m| {
                    let players = by_match.remove(&m.id).unwrap_or_default();
                    (m, players)
                })
                .collect())
        })
        .await
    }
}

// --- daily problem ---
impl Store {
    pub async fn get_daily(&self, guild_id: i64, user_id: i64, day: i64) -> anyhow::Result<Option<DailyRow>> {
        self.run(move |c| {
            Ok(c.query_row("SELECT * FROM daily WHERE guild_id = ? AND user_id = ? AND day = ?", (guild_id, user_id, day), |r| {
                Ok(DailyRow {
                    guild_id: r.get("guild_id")?,
                    user_id: r.get("user_id")?,
                    day: r.get("day")?,
                    problem_key: r.get("problem_key")?,
                    rating: r.get("rating")?,
                    issued_at: r.get("issued_at")?,
                    solved_at: r.get("solved_at")?,
                    match_id: r.get("match_id")?,
                })
            })
            .optional()?)
        })
        .await
    }

    /// `(user_id, UTC day)` pairs with a claimed daily, a solved match or a contest solve, newest day first.
    async fn streak_days(&self, guild_id: i64, user_id: Option<i64>) -> anyhow::Result<Vec<(i64, i64)>> {
        let who = if user_id.is_none() { "" } else { " AND user_id = ?" };
        let who_mashup = who.replace("user_id", "s.user_id");
        let args: Vec<i64> = std::iter::once(guild_id).chain(user_id).collect();
        let args = args.repeat(3);
        let sql = format!(
            "SELECT user_id, solved_at / {DAY_SECONDS} AS day FROM match_players \
             WHERE guild_id = ? AND solved_at IS NOT NULL{who} \
             UNION SELECT s.user_id, s.solved_at / {DAY_SECONDS} FROM mashup_solves s \
             JOIN mashups m ON m.id = s.mashup_id WHERE m.guild_id = ?{who_mashup} \
             UNION SELECT user_id, day FROM daily \
             WHERE guild_id = ? AND solved_at IS NOT NULL{who} \
             ORDER BY day DESC"
        );
        self.run(move |c| {
            let mut stmt = c.prepare(&sql)?;
            Ok(stmt.query_map(params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// UTC days with a claimed daily, a solved match or a contest solve, newest first.
    pub async fn daily_days(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Vec<i64>> {
        Ok(self.streak_days(guild_id, Some(user_id)).await?.into_iter().map(|(_, d)| d).collect())
    }

    /// Every user's streak days in this server, newest first; users in order of first appearance (a Python dict's
    /// insertion order).
    pub async fn daily_days_by_user(&self, guild_id: i64) -> anyhow::Result<Vec<(i64, Vec<i64>)>> {
        let mut out: Vec<(i64, Vec<i64>)> = Vec::new();
        for (uid, day) in self.streak_days(guild_id, None).await? {
            match out.iter_mut().find(|(u, _)| *u == uid) {
                Some((_, days)) => days.push(day),
                None => out.push((uid, vec![day])),
            }
        }
        Ok(out)
    }

    /// Today's problem; an unsolved one is replaced only when it has a different rating.
    pub async fn issue_daily(&self, guild_id: i64, user_id: i64, day: i64, problem_key: &str, rating: i64, now: i64) -> anyhow::Result<()> {
        let problem_key = problem_key.to_string();
        self.write(
            "INSERT INTO daily (guild_id, user_id, day, problem_key, rating, issued_at) \
             VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (guild_id, user_id, day) DO UPDATE SET \
             problem_key = excluded.problem_key, rating = excluded.rating, \
             issued_at = excluded.issued_at \
             WHERE daily.solved_at IS NULL AND daily.rating IS NOT excluded.rating",
            (guild_id, user_id, day, problem_key, rating, now),
        )
        .await?;
        Ok(())
    }

    /// Mark the issued daily solved and book its rating change as a finished solo match.
    /// False when it was already solved (by a concurrent request or a solo match).
    pub async fn record_daily_solve(&self, s: DailySolve) -> anyhow::Result<bool> {
        self.tx(move |tx| {
            let daily: Option<(Option<String>, Option<i64>, Option<i64>)> = tx
                .query_row(
                    "SELECT problem_key, rating, issued_at FROM daily \
                     WHERE guild_id = ? AND user_id = ? AND day = ? AND solved_at IS NULL",
                    (s.guild_id, s.user_id, s.day),
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((problem_key, rating, issued_at)) = daily else {
                return Ok(false);
            };
            tx.execute(
                "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, rating, \
                 problem_key, status, created_at, started_at, finished_at) \
                 VALUES (?, ?, ?, 'solo', 1, ?, ?, 'finished', ?, ?, ?)",
                params![s.guild_id, s.channel_id, s.user_id, rating, problem_key, issued_at, issued_at, s.now],
            )?;
            let match_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, solved_at, delta, active) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
                params![match_id, s.guild_id, s.user_id, s.handle, issued_at, s.solved_at, s.delta],
            )?;
            tx.execute(
                "UPDATE daily SET solved_at = ?, match_id = ? WHERE guild_id = ? AND user_id = ? AND day = ?",
                (s.now, match_id, s.guild_id, s.user_id, s.day),
            )?;
            Ok(true)
        })
        .await
    }
}

// --- ratings and stats ---
impl Store {
    /// Match rating: the start rating plus every stored rating change.
    pub async fn rating(&self, guild_id: i64, user_id: i64) -> anyhow::Result<i64> {
        let sum: i64 = self
            .run(move |c| {
                Ok(c.query_row(
                    &format!("SELECT COALESCE(SUM(delta), 0) FROM ({CHANGES}) WHERE guild_id = ? AND user_id = ?"),
                    (guild_id, user_id),
                    |r| r.get(0),
                )?)
            })
            .await?;
        Ok(START_RATING + sum)
    }

    /// `(user_id, handle, match rating, games played)`, best rated first. `kind` picks finished matches ("matches"),
    /// finished mashups ("contests") or both (anything else, Python uses "all"); the rating counts only the changes
    /// of that kind.
    pub async fn stats(&self, guild_id: i64, kind: &str) -> anyhow::Result<Vec<(i64, String, i64, i64)>> {
        let source = match kind {
            "matches" => "SELECT guild_id, user_id, handle, delta FROM match_players WHERE delta IS NOT NULL",
            "contests" => "SELECT guild_id, user_id, handle, delta FROM mashup_players WHERE delta IS NOT NULL",
            _ => CHANGES,
        };
        let sql = format!(
            "SELECT c.user_id, COALESCE(uh.handle, MAX(c.handle)) AS handle, \
             ? + SUM(c.delta) AS total, COUNT(*) AS played FROM ({source}) c \
             LEFT JOIN user_handle uh ON uh.guild_id = c.guild_id AND uh.user_id = c.user_id \
             WHERE c.guild_id = ? \
             GROUP BY c.user_id ORDER BY total DESC, played DESC, handle"
        );
        self.run(move |c| {
            let mut stmt = c.prepare(&sql)?;
            Ok(stmt.query_map((START_RATING, guild_id), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// `(finished_at, rating change)` of the player's finished matches and mashups, oldest first.
    pub async fn rating_changes(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Vec<(i64, i64)>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT m.finished_at AS at, mp.delta FROM match_players mp \
                 JOIN matches m ON m.id = mp.match_id \
                 WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.delta IS NOT NULL \
                 AND m.status = 'finished' \
                 UNION ALL \
                 SELECT k.ends_at AS at, kp.delta FROM mashup_players kp \
                 JOIN mashups k ON k.id = kp.mashup_id \
                 WHERE kp.guild_id = ? AND kp.user_id = ? AND kp.delta IS NOT NULL \
                 AND k.status = 'finished' ORDER BY at",
            )?;
            Ok(stmt.query_map((guild_id, user_id, guild_id, user_id), |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?)
        })
        .await
    }
}
