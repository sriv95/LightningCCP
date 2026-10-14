import logging
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from src.db.store import MashupPlayer, MashupRow, Store
from src.errors import BotError, MatchClosed, NotLinked
from src.logic import (
    MASHUP_MAX_PLAYERS,
    MashupRank,
    earliest_solve,
    mashup_standings,
    pick_mashup,
    solved_keys,
)

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class MashupView:
    mashup: MashupRow
    problems: list[tuple[str, int]]  # (problem key, rating), easy to hard
    players: list[MashupPlayer]
    solves: dict[int, dict[str, int]]

    @property
    def standings(self) -> list[MashupRank]:
        return mashup_standings(
            ((p.user_id, p.handle, p.joined_at) for p in self.players),
            self.solves,
            self.mashup.started_at,
        )


class MashupService:
    """Timed multi-problem contests. Solves come from Codeforces submissions made between the start
    and the end; the standings rank by problems solved, then total solve time."""

    def __init__(
        self, store: Store, cf: Any, problems: Any, clock: Callable[[], float] = time.time
    ):
        self._store = store
        self._cf = cf
        self._problems = problems
        self._clock = clock

    def _now(self) -> int:
        return int(self._clock())

    async def view(self, mashup_id: int) -> MashupView:
        mashup = await self._store.get_mashup(mashup_id)
        if mashup is None:
            raise MatchClosed("That contest no longer exists.")
        return MashupView(
            mashup,
            await self._store.mashup_problems(mashup_id),
            await self._store.mashup_players(mashup_id),
            await self._store.mashup_solves(mashup_id),
        )

    async def set_message(self, mashup_id: int, channel_id: int, message_id: int) -> None:
        await self._store.set_mashup_message(mashup_id, channel_id, message_id)

    async def _handle(self, guild_id: int, user_id: int) -> str:
        linked = await self._store.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        return linked.handle

    async def create(
        self,
        *,
        guild_id: int,
        channel_id: int,
        host_id: int,
        min_rating: int,
        max_rating: int,
        count: int,
        duration_seconds: int,
    ) -> MashupView:
        if min_rating > max_rating:
            raise BotError("min_rating cannot be higher than max_rating.")
        if duration_seconds <= 0:
            raise BotError("The contest must last at least one minute.")
        handle = await self._handle(guild_id, host_id)
        problems = self._problems.require()
        # Only the host's solved problems are excluded; players who join later may have solved some.
        solved = solved_keys(await self._cf.user_status(handle))
        picked = pick_mashup(problems, min_rating, max_rating, count, solved)
        if picked is None:
            raise BotError(
                f"Not enough unsolved problems rated {min_rating}-{max_rating} for {count}."
            )
        now = self._now()
        mashup_id = await self._store.create_mashup(
            guild_id=guild_id,
            channel_id=channel_id,
            host_id=host_id,
            host_handle=handle,
            problems=[(p.key, p.rating) for p in picked],
            started_at=now,
            ends_at=now + duration_seconds,
        )
        return await self.view(mashup_id)

    async def _open(self, mashup_id: int) -> MashupRow:
        mashup = await self._store.get_mashup(mashup_id)
        if mashup is None or mashup.status != "active" or self._now() >= mashup.ends_at:
            raise MatchClosed("This contest has ended.")
        return mashup

    async def join(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._open(mashup_id)
        handle = await self._handle(mashup.guild_id, user_id)
        why = await self._store.add_mashup_player(
            mashup_id, user_id, handle, self._now(), MASHUP_MAX_PLAYERS
        )
        if why == "joined":
            raise BotError("You already joined this contest.")
        if why == "full":
            raise BotError(f"This contest is full ({MASHUP_MAX_PLAYERS} players).")
        return await self.view(mashup_id)

    async def check(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._open(mashup_id)
        player = next(
            (p for p in await self._store.mashup_players(mashup_id) if p.user_id == user_id), None
        )
        if player is None:
            raise BotError("Join the contest first.")
        await self._refresh(mashup, player)
        return await self.view(mashup_id)

    async def _refresh(self, mashup: MashupRow, player: MashupPlayer) -> int:
        """Reads the player's recent submissions; returns how many new solves it stored."""
        subs = await self._cf.user_status(player.handle, count=100)
        solves = {}
        for key, _ in await self._store.mashup_problems(mashup.id):
            at = earliest_solve(subs, key, mashup.started_at, until=mashup.ends_at)
            if at is not None:
                solves[key] = at
        return await self._store.record_mashup_solves(mashup.id, player.user_id, solves)

    async def tick(self) -> list[MashupView]:
        """Refreshes every active contest and ends the ones that ran out of time. Returns the
        contests whose message needs an update."""
        views: list[MashupView] = []
        for mashup in await self._store.active_mashups():
            try:
                changed = 0
                for player in await self._store.mashup_players(mashup.id):
                    try:
                        changed += await self._refresh(mashup, player)
                    except Exception:
                        # One unreachable handle must not stall the others or the ending.
                        log.warning(
                            "Skipping %s in mashup %s", player.handle, mashup.id, exc_info=True
                        )
                ended = self._now() >= mashup.ends_at
                if ended:
                    await self._store.finish_mashup(mashup.id)
                if changed or ended:
                    views.append(await self.view(mashup.id))
            except Exception:
                log.exception("Could not update mashup %s", mashup.id)
        return views
