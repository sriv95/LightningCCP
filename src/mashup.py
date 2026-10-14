import logging
import time
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from src.db.store import MashupPlayer, MashupRow, Store
from src.errors import BotError, MatchClosed, NotLinked
from src.logic import (
    LOBBY_TTL_SECONDS,
    MASHUP_MAX_PLAYERS,
    MashupRank,
    first_solve,
    mashup_standings,
    pick_mashup,
    solved_keys,
    team_solves,
)

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class MashupView:
    mashup: MashupRow
    problems: list[tuple[str, int]]  # (problem key, rating), easy to hard; empty in the lobby
    players: list[MashupPlayer]
    solves: dict[int, dict[str, tuple[int, int]]]  # user -> problem -> (solve time, wrong)
    note: str | None = None

    @property
    def standings(self) -> list[MashupRank]:
        """Players, or in a team contest the teams: a problem counts for a team once, from the
        teammate who solved it first (MashupRank.user_id is then the team number)."""
        if self.mashup.teams:
            joined: dict[int, int] = {}
            for p in self.players:
                joined[p.team] = min(joined.get(p.team, p.joined_at), p.joined_at)
            return mashup_standings(
                ((team, f"Team {team}", at) for team, at in joined.items()),
                team_solves(((p.user_id, p.team) for p in self.players), self.solves),
                self.mashup.started_at or 0,
                self.mashup.penalty_minutes,
            )
        return mashup_standings(
            ((p.user_id, p.handle, p.joined_at) for p in self.players),
            self.solves,
            self.mashup.started_at or 0,
            self.mashup.penalty_minutes,
        )


class MashupService:
    """Timed multi-problem contests: a lobby, then a start by the host (no late joiners). Solves
    come from Codeforces submissions made between the start and the end; the standings rank by
    problems solved, then total time including wrong-attempt penalties."""

    def __init__(
        self, store: Store, cf: Any, problems: Any, clock: Callable[[], float] = time.time
    ):
        self._store = store
        self._cf = cf
        self._problems = problems
        self._clock = clock

    def _now(self) -> int:
        return int(self._clock())

    async def view(self, mashup_id: int, *, note: str | None = None) -> MashupView:
        mashup = await self._store.get_mashup(mashup_id)
        if mashup is None:
            raise MatchClosed("That contest no longer exists.")
        return MashupView(
            mashup,
            await self._store.mashup_problems(mashup_id),
            await self._store.mashup_players(mashup_id),
            await self._store.mashup_solves(mashup_id),
            note,
        )

    async def set_message(self, mashup_id: int, channel_id: int, message_id: int) -> None:
        await self._store.set_mashup_message(mashup_id, channel_id, message_id)

    async def _handle(self, guild_id: int, user_id: int) -> str:
        linked = await self._store.get_handle(guild_id, user_id)
        if linked is None:
            raise NotLinked()
        return linked.handle

    async def _mashup(self, mashup_id: int) -> MashupRow:
        mashup = await self._store.get_mashup(mashup_id)
        if mashup is None:
            raise MatchClosed("That contest no longer exists.")
        return mashup

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
        penalty_minutes: int = 0,
        teams: int = 0,
        max_players: int = MASHUP_MAX_PLAYERS,
    ) -> MashupView:
        if min_rating > max_rating:
            raise BotError("min_rating cannot be higher than max_rating.")
        if not 1 <= max_players <= MASHUP_MAX_PLAYERS:
            raise BotError(f"The player cap must be from 1 to {MASHUP_MAX_PLAYERS}.")
        if duration_seconds <= 0:
            raise BotError("The contest must last at least one minute.")
        mashup_id = await self._store.create_mashup(
            guild_id=guild_id,
            channel_id=channel_id,
            host_id=host_id,
            host_handle=await self._handle(guild_id, host_id),
            min_rating=min_rating,
            max_rating=max_rating,
            count=count,
            duration_seconds=duration_seconds,
            penalty_minutes=penalty_minutes,
            now=self._now(),
            teams=teams,
            host_team=1 if teams else None,  # the host always starts in team 1
            max_players=max_players,
        )
        return await self.view(mashup_id)

    @staticmethod
    def _check_team(teams: int, team: int | None) -> None:
        if team is None or not 1 <= team <= teams:
            raise BotError(f"Pick a team number from 1 to {teams}.")

    async def join(self, mashup_id: int, user_id: int, team: int | None = None) -> MashupView:
        mashup = await self._mashup(mashup_id)
        if mashup.status != "lobby":
            raise MatchClosed("This contest has already started, so nobody can join.")
        if mashup.teams:
            self._check_team(mashup.teams, team)
        else:
            team = None
        await self._store.add_mashup_player(
            mashup_id,
            mashup.guild_id,
            user_id,
            await self._handle(mashup.guild_id, user_id),
            self._now(),
            mashup.max_players,
            team,
        )
        return await self.view(mashup_id)

    async def leave(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._mashup(mashup_id)
        players = await self._store.mashup_players(mashup_id)
        if user_id not in {p.user_id for p in players}:
            raise BotError("You are not in this contest.")
        if mashup.status != "lobby":
            raise MatchClosed("The contest already started.")
        if user_id == mashup.host_id:
            if not await self._store.cancel_mashup(mashup_id):
                raise MatchClosed()
            return await self.view(mashup_id, note="The host left, so the contest was cancelled.")
        if not await self._store.remove_mashup_player(mashup_id, user_id):
            raise MatchClosed()
        return await self.view(mashup_id)

    async def start(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._mashup(mashup_id)
        if user_id != mashup.host_id:
            raise BotError("Only the host can start the contest.")
        if mashup.status != "lobby":
            raise MatchClosed()
        players = await self._store.mashup_players(mashup_id)
        problems = self._problems.require()
        solved: set[str] = set()
        for p in players:
            solved |= solved_keys(await self._cf.user_status(p.handle))
        picked = pick_mashup(
            problems, mashup.min_rating, mashup.max_rating, mashup.count, frozenset(solved)
        )
        if picked is None:
            raise BotError(
                f"Not enough problems rated {mashup.min_rating}-{mashup.max_rating} "
                f"that nobody here has solved, for {mashup.count} problems."
            )
        now = self._now()
        if not await self._store.start_mashup(
            mashup_id,
            [(p.key, p.rating) for p in picked],
            now,
            now + mashup.duration_seconds,
            [p.user_id for p in players],
        ):
            if (await self._mashup(mashup_id)).status == "lobby":
                raise BotError("The players changed while starting. Press Start again.")
            raise MatchClosed()
        return await self.view(mashup_id)

    async def check(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._mashup(mashup_id)
        if mashup.status != "active" or self._now() >= mashup.ends_at:
            raise MatchClosed("This contest is not running.")
        player = next(
            (p for p in await self._store.mashup_players(mashup_id) if p.user_id == user_id), None
        )
        if player is None:
            raise BotError("You are not in this contest.")
        await self._refresh(mashup, player)
        return await self.view(mashup_id)

    async def _refresh(self, mashup: MashupRow, player: MashupPlayer) -> int:
        """Reads the player's recent submissions; returns how many new solves it stored."""
        keys = [key for key, _ in await self._store.mashup_problems(mashup.id)]
        subs = await self._cf.user_status(player.handle, count=100)
        solves = {}
        for key in keys:
            found = first_solve(subs, key, mashup.started_at, mashup.ends_at)
            if found is not None:
                solves[key] = found
        return await self._store.record_mashup_solves(mashup.id, player.user_id, solves)

    async def tick(self) -> list[MashupView]:
        """Cancels expired lobbies, refreshes active contests and ends the ones out of time.
        Returns the contests whose message needs an update."""
        now = self._now()
        views: list[MashupView] = []
        for mashup in await self._store.mashups_in("lobby", "active"):
            try:
                if mashup.status == "lobby":
                    if now - mashup.created_at >= LOBBY_TTL_SECONDS and (
                        await self._store.cancel_mashup(mashup.id)
                    ):
                        views.append(await self.view(mashup.id, note="The lobby expired."))
                    continue
                changed = await self._refresh_everyone(mashup)
                ended = now >= mashup.ends_at
                if ended:
                    await self._store.finish_mashup(mashup.id)
                if changed or ended:
                    views.append(await self.view(mashup.id))
            except Exception:
                log.exception("Could not update mashup %s", mashup.id)
        return views

    async def _refresh_everyone(self, mashup: MashupRow) -> int:
        changed = 0
        total = len(await self._store.mashup_problems(mashup.id))
        solves = await self._store.mashup_solves(mashup.id)
        players = await self._store.mashup_players(mashup.id)
        if mashup.teams:  # a team that solved everything needs no more reads from any member
            solves = team_solves(((p.user_id, p.team) for p in players), solves)
        for player in players:
            who = player.team if mashup.teams else player.user_id
            if len(solves.get(who, {})) >= total:
                continue  # solved everything: nothing left to read, saves a Codeforces call
            try:
                changed += await self._refresh(mashup, player)
            except Exception:
                # One unreachable handle must not stall the others or the ending.
                log.warning("Skipping %s in mashup %s", player.handle, mashup.id, exc_info=True)
        return changed
