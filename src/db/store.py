import asyncio
import json
import sqlite3
from collections.abc import AsyncIterator, Iterable, Mapping, Sequence
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import aiosqlite

from src.cf.models import Contest, Problem
from src.errors import AlreadyInMatch, HandleTaken, MatchClosed, MatchFull
from src.logic import DAILY_MULTIPLIER, START_RATING

MIGRATIONS = [
    """
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
    """,
    """
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
    """,
    # Points became match ratings: zero the old point changes so everyone restarts at 800.
    "UPDATE match_players SET delta = 0 WHERE delta IS NOT NULL;",
    """
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
    """,
    # Mashup contests got a lobby, a penalty and a one-activity-per-user rule: rebuild their tables.
    """
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
    """,
    # One daily problem per user per UTC day. A solo match solved first that day claims the day
    # without a problem (problem_key stays NULL).
    """
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
    """,
    # Contest recaps are posted once per server and contest.
    """
    CREATE TABLE recap_sent (
        guild_id INTEGER NOT NULL,
        contest_id INTEGER NOT NULL,
        PRIMARY KEY (guild_id, contest_id)
    );
    """,
    # Matches can filter their problem by tags (logic.normalize_tags form, '' for no filter).
    """
    ALTER TABLE matches ADD COLUMN tags TEXT NOT NULL DEFAULT '';
    ALTER TABLE matches ADD COLUMN exclude_tags TEXT NOT NULL DEFAULT '';
    """,
    # Team mashups: teams is the number of teams (0 for an individual contest), team the player's.
    """
    ALTER TABLE mashups ADD COLUMN teams INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE mashup_players ADD COLUMN team INTEGER;
    """,
    # The host can lower the player cap of a mashup (20 until now).
    "ALTER TABLE mashups ADD COLUMN max_players INTEGER NOT NULL DEFAULT 20;",
    # A finished mashup moves match ratings: the change is stored per player (NULL before it ends,
    # and for contests that ended earlier or had a single entrant).
    "ALTER TABLE mashup_players ADD COLUMN delta INTEGER;",
    # The host can open an unrated contest (rated = 0), which never changes ratings.
    "ALTER TABLE mashups ADD COLUMN rated INTEGER NOT NULL DEFAULT 1;",
    # A team contest can cap each team's size (0 for no per-team cap).
    "ALTER TABLE mashups ADD COLUMN team_size INTEGER NOT NULL DEFAULT 0;",
    # Contests can filter their problems by tags (logic.normalize_tags form, '' for no filter).
    """
    ALTER TABLE mashups ADD COLUMN tags TEXT NOT NULL DEFAULT '';
    ALTER TABLE mashups ADD COLUMN exclude_tags TEXT NOT NULL DEFAULT '';
    """,
    # A virtual contest replays a finished Codeforces contest (NULL for a random mashup).
    "ALTER TABLE mashups ADD COLUMN contest_id INTEGER;",
    # Every contest is a team contest now (a team of one is a player): turn the individual
    # contests (teams = 0) into teams of one, numbered in joining order.
    """
    UPDATE mashup_players SET team = (
        SELECT COUNT(*) FROM mashup_players q WHERE q.mashup_id = mashup_players.mashup_id
        AND (q.joined_at, q.rowid) <= (mashup_players.joined_at, mashup_players.rowid)
    ) WHERE mashup_id IN (SELECT id FROM mashups WHERE teams = 0);
    UPDATE mashups SET team_size = 1, teams = MAX(1, CASE WHEN status = 'lobby' THEN max_players
        ELSE (SELECT COUNT(*) FROM mashup_players p WHERE p.mashup_id = mashups.id) END)
    WHERE teams = 0;
    """,
    # Reminders and recaps get separate channels. A reminder row may have no channel (the channel
    # was cleared) and keeps its role, minutes and exclude regex; existing servers keep both.
    """
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
    """,
    # Bingo: a mashup with mode classic or replace plays on a size x size board (win is "line" or
    # "continuous"); every problem sits in a cell, and a replace board stacks problems per cell,
    # each counting only from `since` (NULL = the start).
    """
    ALTER TABLE mashups ADD COLUMN mode TEXT NOT NULL DEFAULT 'contest';
    ALTER TABLE mashups ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE mashups ADD COLUMN win TEXT NOT NULL DEFAULT 'line';
    ALTER TABLE mashup_problems ADD COLUMN cell INTEGER;
    ALTER TABLE mashup_problems ADD COLUMN since INTEGER;
    """,
    # A replace bingo never replaces a square with a problem someone in the lobby solved: the
    # keys solved before the start.
    """
    CREATE TABLE mashup_excluded (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        problem_key TEXT NOT NULL,
        PRIMARY KEY (mashup_id, problem_key)
    );
    """,
    # A bingo has a thread for its solve log; mashup_logged remembers the claims already posted
    # there (at most once each).
    """
    ALTER TABLE mashups ADD COLUMN thread_id INTEGER;
    CREATE TABLE mashup_logged (
        mashup_id INTEGER NOT NULL REFERENCES mashups (id),
        problem_key TEXT NOT NULL,
        PRIMARY KEY (mashup_id, problem_key)
    );
    """,
    # The thread message that carries the board (only the newest log message does).
    "ALTER TABLE mashups ADD COLUMN log_board_id INTEGER;",
    # The weekly leaderboard channel; last_sent is the start of the week (Sunday 00:00 UTC) whose
    # leaderboard was posted last, so each week posts at most once.
    """
    CREATE TABLE weekly (
        guild_id INTEGER PRIMARY KEY,
        channel_id INTEGER NOT NULL,
        last_sent INTEGER NOT NULL
    );
    """,
]


@dataclass(frozen=True)
class LinkedHandle:
    handle: str
    rating: int | None


@dataclass(frozen=True)
class Pending:
    handle: str
    problem_key: str
    issued_at: int


@dataclass(frozen=True)
class MatchRow:
    id: int
    guild_id: int
    channel_id: int
    message_id: int | None
    host_id: int
    mode: str
    max_players: int
    invited_id: int | None
    rating_arg: int | None
    rating: int | None
    problem_key: str | None
    status: str
    created_at: int
    started_at: int | None
    expires_at: int | None
    finished_at: int | None
    tags: str = ""
    exclude_tags: str = ""


@dataclass(frozen=True)
class MatchPlayer:
    match_id: int
    guild_id: int
    user_id: int
    handle: str
    joined_at: int
    solved_at: int | None
    gave_up: bool
    delta: int | None
    active: bool


@dataclass(frozen=True)
class DailyRow:
    guild_id: int
    user_id: int
    day: int
    problem_key: str | None
    rating: int | None
    issued_at: int | None
    solved_at: int | None
    match_id: int | None


@dataclass(frozen=True)
class MashupRow:
    id: int
    guild_id: int
    channel_id: int
    message_id: int | None
    host_id: int
    min_rating: int
    max_rating: int
    count: int
    duration_seconds: int
    penalty_minutes: int
    status: str
    created_at: int
    started_at: int | None
    ends_at: int | None
    teams: int = 0
    max_players: int = 20
    rated: int = 1
    team_size: int = 0
    tags: str = ""
    exclude_tags: str = ""
    contest_id: int | None = None
    mode: str = "contest"  # contest, or bingo: classic or replace
    size: int = 0  # side of a bingo board
    win: str = "line"  # classic bingo: "line" (first full line) or "continuous" (play to the end)
    thread_id: int | None = None  # the thread of a bingo's solve log
    log_board_id: int | None = None  # the newest thread message, which carries the board


@dataclass(frozen=True)
class MashupCell:
    idx: int
    key: str
    rating: int
    cell: int | None
    since: int  # solves count from here (the contest start unless a bingo cell was replaced)


@dataclass(frozen=True)
class MashupPlayer:
    user_id: int
    handle: str
    joined_at: int
    team: int | None = None
    delta: int | None = None
    active: bool = True  # False once the player gave up (or the contest ended)


def _match(row: aiosqlite.Row) -> MatchRow:
    return MatchRow(**dict(row))


def _player(row: aiosqlite.Row) -> MatchPlayer:
    data = dict(row)
    data["gave_up"] = bool(data["gave_up"])
    data["active"] = bool(data["active"])
    return MatchPlayer(**data)


@dataclass(frozen=True)
class Reminder:
    guild_id: int
    channel_id: int | None
    role_id: int | None
    before_mins: tuple[int, ...]
    exclude: str = ""


def _reminder(row: aiosqlite.Row) -> Reminder:
    return Reminder(
        row["guild_id"],
        row["channel_id"],
        row["role_id"],
        tuple(json.loads(row["before_mins"])),
        row["exclude"],
    )


class Store:
    def __init__(self, path: Path):
        self._path = path
        self._db: aiosqlite.Connection
        self._lock = asyncio.Lock()

    async def connect(self) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        self._db = await aiosqlite.connect(self._path)
        self._db.row_factory = aiosqlite.Row
        await self._migrate()

    async def close(self) -> None:
        await self._db.close()

    async def _migrate(self) -> None:
        row = await self._one("PRAGMA user_version")
        version = row[0]
        for number, script in enumerate(MIGRATIONS[version:], start=version + 1):
            # One transaction per migration: a failure leaves neither half of it nor the version.
            try:
                await self._db.executescript(
                    f"BEGIN;\n{script}\nPRAGMA user_version = {number};\nCOMMIT;"
                )
            except BaseException:
                if self._db.in_transaction:
                    await self._db.rollback()
                raise

    @asynccontextmanager
    async def _tx(self) -> AsyncIterator[aiosqlite.Connection]:
        """One serialized transaction: commit on success, roll back and re-raise on any error."""
        async with self._lock:
            try:
                yield self._db
                await self._db.commit()
            except BaseException:
                await self._db.rollback()
                raise

    async def _write(self, sql: str, params: Sequence[Any] = ()) -> int:
        async with self._tx() as db:
            cur = await db.execute(sql, params)
            return cur.rowcount

    async def _one(self, sql: str, params: Sequence[Any] = ()) -> aiosqlite.Row | None:
        async with self._db.execute(sql, params) as cur:
            return await cur.fetchone()

    async def _all(self, sql: str, params: Sequence[Any] = ()) -> list[aiosqlite.Row]:
        async with self._db.execute(sql, params) as cur:
            return list(await cur.fetchall())

    # --- handles ---
    async def link_handle(
        self, guild_id: int, user_id: int, handle: str, rating: int | None
    ) -> None:
        try:
            await self._write(
                "INSERT INTO user_handle (guild_id, user_id, handle, rating) VALUES (?, ?, ?, ?) "
                "ON CONFLICT (guild_id, user_id) DO UPDATE SET "
                "handle = excluded.handle, rating = excluded.rating",
                (guild_id, user_id, handle, rating),
            )
        except sqlite3.IntegrityError as exc:
            raise HandleTaken(f"{handle} is already linked to another member.") from exc

    async def get_handle(self, guild_id: int, user_id: int) -> LinkedHandle | None:
        row = await self._one(
            "SELECT handle, rating FROM user_handle WHERE guild_id = ? AND user_id = ?",
            (guild_id, user_id),
        )
        return LinkedHandle(row["handle"], row["rating"]) if row else None

    async def linked_handles(self, guild_id: int) -> list[tuple[int, str]]:
        rows = await self._all(
            "SELECT user_id, handle FROM user_handle WHERE guild_id = ?", (guild_id,)
        )
        return [(r["user_id"], r["handle"]) for r in rows]

    async def user_for_handle(self, guild_id: int, handle: str) -> int | None:
        row = await self._one(
            "SELECT user_id FROM user_handle WHERE guild_id = ? AND handle = ?", (guild_id, handle)
        )
        return row["user_id"] if row else None

    async def unlink(self, guild_id: int, user_id: int) -> bool:
        n = await self._write(
            "DELETE FROM user_handle WHERE guild_id = ? AND user_id = ?", (guild_id, user_id)
        )
        return n > 0

    # --- pending links ---
    async def save_pending(
        self, guild_id: int, user_id: int, handle: str, problem_key: str, issued_at: int
    ) -> None:
        await self._write(
            "INSERT OR REPLACE INTO pending_link (guild_id, user_id, handle, problem_key, issued_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (guild_id, user_id, handle, problem_key, issued_at),
        )

    async def get_pending(self, guild_id: int, user_id: int) -> Pending | None:
        row = await self._one(
            "SELECT handle, problem_key, issued_at FROM pending_link WHERE guild_id = ? AND user_id = ?",
            (guild_id, user_id),
        )
        return Pending(row["handle"], row["problem_key"], row["issued_at"]) if row else None

    async def delete_pending(self, guild_id: int, user_id: int) -> None:
        await self._write(
            "DELETE FROM pending_link WHERE guild_id = ? AND user_id = ?", (guild_id, user_id)
        )

    # --- matches ---
    async def create_match(
        self,
        *,
        guild_id: int,
        channel_id: int,
        host_id: int,
        host_handle: str,
        mode: str,
        max_players: int,
        invited_id: int | None,
        rating_arg: int | None,
        now: int,
        tags: str = "",
        exclude_tags: str = "",
    ) -> int:
        try:
            async with self._tx() as db:
                if await self._in_mashup(db, guild_id, host_id):
                    raise AlreadyInMatch()
                cur = await db.execute(
                    "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, "
                    "invited_id, rating_arg, status, created_at, tags, exclude_tags) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, 'lobby', ?, ?, ?)",
                    (
                        guild_id,
                        channel_id,
                        host_id,
                        mode,
                        max_players,
                        invited_id,
                        rating_arg,
                        now,
                        tags,
                        exclude_tags,
                    ),
                )
                match_id = cur.lastrowid
                await db.execute(
                    "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at) "
                    "VALUES (?, ?, ?, ?, ?)",
                    (match_id, guild_id, host_id, host_handle, now),
                )
                return match_id
        except sqlite3.IntegrityError as exc:
            raise AlreadyInMatch() from exc

    async def get_match(self, match_id: int) -> MatchRow | None:
        row = await self._one("SELECT * FROM matches WHERE id = ?", (match_id,))
        return _match(row) if row else None

    async def get_players(self, match_id: int) -> list[MatchPlayer]:
        rows = await self._all(
            "SELECT * FROM match_players WHERE match_id = ? ORDER BY joined_at, rowid", (match_id,)
        )
        return [_player(r) for r in rows]

    async def active_match_for(self, guild_id: int, user_id: int) -> int | None:
        row = await self._one(
            "SELECT mp.match_id FROM match_players mp JOIN matches m ON m.id = mp.match_id "
            "WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.active = 1 "
            "AND m.status IN ('lobby', 'active')",
            (guild_id, user_id),
        )
        return row["match_id"] if row else None

    async def set_message(self, match_id: int, channel_id: int, message_id: int) -> None:
        await self._write(
            "UPDATE matches SET channel_id = ?, message_id = ? WHERE id = ?",
            (channel_id, message_id, match_id),
        )

    async def add_player(
        self, match_id: int, guild_id: int, user_id: int, handle: str, now: int
    ) -> None:
        try:
            async with self._tx() as db:
                cur = await db.execute(
                    "SELECT status, max_players FROM matches WHERE id = ?", (match_id,)
                )
                match = await cur.fetchone()
                if match is None or match["status"] != "lobby":
                    raise MatchClosed()
                cur = await db.execute(
                    "SELECT COUNT(*) FROM match_players WHERE match_id = ?", (match_id,)
                )
                if (await cur.fetchone())[0] >= match["max_players"]:
                    raise MatchFull()
                if await self._in_mashup(db, guild_id, user_id):
                    raise AlreadyInMatch()
                await db.execute(
                    "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at) "
                    "VALUES (?, ?, ?, ?, ?)",
                    (match_id, guild_id, user_id, handle, now),
                )
        except sqlite3.IntegrityError as exc:
            raise AlreadyInMatch() from exc

    async def remove_player(self, match_id: int, user_id: int) -> bool:
        n = await self._write(
            "DELETE FROM match_players WHERE match_id = ? AND user_id = ? "
            "AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'lobby')",
            (match_id, user_id, match_id),
        )
        return n > 0

    async def cancel_match(self, match_id: int, now: int) -> bool:
        async with self._tx() as db:
            cur = await db.execute(
                "UPDATE matches SET status = 'cancelled', finished_at = ? "
                "WHERE id = ? AND status = 'lobby'",
                (now, match_id),
            )
            if cur.rowcount == 0:
                return False
            await db.execute("UPDATE match_players SET active = 0 WHERE match_id = ?", (match_id,))
            return True

    async def start_match(
        self,
        match_id: int,
        rating: int,
        problem_key: str,
        now: int,
        expires_at: int,
        player_ids: Sequence[int],
    ) -> bool:
        """Activate a lobby only if its players are still exactly `player_ids`, the set the
        problem was chosen for."""
        async with self._tx() as db:
            cur = await db.execute("SELECT status FROM matches WHERE id = ?", (match_id,))
            match = await cur.fetchone()
            if match is None or match["status"] != "lobby":
                return False
            cur = await db.execute(
                "SELECT user_id FROM match_players WHERE match_id = ?", (match_id,)
            )
            if {r["user_id"] for r in await cur.fetchall()} != set(player_ids):
                return False
            await db.execute(
                "UPDATE matches SET status = 'active', rating = ?, problem_key = ?, "
                "started_at = ?, expires_at = ? WHERE id = ?",
                (rating, problem_key, now, expires_at, match_id),
            )
            return True

    async def mark_gave_up(self, match_id: int, user_id: int) -> bool:
        n = await self._write(
            "UPDATE match_players SET gave_up = 1, active = 0 "
            "WHERE match_id = ? AND user_id = ? AND gave_up = 0 AND solved_at IS NULL "
            "AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'active')",
            (match_id, user_id, match_id),
        )
        return n > 0

    async def record_solves(self, match_id: int, solves: Mapping[int, int]) -> None:
        async with self._tx() as db:
            await db.executemany(
                "UPDATE match_players SET solved_at = ? WHERE match_id = ? AND user_id = ? "
                "AND solved_at IS NULL "
                "AND EXISTS (SELECT 1 FROM matches WHERE id = ? AND status = 'active')",
                [(t, match_id, uid, match_id) for uid, t in solves.items()],
            )

    async def finish_match(
        self, match_id: int, now: int, deltas: Mapping[int, int], bonus_day: int | None = None
    ) -> bool:
        """bonus_day (solo only): the UTC day; a solve that is the user's first claim of that day
        doubles the rating change."""
        async with self._tx() as db:
            cur = await db.execute(
                "UPDATE matches SET status = 'finished', finished_at = ? "
                "WHERE id = ? AND status = 'active'",
                (now, match_id),
            )
            if cur.rowcount == 0:
                return False
            if bonus_day is not None:
                ((uid, gain),) = deltas.items()
                cur = await db.execute("SELECT guild_id FROM matches WHERE id = ?", (match_id,))
                guild_id = (await cur.fetchone())["guild_id"]
                if gain > 0 and await self._claim_daily(
                    db, guild_id, uid, bonus_day, now, match_id
                ):
                    deltas = {uid: gain * DAILY_MULTIPLIER}
            await db.executemany(
                "UPDATE match_players SET delta = ? WHERE match_id = ? AND user_id = ?",
                [(d, match_id, uid) for uid, d in deltas.items()],
            )
            await db.execute("UPDATE match_players SET active = 0 WHERE match_id = ?", (match_id,))
            return True

    @staticmethod
    async def _claim_daily(
        db: aiosqlite.Connection, guild_id: int, user_id: int, day: int, now: int, match_id: int
    ) -> bool:
        cur = await db.execute(
            "INSERT INTO daily (guild_id, user_id, day, solved_at, match_id) "
            "VALUES (?, ?, ?, ?, ?) ON CONFLICT (guild_id, user_id, day) DO UPDATE SET "
            "solved_at = excluded.solved_at, match_id = excluded.match_id "
            "WHERE daily.solved_at IS NULL",
            (guild_id, user_id, day, now, match_id),
        )
        return cur.rowcount > 0

    # --- daily problem ---
    async def get_daily(self, guild_id: int, user_id: int, day: int) -> DailyRow | None:
        row = await self._one(
            "SELECT * FROM daily WHERE guild_id = ? AND user_id = ? AND day = ?",
            (guild_id, user_id, day),
        )
        return DailyRow(**dict(row)) if row else None

    async def daily_days(self, guild_id: int, user_id: int) -> list[int]:
        """UTC days on which the user claimed the daily, newest first."""
        rows = await self._all(
            "SELECT day FROM daily WHERE guild_id = ? AND user_id = ? AND solved_at IS NOT NULL "
            "ORDER BY day DESC",
            (guild_id, user_id),
        )
        return [r["day"] for r in rows]

    async def daily_days_by_user(self, guild_id: int) -> dict[int, list[int]]:
        """Every user's claimed daily days in this server, newest first."""
        rows = await self._all(
            "SELECT user_id, day FROM daily WHERE guild_id = ? AND solved_at IS NOT NULL "
            "ORDER BY day DESC",
            (guild_id,),
        )
        out: dict[int, list[int]] = {}
        for r in rows:
            out.setdefault(r["user_id"], []).append(r["day"])
        return out

    async def issue_daily(
        self, guild_id: int, user_id: int, day: int, problem_key: str, rating: int, now: int
    ) -> None:
        """Today's problem; an unsolved one is replaced only when it has a different rating."""
        await self._write(
            "INSERT INTO daily (guild_id, user_id, day, problem_key, rating, issued_at) "
            "VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (guild_id, user_id, day) DO UPDATE SET "
            "problem_key = excluded.problem_key, rating = excluded.rating, "
            "issued_at = excluded.issued_at "
            "WHERE daily.solved_at IS NULL AND daily.rating IS NOT excluded.rating",
            (guild_id, user_id, day, problem_key, rating, now),
        )

    async def record_daily_solve(
        self,
        *,
        guild_id: int,
        channel_id: int,
        user_id: int,
        handle: str,
        day: int,
        solved_at: int,
        delta: int,
        now: int,
    ) -> bool:
        """Mark the issued daily solved and book its rating change as a finished solo match.
        False when it was already solved (by a concurrent request or a solo match)."""
        async with self._tx() as db:
            cur = await db.execute(
                "SELECT problem_key, rating, issued_at FROM daily "
                "WHERE guild_id = ? AND user_id = ? AND day = ? AND solved_at IS NULL",
                (guild_id, user_id, day),
            )
            daily = await cur.fetchone()
            if daily is None:
                return False
            cur = await db.execute(
                "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, rating, "
                "problem_key, status, created_at, started_at, finished_at) "
                "VALUES (?, ?, ?, 'solo', 1, ?, ?, 'finished', ?, ?, ?)",
                (
                    guild_id,
                    channel_id,
                    user_id,
                    daily["rating"],
                    daily["problem_key"],
                    daily["issued_at"],
                    daily["issued_at"],
                    now,
                ),
            )
            match_id = cur.lastrowid
            await db.execute(
                "INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, "
                "solved_at, delta, active) VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
                (match_id, guild_id, user_id, handle, daily["issued_at"], solved_at, delta),
            )
            await db.execute(
                "UPDATE daily SET solved_at = ?, match_id = ? "
                "WHERE guild_id = ? AND user_id = ? AND day = ?",
                (now, match_id, guild_id, user_id, day),
            )
            return True

    # A player's rating changes come from finished matches and finished mashups.
    _CHANGES = (
        "SELECT guild_id, user_id, handle, delta FROM match_players WHERE delta IS NOT NULL "
        "UNION ALL "
        "SELECT guild_id, user_id, handle, delta FROM mashup_players WHERE delta IS NOT NULL"
    )

    async def rating(self, guild_id: int, user_id: int) -> int:
        """Match rating: the start rating plus every stored rating change."""
        row = await self._one(
            f"SELECT COALESCE(SUM(delta), 0) FROM ({self._CHANGES}) "
            "WHERE guild_id = ? AND user_id = ?",
            (guild_id, user_id),
        )
        return START_RATING + row[0]

    async def stats(self, guild_id: int, kind: str = "all") -> list[tuple[int, str, int, int]]:
        """(user_id, handle, match rating, games played), best rated first. kind picks finished
        matches ("matches"), finished mashups ("contests") or both ("all"); the rating counts
        only the changes of that kind."""
        source = {
            "matches": "SELECT guild_id, user_id, handle, delta FROM match_players "
            "WHERE delta IS NOT NULL",
            "contests": "SELECT guild_id, user_id, handle, delta FROM mashup_players "
            "WHERE delta IS NOT NULL",
        }.get(kind, self._CHANGES)
        rows = await self._all(
            "SELECT c.user_id, COALESCE(uh.handle, MAX(c.handle)) AS handle, "
            f"? + SUM(c.delta) AS total, COUNT(*) AS played FROM ({source}) c "
            "LEFT JOIN user_handle uh ON uh.guild_id = c.guild_id AND uh.user_id = c.user_id "
            "WHERE c.guild_id = ? "
            "GROUP BY c.user_id ORDER BY total DESC, played DESC, handle",
            (START_RATING, guild_id),
        )
        return [(r["user_id"], r["handle"], r["total"], r["played"]) for r in rows]

    async def rating_changes(self, guild_id: int, user_id: int) -> list[tuple[int, int]]:
        """(finished_at, rating change) of the player's finished matches and mashups, oldest
        first."""
        rows = await self._all(
            "SELECT m.finished_at AS at, mp.delta FROM match_players mp "
            "JOIN matches m ON m.id = mp.match_id "
            "WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.delta IS NOT NULL "
            "AND m.status = 'finished' "
            "UNION ALL "
            "SELECT k.ends_at AS at, kp.delta FROM mashup_players kp "
            "JOIN mashups k ON k.id = kp.mashup_id "
            "WHERE kp.guild_id = ? AND kp.user_id = ? AND kp.delta IS NOT NULL "
            "AND k.status = 'finished' ORDER BY at",
            (guild_id, user_id, guild_id, user_id),
        )
        return [(r["at"], r["delta"]) for r in rows]

    async def finished_matches(
        self, guild_id: int, user_id: int, limit: int = 100
    ) -> list[tuple[MatchRow, list[MatchPlayer]]]:
        """The player's newest finished matches with all their players."""
        rows = await self._all(
            "SELECT m.* FROM matches m JOIN match_players mp ON mp.match_id = m.id "
            "WHERE m.guild_id = ? AND mp.user_id = ? AND m.status = 'finished' "
            "ORDER BY m.finished_at DESC, m.id DESC LIMIT ?",
            (guild_id, user_id, limit),
        )
        if not rows:
            return []
        marks = ",".join("?" * len(rows))
        players = await self._all(
            f"SELECT * FROM match_players WHERE match_id IN ({marks}) ORDER BY joined_at, rowid",
            [r["id"] for r in rows],
        )
        by_match: dict[int, list[MatchPlayer]] = {}
        for row in players:
            by_match.setdefault(row["match_id"], []).append(_player(row))
        return [(_match(r), by_match.get(r["id"], [])) for r in rows]

    async def matches_due(self, lobby_before: int, now: int) -> list[MatchRow]:
        rows = await self._all(
            "SELECT * FROM matches WHERE (status = 'lobby' AND created_at <= ?) "
            "OR (status = 'active' AND expires_at <= ?)",
            (lobby_before, now),
        )
        return [_match(r) for r in rows]

    # --- mashup contests ---
    @staticmethod
    async def _in_mashup(db: aiosqlite.Connection, guild_id: int, user_id: int) -> bool:
        cur = await db.execute(
            "SELECT 1 FROM mashup_players WHERE guild_id = ? AND user_id = ? AND active = 1",
            (guild_id, user_id),
        )
        return await cur.fetchone() is not None

    @staticmethod
    async def _in_match(db: aiosqlite.Connection, guild_id: int, user_id: int) -> bool:
        cur = await db.execute(
            "SELECT 1 FROM match_players WHERE guild_id = ? AND user_id = ? AND active = 1",
            (guild_id, user_id),
        )
        return await cur.fetchone() is not None

    async def create_mashup(
        self,
        *,
        guild_id: int,
        channel_id: int,
        host_id: int,
        host_handle: str,
        min_rating: int,
        max_rating: int,
        count: int,
        duration_seconds: int,
        penalty_minutes: int,
        now: int,
        teams: int = 0,
        host_team: int | None = None,
        max_players: int = 20,
        rated: bool = True,
        team_size: int = 0,
        tags: str = "",
        exclude_tags: str = "",
        contest_id: int | None = None,
        mode: str = "contest",
        size: int = 0,
        win: str = "line",
    ) -> int:
        try:
            async with self._tx() as db:
                if await self._in_match(db, guild_id, host_id):
                    raise AlreadyInMatch()
                cur = await db.execute(
                    "INSERT INTO mashups (guild_id, channel_id, host_id, min_rating, max_rating, "
                    "count, duration_seconds, penalty_minutes, status, created_at, teams, "
                    "max_players, rated, team_size, tags, exclude_tags, contest_id, mode, size, win) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'lobby', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        guild_id,
                        channel_id,
                        host_id,
                        min_rating,
                        max_rating,
                        count,
                        duration_seconds,
                        penalty_minutes,
                        now,
                        teams,
                        max_players,
                        int(rated),
                        team_size,
                        tags,
                        exclude_tags,
                        contest_id,
                        mode,
                        size,
                        win,
                    ),
                )
                mashup_id = cur.lastrowid
                await db.execute(
                    "INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, "
                    "team) VALUES (?, ?, ?, ?, ?, ?)",
                    (mashup_id, guild_id, host_id, host_handle, now, host_team),
                )
                return mashup_id
        except sqlite3.IntegrityError as exc:
            raise AlreadyInMatch() from exc

    async def get_mashup(self, mashup_id: int) -> MashupRow | None:
        row = await self._one("SELECT * FROM mashups WHERE id = ?", (mashup_id,))
        return MashupRow(**dict(row)) if row else None

    async def active_mashup_for(self, guild_id: int, user_id: int) -> int | None:
        """The unfinished contest (lobby or running) the user is in, if any."""
        row = await self._one(
            "SELECT mp.mashup_id FROM mashup_players mp JOIN mashups m ON m.id = mp.mashup_id "
            "WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.active = 1 "
            "AND m.status IN ('lobby', 'active')",
            (guild_id, user_id),
        )
        return row["mashup_id"] if row else None

    async def mashups_in(self, *statuses: str) -> list[MashupRow]:
        marks = ",".join("?" * len(statuses))
        rows = await self._all(
            f"SELECT * FROM mashups WHERE status IN ({marks}) ORDER BY id", statuses
        )
        return [MashupRow(**dict(r)) for r in rows]

    async def set_mashup_message(self, mashup_id: int, channel_id: int, message_id: int) -> None:
        await self._write(
            "UPDATE mashups SET channel_id = ?, message_id = ? WHERE id = ?",
            (channel_id, message_id, mashup_id),
        )

    async def finished_mashups(
        self, guild_id: int, user_id: int, limit: int = 50
    ) -> list[MashupRow]:
        """The newest finished contests the user played in."""
        rows = await self._all(
            "SELECT m.* FROM mashups m JOIN mashup_players mp ON mp.mashup_id = m.id "
            "WHERE m.guild_id = ? AND mp.user_id = ? AND m.status = 'finished' "
            "ORDER BY m.ends_at DESC, m.id DESC LIMIT ?",
            (guild_id, user_id, limit),
        )
        return [MashupRow(**dict(r)) for r in rows]

    async def mashup_problems(self, mashup_id: int) -> list[tuple[str, int]]:
        rows = await self._all(
            "SELECT problem_key, rating FROM mashup_problems WHERE mashup_id = ? ORDER BY idx",
            (mashup_id,),
        )
        return [(r["problem_key"], r["rating"]) for r in rows]

    async def mashup_cells(self, mashup_id: int) -> list[MashupCell]:
        """Every problem of the contest in the order it was added."""
        rows = await self._all(
            "SELECT p.idx, p.problem_key, p.rating, p.cell, COALESCE(p.since, m.started_at) AS since "
            "FROM mashup_problems p JOIN mashups m ON m.id = p.mashup_id "
            "WHERE p.mashup_id = ? ORDER BY p.idx",
            (mashup_id,),
        )
        return [MashupCell(*row) for row in rows]

    async def replace_cell(
        self, mashup_id: int, cell: int, after_idx: int, key: str, rating: int, since: int
    ) -> bool:
        """Puts a new problem on a bingo cell of a running contest, only if `after_idx` is still the
        cell's newest problem (so two reads that both saw the claim add one problem)."""
        async with self._tx() as db:
            cur = await db.execute(
                "SELECT MAX(idx) FROM mashup_problems WHERE mashup_id = ? AND cell = ? "
                "AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'active')",
                (mashup_id, cell, mashup_id),
            )
            if (await cur.fetchone())[0] != after_idx:
                return False
            await db.execute(
                "INSERT INTO mashup_problems (mashup_id, idx, problem_key, rating, cell, since) "
                "SELECT ?, MAX(idx) + 1, ?, ?, ?, ? FROM mashup_problems WHERE mashup_id = ?",
                (mashup_id, key, rating, cell, since, mashup_id),
            )
            return True

    async def mashup_players(self, mashup_id: int) -> list[MashupPlayer]:
        rows = await self._all(
            "SELECT user_id, handle, joined_at, team, delta, active FROM mashup_players "
            "WHERE mashup_id = ? ORDER BY joined_at, rowid",
            (mashup_id,),
        )
        return [
            MashupPlayer(
                r["user_id"], r["handle"], r["joined_at"], r["team"], r["delta"], bool(r["active"])
            )
            for r in rows
        ]

    async def add_mashup_player(
        self,
        mashup_id: int,
        guild_id: int,
        user_id: int,
        handle: str,
        now: int,
        cap: int,
        team: int | None = None,
        team_size: int = 0,
    ) -> None:
        try:
            async with self._tx() as db:
                cur = await db.execute("SELECT status FROM mashups WHERE id = ?", (mashup_id,))
                row = await cur.fetchone()
                if row is None or row["status"] != "lobby":
                    raise MatchClosed("This contest is no longer open.")
                cur = await db.execute(
                    "SELECT COUNT(*) FROM mashup_players WHERE mashup_id = ?", (mashup_id,)
                )
                if (await cur.fetchone())[0] >= cap:
                    raise MatchFull()
                if team_size:
                    cur = await db.execute(
                        "SELECT COUNT(*) FROM mashup_players WHERE mashup_id = ? AND team = ?",
                        (mashup_id, team),
                    )
                    if (await cur.fetchone())[0] >= team_size:
                        raise MatchFull("That team is full.")
                if await self._in_match(db, guild_id, user_id):
                    raise AlreadyInMatch()
                await db.execute(
                    "INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, "
                    "team) VALUES (?, ?, ?, ?, ?, ?)",
                    (mashup_id, guild_id, user_id, handle, now, team),
                )
        except sqlite3.IntegrityError as exc:
            raise AlreadyInMatch() from exc

    async def move_mashup_player(
        self, mashup_id: int, user_id: int, team: int, team_size: int = 0
    ) -> None:
        """Puts a lobby player into another team, unless that team is full."""
        async with self._tx() as db:
            cur = await db.execute("SELECT status FROM mashups WHERE id = ?", (mashup_id,))
            row = await cur.fetchone()
            if row is None or row["status"] != "lobby":
                raise MatchClosed("This contest is no longer open.")
            if team_size:
                cur = await db.execute(
                    "SELECT COUNT(*) FROM mashup_players "
                    "WHERE mashup_id = ? AND team = ? AND user_id != ?",
                    (mashup_id, team, user_id),
                )
                if (await cur.fetchone())[0] >= team_size:
                    raise MatchFull("That team is full.")
            await db.execute(
                "UPDATE mashup_players SET team = ? WHERE mashup_id = ? AND user_id = ?",
                (team, mashup_id, user_id),
            )

    async def remove_mashup_player(self, mashup_id: int, user_id: int) -> bool:
        n = await self._write(
            "DELETE FROM mashup_players WHERE mashup_id = ? AND user_id = ? "
            "AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'lobby')",
            (mashup_id, user_id, mashup_id),
        )
        return n > 0

    async def cancel_mashup(self, mashup_id: int) -> bool:
        async with self._tx() as db:
            cur = await db.execute(
                "UPDATE mashups SET status = 'cancelled' WHERE id = ? AND status = 'lobby'",
                (mashup_id,),
            )
            if cur.rowcount == 0:
                return False
            await db.execute(
                "UPDATE mashup_players SET active = 0 WHERE mashup_id = ?", (mashup_id,)
            )
            return True

    async def start_mashup(
        self,
        mashup_id: int,
        problems: Sequence[tuple[str, int]],
        started_at: int,
        ends_at: int,
        player_ids: Sequence[int],
        excluded: Iterable[str] = (),
    ) -> bool:
        """Starts the contest only if it is still a lobby with exactly these players. `excluded`
        are problem keys later replacements must avoid."""
        async with self._tx() as db:
            cur = await db.execute("SELECT status FROM mashups WHERE id = ?", (mashup_id,))
            row = await cur.fetchone()
            if row is None or row["status"] != "lobby":
                return False
            cur = await db.execute(
                "SELECT user_id FROM mashup_players WHERE mashup_id = ?", (mashup_id,)
            )
            if {r["user_id"] for r in await cur.fetchall()} != set(player_ids):
                return False
            await db.execute(
                "UPDATE mashups SET status = 'active', started_at = ?, ends_at = ? WHERE id = ?",
                (started_at, ends_at, mashup_id),
            )
            await db.executemany(
                "INSERT INTO mashup_problems (mashup_id, idx, problem_key, rating, cell) "
                "VALUES (?, ?, ?, ?, ?)",
                [(mashup_id, i, key, rating, i) for i, (key, rating) in enumerate(problems)],
            )
            await db.executemany(
                "INSERT INTO mashup_excluded (mashup_id, problem_key) VALUES (?, ?)",
                [(mashup_id, key) for key in set(excluded)],
            )
            return True

    async def set_mashup_thread(self, mashup_id: int, thread_id: int) -> None:
        await self._write("UPDATE mashups SET thread_id = ? WHERE id = ?", (thread_id, mashup_id))

    async def set_log_board(self, mashup_id: int, message_id: int) -> None:
        await self._write(
            "UPDATE mashups SET log_board_id = ? WHERE id = ?", (message_id, mashup_id)
        )

    async def mark_logged(self, mashup_id: int, problem_key: str) -> bool:
        """True only the first time, so a claim is posted to the thread at most once."""
        n = await self._write(
            "INSERT OR IGNORE INTO mashup_logged (mashup_id, problem_key) VALUES (?, ?)",
            (mashup_id, problem_key),
        )
        return n > 0

    async def mashup_excluded(self, mashup_id: int) -> frozenset[str]:
        rows = await self._all(
            "SELECT problem_key FROM mashup_excluded WHERE mashup_id = ?", (mashup_id,)
        )
        return frozenset(r[0] for r in rows)

    async def record_mashup_solves(
        self, mashup_id: int, user_id: int, solves: Mapping[str, tuple[int, int]]
    ) -> int:
        """Stores new (solve time, wrong attempts) per problem; the first record of each wins.
        Returns how many were new."""
        async with self._tx() as db:
            new = 0
            for key, (solved_at, wrong) in solves.items():
                cur = await db.execute(
                    "INSERT OR IGNORE INTO mashup_solves "
                    "(mashup_id, user_id, problem_key, solved_at, wrong) VALUES (?, ?, ?, ?, ?)",
                    (mashup_id, user_id, key, solved_at, wrong),
                )
                new += cur.rowcount
            return new

    async def mashup_solves(self, mashup_id: int) -> dict[int, dict[str, tuple[int, int]]]:
        rows = await self._all(
            "SELECT user_id, problem_key, solved_at, wrong FROM mashup_solves WHERE mashup_id = ?",
            (mashup_id,),
        )
        out: dict[int, dict[str, tuple[int, int]]] = {}
        for r in rows:
            out.setdefault(r["user_id"], {})[r["problem_key"]] = (r["solved_at"], r["wrong"])
        return out

    async def mark_mashup_gave_up(self, mashup_id: int, user_id: int) -> bool:
        """Frees a player of a running contest for other activities; they keep their place."""
        n = await self._write(
            "UPDATE mashup_players SET active = 0 WHERE mashup_id = ? AND user_id = ? "
            "AND active = 1 AND EXISTS (SELECT 1 FROM mashups WHERE id = ? AND status = 'active')",
            (mashup_id, user_id, mashup_id),
        )
        return n > 0

    async def finish_mashup(
        self, mashup_id: int, deltas: Mapping[int, int] | None = None, now: int | None = None
    ) -> bool:
        """Ends an active contest (at `now` if that is before its scheduled end) and stores each
        player's rating change. False when another request already ended it, so ratings are never
        applied twice."""
        async with self._tx() as db:
            cur = await db.execute(
                "UPDATE mashups SET status = 'finished', ends_at = MIN(ends_at, COALESCE(?, ends_at)) "
                "WHERE id = ? AND status = 'active'",
                (now, mashup_id),
            )
            if cur.rowcount == 0:
                return False
            await db.executemany(
                "UPDATE mashup_players SET delta = ? WHERE mashup_id = ? AND user_id = ?",
                [(d, mashup_id, uid) for uid, d in (deltas or {}).items()],
            )
            await db.execute(
                "UPDATE mashup_players SET active = 0 WHERE mashup_id = ?", (mashup_id,)
            )
            return True

    # --- reminders ---
    async def set_reminder(
        self,
        guild_id: int,
        channel_id: int | None,
        role_id: int | None,
        before_mins: Sequence[int],
        exclude: str = "",
    ) -> None:
        await self._write(
            "INSERT INTO reminder (guild_id, channel_id, role_id, before_mins, exclude) "
            "VALUES (?, ?, ?, ?, ?) ON CONFLICT (guild_id) DO UPDATE SET "
            "channel_id = excluded.channel_id, role_id = excluded.role_id, "
            "before_mins = excluded.before_mins, exclude = excluded.exclude",
            (guild_id, channel_id, role_id, json.dumps(list(before_mins)), exclude),
        )

    async def get_reminder(self, guild_id: int) -> Reminder | None:
        row = await self._one("SELECT * FROM reminder WHERE guild_id = ?", (guild_id,))
        return _reminder(row) if row else None

    async def all_reminders(self) -> list[Reminder]:
        rows = await self._all("SELECT * FROM reminder WHERE channel_id IS NOT NULL")
        return [_reminder(r) for r in rows]

    async def clear_reminder(self, guild_id: int) -> bool:
        """Forget the channel only; the role, minutes and exclude regex stay."""
        await self._write("DELETE FROM reminder_sent WHERE guild_id = ?", (guild_id,))
        return (
            await self._write(
                "UPDATE reminder SET channel_id = NULL WHERE guild_id = ? AND channel_id IS NOT NULL",
                (guild_id,),
            )
            > 0
        )

    async def set_recap(self, guild_id: int, channel_id: int) -> None:
        await self._write(
            "INSERT INTO recap (guild_id, channel_id) VALUES (?, ?) "
            "ON CONFLICT (guild_id) DO UPDATE SET channel_id = excluded.channel_id",
            (guild_id, channel_id),
        )

    async def get_recap(self, guild_id: int) -> int | None:
        row = await self._one("SELECT channel_id FROM recap WHERE guild_id = ?", (guild_id,))
        return row["channel_id"] if row else None

    async def all_recaps(self) -> list[tuple[int, int]]:
        rows = await self._all("SELECT guild_id, channel_id FROM recap")
        return [(r["guild_id"], r["channel_id"]) for r in rows]

    async def clear_recap(self, guild_id: int) -> bool:
        await self._write("DELETE FROM recap_sent WHERE guild_id = ?", (guild_id,))
        return await self._write("DELETE FROM recap WHERE guild_id = ?", (guild_id,)) > 0

    async def set_weekly(self, guild_id: int, channel_id: int, week: int) -> None:
        """week is the current week's start: a new channel waits for the next Sunday."""
        await self._write(
            "INSERT INTO weekly (guild_id, channel_id, last_sent) VALUES (?, ?, ?) "
            "ON CONFLICT (guild_id) DO UPDATE SET channel_id = excluded.channel_id",
            (guild_id, channel_id, week),
        )

    async def get_weekly(self, guild_id: int) -> int | None:
        row = await self._one("SELECT channel_id FROM weekly WHERE guild_id = ?", (guild_id,))
        return row["channel_id"] if row else None

    async def all_weeklies(self) -> list[tuple[int, int, int]]:
        """(guild_id, channel_id, last_sent)"""
        rows = await self._all("SELECT guild_id, channel_id, last_sent FROM weekly")
        return [(r["guild_id"], r["channel_id"], r["last_sent"]) for r in rows]

    async def clear_weekly(self, guild_id: int) -> bool:
        return await self._write("DELETE FROM weekly WHERE guild_id = ?", (guild_id,)) > 0

    async def mark_weekly(self, guild_id: int, week: int) -> bool:
        """True only the first time for this week, which makes the post at-most-once."""
        n = await self._write(
            "UPDATE weekly SET last_sent = ? WHERE guild_id = ? AND last_sent < ?",
            (week, guild_id, week),
        )
        return n > 0

    async def rating_events(self, guild_id: int) -> list[tuple[int, str, int, int]]:
        """(user_id, handle, finished_at, rating change) of every finished match and mashup."""
        rows = await self._all(
            "SELECT e.user_id, COALESCE(uh.handle, e.handle) AS handle, e.at, e.delta FROM ("
            "SELECT mp.guild_id, mp.user_id, mp.handle, m.finished_at AS at, mp.delta "
            "FROM match_players mp JOIN matches m ON m.id = mp.match_id "
            "WHERE mp.delta IS NOT NULL AND m.status = 'finished' "
            "UNION ALL "
            "SELECT kp.guild_id, kp.user_id, kp.handle, k.ends_at, kp.delta "
            "FROM mashup_players kp JOIN mashups k ON k.id = kp.mashup_id "
            "WHERE kp.delta IS NOT NULL AND k.status = 'finished') e "
            "LEFT JOIN user_handle uh ON uh.guild_id = e.guild_id AND uh.user_id = e.user_id "
            "WHERE e.guild_id = ?",
            (guild_id,),
        )
        return [(r["user_id"], r["handle"], r["at"], r["delta"]) for r in rows]

    async def mark_sent(self, guild_id: int, contest_id: int, before_min: int) -> bool:
        n = await self._write(
            "INSERT OR IGNORE INTO reminder_sent (guild_id, contest_id, before_min) VALUES (?, ?, ?)",
            (guild_id, contest_id, before_min),
        )
        return n == 1

    async def sent_for_guild(self, guild_id: int) -> set[tuple[int, int]]:
        rows = await self._all(
            "SELECT contest_id, before_min FROM reminder_sent WHERE guild_id = ?", (guild_id,)
        )
        return {(r["contest_id"], r["before_min"]) for r in rows}

    async def mark_recapped(self, guild_id: int, contest_id: int) -> bool:
        """True only the first time, which makes a contest recap at-most-once."""
        n = await self._write(
            "INSERT OR IGNORE INTO recap_sent (guild_id, contest_id) VALUES (?, ?)",
            (guild_id, contest_id),
        )
        return n == 1

    async def recapped(self, guild_id: int) -> set[int]:
        rows = await self._all("SELECT contest_id FROM recap_sent WHERE guild_id = ?", (guild_id,))
        return {r["contest_id"] for r in rows}

    # --- caches ---
    async def replace_contests(self, contests: Sequence[Contest]) -> None:
        async with self._tx() as db:
            await db.execute("DELETE FROM contest")
            await db.executemany(
                "INSERT INTO contest (id, name, type, phase, start_time, duration) VALUES (?, ?, ?, ?, ?, ?)",
                [(c.id, c.name, c.type, c.phase, c.start_time, c.duration) for c in contests],
            )

    async def load_contests(self) -> list[Contest]:
        rows = await self._all("SELECT * FROM contest")
        return [
            Contest(r["id"], r["name"], r["type"], r["phase"], r["start_time"], r["duration"])
            for r in rows
        ]

    async def replace_problems(self, problems: Sequence[Problem]) -> None:
        async with self._tx() as db:
            await db.execute("DELETE FROM problem")
            await db.executemany(
                "INSERT INTO problem (contest_id, idx, name, rating, tags) VALUES (?, ?, ?, ?, ?)",
                [
                    (p.contest_id, p.index, p.name, p.rating, json.dumps(list(p.tags)))
                    for p in problems
                ],
            )

    async def load_problems(self) -> list[Problem]:
        rows = await self._all("SELECT * FROM problem")
        return [
            Problem(r["contest_id"], r["idx"], r["name"], r["rating"], tuple(json.loads(r["tags"])))
            for r in rows
        ]
