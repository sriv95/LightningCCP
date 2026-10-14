import asyncio

import pytest

from src.cf.models import Problem, Submission, User
from src.db.store import Store
from src.errors import AlreadyInMatch, BotError, CacheNotReady, MatchClosed, MatchFull, NotLinked
from src.logic import LOBBY_TTL_SECONDS, MATCH_TTL_SECONDS
from src.matches import MatchService

PROBLEM = Problem(1700, "A", "Foo", 1000, ())
GUILD = 1


class FakeCF:
    def __init__(self):
        self.ratings: dict[str, int | None] = {}
        self.subs: dict[str, list[Submission]] = {}

    async def user_info(self, handle):
        return User(handle, self.ratings.get(handle, 1000), None, None)

    async def user_status(self, handle, count=None):
        return list(self.subs.get(handle, []))


class FakeProblems:
    def __init__(self, problems):
        self.problems = list(problems)

    def require(self):
        if not self.problems:
            raise CacheNotReady()
        return self.problems


class Clock:
    def __init__(self):
        self.t = 1000

    def __call__(self):
        return self.t


def solve(cf, handle, at, key="1700A", verdict="OK"):
    cf.subs.setdefault(handle, []).append(Submission(at, at, key, verdict))


@pytest.fixture
async def env(tmp_path):
    store = Store(tmp_path / "t.db")
    await store.connect()
    cf = FakeCF()
    clock = Clock()
    svc = MatchService(store, cf, FakeProblems([PROBLEM]), clock=clock)
    for uid in range(1, 7):
        await store.link_handle(GUILD, uid, f"h{uid}", 1000)
    yield svc, store, cf, clock
    await store.close()


async def make(svc, host, mode, max_players, **kw):
    return await svc.create(
        guild_id=GUILD, channel_id=5, host_id=host, mode=mode, max_players=max_players, **kw
    )


def deltas(view):
    return {p.user_id: p.delta for p in view.players}


async def test_solo_solve_awards_points(env):
    svc, store, cf, clock = env
    view = await make(svc, 1, "solo", 1)
    assert view.match.status == "active"
    assert (view.match.problem_key, view.match.rating) == ("1700A", 1000)
    assert view.problem == PROBLEM
    assert (await svc.check(view.match.id, 1)).match.status == "active"
    solve(cf, "h1", 1100)
    done = await svc.check(view.match.id, 1)
    assert done.match.status == "finished"
    assert deltas(done) == {1: 5}
    assert done.totals == {1: 5}
    with pytest.raises(MatchClosed):
        await svc.check(view.match.id, 1)
    assert await svc.current(GUILD, 1) is None


async def test_solo_give_up_scores_zero_and_frees_player(env):
    svc, store, cf, clock = env
    view = await make(svc, 1, "solo", 1)
    done = await svc.give_up(view.match.id, 1)
    assert done.match.status == "finished" and deltas(done) == {1: 0}
    await make(svc, 1, "solo", 1)  # free to play again


async def test_solo_without_free_problem_is_cancelled(env):
    # Review Focus 3
    svc, store, cf, clock = env
    solve(cf, "h1", 1, verdict="OK")
    with pytest.raises(BotError):
        await make(svc, 1, "solo", 1)
    assert await store.active_match_for(GUILD, 1) is None


async def test_create_requires_linked_handle(env):
    svc, *_ = env
    with pytest.raises(NotLinked):
        await make(svc, 99, "solo", 1)


async def test_create_while_in_match(env):
    # Review Focus 5
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    with pytest.raises(AlreadyInMatch):
        await make(svc, 1, "solo", 1)
    current = await svc.current(GUILD, 1)
    assert current.match.id == lobby.match.id


async def test_duel_cannot_invite_self(env):
    svc, *_ = env
    with pytest.raises(BotError):
        await make(svc, 1, "duel", 2, invited_id=1)


async def test_join_reserved_duel(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2, invited_id=2)
    with pytest.raises(BotError):
        await svc.join(lobby.match.id, 3)  # reserved for 2
    joined = await svc.join(lobby.match.id, 2)
    assert [p.user_id for p in joined.players] == [1, 2]


async def test_join_full_lobby(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    with pytest.raises(MatchFull):
        await svc.join(lobby.match.id, 3)


async def test_join_while_in_other_match(env):
    svc, store, cf, clock = env
    await make(svc, 1, "duel", 2)
    other = await make(svc, 2, "custom", 3)
    with pytest.raises(AlreadyInMatch):
        await svc.join(other.match.id, 1)


async def test_join_requires_linked_handle(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    with pytest.raises(NotLinked):
        await svc.join(lobby.match.id, 99)


async def test_start_rules(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    with pytest.raises(BotError):
        await svc.start(lobby.match.id, 1)  # only one player
    await svc.join(lobby.match.id, 2)
    with pytest.raises(BotError):
        await svc.start(lobby.match.id, 2)  # not the host
    started = await svc.start(lobby.match.id, 1)
    assert started.match.status == "active"
    with pytest.raises(MatchClosed):
        await svc.start(lobby.match.id, 1)


async def test_start_without_free_problem_keeps_lobby(env):
    # Review Focus 3
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    solve(cf, "h2", 5)
    with pytest.raises(BotError):
        await svc.start(lobby.match.id, 1)
    assert (await store.get_match(lobby.match.id)).status == "lobby"


async def test_start_on_cancelled_lobby_fails(env):
    # Review Focus 2
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.leave(lobby.match.id, 1)  # host leaves: cancelled
    with pytest.raises(MatchClosed):
        await svc.start(lobby.match.id, 1)


async def test_leave_rules(env):
    # Review Focus 5
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 4)
    await svc.join(lobby.match.id, 2)
    await svc.join(lobby.match.id, 3)
    left = await svc.leave(lobby.match.id, 2)
    assert [p.user_id for p in left.players] == [1, 3]
    await make(svc, 2, "solo", 1)  # player 2 is free again
    with pytest.raises(BotError):
        await svc.leave(lobby.match.id, 4)  # not in the match
    cancelled = await svc.leave(lobby.match.id, 1)  # host leaves
    assert cancelled.match.status == "cancelled"
    assert "host" in cancelled.note.lower()
    assert await svc.current(GUILD, 3) is None and await svc.current(GUILD, 1) is None


async def test_leave_after_start_is_rejected(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    with pytest.raises(MatchClosed):
        await svc.leave(lobby.match.id, 2)


async def test_duel_winner_is_earliest_solve_not_first_click(env):
    # Review Focus 4
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    solve(cf, "h1", 1100)
    solve(cf, "h2", 1200)
    done = await svc.check(lobby.match.id, 2)  # player 2 clicks, player 1 solved first
    assert deltas(done) == {1: 5, 2: -5}
    assert done.totals == {1: 5, 2: -5}


async def test_duel_check_without_solves_keeps_running(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    assert (await svc.check(lobby.match.id, 1)).match.status == "active"


async def test_solve_before_start_does_not_count(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)  # started_at = 1000
    solve(cf, "h1", 999)
    assert (await svc.check(lobby.match.id, 1)).match.status == "active"


async def test_duel_give_up_is_a_forfeit(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    done = await svc.give_up(lobby.match.id, 1)
    assert done.match.status == "finished"
    assert deltas(done) == {1: -5, 2: 5}


async def test_give_up_errors(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 3)
    await svc.join(lobby.match.id, 2)
    await svc.join(lobby.match.id, 3)
    with pytest.raises(MatchClosed):
        await svc.give_up(lobby.match.id, 2)  # lobby: use Leave
    await svc.start(lobby.match.id, 1)
    solve(cf, "h1", 1100)
    await svc.check(lobby.match.id, 1)
    with pytest.raises(BotError):
        await svc.give_up(lobby.match.id, 1)  # already solved
    assert (await svc.give_up(lobby.match.id, 2)).match.status == "active"  # player 3 still plays
    with pytest.raises(BotError):
        await svc.give_up(lobby.match.id, 2)  # already gave up
    with pytest.raises(BotError):
        await svc.check(lobby.match.id, 2)  # gave up: cannot check


async def test_custom_ranks_by_solve_time_and_ties_non_solvers(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 4)
    for uid in (2, 3, 4):
        await svc.join(lobby.match.id, uid)
    await svc.start(lobby.match.id, 1)
    solve(cf, "h2", 1100)
    solve(cf, "h1", 1200)
    view = await svc.check(lobby.match.id, 1)
    assert view.match.status == "active"  # 3 and 4 are still playing
    assert (await svc.give_up(lobby.match.id, 3)).match.status == "active"
    done = await svc.give_up(lobby.match.id, 4)
    assert done.match.status == "finished"
    assert deltas(done) == {2: 5, 1: 3, 3: -4, 4: -4}
    assert done.totals == {1: 3, 2: 5, 3: -4, 4: -4}


async def test_custom_everyone_gave_up_moves_no_points(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 3)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    await svc.give_up(lobby.match.id, 1)
    done = await svc.give_up(lobby.match.id, 2)
    assert done.match.status == "finished" and deltas(done) == {1: 0, 2: 0}


async def test_expiry_without_solvers_moves_no_points(env):
    # Review Focus 3
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 3)
    await svc.join(lobby.match.id, 2)
    await svc.join(lobby.match.id, 3)
    await svc.start(lobby.match.id, 1)
    clock.t += MATCH_TTL_SECONDS - 1
    assert await svc.expire_due() == []
    clock.t += 1
    (view,) = await svc.expire_due()
    assert view.match.status == "finished"
    assert deltas(view) == {1: 0, 2: 0, 3: 0}
    assert "time" in view.note.lower()
    assert await svc.current(GUILD, 1) is None
    assert await svc.expire_due() == []


async def test_expiry_ranks_solvers(env):
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "custom", 3)
    await svc.join(lobby.match.id, 2)
    await svc.join(lobby.match.id, 3)
    await svc.start(lobby.match.id, 1)
    solve(cf, "h2", 1500)
    clock.t += MATCH_TTL_SECONDS
    (view,) = await svc.expire_due()
    assert deltas(view) == {2: 5, 1: -3, 3: -3}


async def test_lobby_expiry_cancels(env):
    svc, store, cf, clock = env
    await make(svc, 1, "duel", 2)
    clock.t += LOBBY_TTL_SECONDS
    (view,) = await svc.expire_due()
    assert view.match.status == "cancelled"
    assert "expired" in view.note.lower()
    assert await svc.current(GUILD, 1) is None


async def test_concurrent_checks_score_once(env):
    # Review Focus 1
    svc, store, cf, clock = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.join(lobby.match.id, 2)
    await svc.start(lobby.match.id, 1)
    solve(cf, "h1", 1100)
    solve(cf, "h2", 1200)
    results = await asyncio.gather(
        svc.check(lobby.match.id, 1), svc.check(lobby.match.id, 2), return_exceptions=True
    )
    assert all(isinstance(r, MatchClosed) or r.match.status == "finished" for r in results)
    assert await store.points_total(GUILD, 1) == 5
    assert await store.points_total(GUILD, 2) == -5


async def test_set_message_is_reflected(env):
    svc, *_ = env
    lobby = await make(svc, 1, "duel", 2)
    await svc.set_message(lobby.match.id, 77, 88)
    current = await svc.current(GUILD, 1)
    assert (current.match.channel_id, current.match.message_id) == (77, 88)
