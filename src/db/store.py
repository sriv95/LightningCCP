import json
import sqlite3
from collections.abc import Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import aiosqlite

from src.cf.models import Contest, Problem
from src.errors import ChallengeConflict, HandleTaken

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
class Challenge:
    id: int
    problem_key: str
    rating: int
    issued_at: int


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

    async def _write(self, sql: str, params: Sequence[Any] = ()) -> int:
        cur = await self._db.execute(sql, params)
        await self._db.commit()
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

    # --- challenges ---
    async def open_challenge(
        self, guild_id: int, user_id: int, problem_key: str, rating: int, issued_at: int
    ) -> None:
        try:
            await self._write(
                "INSERT INTO challenge (guild_id, user_id, problem_key, rating, issued_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (guild_id, user_id, problem_key, rating, issued_at),
            )
        except sqlite3.IntegrityError as exc:
            raise ChallengeConflict() from exc

    async def get_open_challenge(self, guild_id: int, user_id: int) -> Challenge | None:
        row = await self._one(
            "SELECT id, problem_key, rating, issued_at FROM challenge "
            "WHERE guild_id = ? AND user_id = ? AND solved_at IS NULL",
            (guild_id, user_id),
        )
        return (
            Challenge(row["id"], row["problem_key"], row["rating"], row["issued_at"])
            if row
            else None
        )

    async def close_challenge(self, challenge_id: int, solved_at: int) -> None:
        await self._write(
            "UPDATE challenge SET solved_at = ? WHERE id = ?", (solved_at, challenge_id)
        )

    async def cancel_challenge(self, guild_id: int, user_id: int) -> bool:
        n = await self._write(
            "DELETE FROM challenge WHERE guild_id = ? AND user_id = ? AND solved_at IS NULL",
            (guild_id, user_id),
        )
        return n > 0

    async def solved_ratings(self, guild_id: int, user_id: int) -> list[int]:
        rows = await self._all(
            "SELECT rating FROM challenge WHERE guild_id = ? AND user_id = ? AND solved_at IS NOT NULL",
            (guild_id, user_id),
        )
        return [r["rating"] for r in rows]

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
        await self._db.execute("DELETE FROM contest")
        await self._db.executemany(
            "INSERT INTO contest (id, name, type, phase, start_time, duration) VALUES (?, ?, ?, ?, ?, ?)",
            [(c.id, c.name, c.type, c.phase, c.start_time, c.duration) for c in contests],
        )
        await self._db.commit()

    async def load_contests(self) -> list[Contest]:
        rows = await self._all("SELECT * FROM contest")
        return [
            Contest(r["id"], r["name"], r["type"], r["phase"], r["start_time"], r["duration"])
            for r in rows
        ]

    async def replace_problems(self, problems: Sequence[Problem]) -> None:
        await self._db.execute("DELETE FROM problem")
        await self._db.executemany(
            "INSERT INTO problem (contest_id, idx, name, rating, tags) VALUES (?, ?, ?, ?, ?)",
            [(p.contest_id, p.index, p.name, p.rating, json.dumps(list(p.tags))) for p in problems],
        )
        await self._db.commit()

    async def load_problems(self) -> list[Problem]:
        rows = await self._all("SELECT * FROM problem")
        return [
            Problem(r["contest_id"], r["idx"], r["name"], r["rating"], tuple(json.loads(r["tags"])))
            for r in rows
        ]
