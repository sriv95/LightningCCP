import random
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from src.cf.models import Problem
from src.db.store import DailyRow, Store
from src.errors import BotError, NotLinked
from src.logic import (
    DAILY_MULTIPLIER,
    daily_streak,
    earliest_solve,
    filter_problems,
    floor_rating,
    solo_delta,
    solved_keys,
    utc_day,
)


@dataclass(frozen=True)
class DailyView:
    row: DailyRow
    problem: Problem | None
    delta: int | None = None  # rating change booked by this call, when it just solved the daily
    streak: int = 0  # consecutive claimed days, one missed day forgiven


class DailyService:
    """The daily problem: one per user per UTC day, solved for double the solo rating change."""

    def __init__(
        self, store: Store, cf: Any, problems: Any, clock: Callable[[], float] = time.time
    ):
        self._store = store
        self._cf = cf
        self._problems = problems
        self._clock = clock

    def _problem(self, row: DailyRow) -> Problem | None:
        return next((p for p in self._problems.problems if p.key == row.problem_key), None)

    async def _view(self, row: DailyRow, delta: int | None = None) -> DailyView:
        days = await self._store.daily_days(row.guild_id, row.user_id)
        return DailyView(row, self._problem(row), delta, daily_streak(days, row.day))

    async def claim(
        self, guild_id: int, channel_id: int, user_id: int, rating: int | None = None
    ) -> DailyView:
        """Issue today's problem, or check whether the user solved it since it was issued.

        rating picks the problem's rating (default: match rating floored to 100). Asking for a
        different rating than an unsolved daily has replaces it with a new problem."""
        linked = await self._store.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        now = int(self._clock())
        day = utc_day(now)
        row = await self._store.get_daily(guild_id, user_id, day)
        if row is not None and row.solved_at is not None:
            return DailyView(row, self._problem(row))
        reroll = row is None or (rating is not None and rating != row.rating)
        # A new problem needs every solved problem; a plain check only the recent submissions.
        subs = await self._cf.user_status(linked.handle, **({} if reroll else {"count": 100}))
        if row is not None:
            solved_at = earliest_solve(subs, row.problem_key, row.issued_at)
            if solved_at is not None:
                return await self._book(guild_id, channel_id, linked.handle, row, solved_at, now)
            if not reroll:
                return await self._view(row)
        if rating is None:
            rating = floor_rating(await self._store.rating(guild_id, user_id))
        pool = filter_problems(
            self._problems.require(),
            min_rating=rating,
            max_rating=rating,
            solved=solved_keys(subs),
        )
        if not pool:
            raise BotError(f"You have solved every problem rated {rating}.")
        await self._store.issue_daily(guild_id, user_id, day, random.choice(pool).key, rating, now)
        return await self._view(await self._store.get_daily(guild_id, user_id, day))

    async def _book(
        self, guild_id: int, channel_id: int, handle: str, row: DailyRow, solved_at: int, now: int
    ) -> DailyView:
        mine = await self._store.rating(guild_id, row.user_id)
        delta = DAILY_MULTIPLIER * solo_delta(mine, row.rating, True)
        booked = await self._store.record_daily_solve(
            guild_id=guild_id,
            channel_id=channel_id,
            user_id=row.user_id,
            handle=handle,
            day=row.day,
            solved_at=solved_at,
            delta=delta,
            now=now,
        )
        row = await self._store.get_daily(guild_id, row.user_id, row.day)
        return await self._view(row, delta if booked else None)
