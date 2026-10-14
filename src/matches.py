import logging
import random
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from src.cf.models import Problem
from src.db.store import MatchPlayer, MatchRow, Store
from src.errors import BotError, MatchClosed, NotLinked
from src.logic import (
    LOBBY_TTL_SECONDS,
    MATCH_TTL_SECONDS,
    earliest_solve,
    filter_problems,
    match_deltas,
    match_rating,
    rank_groups,
    solo_delta,
    solved_keys,
)

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class MatchView:
    match: MatchRow
    players: list[MatchPlayer]
    problem: Problem | None = None
    ratings: dict[int, int] | None = None  # new match ratings; set only for finished matches
    note: str | None = None


class MatchService:
    """Every match state transition. Each transition is a guarded store call, so concurrent
    button presses cannot start, cancel or score a match twice."""

    def __init__(
        self, store: Store, cf: Any, problems: Any, clock: Callable[[], float] = time.time
    ):
        self._store = store
        self._cf = cf
        self._problems = problems
        self._clock = clock

    def _now(self) -> int:
        return int(self._clock())

    async def _match(self, match_id: int) -> MatchRow:
        match = await self._store.get_match(match_id)
        if match is None:
            raise MatchClosed("That match no longer exists.")
        return match

    async def _view(self, match_id: int, *, note: str | None = None) -> MatchView:
        match = await self._match(match_id)
        players = await self._store.get_players(match_id)
        problem = None
        if match.problem_key:
            problem = next((p for p in self._problems.problems if p.key == match.problem_key), None)
        ratings = None
        if match.status == "finished":
            ratings = {
                p.user_id: await self._store.rating(match.guild_id, p.user_id) for p in players
            }
        return MatchView(match, players, problem, ratings, note)

    async def _linked_handle(self, guild_id: int, user_id: int) -> str:
        linked = await self._store.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        return linked.handle

    async def set_message(self, match_id: int, channel_id: int, message_id: int) -> None:
        await self._store.set_message(match_id, channel_id, message_id)

    async def current(self, guild_id: int, user_id: int) -> MatchView | None:
        match_id = await self._store.active_match_for(guild_id, user_id)
        return await self._view(match_id) if match_id is not None else None

    async def create(
        self,
        *,
        guild_id: int,
        channel_id: int,
        host_id: int,
        mode: str,
        max_players: int,
        invited_id: int | None = None,
        rating_arg: int | None = None,
    ) -> MatchView:
        handle = await self._linked_handle(guild_id, host_id)
        if invited_id is not None and invited_id == host_id:
            raise BotError("You cannot duel yourself.")
        match_id = await self._store.create_match(
            guild_id=guild_id,
            channel_id=channel_id,
            host_id=host_id,
            host_handle=handle,
            mode=mode,
            max_players=max_players,
            invited_id=invited_id,
            rating_arg=rating_arg,
            now=self._now(),
        )
        if mode != "solo":
            return await self._view(match_id)
        try:
            return await self._start(match_id)
        except Exception:
            await self._store.cancel_match(match_id, self._now())
            raise

    async def rematch(self, match_id: int, user_id: int, channel_id: int) -> MatchView:
        """New duel lobby against the same opponent, hosted by whoever pressed Rematch."""
        match = await self._match(match_id)
        if match.status != "finished" or match.mode != "duel":
            raise MatchClosed("Only a finished duel can be rematched.")
        ids = [p.user_id for p in await self._store.get_players(match_id)]
        if user_id not in ids:
            raise BotError("Only the players of this duel can ask for a rematch.")
        opponent = next(uid for uid in ids if uid != user_id)
        return await self.create(
            guild_id=match.guild_id,
            channel_id=channel_id,
            host_id=user_id,
            mode="duel",
            max_players=2,
            invited_id=opponent,
            rating_arg=match.rating_arg,
        )

    async def join(self, match_id: int, user_id: int) -> MatchView:
        match = await self._match(match_id)
        if match.status != "lobby":
            raise MatchClosed()
        if match.invited_id is not None and user_id != match.invited_id:
            raise BotError(f"This duel is reserved for <@{match.invited_id}>.")
        handle = await self._linked_handle(match.guild_id, user_id)
        await self._store.add_player(match_id, match.guild_id, user_id, handle, self._now())
        return await self._view(match_id)

    async def leave(self, match_id: int, user_id: int) -> MatchView:
        match = await self._match(match_id)
        players = await self._store.get_players(match_id)
        if user_id not in {p.user_id for p in players}:
            raise BotError("You are not in this match.")
        if match.status == "active":
            raise MatchClosed("The match already started. Use Give Up instead.")
        if match.status != "lobby":
            raise MatchClosed()
        if user_id == match.host_id:
            if not await self._store.cancel_match(match_id, self._now()):
                raise MatchClosed()
            return await self._view(match_id, note="The host left, so the match was cancelled.")
        if not await self._store.remove_player(match_id, user_id):
            raise MatchClosed()
        return await self._view(match_id)

    async def start(self, match_id: int, user_id: int) -> MatchView:
        match = await self._match(match_id)
        if user_id != match.host_id:
            raise BotError("Only the host can start the match.")
        if match.status != "lobby":
            raise MatchClosed()
        if len(await self._store.get_players(match_id)) < 2:
            raise BotError("You need at least 2 players to start.")
        return await self._start(match_id)

    async def _start(self, match_id: int) -> MatchView:
        match = await self._match(match_id)
        players = await self._store.get_players(match_id)
        if match.mode != "solo" and len(players) < 2:
            raise BotError("You need at least 2 players to start.")
        problems = self._problems.require()
        ratings = [await self._store.rating(match.guild_id, p.user_id) for p in players]
        solved: set[str] = set()
        for p in players:
            solved |= solved_keys(await self._cf.user_status(p.handle))
        rating = match_rating(match.rating_arg, ratings)
        pool = filter_problems(
            problems, min_rating=rating, max_rating=rating, solved=frozenset(solved)
        )
        if not pool:
            raise BotError(f"No problem rated {rating} is unsolved by every player.")
        problem = random.choice(pool)
        now = self._now()
        if not await self._store.start_match(
            match_id,
            rating,
            problem.key,
            now,
            now + MATCH_TTL_SECONDS,
            [p.user_id for p in players],
        ):
            if (await self._match(match_id)).status == "lobby":
                raise BotError("The players changed while starting. Press Start again.")
            raise MatchClosed()
        return await self._view(match_id)

    async def check(self, match_id: int, user_id: int) -> MatchView:
        match = await self._match(match_id)
        if match.status != "active":
            raise MatchClosed("This match is not running.")
        players = await self._store.get_players(match_id)
        me = next((p for p in players if p.user_id == user_id), None)
        if me is None or me.gave_up:
            raise BotError("You are not playing in this match.")
        players = await self._refresh_solves(match, players, tolerant=True, strict_user=user_id)
        return await self._maybe_finish(match, players)

    async def give_up(self, match_id: int, user_id: int) -> MatchView:
        match = await self._match(match_id)
        if match.status != "active":
            raise MatchClosed("This match is not running.")
        players = await self._store.get_players(match_id)
        me = next((p for p in players if p.user_id == user_id), None)
        if me is None or me.gave_up:
            raise BotError("You are not playing in this match.")
        if me.solved_at is not None:
            raise BotError("You already solved it, so there is nothing to give up.")
        if not await self._store.mark_gave_up(match_id, user_id):
            raise MatchClosed()
        players = await self._store.get_players(match_id)
        if match.mode == "solo":
            return await self._finish(match, players)
        if match.mode == "duel":
            opponent = next(p for p in players if p.user_id != user_id)
            return await self._finish(match, players, groups=[[opponent.user_id], [user_id]])
        return await self._maybe_finish(match, players)

    async def expire_due(self) -> list[MatchView]:
        now = self._now()
        views: list[MatchView] = []
        for match in await self._store.matches_due(now - LOBBY_TTL_SECONDS, now):
            try:
                if match.status == "lobby":
                    if await self._store.cancel_match(match.id, now):
                        views.append(await self._view(match.id, note="The lobby expired."))
                else:
                    players = await self._refresh_solves(
                        match, await self._store.get_players(match.id), tolerant=True
                    )
                    views.append(
                        await self._maybe_finish(match, players, force=True, note="Time is up.")
                    )
            except Exception:
                log.exception("Could not expire match %s", match.id)
        return views

    async def _refresh_solves(
        self,
        match: MatchRow,
        players: list[MatchPlayer],
        *,
        tolerant: bool = False,
        strict_user: int | None = None,
    ) -> list[MatchPlayer]:
        found: dict[int, int] = {}
        for p in players:
            if p.solved_at is not None or p.gave_up:
                continue
            try:
                subs = await self._cf.user_status(p.handle, count=100)
            except Exception:
                if not tolerant or p.user_id == strict_user:
                    raise
                # One unreachable handle (e.g. renamed) must not block expiry or other players' Check.
                log.warning(
                    "Skipping %s while expiring match %s", p.handle, match.id, exc_info=True
                )
                continue
            solved_at = earliest_solve(subs, match.problem_key, match.started_at)
            if solved_at is not None:
                found[p.user_id] = solved_at
        if found:
            await self._store.record_solves(match.id, found)
        return await self._store.get_players(match.id)

    async def _maybe_finish(
        self,
        match: MatchRow,
        players: list[MatchPlayer],
        *,
        force: bool = False,
        note: str | None = None,
    ) -> MatchView:
        if match.mode == "custom":
            done = all(p.solved_at is not None or p.gave_up for p in players)
        else:  # solo and duel end at the first solve
            done = any(p.solved_at is not None for p in players)
        if not (done or force):
            return await self._view(match.id)
        return await self._finish(match, players, note=note)

    async def _finish(
        self,
        match: MatchRow,
        players: list[MatchPlayer],
        *,
        groups: list[list[int]] | None = None,
        note: str | None = None,
    ) -> MatchView:
        ratings = {p.user_id: await self._store.rating(match.guild_id, p.user_id) for p in players}
        any_solved = any(p.solved_at is not None for p in players)
        if match.mode == "solo":
            (p,) = players
            deltas = {
                p.user_id: solo_delta(ratings[p.user_id], match.rating, p.solved_at is not None)
            }
        elif groups is not None:
            deltas = match_deltas(groups, ratings)
        elif not any_solved:
            deltas = {p.user_id: 0 for p in players}
        else:
            deltas = match_deltas(rank_groups([(p.user_id, p.solved_at) for p in players]), ratings)
        # False means another request already finished this match; show its result.
        await self._store.finish_match(match.id, self._now(), deltas)
        return await self._view(match.id, note=note)
