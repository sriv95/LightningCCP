import asyncio

import pytest

from src.cf.models import Problem, Submission
from src.db.store import Store
from src.errors import AlreadyInMatch, BotError, CacheNotReady, MatchClosed, MatchFull, NotLinked
from src.logic import LOBBY_TTL_SECONDS, MASHUP_MAX_PLAYERS
from src.mashup import MashupService
from src.matches import MatchService
from src.ui.embeds import mashup_embed

GUILD = 1
PROBLEMS = [Problem(i, "A", f"Name{i}", r, ()) for i, r in enumerate((800, 1000, 1200), start=1)]


class FakeCF:
    def __init__(self):
        self.subs: dict[str, list[Submission]] = {}
        self.errors: dict[str, Exception] = {}
        self.calls: list[str] = []

    async def user_status(self, handle, count=None):
        await asyncio.sleep(0)
        self.calls.append(handle)
        if handle in self.errors:
            raise self.errors[handle]
        return list(self.subs.get(handle, []))


class FakeProblems:
    def __init__(self, problems):
        self.problems = list(problems)

    def require(self):
        if not self.problems:
            raise CacheNotReady()
        return self.problems


class Clock:
    t = 1000

    def __call__(self):
        return self.t


def sub(cf, handle, key, at, verdict="OK"):
    cf.subs.setdefault(handle, []).append(Submission(at, at, key, verdict))


@pytest.fixture
async def env(tmp_path):
    store = Store(tmp_path / "t.db")
    await store.connect()
    cf, clock = FakeCF(), Clock()
    clock.t = 1000
    problems = FakeProblems(PROBLEMS)
    svc = MashupService(store, cf, problems, clock=clock)
    for uid in range(1, 5):
        await store.link_handle(GUILD, uid, f"h{uid}", None)
    svc.matches = MatchService(store, cf, problems, clock=clock)  # for the one-activity tests
    yield svc, store, cf, clock
    await store.close()


async def lobby(svc, host=1, **kw):
    args = dict(
        guild_id=GUILD,
        channel_id=5,
        host_id=host,
        min_rating=800,
        max_rating=1200,
        count=3,
        duration_seconds=7200,
    )
    args.update(kw)
    return await svc.create(**args)


async def started(svc, *joiners, **kw):
    view = await lobby(svc, **kw)
    for uid in joiners:
        await svc.join(view.mashup.id, uid)
    return await svc.start(view.mashup.id, view.mashup.host_id)


async def test_create_opens_a_lobby_without_problems_or_codeforces_calls(env):
    svc, store, cf, clock = env
    view = await lobby(svc, penalty_minutes=5)
    assert (view.mashup.status, view.mashup.penalty_minutes) == ("lobby", 5)
    assert view.problems == [] and [p.handle for p in view.players] == ["h1"]
    assert cf.calls == []


async def test_create_rules(env):
    svc, store, cf, clock = env
    with pytest.raises(BotError):
        await lobby(svc, min_rating=1500, max_rating=800)
    with pytest.raises(BotError):
        await lobby(svc, duration_seconds=0)
    with pytest.raises(NotLinked):
        await lobby(svc, host=9)


async def test_join_and_leave_in_the_lobby(env):
    svc, store, cf, clock = env
    mid = (await lobby(svc)).mashup.id
    assert [p.user_id for p in (await svc.join(mid, 2)).players] == [1, 2]
    with pytest.raises(AlreadyInMatch):
        await svc.join(mid, 2)
    with pytest.raises(NotLinked):
        await svc.join(mid, 9)
    assert [p.user_id for p in (await svc.leave(mid, 2)).players] == [1]
    with pytest.raises(BotError, match="not in"):
        await svc.leave(mid, 2)
    cancelled = await svc.leave(mid, 1)  # the host leaving cancels the contest
    assert cancelled.mashup.status == "cancelled" and "host left" in cancelled.note
    with pytest.raises(MatchClosed):
        await svc.join(mid, 3)
    assert (await lobby(svc, host=2)).mashup.status == "lobby"  # everyone is free again


async def test_lobby_stops_at_the_player_cap(env):
    svc, store, cf, clock = env
    mid = (await lobby(svc)).mashup.id
    for uid in range(100, 100 + MASHUP_MAX_PLAYERS - 1):
        await store.link_handle(GUILD, uid, f"h{uid}", None)
        await svc.join(mid, uid)
    with pytest.raises(MatchFull):
        await svc.join(mid, 2)


async def test_start_rules_and_late_joiners_are_rejected(env):
    svc, store, cf, clock = env
    mid = (await lobby(svc)).mashup.id
    await svc.join(mid, 2)
    with pytest.raises(BotError, match="host"):
        await svc.start(mid, 2)
    view = await svc.start(mid, 1)
    assert view.mashup.status == "active" and (view.mashup.started_at, view.mashup.ends_at) == (
        1000,
        8200,
    )
    assert [k for k, _ in view.problems] == ["1A", "2A", "3A"]
    with pytest.raises(MatchClosed, match="already started"):
        await svc.join(mid, 3)
    with pytest.raises(MatchClosed):
        await svc.start(mid, 1)
    with pytest.raises(MatchClosed):
        await svc.leave(mid, 2)


async def test_start_excludes_problems_anyone_in_the_lobby_solved(env):
    svc, store, cf, clock = env
    sub(cf, "h2", "1A", 5)  # a joiner (not the host) solved the easiest one
    mid = (await lobby(svc, count=2)).mashup.id
    await svc.join(mid, 2)
    assert [k for k, _ in (await svc.start(mid, 1)).problems] == ["2A", "3A"]
    low = (await lobby(svc, host=3, count=3)).mashup.id
    await svc.join(low, 4)
    sub(cf, "h4", "2A", 5)
    with pytest.raises(BotError, match="Not enough"):
        await svc.start(low, 3)
    assert (await store.get_mashup(low)).status == "lobby"  # stays open to retry


async def test_start_fails_when_the_players_changed_underneath(env):
    svc, store, cf, clock = env
    mid = (await lobby(svc)).mashup.id

    async def join_midway(handle):
        if handle == "h1" and not hasattr(join_midway, "done"):
            join_midway.done = True
            await svc.join(mid, 2)

    original = cf.user_status

    async def slow(handle, count=None):
        await join_midway(handle)
        return await original(handle, count)

    cf.user_status = slow
    with pytest.raises(BotError, match="changed"):
        await svc.start(mid, 1)
    assert [p.user_id for p in (await svc.view(mid)).players] == [1, 2]
    assert (await svc.start(mid, 1)).mashup.status == "active"


async def test_check_scores_only_solves_inside_the_window_with_wrong_attempts(env):
    svc, store, cf, clock = env
    mid = (await started(svc, penalty_minutes=20)).mashup.id
    sub(cf, "h1", "1A", 900)  # before the start
    sub(cf, "h1", "2A", 1200, "WRONG_ANSWER")
    sub(cf, "h1", "2A", 1300)
    sub(cf, "h1", "3A", 9000)  # after the end
    sub(cf, "h1", "9A", 1300)  # not a contest problem
    view = await svc.check(mid, 1)
    assert view.solves == {1: {"2A": (1300, 1)}}
    assert [(r.solved, r.total_seconds) for r in view.standings] == [(1, 300 + 20 * 60)]
    with pytest.raises(BotError, match="not in"):
        await svc.check(mid, 2)


async def test_check_after_the_end_is_rejected(env):
    svc, store, cf, clock = env
    mid = (await started(svc)).mashup.id
    clock.t = 8200
    with pytest.raises(MatchClosed):
        await svc.check(mid, 1)


async def test_tick_reports_changes_once_and_skips_players_who_solved_everything(env):
    svc, store, cf, clock = env
    await started(svc, 2)
    cf.calls.clear()
    assert await svc.tick() == []  # nothing solved yet
    assert sorted(cf.calls) == ["h1", "h2"]
    for key in ("1A", "2A", "3A"):
        sub(cf, "h2", key, 1100)
    (view,) = await svc.tick()
    assert len(view.solves[2]) == 3
    cf.calls.clear()
    assert await svc.tick() == []
    assert cf.calls == ["h1"]  # h2 solved everything, so no call for them


async def test_tick_ends_the_contest_frees_players_and_ignores_a_failing_handle(env):
    svc, store, cf, clock = env
    await started(svc, 2)
    cf.errors["h2"] = RuntimeError("renamed")
    sub(cf, "h1", "1A", 1100)
    clock.t = 8200
    (view,) = await svc.tick()
    assert view.mashup.status == "finished" and view.solves == {1: {"1A": (1100, 0)}}
    assert await svc.tick() == []
    assert (await lobby(svc, host=2)).mashup.status == "lobby"  # players are free again


async def test_tick_cancels_an_expired_lobby(env):
    svc, store, cf, clock = env
    await lobby(svc)
    clock.t += LOBBY_TTL_SECONDS - 1
    assert await svc.tick() == []
    clock.t += 1
    (view,) = await svc.tick()
    assert view.mashup.status == "cancelled" and view.note == "The lobby expired."
    assert await svc.tick() == []
    assert (await lobby(svc)).mashup.status == "lobby"


async def test_one_contest_or_match_at_a_time(env):
    svc, store, cf, clock = env
    mid = (await lobby(svc)).mashup.id  # h1 in a lobby
    with pytest.raises(AlreadyInMatch):
        await lobby(svc)  # another contest
    with pytest.raises(AlreadyInMatch):
        await svc.matches.create(
            guild_id=GUILD, channel_id=5, host_id=1, mode="duel", max_players=2
        )
    match = await svc.matches.create(
        guild_id=GUILD, channel_id=5, host_id=2, mode="duel", max_players=2
    )
    with pytest.raises(AlreadyInMatch):
        await svc.join(mid, 2)  # busy in a match
    await svc.matches.join(match.match.id, 3)
    other = (await lobby(svc, host=4)).mashup.id
    with pytest.raises(AlreadyInMatch):
        await svc.join(other, 3)  # busy in the match lobby
    started_ = await svc.start(mid, 1)
    assert started_.mashup.status == "active"
    with pytest.raises(AlreadyInMatch):  # still busy while the contest runs
        await svc.matches.create(
            guild_id=GUILD, channel_id=5, host_id=1, mode="solo", max_players=1
        )
    assert await svc.leave(other, 4) is not None


async def test_match_join_blocked_by_a_contest(env):
    svc, store, cf, clock = env
    await lobby(svc, host=3)
    match = await svc.matches.create(
        guild_id=GUILD, channel_id=5, host_id=1, mode="duel", max_players=2
    )
    with pytest.raises(AlreadyInMatch):
        await svc.matches.join(match.match.id, 3)


async def test_embed_lobby_active_cancelled(env):
    svc, store, cf, clock = env
    by_key = {p.key: p for p in PROBLEMS}
    view = await lobby(svc, penalty_minutes=20)
    text = mashup_embed(view, by_key).description
    assert "3 problems rated 800-1200" in text and "20 min per wrong attempt" in text
    assert mashup_embed(view, by_key).title == "Mashup lobby" and "<@1> (h1)" in text
    view = await svc.start(view.mashup.id, 1)
    sub(cf, "h1", "1A", 1100, "WRONG_ANSWER")
    sub(cf, "h1", "1A", 1160)
    sub(cf, "h1", "2A", 1700)
    view = await svc.check(view.mashup.id, 1)
    text = mashup_embed(view, by_key).description
    assert "A. [1A Name1](" in text and "C. [3A Name3](" in text and "(1200)" in text
    assert text.splitlines()[-1] == "1. h1 - 2 solved, 0:34 (A 2m (1 wrong), B 11m)"
    clock.t = 8200
    (final,) = await svc.tick()
    assert mashup_embed(final, by_key).title == "Mashup contest - final standings"
    gone = await svc.leave((await lobby(svc, host=3)).mashup.id, 3)
    assert mashup_embed(gone, by_key).title == "Mashup cancelled"
