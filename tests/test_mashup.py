import asyncio

import pytest

from src.cf.models import Problem, Submission
from src.db.store import Store
from src.errors import BotError, CacheNotReady, MatchClosed, NotLinked
from src.logic import MASHUP_MAX_PLAYERS
from src.mashup import MashupService
from src.ui.embeds import mashup_embed

GUILD = 1
PROBLEMS = [Problem(i, "A", f"Name{i}", r, ()) for i, r in enumerate((800, 1000, 1200), start=1)]


class FakeCF:
    def __init__(self):
        self.subs: dict[str, list[Submission]] = {}
        self.errors: dict[str, Exception] = {}

    async def user_status(self, handle, count=None):
        await asyncio.sleep(0)
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


def solve(cf, handle, key, at):
    cf.subs.setdefault(handle, []).append(Submission(at, at, key, "OK"))


@pytest.fixture
async def env(tmp_path):
    store = Store(tmp_path / "t.db")
    await store.connect()
    cf, clock = FakeCF(), Clock()
    clock.t = 1000
    svc = MashupService(store, cf, FakeProblems(PROBLEMS), clock=clock)
    for uid in range(1, 4):
        await store.link_handle(GUILD, uid, f"h{uid}", None)
    yield svc, store, cf, clock
    await store.close()


async def make(svc, host=1, **kw):
    args = dict(
        guild_id=GUILD, channel_id=5, host_id=host, min_rating=800, max_rating=1200, count=3
    )
    args.update(kw, duration_seconds=kw.get("duration_seconds", 7200))
    return await svc.create(**args)


async def test_create_picks_problems_and_adds_the_host(env):
    svc, store, cf, clock = env
    view = await make(svc)
    assert view.problems == [("1A", 800), ("2A", 1000), ("3A", 1200)]
    assert [p.handle for p in view.players] == ["h1"]
    assert (view.mashup.started_at, view.mashup.ends_at, view.mashup.status) == (
        1000,
        8200,
        "active",
    )


async def test_create_rules(env):
    svc, store, cf, clock = env
    with pytest.raises(BotError):
        await make(svc, min_rating=1500, max_rating=800)
    with pytest.raises(BotError):
        await make(svc, duration_seconds=0)
    with pytest.raises(NotLinked):
        await make(svc, host=9)
    solve(cf, "h1", "1A", 5)  # the host solved one, so only two problems are left
    with pytest.raises(BotError, match="Not enough"):
        await make(svc)
    assert [k for k, _ in (await make(svc, count=2)).problems] == ["2A", "3A"]


async def test_join_rules(env):
    svc, store, cf, clock = env
    view = await make(svc)
    mid = view.mashup.id
    assert [p.user_id for p in (await svc.join(mid, 2)).players] == [1, 2]
    with pytest.raises(BotError, match="already joined"):
        await svc.join(mid, 2)
    with pytest.raises(NotLinked):
        await svc.join(mid, 9)
    clock.t = 8200
    with pytest.raises(MatchClosed):
        await svc.join(mid, 3)


async def test_join_stops_at_the_player_cap(env):
    svc, store, cf, clock = env
    mid = (await make(svc)).mashup.id
    for uid in range(100, 100 + MASHUP_MAX_PLAYERS - 1):
        await store.link_handle(GUILD, uid, f"h{uid}", None)
        await svc.join(mid, uid)
    with pytest.raises(BotError, match="full"):
        await svc.join(mid, 2)


async def test_check_scores_only_solves_inside_the_window(env):
    svc, store, cf, clock = env
    mid = (await make(svc)).mashup.id
    solve(cf, "h1", "1A", 900)  # before the start
    solve(cf, "h1", "2A", 1300)
    solve(cf, "h1", "3A", 9000)  # after the end
    solve(cf, "h1", "9A", 1300)  # not a contest problem
    view = await svc.check(mid, 1)
    assert view.solves == {1: {"2A": 1300}}
    solve(cf, "h1", "1A", 1500)
    view = await svc.check(mid, 1)
    assert view.solves == {1: {"2A": 1300, "1A": 1500}}
    assert [(r.handle, r.solved, r.total_seconds) for r in view.standings] == [("h1", 2, 800)]
    with pytest.raises(BotError, match="Join"):
        await svc.check(mid, 2)


async def test_tick_refreshes_everyone_and_reports_changes_once(env):
    svc, store, cf, clock = env
    mid = (await make(svc)).mashup.id
    await svc.join(mid, 2)
    assert await svc.tick() == []  # nothing solved yet
    solve(cf, "h2", "1A", 1100)
    (view,) = await svc.tick()
    assert view.solves == {2: {"1A": 1100}}
    assert await svc.tick() == []  # no new solves, so no message edit


async def test_tick_ends_the_contest_and_ignores_a_failing_handle(env):
    svc, store, cf, clock = env
    mid = (await make(svc)).mashup.id
    await svc.join(mid, 2)
    cf.errors["h2"] = RuntimeError("renamed")
    solve(cf, "h1", "1A", 1100)
    clock.t = 8200
    (view,) = await svc.tick()
    assert view.mashup.status == "finished" and view.solves == {1: {"1A": 1100}}
    assert await svc.tick() == []  # finished contests are no longer touched
    with pytest.raises(MatchClosed):
        await svc.check(mid, 1)


async def test_embed_lists_problems_and_standings(env):
    svc, store, cf, clock = env
    mid = (await make(svc)).mashup.id
    await svc.join(mid, 2)
    solve(cf, "h2", "1A", 1100)
    solve(cf, "h2", "2A", 1700)
    view = await svc.check(mid, 2)
    by_key = {p.key: p for p in PROBLEMS}
    text = mashup_embed(view, by_key).description
    assert "A. [1A Name1](" in text and "C. [3A Name3](" in text and "(1200)" in text
    lines = text.splitlines()
    assert lines[-2:] == ["1. h2 - 2 solved, 0:13 (A 1m, B 11m)", "2. h1 - 0 solved, 0:00"]
    assert mashup_embed(view, {}).title == "Mashup contest"
