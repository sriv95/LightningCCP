import logging
import math
import random
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from src.cf.models import Problem
from src.db.store import MashupCell, MashupPlayer, MashupRow, Store
from src.errors import BotError, MatchClosed, MatchFull, NotLinked
from src.logic import (
    BINGO_MAX_SIZE,
    BINGO_MIN_SIZE,
    LOBBY_TTL_SECONDS,
    MASHUP_MAX_INTERVAL,
    MASHUP_MAX_PLAYERS,
    MASHUP_MAX_PROBLEMS,
    MASHUP_MAX_TEAM_SIZE,
    MASHUP_MAX_TEAMS,
    MASHUP_MIN_INTERVAL,
    MASHUP_READS_PER_SECOND,
    BingoBoard,
    MashupRank,
    bingo_board,
    bingo_standings,
    first_solve,
    mashup_standings,
    match_deltas,
    normalize_tags,
    parse_tags,
    pick_mashup,
    pick_replacement,
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
    cells: list[MashupCell] = field(default_factory=list)  # bingo only: every problem on the board

    @property
    def team_solves(self) -> dict[int, dict[str, tuple[int, int]]]:
        return team_solves(((p.user_id, p.team) for p in self.players), self.solves)

    @property
    def log(self) -> list[tuple[int, int, str, int, int]]:
        """Bingo claims oldest first as (time, team, problem key, rating, user who solved it)."""
        rating = {c.key: c.rating for c in self.cells}
        out = []
        for key, (team, at) in self.board.claims.items():
            user = next(
                p.user_id
                for p in self.players
                if p.team == team and self.solves.get(p.user_id, {}).get(key, (None,))[0] == at
            )
            out.append((at, team, key, rating[key], user))
        return sorted(out)

    @property
    def board(self) -> BingoBoard:
        m = self.mashup
        return bingo_board(
            m.size, m.mode, m.win, [(c.key, c.rating, c.cell) for c in self.cells], self.team_solves
        )

    @property
    def standings(self) -> list[MashupRank]:
        """The teams (MashupRank.user_id is the team number; a team of one shows its player's
        handle): a problem counts for a team once, from the teammate who solved it first. A bingo
        ranks by lines and squares claimed."""
        joined: dict[int, tuple[int, str]] = {}
        for p in self.players:
            if p.team not in joined or p.joined_at < joined[p.team][0]:
                joined[p.team] = (p.joined_at, p.handle)
        solo = self.mashup.team_size == 1
        teams = [
            (team, handle if solo else f"Team {team}", at) for team, (at, handle) in joined.items()
        ]
        if self.mashup.mode != "contest":
            return bingo_standings(teams, self.board, self.mashup.started_at or 0)
        return mashup_standings(
            teams, self.team_solves, self.mashup.started_at or 0, self.mashup.penalty_minutes
        )


@dataclass(frozen=True)
class MashupRecord:
    """How one player (or their team) did in a finished contest."""

    mashup: MashupRow
    rank: int
    entrants: int  # teams (a team of one is a player)
    solved: int
    team: int | None
    delta: int | None = None  # match rating change; None when the contest did not rate


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
        self.interval = MASHUP_MAX_INTERVAL  # seconds until the next tick, set by every tick
        self._read_at: dict[tuple[int, int], int] = {}  # (contest, user) -> last read, in memory

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
            await self._store.mashup_cells(mashup_id) if mashup.mode != "contest" else [],
        )

    async def history(self, guild_id: int, user_id: int, limit: int = 50) -> list[MashupRecord]:
        """The user's newest finished contests with their place, newest first."""
        records = []
        for mashup in await self._store.finished_mashups(guild_id, user_id, limit):
            view = await self.view(mashup.id)
            me = next(p for p in view.players if p.user_id == user_id)
            ranks = view.standings
            mine = next(r for r in ranks if r.user_id == me.team)
            records.append(
                MashupRecord(mashup, mine.rank, len(ranks), mine.solved, me.team, me.delta)
            )
        return records

    async def claims_to_log(self, view: MashupView) -> list[tuple[int, int, str, int, int]]:
        """The claims of a bingo not yet posted to its thread (each is returned once)."""
        if view.mashup.thread_id is None:
            return []
        return [e for e in view.log if await self._store.mark_logged(view.mashup.id, e[2])]

    async def set_log_board(self, mashup_id: int, message_id: int) -> None:
        await self._store.set_log_board(mashup_id, message_id)

    async def set_thread(self, mashup_id: int, thread_id: int) -> None:
        await self._store.set_mashup_thread(mashup_id, thread_id)

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

    def _contest_problems(self, contest_id: int) -> list[Problem]:
        """The rated problems of a Codeforces contest in contest order."""
        found = sorted(
            (p for p in self._problems.require() if p.contest_id == contest_id and p.rating),
            key=lambda p: p.index,
        )
        if not found:
            raise BotError(f"I know no rated problems of contest {contest_id} yet.")
        if len(found) > MASHUP_MAX_PROBLEMS:
            raise BotError(f"That contest has more than {MASHUP_MAX_PROBLEMS} rated problems.")
        return found

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
        teams: int = MASHUP_MAX_TEAMS,
        team_size: int = 1,
        rated: bool = True,
        tags: str | None = None,
        exclude_tags: str | None = None,
        contest_id: int | None = None,
        mode: str = "contest",
        size: int = 0,
        win: str = "line",
    ) -> MashupView:
        if mode != "contest":  # bingo: the board is size x size problems
            if not BINGO_MIN_SIZE <= size <= BINGO_MAX_SIZE:
                raise BotError(f"The board size must be from {BINGO_MIN_SIZE} to {BINGO_MAX_SIZE}.")
            count, penalty_minutes = size * size, 0
        if contest_id is not None:  # a virtual contest: the problems are the contest's own
            problems = self._contest_problems(contest_id)
            min_rating = min(p.rating for p in problems)
            max_rating = max(p.rating for p in problems)
            count = len(problems)
            tags = exclude_tags = None
        if min_rating > max_rating:
            raise BotError("min_rating cannot be higher than max_rating.")
        if not 1 <= teams <= MASHUP_MAX_TEAMS:
            raise BotError(f"The number of teams must be from 1 to {MASHUP_MAX_TEAMS}.")
        if not 1 <= team_size <= MASHUP_MAX_TEAM_SIZE:
            raise BotError(f"Players per team must be from 1 to {MASHUP_MAX_TEAM_SIZE}.")
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
            host_team=1,  # the host always starts in team 1
            max_players=min(MASHUP_MAX_PLAYERS, teams * team_size),
            rated=rated,
            team_size=team_size,
            tags=normalize_tags(tags),
            exclude_tags=normalize_tags(exclude_tags),
            contest_id=contest_id,
            mode=mode,
            size=size,
            win=win,
        )
        return await self.view(mashup_id)

    @staticmethod
    def _check_team(teams: int, team: int | None) -> None:
        if team is None or not 1 <= team <= teams:
            raise BotError(f"Pick a team number from 1 to {teams}.")

    async def rematch(self, mashup_id: int, user_id: int, channel_id: int) -> MashupView:
        """New lobby with the same settings, hosted by whoever pressed Rematch."""
        m = await self._mashup(mashup_id)
        if m.status != "finished":
            raise MatchClosed("Only a finished contest can be rematched.")
        if user_id not in {p.user_id for p in await self._store.mashup_players(mashup_id)}:
            raise BotError("Only the players of this contest can ask for a rematch.")
        return await self.create(
            guild_id=m.guild_id,
            channel_id=channel_id,
            host_id=user_id,
            min_rating=m.min_rating,
            max_rating=m.max_rating,
            count=m.count,
            duration_seconds=m.duration_seconds,
            penalty_minutes=m.penalty_minutes,
            teams=m.teams,
            team_size=m.team_size,
            rated=bool(m.rated),
            tags=m.tags,
            exclude_tags=m.exclude_tags,
            contest_id=m.contest_id,
            mode=m.mode,
            size=m.size,
            win=m.win,
        )

    async def join(self, mashup_id: int, user_id: int, team: int | None = None) -> MashupView:
        mashup = await self._mashup(mashup_id)
        if mashup.status != "lobby":
            raise MatchClosed("This contest has already started, so nobody can join.")
        players = await self._store.mashup_players(mashup_id)
        if mashup.team_size == 1:  # every player is a team: take the first free number
            taken = {p.team for p in players}
            team = next((t for t in range(1, mashup.teams + 1) if t not in taken), None)
            if team is None:
                raise MatchFull()
        else:
            self._check_team(mashup.teams, team)
            if user_id in {p.user_id for p in players}:
                if user_id == mashup.host_id:
                    raise BotError("The host stays in team 1.")
                await self._store.move_mashup_player(mashup_id, user_id, team, mashup.team_size)
                return await self.view(mashup_id)
        await self._store.add_mashup_player(
            mashup_id,
            mashup.guild_id,
            user_id,
            await self._handle(mashup.guild_id, user_id),
            self._now(),
            mashup.max_players,
            team,
            mashup.team_size,
        )
        return await self.view(mashup_id)

    async def leave(self, mashup_id: int, user_id: int) -> MashupView:
        mashup = await self._mashup(mashup_id)
        players = await self._store.mashup_players(mashup_id)
        if user_id not in {p.user_id for p in players}:
            raise BotError("You are not in this contest.")
        if mashup.status != "lobby":
            raise MatchClosed("The contest already started. Use Give Up instead.")
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
        if mashup.contest_id is not None:
            picked = self._contest_problems(mashup.contest_id)  # earlier solves do not matter
        else:
            solved: set[str] = set()
            for p in players:
                solved |= solved_keys(await self._cf.user_status(p.handle))
            picked = pick_mashup(
                self._problems.require(),
                mashup.min_rating,
                mashup.max_rating,
                mashup.count,
                frozenset(solved),
                tags=parse_tags(mashup.tags),
                exclude_tags=parse_tags(mashup.exclude_tags),
            )
        if picked is None:
            raise BotError(
                f"Not enough problems rated {mashup.min_rating}-{mashup.max_rating} "
                f"{' with those tags' if mashup.tags or mashup.exclude_tags else ''} "
                f"that nobody here has solved, for {mashup.count} problems."
            )
        if mashup.mode != "contest":
            random.shuffle(picked)  # pick_mashup lists easy to hard; a board is mixed
        now = self._now()
        if not await self._store.start_mashup(
            mashup_id,
            [(p.key, p.rating) for p in picked],
            now,
            now + mashup.duration_seconds,
            [p.user_id for p in players],
            solved if mashup.mode == "replace" else (),
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
        if not player.active:
            raise BotError("You gave up this contest.")
        await self._refresh(mashup, player)
        return await self.view(mashup_id)

    async def give_up(self, mashup_id: int, user_id: int) -> MashupView:
        """Leave a running contest: the place and rating change stay (from the solves so far, no
        more are read) but the player is free to join another match or contest."""
        mashup = await self._mashup(mashup_id)
        if mashup.status != "active" or self._now() >= mashup.ends_at:
            raise MatchClosed("This contest is not running.")
        me = next(
            (p for p in await self._store.mashup_players(mashup_id) if p.user_id == user_id), None
        )
        if me is None or not me.active:
            raise BotError("You are not playing in this contest.")
        if not await self._store.mark_mashup_gave_up(mashup_id, user_id):
            raise MatchClosed()
        return await self.view(mashup_id)

    async def _everyone_done(self, mashup: MashupRow) -> bool:
        """Every player still in has a team that solved every problem (true when all gave up). A
        bingo is over when it has a winner (or a full classic board) or all gave up."""
        if mashup.mode != "contest":
            view = await self.view(mashup.id)
            return view.board.over or not any(p.active for p in view.players)
        total = len(await self._store.mashup_problems(mashup.id))
        players = await self._store.mashup_players(mashup.id)
        solves = team_solves(
            ((p.user_id, p.team) for p in players), await self._store.mashup_solves(mashup.id)
        )
        return all(len(solves.get(p.team, {})) >= total for p in players if p.active)

    async def _refresh(self, mashup: MashupRow, player: MashupPlayer) -> int:
        """Reads the player's recent submissions; returns how many new solves it stored."""
        cells = await self._store.mashup_cells(mashup.id)
        subs = await self._cf.user_status(player.handle, count=100)
        solves = {}
        for c in cells:
            found = first_solve(subs, c.key, c.since, mashup.ends_at)
            if found is not None:
                solves[c.key] = found
        new = await self._store.record_mashup_solves(mashup.id, player.user_id, solves)
        if new and mashup.mode == "replace":
            await self._replace_claimed(mashup)
        return new

    async def _replace_claimed(self, mashup: MashupRow) -> None:
        """Gives the newest problem of every claimed cell of a replace bingo a successor rated 100
        higher, which counts from the claim and lets another team take the cell over. It is a
        problem nobody in the lobby had solved before the start (attempts are not checked)."""
        view = await self.view(mashup.id)
        claims = view.board.claims
        used = frozenset(c.key for c in view.cells) | await self._store.mashup_excluded(mashup.id)
        newest = {c.cell: c for c in view.cells}  # the cells come in the order they were added
        for cell, current in newest.items():
            if current.key not in claims:
                continue
            pick = pick_replacement(
                self._problems.require(),
                current.rating,
                used,
                mashup.max_rating,
                tags=parse_tags(mashup.tags),
                exclude_tags=parse_tags(mashup.exclude_tags),
            )
            if pick is None:
                log.warning("No replacement for %s in mashup %s", current.key, mashup.id)
                continue
            used |= {pick.key}
            await self._store.replace_cell(
                mashup.id, cell, current.idx, pick.key, pick.rating, claims[current.key][1]
            )

    async def tick(self) -> list[MashupView]:
        """Cancels expired lobbies, reads the stalest players of running contests (the reads the
        last interval pays for, everyone in a contest that is out of time) and ends the contests
        out of time. Returns the contests whose message needs an update. Sets `interval`: the
        fewer players are waiting for a read, the sooner the next tick."""
        now = self._now()
        views: list[MashupView] = []
        running: list[MashupRow] = []
        for mashup in await self._store.mashups_in("lobby", "active"):
            try:
                if mashup.status == "active":
                    running.append(mashup)
                elif now - mashup.created_at >= LOBBY_TTL_SECONDS and (
                    await self._store.cancel_mashup(mashup.id)
                ):
                    views.append(await self.view(mashup.id, note="The lobby expired."))
            except Exception:
                log.exception("Could not update mashup %s", mashup.id)
        changed, waiting = await self._read_players(running, now)
        # A bingo that is won gets one full read first: a team may have claimed a square earlier
        # than the solves read so far show.
        won = [m for m in running if m.mode != "contest" and now < m.ends_at]
        won = [m for m in won if await self._everyone_done(m)]
        if won:
            for mashup_id, n in (await self._read_players(won, now, {m.id for m in won}))[
                0
            ].items():
                changed[mashup_id] = changed.get(mashup_id, 0) + n
        self.interval = min(
            MASHUP_MAX_INTERVAL,
            max(MASHUP_MIN_INTERVAL, math.ceil(waiting / MASHUP_READS_PER_SECOND)),
        )
        for mashup in running:
            try:
                ended = now >= mashup.ends_at or mashup in won or await self._everyone_done(mashup)
                if ended:
                    await self._store.finish_mashup(
                        mashup.id, await self._rating_changes(await self.view(mashup.id)), now
                    )
                    self._read_at = {k: t for k, t in self._read_at.items() if k[0] != mashup.id}
                if changed.get(mashup.id) or ended:
                    views.append(await self.view(mashup.id))
            except Exception:
                log.exception("Could not update mashup %s", mashup.id)
        return views

    async def _rating_changes(self, view: MashupView) -> dict[int, int]:
        """Match rating change per player: every pair of teams (rated at their members' average,
        a team of one at its player's rating) is a duel decided by the standings, as in a custom match. Empty
        for a single entrant or an unrated contest."""
        ranks = view.standings
        if len(ranks) < 2 or not view.mashup.rated:
            return {}
        guild_id = view.mashup.guild_id
        ratings = {p.user_id: await self._store.rating(guild_id, p.user_id) for p in view.players}
        members: dict[int, list[int]] = {}
        for p in view.players:
            members.setdefault(p.team, []).append(ratings[p.user_id])
        entrant_ratings = {team: sum(r) // len(r) for team, r in members.items()}
        groups: dict[int, list[int]] = {}
        for r in ranks:
            groups.setdefault(r.rank, []).append(r.user_id)
        changes = match_deltas([groups[k] for k in sorted(groups)], entrant_ratings)
        return {p.user_id: changes[p.team] for p in view.players}

    async def _read_players(
        self, running: list[MashupRow], now: int, full: frozenset[int] | set[int] = frozenset()
    ) -> tuple[dict[int, int], int]:
        """Reads submissions; returns the new solves per contest and how many players of contests
        still running are due the next time. A contest out of time (or in `full`) is read in full (its
        last solves count); the others share the budget of the last interval, the player read longest
        ago first. A team that solved everything is not read."""
        due: list[tuple[int, MashupRow, MashupPlayer]] = []
        for mashup in running:
            try:
                bingo = mashup.mode != "contest"  # a board is never solved by one team
                total = len(await self._store.mashup_problems(mashup.id))
                players = await self._store.mashup_players(mashup.id)
                solves = team_solves(
                    ((p.user_id, p.team) for p in players),
                    await self._store.mashup_solves(mashup.id),
                )
            except Exception:
                log.exception("Could not list the players of mashup %s", mashup.id)
                continue
            # out of time: before everyone, no budget
            last = -1 if now >= mashup.ends_at or mashup.id in full else 0
            for p in players:
                if p.active and (bingo or len(solves.get(p.team, {})) < total):
                    due.append((last or self._read_at.get((mashup.id, p.user_id), 0), mashup, p))
        due.sort(key=lambda d: d[0])  # stable: equal staleness keeps contest and joining order
        budget = max(1, round(MASHUP_READS_PER_SECOND * self.interval))
        changed: dict[int, int] = {}
        for stale, mashup, player in due:
            if stale >= 0 and budget <= 0:
                break
            budget -= stale >= 0
            self._read_at[(mashup.id, player.user_id)] = now
            try:
                changed[mashup.id] = changed.get(mashup.id, 0) + await self._refresh(mashup, player)
            except Exception:
                # One unreachable handle must not stall the others or the ending.
                log.warning("Skipping %s in mashup %s", player.handle, mashup.id, exc_info=True)
        return changed, sum(1 for stale, _, _ in due if stale >= 0)
