import pytest

from src.cache.contests import ContestCache
from src.cache.problems import ProblemCache
from src.cf.models import Contest, Problem
from src.db.store import Store
from src.errors import CacheNotReady, CodeforcesError


class FakeCF:
    def __init__(self, problems=(), contests=()):
        self._problems = list(problems)
        self._contests = list(contests)

    async def problemset(self):
        return self._problems

    async def contests(self):
        return self._contests


@pytest.fixture
async def store(tmp_path):
    s = Store(tmp_path / "t.db")
    await s.connect()
    yield s
    await s.close()


async def test_require_raises_when_empty(store):
    # Review Focus 5
    cache = ProblemCache(FakeCF(), store)
    with pytest.raises(CacheNotReady):
        cache.require()


async def test_problem_refresh_sets_tags_and_persists(store):
    p = Problem(1, "A", "x", 800, ("dp", "math"))
    cache = ProblemCache(FakeCF(problems=[p]), store)
    await cache.refresh()
    assert cache.require() == [p]
    assert cache.tags == {"dp", "math"}
    fresh = ProblemCache(FakeCF(), store)
    await fresh.load()  # restart: data comes back from the DB
    assert fresh.require() == [p]


async def test_contest_refresh_and_load(store):
    c = Contest(1, "A", "CF", "BEFORE", 1000, 7200)
    cache = ContestCache(FakeCF(contests=[c]), store)
    await cache.refresh()
    assert cache.contests == [c]
    fresh = ContestCache(FakeCF(), store)
    await fresh.load()
    assert fresh.contests == [c]


async def test_problem_refresh_empty_keeps_old_data(store):
    p = Problem(1, "A", "x", 800, ("dp",))
    cache = ProblemCache(FakeCF(problems=[p]), store)
    await cache.refresh()
    cache._cf = FakeCF()
    with pytest.raises(CodeforcesError):
        await cache.refresh()
    assert cache.require() == [p]
    assert await store.load_problems() == [p]


async def test_contest_refresh_empty_keeps_old_data(store):
    c = Contest(1, "A", "CF", "BEFORE", 1000, 7200)
    cache = ContestCache(FakeCF(contests=[c]), store)
    await cache.refresh()
    cache._cf = FakeCF()
    with pytest.raises(CodeforcesError):
        await cache.refresh()
    assert cache.contests == [c]
    assert await store.load_contests() == [c]
