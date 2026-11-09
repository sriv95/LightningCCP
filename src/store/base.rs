//! The store core: connection, migrations, transaction helper, and the handle, pending-link, reminder, recap,
//! weekly, new-contest and Codeforces cache methods (port of `src/db/store.py`). The match and mashup methods
//! live in `matches.rs` and `mashup.rs` as further `impl Store` blocks and reach the connection through
//! [`Store::run`] and [`Store::tx`].

/// The 24 schema scripts, copied verbatim from `src/db/store.py` `MIGRATIONS` (whitespace is part of `sqlite_master.sql`).
/// Append only; never edit an old one.
pub static MIGRATIONS: [&str; 24] = [
    r#"
    CREATE TABLE user_handle (
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        handle TEXT NOT NULL COLLATE NOCASE,
        rating INTEGER,
        PRIMARY KEY (guild_id, user_id),
        UNIQUE (guild_id, handle)
    );
    CREATE TABLE pending_link (
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        handle TEXT NOT NULL,
        problem_key TEXT NOT NULL,
        issued_at INTEGER NOT NULL,
        PRIMARY KEY (guild_id, user_id)
    );
    CREATE TABLE challenge (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        problem_key TEXT NOT NULL,
        rating INTEGER NOT NULL,
        issued_at INTEGER NOT NULL,
        solved_at INTEGER
    );
    CREATE UNIQUE INDEX one_open_challenge ON challenge (guild_id, user_id)
        WHERE solved_at IS NULL;
    CREATE TABLE reminder (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER NOT NULL,
        role_id INTEGER,
        before_mins TEXT NOT NULL
    );
    CREATE TABLE reminder_sent (
        guild_id INTEGER NOT NULL,
        contest_id INTEGER NOT NULL,
        before_min INTEGER NOT NULL,
        PRIMARY KEY (guild_id, contest_id, before_min)
    );
    CREATE TABLE contest (
        id INTEGER PRIMARY KEY,
        name TEXT NOT NULL,
        type TEXT NOT NULL,
        phase TEXT NOT NULL,
        start_time INTEGER,
        duration INTEGER NOT NULL
    );
    CREATE TABLE problem (
        contest_id INTEGER NOT NULL,
        idx TEXT NOT NULL,
        name TEXT NOT NULL,
        rating INTEGER,
        tags TEXT NOT NULL,
        PRIMARY KEY (contest_id, idx)
    );
    "#,
    r#"
    DROP TABLE challenge;
    CREATE TABLE matches (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        message_id INTEGER,
        host_id INTEGER NOT NULL,
        mode TEXT NOT NULL CHECK (mode IN ('solo', 'duel', 'custom')),
        max_players INTEGER NOT NULL,
        invited_id INTEGER,
        rating_arg INTEGER,
        rating INTEGER,
        problem_key TEXT,
        status TEXT NOT NULL CHECK (status IN ('lobby', 'active', 'finished', 'cancelled')),
        created_at INTEGER NOT NULL,
        started_at INTEGER,
        expires_at INTEGER,
        finished_at INTEGER
    );
    CREATE INDEX matches_status ON matches (status);
    CREATE TABLE match_players (
        match_id INTEGER NOT NULL REFERENCES matches (id),
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        handle TEXT NOT NULL,
        joined_at INTEGER NOT NULL,
        solved_at INTEGER,
        gave_up INTEGER NOT NULL DEFAULT 0,
        delta INTEGER,
        active INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (match_id, user_id)
    );
    CREATE UNIQUE INDEX one_active_match ON match_players (guild_id, user_id) WHERE active = 1;
    "#,
    // Points became match ratings: zero the old point changes so everyone restarts at 800.
    r#"UPDATE match_players SET delta = 0 WHERE delta IS NOT NULL;"#,
    r#"
    CREATE TABLE mashups (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        message_id INTEGER,
        host_id INTEGER NOT NULL,
        started_at INTEGER NOT NULL,
        ends_at INTEGER NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('active', 'finished'))
    );
    CREATE INDEX mashups_status ON mashups (status);
    CREATE TABLE mashup_problems (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        idx INTEGER NOT NULL,
        problem_key TEXT NOT NULL,
        rating INTEGER NOT NULL,
        PRIMARY KEY (mashup_id, idx)
    );
    CREATE TABLE mashup_players (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        user_id INTEGER NOT NULL,
        handle TEXT NOT NULL,
        joined_at INTEGER NOT NULL,
        PRIMARY KEY (mashup_id, user_id)
    );
    CREATE TABLE mashup_solves (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        user_id INTEGER NOT NULL,
        problem_key TEXT NOT NULL,
        solved_at INTEGER NOT NULL,
        PRIMARY KEY (mashup_id, user_id, problem_key)
    );
    "#,
    // Mashup contests got a lobby, a penalty and a one-activity-per-user rule: rebuild their tables.
    r#"
    DROP TABLE mashup_solves;
    DROP TABLE mashup_players;
    DROP TABLE mashup_problems;
    DROP TABLE mashups;
    CREATE TABLE mashups (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        guild_id INTEGER NOT NULL,
        channel_id INTEGER NOT NULL,
        message_id INTEGER,
        host_id INTEGER NOT NULL,
        min_rating INTEGER NOT NULL,
        max_rating INTEGER NOT NULL,
        count INTEGER NOT NULL,
        duration_seconds INTEGER NOT NULL,
        penalty_minutes INTEGER NOT NULL,
        status TEXT NOT NULL CHECK (status IN ('lobby', 'active', 'finished', 'cancelled')),
        created_at INTEGER NOT NULL,
        started_at INTEGER,
        ends_at INTEGER
    );
    CREATE INDEX mashups_status ON mashups (status);
    CREATE TABLE mashup_problems (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        idx INTEGER NOT NULL,
        problem_key TEXT NOT NULL,
        rating INTEGER NOT NULL,
        PRIMARY KEY (mashup_id, idx)
    );
    CREATE TABLE mashup_players (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        handle TEXT NOT NULL,
        joined_at INTEGER NOT NULL,
        active INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (mashup_id, user_id)
    );
    CREATE UNIQUE INDEX one_active_mashup ON mashup_players (guild_id, user_id) WHERE active = 1;
    CREATE TABLE mashup_solves (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        user_id INTEGER NOT NULL,
        problem_key TEXT NOT NULL,
        solved_at INTEGER NOT NULL,
        wrong INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (mashup_id, user_id, problem_key)
    );
    "#,
    // One daily problem per user per UTC day. A solo match solved first that day claims the day
    // without a problem (problem_key stays NULL).
    r#"
    CREATE TABLE daily (
        guild_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        day INTEGER NOT NULL,
        problem_key TEXT,
        rating INTEGER,
        issued_at INTEGER,
        solved_at INTEGER,
        match_id INTEGER REFERENCES matches (id),
        PRIMARY KEY (guild_id, user_id, day)
    );
    "#,
    // Contest recaps are posted once per server and contest.
    r#"
    CREATE TABLE recap_sent (
        guild_id INTEGER NOT NULL,
        contest_id INTEGER NOT NULL,
        PRIMARY KEY (guild_id, contest_id)
    );
    "#,
    // Matches can filter their problem by tags (logic.normalize_tags form, '' for no filter).
    r#"
    ALTER TABLE matches ADD COLUMN tags TEXT NOT NULL DEFAULT '';
    ALTER TABLE matches ADD COLUMN exclude_tags TEXT NOT NULL DEFAULT '';
    "#,
    // Team mashups: teams is the number of teams (0 for an individual contest), team the player's.
    r#"
    ALTER TABLE mashups ADD COLUMN teams INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE mashup_players ADD COLUMN team INTEGER;
    "#,
    // The host can lower the player cap of a mashup (20 until now).
    r#"ALTER TABLE mashups ADD COLUMN max_players INTEGER NOT NULL DEFAULT 20;"#,
    // A finished mashup moves match ratings: the change is stored per player (NULL before it ends,
    // and for contests that ended earlier or had a single entrant).
    r#"ALTER TABLE mashup_players ADD COLUMN delta INTEGER;"#,
    // The host can open an unrated contest (rated = 0), which never changes ratings.
    r#"ALTER TABLE mashups ADD COLUMN rated INTEGER NOT NULL DEFAULT 1;"#,
    // A team contest can cap each team's size (0 for no per-team cap).
    r#"ALTER TABLE mashups ADD COLUMN team_size INTEGER NOT NULL DEFAULT 0;"#,
    // Contests can filter their problems by tags (logic.normalize_tags form, '' for no filter).
    r#"
    ALTER TABLE mashups ADD COLUMN tags TEXT NOT NULL DEFAULT '';
    ALTER TABLE mashups ADD COLUMN exclude_tags TEXT NOT NULL DEFAULT '';
    "#,
    // A virtual contest replays a finished Codeforces contest (NULL for a random mashup).
    r#"ALTER TABLE mashups ADD COLUMN contest_id INTEGER;"#,
    // Every contest is a team contest now (a team of one is a player): turn the individual
    // contests (teams = 0) into teams of one, numbered in joining order.
    r#"
    UPDATE mashup_players SET team = (
        SELECT COUNT(*) FROM mashup_players q WHERE q.mashup_id = mashup_players.mashup_id
        AND (q.joined_at, q.rowid) <= (mashup_players.joined_at, mashup_players.rowid)
    ) WHERE mashup_id IN (SELECT id FROM mashups WHERE teams = 0);
    UPDATE mashups SET team_size = 1, teams = MAX(1, CASE WHEN status = 'lobby' THEN max_players
        ELSE (SELECT COUNT(*) FROM mashup_players p WHERE p.mashup_id = mashups.id) END)
    WHERE teams = 0;
    "#,
    // Reminders and recaps get separate channels. A reminder row may have no channel (the channel
    // was cleared) and keeps its role, minutes and exclude regex; existing servers keep both.
    r#"
    CREATE TABLE reminder_new (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER,
        role_id INTEGER,
        before_mins TEXT NOT NULL,
        exclude TEXT NOT NULL DEFAULT ''
    );
    INSERT INTO reminder_new (guild_id, channel_id, role_id, before_mins)
        SELECT guild_id, channel_id, role_id, before_mins FROM reminder;
    CREATE TABLE recap (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER NOT NULL
    );
    INSERT INTO recap (guild_id, channel_id) SELECT guild_id, channel_id FROM reminder;
    DROP TABLE reminder;
    ALTER TABLE reminder_new RENAME TO reminder;
    "#,
    // Bingo: a mashup with mode classic or replace plays on a size x size board (win is "line" or
    // "continuous"); every problem sits in a cell, and a replace board stacks problems per cell,
    // each counting only from `since` (NULL = the start).
    r#"
    ALTER TABLE mashups ADD COLUMN mode TEXT NOT NULL DEFAULT 'contest';
    ALTER TABLE mashups ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE mashups ADD COLUMN win TEXT NOT NULL DEFAULT 'line';
    ALTER TABLE mashup_problems ADD COLUMN cell INTEGER;
    ALTER TABLE mashup_problems ADD COLUMN since INTEGER;
    "#,
    // A replace bingo never replaces a square with a problem someone in the lobby solved: the
    // keys solved before the start.
    r#"
    CREATE TABLE mashup_excluded (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        problem_key TEXT NOT NULL,
        PRIMARY KEY (mashup_id, problem_key)
    );
    "#,
    // A bingo has a thread for its solve log; mashup_logged remembers the claims already posted
    // there (at most once each).
    r#"
    ALTER TABLE mashups ADD COLUMN thread_id INTEGER;
    CREATE TABLE mashup_logged (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        problem_key TEXT NOT NULL,
        PRIMARY KEY (mashup_id, problem_key)
    );
    "#,
    // The thread message that carries the board (only the newest log message does).
    r#"ALTER TABLE mashups ADD COLUMN log_board_id INTEGER;"#,
    // The weekly leaderboard channel; last_sent is the start of the week (Sunday 00:00 UTC) whose
    // leaderboard was posted last, so each week posts at most once.
    r#"
    CREATE TABLE weekly (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER NOT NULL,
        last_sent INTEGER NOT NULL
    );
    "#,
    // New-contest announcements: the channel, the contests already posted per server (at-most-once),
    // and members who pressed Add to calendar (they get pinged in the contest's reminder).
    r#"
    CREATE TABLE newcontest (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER NOT NULL
    );
    CREATE TABLE newcontest_sent (
        guild_id INTEGER NOT NULL,
        contest_id INTEGER NOT NULL,
        PRIMARY KEY (guild_id, contest_id)
    );
    CREATE TABLE contest_remind (
        guild_id INTEGER NOT NULL,
        contest_id INTEGER NOT NULL,
        user_id INTEGER NOT NULL,
        PRIMARY KEY (guild_id, contest_id, user_id)
    );
    "#,
    // soon = 1 once the "starts within 48 hours" post of the contest went out
    r#"ALTER TABLE newcontest_sent ADD COLUMN soon INTEGER NOT NULL DEFAULT 0;"#,
];

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::cf::models::{Contest, Problem};
use crate::errors::BotError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedHandle {
    pub handle: String,
    pub rating: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub handle: String,
    pub problem_key: String,
    pub issued_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub guild_id: i64,
    pub channel_id: Option<i64>,
    pub role_id: Option<i64>,
    pub before_mins: Vec<i64>,
    pub exclude: String,
}

/// Python `json.dumps(list_of_ints)`: `[60, 10]` (note the space; the stored bytes must match Python's).
fn json_ints(values: &[i64]) -> String {
    let items: Vec<String> = values.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(", "))
}

/// Python `json.dumps(list_of_str)` with its defaults: `", "` separators and `ensure_ascii` escapes.
fn json_strings(values: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in values.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push('"');
        for ch in s.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{8}' => out.push_str("\\b"),
                '\u{c}' => out.push_str("\\f"),
                c if (' '..='~').contains(&c) => out.push(c),
                c => {
                    let mut units = [0u16; 2];
                    for u in c.encode_utf16(&mut units) {
                        out.push_str(&format!("\\u{u:04x}"));
                    }
                }
            }
        }
        out.push('"');
    }
    out.push(']');
    out
}

fn parse_list<T: serde::de::DeserializeOwned>(text: &str) -> anyhow::Result<Vec<T>> {
    Ok(serde_json::from_str(text)?)
}

/// Run every migration after the stored `user_version`, each in its own transaction with its version bump
/// (same statements as Python's `executescript("BEGIN; ...; PRAGMA user_version = n; COMMIT;")`).
pub fn migrate(conn: &mut rusqlite::Connection, migrations: &[&str]) -> rusqlite::Result<()> {
    let version = conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))? as usize;
    for (i, script) in migrations.iter().enumerate().skip(version) {
        let number = i + 1;
        let sql = format!("BEGIN;\n{script}\nPRAGMA user_version = {number};\nCOMMIT;");
        if let Err(e) = conn.execute_batch(&sql) {
            if !conn.is_autocommit() {
                conn.execute_batch("ROLLBACK")?;
            }
            return Err(e);
        }
    }
    Ok(())
}

pub fn is_constraint_violation(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<rusqlite::Error>(),
        Some(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

/// One SQLite connection on its own thread: every call runs to completion in order, so a closure given to
/// [`Store::tx`] is one serialized transaction (the role of Python's `_tx` lock).
pub struct Store {
    pub(crate) conn: tokio_rusqlite::Connection,
}

impl Store {
    pub async fn connect(path: impl AsRef<Path>) -> anyhow::Result<Store> {
        Self::connect_with(path, &MIGRATIONS).await
    }

    /// Like [`Store::connect`] with a custom migration list (tests).
    pub async fn connect_with(path: impl AsRef<Path>, migrations: &[&str]) -> anyhow::Result<Store> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let conn = tokio_rusqlite::Connection::open(path).await?;
        let migrations: Vec<String> = migrations.iter().map(|s| s.to_string()).collect();
        let store = Store { conn };
        let migrated = store
            .run(move |c| {
                // bundled SQLite defaults foreign keys on; Python's sqlite3 leaves them off, and migration 5 relies on it
                c.execute_batch("PRAGMA foreign_keys = OFF")?;
                let list: Vec<&str> = migrations.iter().map(String::as_str).collect();
                Ok(migrate(c, &list)?)
            })
            .await;
        if let Err(e) = migrated {
            let _ = store.conn.close().await;
            return Err(e);
        }
        Ok(store)
    }

    pub async fn close(self) -> anyhow::Result<()> {
        self.conn.close().await.map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Run `f` on the connection thread, outside any transaction (reads).
    pub(crate) async fn run<R, F>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&mut rusqlite::Connection) -> anyhow::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        match self.conn.call(f).await {
            Ok(v) => Ok(v),
            Err(tokio_rusqlite::Error::Error(e)) => Err(e),
            Err(e) => Err(anyhow::anyhow!("{e}")),
        }
    }

    /// One serialized transaction: commit when `f` returns `Ok`, roll back and return the error otherwise.
    pub(crate) async fn tx<R, F>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Transaction) -> anyhow::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.run(move |c| {
            let tx = c.transaction()?;
            let value = f(&tx)?;
            tx.commit()?;
            Ok(value)
        })
        .await
    }

    /// One write statement; returns the number of rows changed (Python `cursor.rowcount`).
    pub(crate) async fn write<P>(&self, sql: &'static str, params: P) -> anyhow::Result<usize>
    where
        P: rusqlite::Params + Send + 'static,
    {
        self.tx(move |tx| Ok(tx.execute(sql, params)?)).await
    }

    // --- handles ---
    pub async fn link_handle(&self, guild_id: i64, user_id: i64, handle: &str, rating: Option<i64>) -> anyhow::Result<()> {
        let owned = handle.to_string();
        let result = self
            .write(
                "INSERT INTO user_handle (guild_id, user_id, handle, rating) VALUES (?, ?, ?, ?) \
                 ON CONFLICT (guild_id, user_id) DO UPDATE SET \
                 handle = excluded.handle, rating = excluded.rating",
                (guild_id, user_id, owned, rating),
            )
            .await;
        match result {
            Err(e) if is_constraint_violation(&e) => {
                Err(BotError::HandleTaken(format!("{handle} is already linked to another member.")).into())
            }
            other => other.map(|_| ()),
        }
    }

    pub async fn get_handle(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Option<LinkedHandle>> {
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT handle, rating FROM user_handle WHERE guild_id = ? AND user_id = ?",
                (guild_id, user_id),
                |r| Ok(LinkedHandle { handle: r.get(0)?, rating: r.get(1)? }),
            )
            .optional()?)
        })
        .await
    }

    pub async fn linked_handles(&self, guild_id: i64) -> anyhow::Result<Vec<(i64, String)>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT user_id, handle FROM user_handle WHERE guild_id = ?")?;
            let rows = stmt.query_map([guild_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn user_for_handle(&self, guild_id: i64, handle: &str) -> anyhow::Result<Option<i64>> {
        let handle = handle.to_string();
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT user_id FROM user_handle WHERE guild_id = ? AND handle = ?",
                (guild_id, handle),
                |r| r.get(0),
            )
            .optional()?)
        })
        .await
    }

    pub async fn unlink(&self, guild_id: i64, user_id: i64) -> anyhow::Result<bool> {
        Ok(self.write("DELETE FROM user_handle WHERE guild_id = ? AND user_id = ?", (guild_id, user_id)).await? > 0)
    }

    // --- pending links ---
    pub async fn save_pending(&self, guild_id: i64, user_id: i64, handle: &str, problem_key: &str, issued_at: i64) -> anyhow::Result<()> {
        self.write(
            "INSERT OR REPLACE INTO pending_link (guild_id, user_id, handle, problem_key, issued_at) VALUES (?, ?, ?, ?, ?)",
            (guild_id, user_id, handle.to_string(), problem_key.to_string(), issued_at),
        )
        .await?;
        Ok(())
    }

    pub async fn get_pending(&self, guild_id: i64, user_id: i64) -> anyhow::Result<Option<Pending>> {
        self.run(move |c| {
            Ok(c.query_row(
                "SELECT handle, problem_key, issued_at FROM pending_link WHERE guild_id = ? AND user_id = ?",
                (guild_id, user_id),
                |r| Ok(Pending { handle: r.get(0)?, problem_key: r.get(1)?, issued_at: r.get(2)? }),
            )
            .optional()?)
        })
        .await
    }

    pub async fn delete_pending(&self, guild_id: i64, user_id: i64) -> anyhow::Result<()> {
        self.write("DELETE FROM pending_link WHERE guild_id = ? AND user_id = ?", (guild_id, user_id)).await?;
        Ok(())
    }

    // --- reminders ---
    pub async fn set_reminder(&self, guild_id: i64, channel_id: Option<i64>, role_id: Option<i64>, before_mins: &[i64], exclude: &str) -> anyhow::Result<()> {
        self.write(
            "INSERT INTO reminder (guild_id, channel_id, role_id, before_mins, exclude) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (guild_id) DO UPDATE SET \
             channel_id = excluded.channel_id, role_id = excluded.role_id, \
             before_mins = excluded.before_mins, exclude = excluded.exclude",
            (guild_id, channel_id, role_id, json_ints(before_mins), exclude.to_string()),
        )
        .await?;
        Ok(())
    }

    pub async fn get_reminder(&self, guild_id: i64) -> anyhow::Result<Option<Reminder>> {
        self.run(move |c| {
            let row = c
                .query_row("SELECT guild_id, channel_id, role_id, before_mins, exclude FROM reminder WHERE guild_id = ?", [guild_id], reminder_row)
                .optional()?;
            row.map(reminder).transpose()
        })
        .await
    }

    pub async fn all_reminders(&self) -> anyhow::Result<Vec<Reminder>> {
        self.run(|c| {
            let mut stmt = c.prepare("SELECT guild_id, channel_id, role_id, before_mins, exclude FROM reminder WHERE channel_id IS NOT NULL")?;
            let rows: Vec<_> = stmt.query_map([], reminder_row)?.collect::<Result<_, _>>()?;
            rows.into_iter().map(reminder).collect()
        })
        .await
    }

    /// Forget the channel only; the role, minutes and exclude regex stay.
    pub async fn clear_reminder(&self, guild_id: i64) -> anyhow::Result<bool> {
        self.write("DELETE FROM reminder_sent WHERE guild_id = ?", (guild_id,)).await?;
        Ok(self
            .write("UPDATE reminder SET channel_id = NULL WHERE guild_id = ? AND channel_id IS NOT NULL", (guild_id,))
            .await?
            > 0)
    }

    pub async fn set_recap(&self, guild_id: i64, channel_id: i64) -> anyhow::Result<()> {
        self.write(
            "INSERT INTO recap (guild_id, channel_id) VALUES (?, ?) ON CONFLICT (guild_id) DO UPDATE SET channel_id = excluded.channel_id",
            (guild_id, channel_id),
        )
        .await?;
        Ok(())
    }

    pub async fn get_recap(&self, guild_id: i64) -> anyhow::Result<Option<i64>> {
        self.channel_of("SELECT channel_id FROM recap WHERE guild_id = ?", guild_id).await
    }

    pub async fn all_recaps(&self) -> anyhow::Result<Vec<(i64, i64)>> {
        self.pairs("SELECT guild_id, channel_id FROM recap").await
    }

    pub async fn clear_recap(&self, guild_id: i64) -> anyhow::Result<bool> {
        self.write("DELETE FROM recap_sent WHERE guild_id = ?", (guild_id,)).await?;
        Ok(self.write("DELETE FROM recap WHERE guild_id = ?", (guild_id,)).await? > 0)
    }

    /// `week` is the current week's start: a new channel waits for the next Sunday.
    pub async fn set_weekly(&self, guild_id: i64, channel_id: i64, week: i64) -> anyhow::Result<()> {
        self.write(
            "INSERT INTO weekly (guild_id, channel_id, last_sent) VALUES (?, ?, ?) \
             ON CONFLICT (guild_id) DO UPDATE SET channel_id = excluded.channel_id",
            (guild_id, channel_id, week),
        )
        .await?;
        Ok(())
    }

    pub async fn get_weekly(&self, guild_id: i64) -> anyhow::Result<Option<i64>> {
        self.channel_of("SELECT channel_id FROM weekly WHERE guild_id = ?", guild_id).await
    }

    /// `(guild_id, channel_id, last_sent)`
    pub async fn all_weeklies(&self) -> anyhow::Result<Vec<(i64, i64, i64)>> {
        self.run(|c| {
            let mut stmt = c.prepare("SELECT guild_id, channel_id, last_sent FROM weekly")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn clear_weekly(&self, guild_id: i64) -> anyhow::Result<bool> {
        Ok(self.write("DELETE FROM weekly WHERE guild_id = ?", (guild_id,)).await? > 0)
    }

    /// True only the first time for this week, which makes the post at-most-once.
    pub async fn mark_weekly(&self, guild_id: i64, week: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("UPDATE weekly SET last_sent = ? WHERE guild_id = ? AND last_sent < ?", (week, guild_id, week))
            .await?
            > 0)
    }

    /// `(user_id, handle, finished_at, rating change)` of every finished match and mashup.
    pub async fn rating_events(&self, guild_id: i64) -> anyhow::Result<Vec<(i64, String, i64, i64)>> {
        self.run(move |c| {
            let mut stmt = c.prepare(
                "SELECT e.user_id, COALESCE(uh.handle, e.handle) AS handle, e.at, e.delta FROM (\
                 SELECT mp.guild_id, mp.user_id, mp.handle, m.finished_at AS at, mp.delta \
                 FROM match_players mp JOIN matches m ON m.id = mp.match_id \
                 WHERE mp.delta IS NOT NULL AND m.status = 'finished' \
                 UNION ALL \
                 SELECT kp.guild_id, kp.user_id, kp.handle, k.ends_at, kp.delta \
                 FROM mashup_players kp JOIN mashups k ON k.id = kp.mashup_id \
                 WHERE kp.delta IS NOT NULL AND k.status = 'finished') e \
                 LEFT JOIN user_handle uh ON uh.guild_id = e.guild_id AND uh.user_id = e.user_id \
                 WHERE e.guild_id = ?",
            )?;
            let rows = stmt.query_map([guild_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// Set the channel; `known` contests count as posted, so only later ones are announced.
    pub async fn set_newcontest(&self, guild_id: i64, channel_id: i64, known: &[i64]) -> anyhow::Result<()> {
        let known = known.to_vec();
        self.tx(move |tx| {
            tx.execute(
                "INSERT INTO newcontest (guild_id, channel_id) VALUES (?, ?) \
                 ON CONFLICT(guild_id) DO UPDATE SET channel_id = excluded.channel_id",
                (guild_id, channel_id),
            )?;
            let mut stmt = tx.prepare("INSERT OR IGNORE INTO newcontest_sent (guild_id, contest_id) VALUES (?, ?)")?;
            for c in known {
                stmt.execute((guild_id, c))?;
            }
            Ok(())
        })
        .await
    }

    pub async fn get_newcontest(&self, guild_id: i64) -> anyhow::Result<Option<i64>> {
        self.channel_of("SELECT channel_id FROM newcontest WHERE guild_id = ?", guild_id).await
    }

    pub async fn all_newcontests(&self) -> anyhow::Result<Vec<(i64, i64)>> {
        self.pairs("SELECT guild_id, channel_id FROM newcontest").await
    }

    pub async fn clear_newcontest(&self, guild_id: i64) -> anyhow::Result<bool> {
        self.write("DELETE FROM newcontest_sent WHERE guild_id = ?", (guild_id,)).await?;
        Ok(self.write("DELETE FROM newcontest WHERE guild_id = ?", (guild_id,)).await? > 0)
    }

    /// True only the first time, which makes an announcement at-most-once.
    pub async fn mark_posted(&self, guild_id: i64, contest_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("INSERT OR IGNORE INTO newcontest_sent (guild_id, contest_id) VALUES (?, ?)", (guild_id, contest_id))
            .await?
            == 1)
    }

    pub async fn posted(&self, guild_id: i64) -> anyhow::Result<HashSet<i64>> {
        self.id_set("SELECT contest_id FROM newcontest_sent WHERE guild_id = ?", guild_id).await
    }

    /// True only the first time, which makes the 48 hour post at-most-once.
    pub async fn mark_soon(&self, guild_id: i64, contest_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("UPDATE newcontest_sent SET soon = 1 WHERE guild_id = ? AND contest_id = ? AND soon = 0", (guild_id, contest_id))
            .await?
            == 1)
    }

    pub async fn soon_posted(&self, guild_id: i64) -> anyhow::Result<HashSet<i64>> {
        self.id_set("SELECT contest_id FROM newcontest_sent WHERE guild_id = ? AND soon = 1", guild_id).await
    }

    /// True if the member was not on the list yet.
    pub async fn add_contest_remind(&self, guild_id: i64, contest_id: i64, user_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("INSERT OR IGNORE INTO contest_remind (guild_id, contest_id, user_id) VALUES (?, ?, ?)", (guild_id, contest_id, user_id))
            .await?
            == 1)
    }

    /// Contest id to the members who asked for a ping, in the order they asked; contests in first-seen row order.
    pub async fn contest_reminders(&self, guild_id: i64) -> anyhow::Result<Vec<(i64, Vec<i64>)>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT contest_id, user_id FROM contest_remind WHERE guild_id = ? ORDER BY rowid")?;
            let rows = stmt.query_map([guild_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
            let mut out: Vec<(i64, Vec<i64>)> = Vec::new();
            for row in rows {
                let (contest, user) = row?;
                // first-seen order, like the Python dict; linear scan is fine for a few contests
                match out.iter_mut().find(|(k, _)| *k == contest) {
                    Some((_, users)) => users.push(user),
                    None => out.push((contest, vec![user])),
                }
            }
            Ok(out)
        })
        .await
    }

    pub async fn mark_sent(&self, guild_id: i64, contest_id: i64, before_min: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("INSERT OR IGNORE INTO reminder_sent (guild_id, contest_id, before_min) VALUES (?, ?, ?)", (guild_id, contest_id, before_min))
            .await?
            == 1)
    }

    pub async fn sent_for_guild(&self, guild_id: i64) -> anyhow::Result<HashSet<(i64, i64)>> {
        self.run(move |c| {
            let mut stmt = c.prepare("SELECT contest_id, before_min FROM reminder_sent WHERE guild_id = ?")?;
            let rows = stmt.query_map([guild_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    /// True only the first time, which makes a contest recap at-most-once.
    pub async fn mark_recapped(&self, guild_id: i64, contest_id: i64) -> anyhow::Result<bool> {
        Ok(self
            .write("INSERT OR IGNORE INTO recap_sent (guild_id, contest_id) VALUES (?, ?)", (guild_id, contest_id))
            .await?
            == 1)
    }

    pub async fn recapped(&self, guild_id: i64) -> anyhow::Result<HashSet<i64>> {
        self.id_set("SELECT contest_id FROM recap_sent WHERE guild_id = ?", guild_id).await
    }

    // --- caches ---
    pub async fn replace_contests(&self, contests: &[Contest]) -> anyhow::Result<()> {
        let contests = contests.to_vec();
        self.tx(move |tx| {
            tx.execute("DELETE FROM contest", [])?;
            let mut stmt = tx.prepare("INSERT INTO contest (id, name, type, phase, start_time, duration) VALUES (?, ?, ?, ?, ?, ?)")?;
            for c in &contests {
                stmt.execute(params![c.id, c.name, c.kind, c.phase, c.start_time, c.duration])?;
            }
            Ok(())
        })
        .await
    }

    pub async fn load_contests(&self) -> anyhow::Result<Vec<Contest>> {
        self.run(|c| {
            let mut stmt = c.prepare("SELECT id, name, type, phase, start_time, duration FROM contest")?;
            let rows = stmt.query_map([], |r| {
                Ok(Contest { id: r.get(0)?, name: r.get(1)?, kind: r.get(2)?, phase: r.get(3)?, start_time: r.get(4)?, duration: r.get(5)? })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    pub async fn replace_problems(&self, problems: &[Problem]) -> anyhow::Result<()> {
        let problems = problems.to_vec();
        self.tx(move |tx| {
            tx.execute("DELETE FROM problem", [])?;
            let mut stmt = tx.prepare("INSERT INTO problem (contest_id, idx, name, rating, tags) VALUES (?, ?, ?, ?, ?)")?;
            for p in &problems {
                stmt.execute(params![p.contest_id, p.index, p.name, p.rating, json_strings(&p.tags)])?;
            }
            Ok(())
        })
        .await
    }

    pub async fn load_problems(&self) -> anyhow::Result<Vec<Problem>> {
        self.run(|c| {
            let mut stmt = c.prepare("SELECT contest_id, idx, name, rating, tags FROM problem")?;
            let rows: Vec<(i64, String, String, Option<i64>, String)> = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
                .collect::<Result<_, _>>()?;
            rows.into_iter()
                .map(|(contest_id, index, name, rating, tags)| Ok(Problem { contest_id, index, name, rating, tags: parse_list(&tags)? }))
                .collect()
        })
        .await
    }

    async fn channel_of(&self, sql: &'static str, guild_id: i64) -> anyhow::Result<Option<i64>> {
        self.run(move |c| Ok(c.query_row(sql, [guild_id], |r| r.get(0)).optional()?)).await
    }

    async fn pairs(&self, sql: &'static str) -> anyhow::Result<Vec<(i64, i64)>> {
        self.run(move |c| {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn id_set(&self, sql: &'static str, guild_id: i64) -> anyhow::Result<HashSet<i64>> {
        self.run(move |c| {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([guild_id], |r| r.get(0))?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
        .await
    }
}

type ReminderRow = (i64, Option<i64>, Option<i64>, String, String);

fn reminder_row(r: &rusqlite::Row) -> rusqlite::Result<ReminderRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn reminder((guild_id, channel_id, role_id, before_mins, exclude): ReminderRow) -> anyhow::Result<Reminder> {
    Ok(Reminder { guild_id, channel_id, role_id, before_mins: parse_list(&before_mins)?, exclude })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_json_formats() {
        assert_eq!(json_ints(&[60, 10]), "[60, 10]");
        assert_eq!(json_ints(&[]), "[]");
        let tags = ["math".to_string(), "dp".into(), "\u{e9}\"\u{1F600}\n".into()];
        assert_eq!(json_strings(&tags), "[\"math\", \"dp\", \"\\u00e9\\\"\\ud83d\\ude00\\n\"]");
    }
}
