import pytest

from src.cf.models import Problem, Submission
from src.daily import DailyService
from src.db.store import Store
from src.errors import BotError, NotLinked
from src.logic import daily_streak, floor_rating, utc_day
from src.matches import MatchService
from src.ui.embeds import daily_embed

GUILD = 1
P800 = Problem(1700, "A", "Foo", 800, ())
P900 = Problem(1701, "A", "Bar", 900, ())


class FakeCF:
    def __init__(self):
        self.subs: list[Submission] = []

    async def user_status(self, handle, count=None, since=None):
        return list(self.subs)


class FakeProblems:
    def __init__(self, problems):
        self.problems = list(problems)

    def require(self):
        return self.problems


class Clock:
    t = 1000

    def __call__(self):
        return self.t


@pytest.fixture
async def env(tmp_path):
    store = Store(tmp_path / "t.db")
    await store.connect()
    cf, clock, problems = FakeCF(), Clock(), FakeProblems([P800])
    await store.link_handle(GUILD, 1, "h1", 1000)
    yield DailyService(store, cf, problems, clock), store, cf, clock, problems
    await store.close()


def test_floor_rating_and_utc_day():
    assert [floor_rating(r) for r in (500, 800, 899, 1099, 4000)] == [800, 800, 800, 1000, 3500]
    assert utc_day(86399) == 0 and utc_day(86400) == 1


async def test_issue_then_solve_doubles_and_counts_toward_rating(env):
    svc, store, cf, clock, _ = env
    first = await svc.claim(GUILD, 5, 1)
    assert first.problem == P800 and first.row.rating == 800 and first.row.solved_at is None
    assert (await svc.claim(GUILD, 5, 1)).row.problem_key == "1700A"  # stable within the day
    cf.subs.append(Submission(1100, 1100, "1700A", "OK"))
    done = await svc.claim(GUILD, 5, 1)
    assert done.delta == 20 and done.row.solved_at is not None
    assert await store.rating(GUILD, 1) == 820
    assert [d for _, d in await store.rating_changes(GUILD, 1)] == [20]
    again = await svc.claim(GUILD, 5, 1)
    assert again.delta is None and again.row.solved_at is not None
    assert await store.rating(GUILD, 1) == 820  # no second payout


async def test_solve_before_issue_does_not_count(env):
    svc, store, cf, clock, _ = env
    await svc.claim(GUILD, 5, 1)
    cf.subs.append(Submission(1, 999, "1700A", "OK"))  # before issued_at (1000)
    assert (await svc.claim(GUILD, 5, 1)).row.solved_at is None


async def test_new_day_new_problem_from_floored_rating(env):
    svc, store, cf, clock, problems = env
    problems.problems.append(P900)
    await store.link_handle(GUILD, 2, "h2", 1000)
    mid = await store.create_match(
        guild_id=GUILD, channel_id=5, host_id=2, host_handle="h2", mode="solo",
        max_players=1, invited_id=None, rating_arg=None, now=1,
    )  # fmt: skip
    await store.start_match(mid, 800, "1700A", 1, 99, [2])
    await store.finish_match(
        mid, 2, {2: 150}
    )  # match rating 950 floors to 900 (rounding would give 1000)
    assert (await svc.claim(GUILD, 5, 2)).row.rating == 900
    clock.t += 86400
    assert (await svc.claim(GUILD, 5, 2)).row.day == 1


async def test_solo_win_first_claims_the_day(env):
    svc, store, cf, clock, problems = env
    matches = MatchService(store, cf, problems, clock=clock)
    match = await matches.create(
        guild_id=GUILD, channel_id=5, host_id=1, mode="solo", max_players=1
    )
    cf.subs.append(Submission(1100, 1100, "1700A", "OK"))
    assert (await matches.check(match.match.id, 1)).players[0].delta == 20
    done = await svc.claim(GUILD, 5, 1)
    assert done.row.problem_key is None and done.row.solved_at is not None
    assert await store.rating(GUILD, 1) == 820  # /daily adds nothing on top


async def test_daily_after_issue_then_solo_win_is_not_paid_twice(env):
    svc, store, cf, clock, problems = env
    await svc.claim(GUILD, 5, 1)  # issued, unsolved
    matches = MatchService(store, cf, problems, clock=clock)
    match = await matches.create(
        guild_id=GUILD, channel_id=5, host_id=1, mode="solo", max_players=1
    )
    cf.subs.append(Submission(1100, 1100, "1700A", "OK"))
    assert (await matches.check(match.match.id, 1)).players[0].delta == 20
    assert (await svc.claim(GUILD, 5, 1)).delta is None
    assert await store.rating(GUILD, 1) == 820


async def test_errors(env):
    svc, store, cf, clock, problems = env
    with pytest.raises(NotLinked):
        await svc.claim(GUILD, 5, 99)
    cf.subs.append(Submission(1, 1, "1700A", "OK"))  # the only 800 problem is already solved
    with pytest.raises(BotError):
        await svc.claim(GUILD, 5, 1)


def test_daily_streak_forgives_one_missed_day():
    assert daily_streak([], 10) == 0
    assert daily_streak([10, 9, 8], 10) == 3
    assert daily_streak([10, 8, 6], 10) == 3  # one missed day between claims keeps it alive
    assert daily_streak([10, 7, 6], 10) == 1  # two missed days in a row end it
    assert daily_streak([8], 10) == 1  # one day missed so far, still claimable
    assert daily_streak([7], 10) == 0  # two days missed: over


async def test_streak_counts_claimed_days_and_shows_in_embed(env):
    svc, store, cf, clock, _ = env
    assert (await svc.claim(GUILD, 5, 1)).streak == 0
    cf.subs.append(Submission(1100, 1100, "1700A", "OK"))
    assert (await svc.claim(GUILD, 5, 1)).streak == 1
    assert (await store.daily_days(GUILD, 1)) == [0]
    clock.t += 2 * 86400  # day 2, day 1 missed
    cf.subs.clear()
    assert (await svc.claim(GUILD, 5, 1)).streak == 1  # new problem issued, streak alive
    cf.subs.append(Submission(clock.t, clock.t + 1, "1700A", "OK"))
    view = await svc.claim(GUILD, 5, 1)
    assert view.streak == 2
    assert {f.name: f.value for f in daily_embed(view).fields} == {"Streak": "2 days"}
    clock.t += 3 * 86400  # days 3, 4 missed
    cf.subs.clear()
    assert (await svc.claim(GUILD, 5, 1)).streak == 0


async def test_daily_days_by_user(env):
    svc, store, cf, clock, _ = env
    await svc.claim(GUILD, 5, 1)
    cf.subs.append(Submission(1100, 1100, "1700A", "OK"))
    await svc.claim(GUILD, 5, 1)
    assert await store.daily_days_by_user(GUILD) == {1: [0]}
    assert await store.daily_days_by_user(2) == {}


async def test_chosen_rating_and_reroll_on_a_different_rating(env):
    svc, store, cf, clock, problems = env
    problems.problems.append(P900)
    first = await svc.claim(GUILD, 5, 1, 900)
    assert first.row.rating == 900 and first.problem == P900
    assert (await svc.claim(GUILD, 5, 1, 900)).row.problem_key == "1701A"  # same rating: kept
    assert (await svc.claim(GUILD, 5, 1)).row.problem_key == "1701A"  # no rating: kept
    clock.t += 10
    swapped = await svc.claim(GUILD, 5, 1, 800)
    assert swapped.row.problem_key == "1700A" and swapped.row.issued_at == 1010
    assert swapped.row.rating == 800


async def test_solved_daily_is_claimed_before_a_reroll_and_stays_solved(env):
    svc, store, cf, clock, problems = env
    problems.problems.append(P900)
    await svc.claim(GUILD, 5, 1, 900)
    cf.subs.append(Submission(1100, 1100, "1701A", "OK"))
    done = await svc.claim(GUILD, 5, 1, 800)  # solved: claim it, ignore the new rating
    assert done.row.solved_at is not None and done.delta is not None
    again = await svc.claim(GUILD, 5, 1, 800)
    assert again.row.problem_key == "1701A" and again.delta is None
