import asyncio
import json
import sqlite3
from collections.abc import AsyncIterator, Mapping, Sequence
from contextlib import asynccontextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import aiosqlite

from src.cf.models import Contest, Problem
from src.errors import AlreadyInMatch, HandleTaken, MatchClosed, MatchFull
from src.logic import START_RATING

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
class MashupRow:
    id: int
    guild_id: int
    channel_id: int
    message_id: int | None
    host_id: int
    started_at: int
    ends_at: int
    status: str


@dataclass(frozen=True)
class MashupPlayer:
    user_id: int
    handle: str
    joined_at: int


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
    channel_id: int
    role_id: int | None
    before_mins: tuple[int, ...]


def _reminder(row: aiosqlite.Row) -> Reminder:
    return Reminder(
        row["guild_id"], row["channel_id"], row["role_id"], tuple(json.loads(row["before_mins"]))
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
            await self._db.executescript(f"{script}\nPRAGMA user_version = {number};")
        await self._db.commit()

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
    ) -> int:
        try:
            async with self._tx() as db:
                cur = await db.execute(
                    "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, "
                    "invited_id, rating_arg, status, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, 'lobby', ?)",
                    (guild_id, channel_id, host_id, mode, max_players, invited_id, rating_arg, now),
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

    async def finish_match(self, match_id: int, now: int, deltas: Mapping[int, int]) -> bool:
        async with self._tx() as db:
            cur = await db.execute(
                "UPDATE matches SET status = 'finished', finished_at = ? "
                "WHERE id = ? AND status = 'active'",
                (now, match_id),
            )
            if cur.rowcount == 0:
                return False
            await db.executemany(
                "UPDATE match_players SET delta = ? WHERE match_id = ? AND user_id = ?",
                [(d, match_id, uid) for uid, d in deltas.items()],
            )
            await db.execute("UPDATE match_players SET active = 0 WHERE match_id = ?", (match_id,))
            return True

    async def rating(self, guild_id: int, user_id: int) -> int:
        """Match rating: the start rating plus every stored rating change."""
        row = await self._one(
            "SELECT COALESCE(SUM(delta), 0) FROM match_players "
            "WHERE guild_id = ? AND user_id = ? AND delta IS NOT NULL",
            (guild_id, user_id),
        )
        return START_RATING + row[0]

    async def stats(self, guild_id: int) -> list[tuple[int, str, int, int]]:
        """(user_id, handle, match rating, finished matches), best rated first. Finished matches only."""
        rows = await self._all(
            "SELECT mp.user_id, COALESCE(uh.handle, MAX(mp.handle)) AS handle, "
            "? + SUM(mp.delta) AS total, COUNT(*) AS played "
            "FROM match_players mp "
            "LEFT JOIN user_handle uh ON uh.guild_id = mp.guild_id AND uh.user_id = mp.user_id "
            "WHERE mp.guild_id = ? AND mp.delta IS NOT NULL "
            "GROUP BY mp.user_id ORDER BY total DESC, played DESC, handle",
            (START_RATING, guild_id),
        )
        return [(r["user_id"], r["handle"], r["total"], r["played"]) for r in rows]

    async def rating_changes(self, guild_id: int, user_id: int) -> list[tuple[int, int]]:
        """(finished_at, rating change) of the player's finished matches, oldest first."""
        rows = await self._all(
            "SELECT m.finished_at, mp.delta FROM match_players mp "
            "JOIN matches m ON m.id = mp.match_id "
            "WHERE mp.guild_id = ? AND mp.user_id = ? AND mp.delta IS NOT NULL "
            "AND m.status = 'finished' ORDER BY m.finished_at, m.id",
            (guild_id, user_id),
        )
        return [(r["finished_at"], r["delta"]) for r in rows]

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
    async def create_mashup(
        self,
        *,
        guild_id: int,
        channel_id: int,
        host_id: int,
        host_handle: str,
        problems: Sequence[tuple[str, int]],
        started_at: int,
        ends_at: int,
    ) -> int:
        async with self._tx() as db:
            cur = await db.execute(
                "INSERT INTO mashups (guild_id, channel_id, host_id, started_at, ends_at, status) "
                "VALUES (?, ?, ?, ?, ?, 'active')",
                (guild_id, channel_id, host_id, started_at, ends_at),
            )
            mashup_id = cur.lastrowid
            await db.executemany(
                "INSERT INTO mashup_problems (mashup_id, idx, problem_key, rating) "
                "VALUES (?, ?, ?, ?)",
                [(mashup_id, i, key, rating) for i, (key, rating) in enumerate(problems)],
            )
            await db.execute(
                "INSERT INTO mashup_players (mashup_id, user_id, handle, joined_at) "
                "VALUES (?, ?, ?, ?)",
                (mashup_id, host_id, host_handle, started_at),
            )
            return mashup_id

    async def get_mashup(self, mashup_id: int) -> MashupRow | None:
        row = await self._one("SELECT * FROM mashups WHERE id = ?", (mashup_id,))
        return MashupRow(**dict(row)) if row else None

    async def active_mashups(self) -> list[MashupRow]:
        rows = await self._all("SELECT * FROM mashups WHERE status = 'active' ORDER BY id")
        return [MashupRow(**dict(r)) for r in rows]

    async def set_mashup_message(self, mashup_id: int, channel_id: int, message_id: int) -> None:
        await self._write(
            "UPDATE mashups SET channel_id = ?, message_id = ? WHERE id = ?",
            (channel_id, message_id, mashup_id),
        )

    async def mashup_problems(self, mashup_id: int) -> list[tuple[str, int]]:
        rows = await self._all(
            "SELECT problem_key, rating FROM mashup_problems WHERE mashup_id = ? ORDER BY idx",
            (mashup_id,),
        )
        return [(r["problem_key"], r["rating"]) for r in rows]

    async def mashup_players(self, mashup_id: int) -> list[MashupPlayer]:
        rows = await self._all(
            "SELECT user_id, handle, joined_at FROM mashup_players WHERE mashup_id = ? "
            "ORDER BY joined_at, rowid",
            (mashup_id,),
        )
        return [MashupPlayer(r["user_id"], r["handle"], r["joined_at"]) for r in rows]

    async def add_mashup_player(
        self, mashup_id: int, user_id: int, handle: str, now: int, cap: int
    ) -> str | None:
        """Adds the player; returns why not ("full", "joined") or None on success."""
        async with self._tx() as db:
            cur = await db.execute(
                "SELECT COUNT(*), COALESCE(SUM(user_id = ?), 0) FROM mashup_players "
                "WHERE mashup_id = ?",
                (user_id, mashup_id),
            )
            total, already = await cur.fetchone()
            if already:
                return "joined"
            if total >= cap:
                return "full"
            await db.execute(
                "INSERT INTO mashup_players (mashup_id, user_id, handle, joined_at) "
                "VALUES (?, ?, ?, ?)",
                (mashup_id, user_id, handle, now),
            )
            return None

    async def record_mashup_solves(
        self, mashup_id: int, user_id: int, solves: Mapping[str, int]
    ) -> int:
        """Stores new solves (the first time of each problem wins); returns how many were new."""
        async with self._tx() as db:
            new = 0
            for key, solved_at in solves.items():
                cur = await db.execute(
                    "INSERT OR IGNORE INTO mashup_solves "
                    "(mashup_id, user_id, problem_key, solved_at) VALUES (?, ?, ?, ?)",
                    (mashup_id, user_id, key, solved_at),
                )
                new += cur.rowcount
            return new

    async def mashup_solves(self, mashup_id: int) -> dict[int, dict[str, int]]:
        rows = await self._all(
            "SELECT user_id, problem_key, solved_at FROM mashup_solves WHERE mashup_id = ?",
            (mashup_id,),
        )
        out: dict[int, dict[str, int]] = {}
        for r in rows:
            out.setdefault(r["user_id"], {})[r["problem_key"]] = r["solved_at"]
        return out

    async def finish_mashup(self, mashup_id: int) -> bool:
        return (
            await self._write(
                "UPDATE mashups SET status = 'finished' WHERE id = ? AND status = 'active'",
                (mashup_id,),
            )
            > 0
        )

    # --- reminders ---
    async def set_reminder(
        self, guild_id: int, channel_id: int, role_id: int | None, before_mins: Sequence[int]
    ) -> None:
        await self._write(
            "INSERT INTO reminder (guild_id, channel_id, role_id, before_mins) VALUES (?, ?, ?, ?) "
            "ON CONFLICT (guild_id) DO UPDATE SET channel_id = excluded.channel_id, "
            "role_id = excluded.role_id, before_mins = excluded.before_mins",
            (guild_id, channel_id, role_id, json.dumps(list(before_mins))),
        )

    async def get_reminder(self, guild_id: int) -> Reminder | None:
        row = await self._one("SELECT * FROM reminder WHERE guild_id = ?", (guild_id,))
        return _reminder(row) if row else None

    async def all_reminders(self) -> list[Reminder]:
        return [_reminder(r) for r in await self._all("SELECT * FROM reminder")]

    async def clear_reminder(self, guild_id: int) -> bool:
        await self._write("DELETE FROM reminder_sent WHERE guild_id = ?", (guild_id,))
        return await self._write("DELETE FROM reminder WHERE guild_id = ?", (guild_id,)) > 0

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
