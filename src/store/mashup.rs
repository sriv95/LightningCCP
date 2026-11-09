//! Mashups (contests, bingo): `mashups`, `mashup_problems`, `mashup_players`, `mashup_solves`,
//! `mashup_excluded` and `mashup_logged`. Port of `src/db/store.py` lines 980-1348.
use std::collections::HashSet;

use rusqlite::{Row, Transaction, params, params_from_iter};

use super::base::{Store, is_constraint_violation};
use crate::errors::BotError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MashupRow {
    pub id: i64,
    pub guild_id: i64,
    pub channel_id: i64,
    pub message_id: Option<i64>,
    pub host_id: i64,
    pub min_rating: i64,
    pub max_rating: i64,
    pub count: i64,
    pub duration_seconds: i64,
    pub penalty_minutes: i64,
    pub status: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub teams: i64,
    pub max_players: i64,
    pub rated: i64,
    pub team_size: i64,
    pub tags: String,
    pub exclude_tags: String,
    pub contest_id: Option<i64>,
    /// `contest`, or bingo: `classic` or `replace`.
    pub mode: String,
    /// Side of a bingo board.
    pub size: i64,
    /// Classic bingo: `line` (first full line) or `continuous` (play to the end).
    pub win: String,
    /// The thread of a bingo's solve log.
    pub thread_id: Option<i64>,
    /// The newest thread message, which carries the board.
    pub log_board_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MashupCell {
    pub idx: i64,
    pub key: String,
    pub rating: i64,
    pub cell: Option<i64>,
    /// Solves count from here (the contest start unless a bingo cell was replaced).
    pub since: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MashupPlayer {
    pub user_id: i64,
    pub handle: String,
    pub joined_at: i64,
    pub team: Option<i64>,
    pub delta: Option<i64>,
    /// False once the player gave up (or the contest ended).
    pub active: bool,
}

/// Arguments of [`Store::create_mashup`]; `Default` carries the Python keyword defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMashup {
    pub guild_id: i64,
    pub channel_id: i64,
    pub host_id: i64,
    pub host_handle: String,
    pub min_rating: i64,
    pub max_rating: i64,
    pub count: i64,
    pub duration_seconds: i64,
    pub penalty_minutes: i64,
    pub now: i64,
    pub teams: i64,
    pub host_team: Option<i64>,
    pub max_players: i64,
    pub rated: bool,
    pub team_size: i64,
    pub tags: String,
    pub exclude_tags: String,
    pub contest_id: Option<i64>,
    pub mode: String,
    pub size: i64,
    pub win: String,
}

impl Default for NewMashup {
    fn default() -> Self {
        NewMashup {
            guild_id: 0,
            channel_id: 0,
            host_id: 0,
            host_handle: String::new(),
            min_rating: 0,
            max_rating: 0,
            count: 0,
            duration_seconds: 0,
            penalty_minutes: 0,
            now: 0,
            teams: 0,
            host_team: None,
            max_players: 20,
            rated: true,
            team_size: 0,
            tags: String::new(),
            exclude_tags: String::new(),
            contest_id: None,
            mode: "contest".into(),
            size: 0,
            win: "line".into(),
        }
    }
}

/// Python `_in_match`: the user is in a match as an active player.
fn in_match(tx: &Transaction, guild_id: i64, user_id: i64) -> rusqlite::Result<bool> {
    let mut stmt = tx.prepare("SELECT 1 FROM match_players WHERE guild_id = ? AND user_id = ? AND active = 1")?;
    stmt.exists([guild_id, user_id])
}

fn mashup_row(r: &Row) -> rusqlite::Result<MashupRow> {
    Ok(MashupRow {
        id: r.get("id")?,
        guild_id: r.get("guild_id")?,
        channel_id: r.get("channel_id")?,
        message_id: r.get("message_id")?,
        host_id: r.get("host_id")?,
        min_rating: r.get("min_rating")?,
        max_rating: r.get("max_rating")?,
        count: r.get("count")?,
        duration_seconds: r.get("duration_seconds")?,
        penalty_minutes: r.get("penalty_minutes")?,
        status: r.get("status")?,
        created_at: r.get("created_at")?,
        started_at: r.get("started_at")?,
        ends_at: r.get("ends_at")?,
        teams: r.get("teams")?,
        max_players: r.get("max_players")?,
        rated: r.get("rated")?,
        team_size: r.get("team_size")?,
        tags: r.get("tags")?,
        exclude_tags: r.get("exclude_tags")?,
        contest_id: r.get("contest_id")?,
        mode: r.get("mode")?,
        size: r.get("size")?,
        win: r.get("win")?,
        thread_id: r.get("thread_id")?,
        log_board_id: r.get("log_board_id")?,
    })
}

fn mashup_status(tx: &Transaction, mashup_id: i64) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    tx.query_row("SELECT status FROM mashups WHERE id = ?", [mashup_id], |r| r.get(0)).optional()
}

fn closed() -> anyhow::Error {
    BotError::MatchClosed("This contest is no longer open.".into()).into()
}

fn count(tx: &Transaction, sql: &str, p: impl rusqlite::Params) -> rusqlite::Result<i64> {
    tx.query_row(sql, p, |r| r.get(0))
}

/// A unique-index clash means the user is already playing (Python catches `sqlite3.IntegrityError`).
fn already_in_match(e: anyhow::Error) -> anyhow::Error {
    if is_constraint_violation(&e) { BotError::AlreadyInMatch.into() } else { e }
}

impl Store {
    pub async fn create_mashup(&self, m: NewMashup) -> anyhow::Result<i64> {
        self.tx(move |tx| {
            if in_match(tx, m.guild_id, m.host_id)? {
                return Err(BotError::AlreadyInMatch.into());
            }
            tx.execute(
                "INSERT INTO mashups (guild_id, channel_id, host_id, min_rating, max_rating, \
                 count, duration_seconds, penalty_minutes, status, created_at, teams, \
                 max_players, rated, team_size, tags, exclude_tags, contest_id, mode, size, win) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'lobby', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                params![
                    m.guild_id,
                    m.channel_id,
                    m.host_id,
                    m.min_rating,
                    m.max_rating,
                    m.count,
                    m.duration_seconds,
                    m.penalty_minutes,
                    m.now,
                    m.teams,
                    m.max_players,
                    m.rated as i64,
                    m.team_size,
                    m.tags,
                    m.exclude_tags,
                    m.contest_id,
                    m.mode,
                    m.size,
                    m.win
                ],
            )?;
            let mashup_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, team) VALUES (?, ?, ?, ?, ?, ?)",
                params![mashup_id, m.guild_id, m.host_id, m.host_handle, m.now, m.host_team],
            )?;
            Ok(mashup_id)
        })
        .await
        .map_err(already_in_match)
    }

    pub async fn get_mashup(&self, mashup_id: i64) -> anyhow::Result<Option<MashupRow>> {
        use rusqlite::OptionalExtension;
        self.run(move |c| Ok(c.query_row("SELECT * FROM mashups WHERE id = ?", [mashup_id], mashup_row).optional()?)).await
    }

    /// The unfinished contest (lobby or running) the user is in, if any.
    pub async fn active_mashup_for(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Option<i64>> {
        use rusqlite::OptionalExtension;
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT mp.mashup_id FROM mashup_players mp JOIN mashups m ON m.id = mp.mashup_id \
                 WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.active = 1 \
                 AND m.status IN ('lobby', 'active')",
                [guild_id, user_id],
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
    }

    pub async fn mashups_in(&self, statuses: &[&str]) -> anyhow::Result<Vec<MashupRow>> {
        let statuses: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
        self.run(move |c| {
            let marks = vec!["?"; statuses.len()].join(",");
            let mut stmt = c.prepare(&format!("SELECT * FROM mashups WHERE status IN ({marks}) ORDER BY id"))?;
            let rows = stmt.query_map(params_from_iter(&statuses), mashup_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn set_mashup_message(&self, mashup_id: i64, channel_id: i64, message_id: i64) -> anyhow::Result<()> {
        self.write("UPDATE mashups SET channel_id = ?, message_id = ? WHERE id = ?", (channel_id, message_id, mashup_id)).await?;
        Ok(())
    }

    /// The newest finished contests the user played in.
    pub async fn finished_mashups(&self, guild_id: i64, user_id: i64, limit: i64) -> anyhow::Result<Vec<MashupRow>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT m.* FROM mashups m JOIN mashup_players mp ON mp.mashup_id = m.id \
                 WHERE m.guild_id = ? AND mp.user_id = ? AND m.status = 'finished' \
                 ORDER BY m.ends_at DESC, m.id DESC LIMIT ?",
            )?;
            let rows = stmt.query_map([guild_id, user_id, limit], mashup_row)?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn mashup_problems(&self, mashup_id: i64) -> anyhow::Result<Vec<(String, i64)>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT problem_key, rating FROM mashup_problems WHERE mashup_id = ? ORDER BY idx")?;
            let rows = stmt.query_map([mashup_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Every problem of the contest in the order it was added.
    pub async fn mashup_cells(&self, mashup_id: i64) -> anyhow::Result<Vec<MashupCell>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT p.idx, p.problem_key, p.rating, p.cell, COALESCE(p.since, m.started_at) AS since \
                 FROM mashup_problems p JOIN mashups m ON m.id = p.mashup_id \
                 WHERE p.mashup_id = ? ORDER BY p.idx",
            )?;
            let rows = stmt.query_map([mashup_id], |r| {
                Ok(MashupCell { idx: r.get(0)?, key: r.get(1)?, rating: r.get(2)?, cell: r.get(3)?, since: r.get(4)? })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Puts a new problem on a bingo cell of a running contest, only if `after_idx` is still the cell's
    /// newest problem (so two reads that both saw the claim add one problem).
    pub async fn replace_cell(&self, mashup_id: i64, cell: i64, after_idx: i64, key: &str, rating: i64, since: i64) -> anyhow::Result<bool> {
        let key = key.to_string();
        self.tx(move |tx| {
            let newest: Option<i64> = tx.query_row(
                "SELECT MAX(idx) FROM mashup_problems WHERE mashup_id = ? AND cell = ? \
                 AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'active')",
                [mashup_id, cell, mashup_id],
                |r| r.get(0),
            )?;
            if newest != Some(after_idx) {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO mashup_problems (mashup_id, idx, problem_key, rating, cell, since) \
                 SELECT ?, MAX(idx) + 1, ?, ?, ?, ? FROM mashup_problems WHERE mashup_id = ?",
                params![mashup_id, key, rating, cell, since, mashup_id],
            )?;
            Ok(true)
        })
        .await
    }

    pub async fn mashup_players(&self, mashup_id: i64) -> anyhow::Result<Vec<MashupPlayer>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT user_id, handle, joined_at, team, delta, active FROM mashup_players \
                 WHERE mashup_id = ? ORDER BY joined_at, rowid",
            )?;
            let rows = stmt.query_map([mashup_id], |r| {
                Ok(MashupPlayer {
                    user_id: r.get(0)?,
                    handle: r.get(1)?,
                    joined_at: r.get(2)?,
                    team: r.get(3)?,
                    delta: r.get(4)?,
                    active: r.get::<_, i64>(5)? != 0,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_mashup_player(
        &self,
        mashup_id: i64,
        guild_id: i64,
        user_id: i64,
        handle: &str,
        now: i64,
        cap: i64,
        team: Option<i64>,
        team_size: i64,
    ) -> anyhow::Result<()> {
        let handle = handle.to_string();
        self.tx(move |tx| {
            if mashup_status(tx, mashup_id)?.as_deref() != Some("lobby") {
                return Err(closed());
            }
            if count(tx, "SELECT COUNT(*) FROM mashup_players WHERE mashup_id = ?", [mashup_id])? >= cap {
                return Err(BotError::match_full().into());
            }
            if team_size != 0
                && count(tx, "SELECT COUNT(*) FROM mashup_players WHERE mashup_id = ? AND team = ?", params![mashup_id, team])? >= team_size
            {
                return Err(BotError::MatchFull("That team is full.".into()).into());
            }
            if in_match(tx, guild_id, user_id)? {
                return Err(BotError::AlreadyInMatch.into());
            }
            tx.execute(
                "INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, team) VALUES (?, ?, ?, ?, ?, ?)",
                params![mashup_id, guild_id, user_id, handle, now, team],
            )?;
            Ok(())
        })
        .await
        .map_err(already_in_match)
    }

    /// Puts a lobby player into another team, unless that team is full.
    pub async fn move_mashup_player(&self, mashup_id: i64, user_id: i64, team: i64, team_size: i64) -> anyhow::Result<()> {
        self.tx(move |tx| {
            if mashup_status(tx, mashup_id)?.as_deref() != Some("lobby") {
                return Err(closed());
            }
            if team_size != 0
                && count(tx, "SELECT COUNT(*) FROM mashup_players WHERE mashup_id = ? AND team = ? AND user_id != ?", [mashup_id, team, user_id])?
                    >= team_size
            {
                return Err(BotError::MatchFull("That team is full.".into()).into());
            }
            tx.execute("UPDATE mashup_players SET team = ? WHERE mashup_id = ? AND user_id = ?", [team, mashup_id, user_id])?;
            Ok(())
        })
        .await
    }

    pub async fn remove_mashup_player(&self, mashup_id: i64, user_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write(
                "DELETE FROM mashup_players WHERE mashup_id = ? AND user_id = ? \
                 AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'lobby')",
                (mashup_id, user_id, mashup_id),
            )
            .await?
            > 0)
    }

    pub async fn cancel_mashup(&self, mashup_id: i64) -> anyhow::Result<bool> {
        self.tx(move |tx| {
            if tx.execute("UPDATE mashups SET status = 'cancelled' WHERE id = ? AND status = 'lobby'", [mashup_id])? == 0 {
                return Ok(false);
            }
            tx.execute("UPDATE mashup_players SET active = 0 WHERE mashup_id = ?", [mashup_id])?;
            Ok(true)
        })
        .await
    }

    /// Starts the contest only if it is still a lobby with exactly these players. `excluded` are problem
    /// keys later replacements must avoid.
    pub async fn start_mashup(
        &self,
        mashup_id: i64,
        problems: &[(String, i64)],
        started_at: i64,
        ends_at: i64,
        player_ids: &[i64],
        excluded: &[String],
    ) -> anyhow::Result<bool> {
        let problems = problems.to_vec();
        let wanted: HashSet<i64> = player_ids.iter().copied().collect();
        let excluded: HashSet<String> = excluded.iter().cloned().collect();
        self.tx(move |tx| {
            if mashup_status(tx, mashup_id)?.as_deref() != Some("lobby") {
                return Ok(false);
            }
            let mut stmt = tx.prepare("SELECT user_id FROM mashup_players WHERE mashup_id = ?")?;
            let players: HashSet<i64> = stmt.query_map([mashup_id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            if players != wanted {
                return Ok(false);
            }
            tx.execute("UPDATE mashups SET status = 'active', started_at = ?, ends_at = ? WHERE id = ?", [started_at, ends_at, mashup_id])?;
            let mut insert = tx.prepare("INSERT INTO mashup_problems (mashup_id, idx, problem_key, rating, cell) VALUES (?, ?, ?, ?, ?)")?;
            for (i, (key, rating)) in problems.iter().enumerate() {
                insert.execute(params![mashup_id, i as i64, key, rating, i as i64])?;
            }
            let mut insert = tx.prepare("INSERT INTO mashup_excluded (mashup_id, problem_key) VALUES (?, ?)")?;
            for key in &excluded {
                insert.execute(params![mashup_id, key])?;
            }
            Ok(true)
        })
        .await
    }

    pub async fn set_mashup_thread(&self, mashup_id: i64, thread_id: i64) -> anyhow::Result<()> {
        self.write("UPDATE mashups SET thread_id = ? WHERE id = ?", (thread_id, mashup_id)).await?;
        Ok(())
    }

    pub async fn set_log_board(&self, mashup_id: i64, message_id: i64) -> anyhow::Result<()> {
        self.write("UPDATE mashups SET log_board_id = ? WHERE id = ?", (message_id, mashup_id)).await?;
        Ok(())
    }

    /// True only the first time, so a claim is posted to the thread at most once.
    pub async fn mark_logged(&self, mashup_id: i64, problem_key: &str) -> anyhow::Result<bool> {
        let key = problem_key.to_string();
        self.tx(move |tx| Ok(tx.execute("INSERT OR IGNORE INTO mashup_logged (mashup_id, problem_key) VALUES (?, ?)", params![mashup_id, key])? > 0))
            .await
    }

    pub async fn mashup_excluded(&self, mashup_id: i64) -> anyhow::Result<HashSet<String>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT problem_key FROM mashup_excluded WHERE mashup_id = ?")?;
            let rows = stmt.query_map([mashup_id], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    /// Stores new (solve time, wrong attempts) per problem; the first record of each wins. Returns how many
    /// were new.
    pub async fn record_mashup_solves(&self, mashup_id: i64, user_id: i64, solves: &[(String, (i64, i64))]) -> anyhow::Result<usize> {
        let solves = solves.to_vec();
        self.tx(move |tx| {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO mashup_solves (mashup_id, user_id, problem_key, solved_at, wrong) VALUES (?, ?, ?, ?, ?)",
            )?;
            let mut new = 0;
            for (key, (solved_at, wrong)) in &solves {
                new += stmt.execute(params![mashup_id, user_id, key, solved_at, wrong])?;
            }
            Ok(new)
        })
        .await
    }

    /// User id to (problem key to (solve time, wrong attempts)), users and problems in row order.
    #[allow(clippy::type_complexity)]
    pub async fn mashup_solves(&self, mashup_id: i64) -> anyhow::Result<Vec<(i64, Vec<(String, (i64, i64))>)>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT user_id, problem_key, solved_at, wrong FROM mashup_solves WHERE mashup_id = ?")?;
            let rows = stmt.query_map([mashup_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)))?;
            let mut out: Vec<(i64, Vec<(String, (i64, i64))>)> = Vec::new();
            for row in rows {
                let (user, key, solved_at, wrong) = row?;
                // first-seen order, like the Python dict
                match out.iter_mut().find(|(u, _)| *u == user) {
                    Some((_, solved)) => solved.push((key, (solved_at, wrong))),
                    None => out.push((user, vec![(key, (solved_at, wrong))])),
                }
            }
            Ok(out)
        })
        .await
    }

    /// Frees a player of a running contest for other activities; they keep their place.
    pub async fn mark_mashup_gave_up(&self, mashup_id: i64, user_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write(
                "UPDATE mashup_players SET active = 0 WHERE mashup_id = ? AND user_id = ? \
                 AND active = 1 AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'active')",
                (mashup_id, user_id, mashup_id),
            )
            .await?
            > 0)
    }

    /// Ends an active contest (at `now` if that is before its scheduled end) and stores each player's
    /// rating change. False when another request already ended it, so ratings are never applied twice.
    pub async fn finish_mashup(&self, mashup_id: i64, deltas: &[(i64, i64)], now: Option<i64>) -> anyhow::Result<bool> {
        let deltas = deltas.to_vec();
        self.tx(move |tx| {
            let changed = tx.execute(
                "UPDATE mashups SET status = 'finished', ends_at = MIN(ends_at, COALESCE(?, ends_at)) \
                 WHERE id = ? AND status = 'active'",
                params![now, mashup_id],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let mut stmt = tx.prepare("UPDATE mashup_players SET delta = ? WHERE mashup_id = ? AND user_id = ?")?;
            for (user_id, delta) in &deltas {
                stmt.execute([delta, &mashup_id, user_id])?;
            }
            drop(stmt);
            tx.execute("UPDATE mashup_players SET active = 0 WHERE mashup_id = ?", [mashup_id])?;
            Ok(true)
        })
        .await
    }
}
