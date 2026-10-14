import asyncio

import pytest

from src.cf.models import Problem, Submission
from src.cogs.mashup import mashup_components
from src.db.store import Store
from src.errors import AlreadyInMatch, BotError, CacheNotReady, MatchClosed, MatchFull, NotLinked
from src.logic import LOBBY_TTL_SECONDS, MASHUP_MAX_PLAYERS, MASHUP_MAX_TEAMS
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
    for uid in range(100, 100 + MASHUP_MAX_TEAMS - 1):  # a default contest has 10 teams of one
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


async def team_lobby(svc, teams=2, **kw):
    return await lobby(svc, teams=teams, **{"team_size": 5, **kw})


async def test_team_contest_join_needs_a_valid_team(env):
    svc, store, cf, clock = env
    view = await team_lobby(svc, teams=3)
    assert [(p.user_id, p.team) for p in view.players] == [(1, 1)]  # the host is in team 1
    assert view.mashup.teams == 3
    mid = view.mashup.id
    for bad in (None, 0, 4):
        with pytest.raises(BotError, match="1 to 3"):
            await svc.join(mid, 2, bad)
    assert [(p.user_id, p.team) for p in (await svc.join(mid, 2, 3)).players] == [(1, 1), (2, 3)]
    solo = await lobby(svc, host=4)  # teams of one ignore the number and take the first free team
    assert (await svc.join(solo.mashup.id, 3, 7)).players[1].team == 2


async def test_team_standings_count_a_problem_once_from_the_first_solver(env):
    svc, store, cf, clock = env
    view = await team_lobby(svc, penalty_minutes=10)
    mid = view.mashup.id
    await svc.join(mid, 2, 1)
    await svc.join(mid, 3, 2)
    view = await svc.start(mid, 1)
    keys = [k for k, _ in view.problems]
    sub(cf, "h1", keys[0], 1100)  # team 1: A at 100 s by h1, also A at 150 s by h2
    sub(cf, "h2", keys[0], 1150)
    sub(cf, "h2", keys[1], 1200)
    sub(cf, "h3", keys[0], 1050)  # team 2: A at 50 s
    sub(cf, "h3", keys[1], 1020, verdict="WRONG_ANSWER")
    sub(cf, "h3", keys[1], 1300)
    for uid in (1, 2, 3):
        await svc.check(mid, uid)
    view = await svc.view(mid)
    ranks = view.standings
    assert [(r.rank, r.user_id, r.handle, r.solved) for r in ranks] == [
        (1, 1, "Team 1", 2),
        (2, 2, "Team 2", 2),
    ]
    assert ranks[0].total_seconds == 100 + 200  # not 150: the earliest solve of A counts
    assert ranks[1].total_seconds == 50 + 300 + 10 * 60  # one wrong attempt before B


async def test_team_contest_embeds_and_refresh_skips_finished_teams(env):
    svc, store, cf, clock = env
    view = await team_lobby(svc, count=1, min_rating=800, max_rating=800)
    mid = view.mashup.id
    await svc.join(mid, 2, 2)
    text = mashup_embed(await svc.view(mid), {}).description
    assert "Team 1: <@1> (h1)" in text and "Team 2: <@2> (h2)" in text
    assert "Join asks for a team number (1-2)" in text and "2 teams" in text
    await svc.join(mid, 3, 1)
    await svc.start(mid, 1)
    sub(cf, "h1", "1A", 1100)
    await svc.check(mid, 1)
    cf.calls.clear()
    clock.t = 1200
    await svc.tick()
    assert cf.calls == ["h2"]  # team 1 solved everything: neither h1 nor teammate h3 is read
    embed = mashup_embed(await svc.view(mid), {})
    assert embed.title == "Team mashup" and "1. Team 1 (<@1>, <@3>) - 1 solved" in embed.description


def test_team_solves_picks_the_earliest_solve_per_problem():
    from src.logic import team_solves

    solves = {1: {"A": (100, 0), "B": (500, 1)}, 2: {"A": (90, 2)}, 3: {"A": (10, 0)}}
    out = team_solves([(1, 1), (2, 1), (3, 2)], solves)
    assert out == {1: {"A": (90, 2), "B": (500, 1)}, 2: {"A": (10, 0)}}


async def test_player_cap_is_chosen_by_the_host(env):
    svc, store, cf, clock = env
    view = await lobby(svc, teams=2)
    assert view.mashup.max_players == 2
    assert "1/2 players" in mashup_embed(view, {}).description
    await svc.join(view.mashup.id, 2)
    with pytest.raises(MatchFull):
        await svc.join(view.mashup.id, 3)
    for bad in (0, MASHUP_MAX_TEAMS + 1):
        with pytest.raises(BotError, match="teams must be from 1 to 10"):
            await lobby(svc, host=4, teams=bad)
    for bad in (0, 6):
        with pytest.raises(BotError, match="per team must be from 1 to 5"):
            await lobby(svc, host=4, team_size=bad)
    assert (await lobby(svc, host=3)).mashup.max_players == 10  # default: 10 teams of one
    big = await lobby(svc, host=4, teams=10, team_size=5)
    assert big.mashup.max_players == MASHUP_MAX_PLAYERS  # the 20 player limit still holds


async def finish_contest(svc, cf, clock, view, solves):
    """solves: {handle: [(problem index, time)]}; then run past the end."""
    keys = [k for k, _ in view.problems]
    for handle, found in solves.items():
        for index, at in found:
            sub(cf, handle, keys[index], at)
    clock.t = view.mashup.ends_at
    await svc.tick()
    return await svc.view(view.mashup.id)


async def test_history_lists_finished_contests_with_the_users_place(env):
    from src.ui.embeds import history_pages

    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    done = await finish_contest(
        svc, cf, clock, view, {"h1": [(0, 1100)], "h2": [(0, 1100), (1, 1200)]}
    )
    assert done.mashup.status == "finished"
    records = await svc.history(GUILD, 1)
    assert [(r.rank, r.entrants, r.solved, r.team) for r in records] == [(2, 3, 1, 1)]
    assert [r.rank for r in await svc.history(GUILD, 3)] == [3]  # nothing solved ranks last
    assert await svc.history(GUILD, 4) == []
    page = history_pages("t", [], 1, {}, records)[0].description
    assert page.endswith("Mashup - 2nd of 3, 1/3 solved, +0")
    nothing = history_pages("t", [], 3, {}, await svc.history(GUILD, 3))[0].description
    assert nothing.endswith("Mashup - nothing solved, 0/3 solved, -49")


async def test_history_of_a_team_contest_shows_the_team_place(env):
    from src.ui.embeds import history_pages

    svc, store, cf, clock = env
    lobby_view = await team_lobby(svc)
    mid = lobby_view.mashup.id
    await svc.join(mid, 2, 2)
    await svc.join(mid, 3, 2)
    view = await svc.start(mid, 1)
    await finish_contest(svc, cf, clock, view, {"h3": [(0, 1100)]})
    (record,) = await svc.history(GUILD, 2)
    assert (record.rank, record.entrants, record.solved, record.team) == (1, 2, 1, 2)
    line = history_pages("t", [], 2, {}, [record])[0].description
    assert line.endswith("Team mashup (team 2) - 1st of 2, 1/3 solved, +49")


def test_history_pages_merges_matches_and_mashups_newest_first():
    from src.db.store import MashupRow
    from src.mashup import MashupRecord
    from src.ui.embeds import history_pages
    from tests.test_match_embeds import player, row

    match = row(mode="solo", status="finished", rating=800, problem_key="1A", finished_at=2000)
    solo = [player(10, solved_at=1500, delta=10)]
    mashup = MashupRow(
        1, 1, 5, None, 10, 800, 1200, 3, 7200, 0, "finished", 0, 0, 3000, team_size=1
    )
    new = MashupRecord(mashup, 1, 4, 3, None)
    pages = history_pages("t", [(match, solo)], 10, {}, [new])
    lines = pages[0].description.splitlines()
    assert "Mashup - 1st of 4" in lines[0] and "Solo" in lines[1]


async def test_finished_mashup_moves_match_ratings(env):
    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    done = await finish_contest(
        svc, cf, clock, view, {"h1": [(0, 1100)], "h2": [(0, 1100), (1, 1200)]}
    )
    assert {p.user_id: p.delta for p in done.players} == {1: 0, 2: 49, 3: -49}
    assert [await store.rating(GUILD, u) for u in (1, 2, 3)] == [800, 849, 751]
    assert dict((u, n) for u, _, _, n in await store.stats(GUILD)) == {1: 1, 2: 1, 3: 1}
    assert [d for _, d in await store.rating_changes(GUILD, 2)] == [49]
    assert await store.stats(GUILD, "matches") == []
    assert {u: r for u, _, r, _ in await store.stats(GUILD, "contests")} == {1: 800, 2: 849, 3: 751}
    text = mashup_embed(done, {}).description
    assert "1. h2 - 2 solved" in text and text.count("rating +49") == 1
    clock.t += 5000
    await svc.tick()  # a second tick must not apply the change again
    assert await store.rating(GUILD, 2) == 849


async def test_team_contest_rates_every_member_by_team_average(env):
    svc, store, cf, clock = env
    await store.link_handle(GUILD, 5, "h5", None)
    mid = (await team_lobby(svc)).mashup.id
    await svc.join(mid, 2, 1)
    await svc.join(mid, 3, 2)
    await svc.join(mid, 5, 2)
    view = await svc.start(mid, 1)
    done = await finish_contest(svc, cf, clock, view, {"h5": [(0, 1100)]})
    assert {p.user_id: p.delta for p in done.players} == {1: -49, 2: -49, 3: 49, 5: 49}
    assert "rating +49" in mashup_embed(done, {}).description


async def test_nothing_solved_or_one_entrant_changes_no_rating(env):
    svc, store, cf, clock = env
    done = await finish_contest(svc, cf, clock, await started(svc, 2), {})
    assert {p.user_id: p.delta for p in done.players} == {1: 0, 2: 0}  # a tie moves nothing
    alone = await finish_contest(svc, cf, clock, await started(svc, host=3), {"h3": [(0, 9999)]})
    assert [p.delta for p in alone.players] == [None]  # one entrant: not rated
    assert await store.rating(GUILD, 3) == 800
    assert {u for u, *_ in await store.stats(GUILD)} == {1, 2}  # player 3 played nothing rated


async def test_unrated_contest_changes_no_rating(env):
    svc, store, cf, clock = env
    view = await started(svc, 2, rated=False)
    assert "unrated" in mashup_embed(view, {}).description
    done = await finish_contest(svc, cf, clock, view, {"h2": [(0, 1100)]})
    assert [p.delta for p in done.players] == [None, None]
    assert [await store.rating(GUILD, u) for u in (1, 2)] == [800, 800]
    assert "rating" not in mashup_embed(done, {}).description.split("**Standings**")[1]


async def test_team_size_caps_each_team(env):
    svc, store, cf, clock = env
    mid = (await team_lobby(svc, team_size=2)).mashup.id
    await svc.join(mid, 2, 1)  # team 1 now has the host and player 2
    with pytest.raises(MatchFull):
        await svc.join(mid, 3, 1)
    assert [p.team for p in (await svc.join(mid, 3, 2)).players] == [1, 1, 2]
    with pytest.raises(BotError):
        await team_lobby(svc, host=4, teams=0, team_size=2)  # needs a team contest
    with pytest.raises(BotError):
        await team_lobby(svc, host=4, team_size=MASHUP_MAX_PLAYERS + 1)


async def test_join_again_changes_team(env):
    svc, store, cf, clock = env
    mid = (await team_lobby(svc, team_size=2)).mashup.id
    await svc.join(mid, 2, 1)
    await svc.join(mid, 3, 2)
    with pytest.raises(MatchFull):
        await svc.join(mid, 3, 1)  # team 1 is full; player 3 stays in team 2
    view = await svc.join(mid, 2, 2)
    assert {p.user_id: p.team for p in view.players} == {1: 1, 2: 2, 3: 2}
    assert (await svc.join(mid, 2, 2)).players[1].team == 2  # same team again changes nothing
    with pytest.raises(BotError, match="host"):
        await svc.join(mid, 1, 2)
    with pytest.raises(BotError):
        await svc.join(mid, 2, 9)  # an unknown team


async def test_tags_filter_the_problems_and_show_in_the_lobby(env):
    svc, store, cf, clock = env
    view = await lobby(svc, tags="DP, math", exclude_tags="graphs")
    assert (view.mashup.tags, view.mashup.exclude_tags) == ("dp,math", "graphs")
    assert "tags dp, math, without graphs" in mashup_embed(view, {}).description
    with pytest.raises(BotError, match="with those tags"):
        await svc.start(view.mashup.id, 1)  # no test problem has those tags
    await svc.leave(view.mashup.id, 1)
    plain = await lobby(svc, exclude_tags="dp")
    assert (await svc.start(plain.mashup.id, 1)).mashup.status == "active"


async def test_rematch_copies_the_settings_for_a_player(env):
    svc, store, cf, clock = env
    view = await started(svc, 2, tags="", exclude_tags="graphs", penalty_minutes=5, rated=False)
    with pytest.raises(MatchClosed):
        await svc.rematch(view.mashup.id, 2, 77)  # still running
    done = await finish_contest(svc, cf, clock, view, {})
    with pytest.raises(BotError, match="players"):
        await svc.rematch(done.mashup.id, 9, 77)
    new = await svc.rematch(done.mashup.id, 2, 77)
    m, old = new.mashup, done.mashup
    assert (m.status, m.host_id, m.channel_id) == ("lobby", 2, 77)
    assert [p.user_id for p in new.players] == [2]
    assert (m.min_rating, m.max_rating, m.count, m.duration_seconds) == (
        old.min_rating,
        old.max_rating,
        old.count,
        old.duration_seconds,
    )
    assert (m.penalty_minutes, m.exclude_tags, m.rated) == (5, "graphs", 0)
    assert mashup_components(done) is not None and mashup_components(new) is not None


async def test_virtual_contest_replays_a_contests_rated_problems(env):
    svc, store, cf, clock = env
    svc._problems.problems += [
        Problem(500, "B", "Second", 1400, ()),
        Problem(500, "A", "First", 900, ()),
        Problem(500, "C", "Unrated", None, ()),
    ]
    sub(cf, "h1", "500A", 100)  # solved long ago: still part of the contest
    view = await lobby(svc, contest_id=500, tags="dp", count=9, min_rating=2000)
    m = view.mashup
    assert (m.contest_id, m.count, m.min_rating, m.max_rating, m.tags) == (500, 2, 900, 1400, "")
    assert "virtual contest 500" in mashup_embed(view, {}).description
    started_ = await svc.start(m.id, 1)
    assert started_.problems == [("500A", 900), ("500B", 1400)]
    with pytest.raises(BotError, match="no rated problems"):
        await lobby(svc, host=2, contest_id=12345)


async def test_team_virtual_contest_and_rematch(env):
    svc, store, cf, clock = env
    svc._problems.problems += [
        Problem(500, "A", "First", 900, ()),
        Problem(500, "B", "Two", 1400, ()),
    ]
    view = await lobby(svc, contest_id=500, teams=2, team_size=2)
    await svc.join(view.mashup.id, 2, 2)
    started_ = await svc.start(view.mashup.id, 1)
    assert [k for k, _ in started_.problems] == ["500A", "500B"] and started_.mashup.teams == 2
    done = await finish_contest(svc, cf, clock, started_, {"h2": [(0, 1100)]})
    assert [p.delta for p in done.players] == [-49, 49]
    again = await svc.rematch(done.mashup.id, 2, 5)
    assert (again.mashup.contest_id, again.mashup.teams, again.mashup.team_size) == (500, 2, 2)
    assert [(p.user_id, p.team) for p in again.players] == [(2, 1)]


async def test_tick_reads_the_stalest_players_within_the_budget(env, monkeypatch):
    monkeypatch.setattr("src.mashup.MASHUP_READS_PER_SECOND", 2 / 60)  # 2 reads in a 60 s interval
    svc, store, cf, clock = env
    view = await started(svc, 2, 3, 4)
    cf.calls.clear()
    clock.t = 1060
    await svc.tick()
    assert cf.calls == ["h1", "h2"]  # nobody read yet: joining order
    clock.t = 1120
    await svc.tick()
    assert cf.calls == ["h1", "h2", "h3", "h4"]  # the two read longest ago come next
    clock.t = 1180
    await svc.tick()
    assert cf.calls[4:] == ["h1", "h2"]
    cf.calls.clear()
    clock.t = view.mashup.ends_at  # out of time: everyone is read, whatever the budget
    await svc.tick()
    assert sorted(cf.calls) == ["h1", "h2", "h3", "h4"]
    assert (await svc.view(view.mashup.id)).mashup.status == "finished"


async def test_interval_shrinks_with_fewer_players_waiting(env):
    svc, store, cf, clock = env
    assert svc.interval == 60  # nothing running yet
    await started(svc, 2)  # 2 players: 4 s of reads at 0.5 per second, so the minimum
    cf.calls.clear()
    clock.t = 1060
    await svc.tick()
    assert (svc.interval, cf.calls) == (10, ["h1", "h2"])
    await svc.tick()
    assert cf.calls == ["h1", "h2"] * 2  # 10 s pay for 5 reads, so both are read again


async def test_give_up_keeps_the_place_but_frees_the_player(env):
    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    mid = view.mashup.id
    keys = [k for k, _ in view.problems]
    sub(cf, "h2", keys[0], 1100)
    await svc.check(mid, 2)
    with pytest.raises(BotError, match="not playing"):
        await svc.give_up(mid, 4)
    gone = await svc.give_up(mid, 2)
    assert [p.active for p in gone.players] == [True, False, True]
    assert "1. h2 - 1 solved" in mashup_embed(gone, {}).description
    assert "(gave up)" in mashup_embed(gone, {}).description
    with pytest.raises(BotError, match="not playing"):
        await svc.give_up(mid, 2)
    with pytest.raises(BotError, match="gave up"):
        await svc.check(mid, 2)
    with pytest.raises(MatchClosed):
        await svc.leave(mid, 3)  # an active contest is left with Give Up
    other = await lobby(svc, host=2)  # free for another contest
    assert other.mashup.status == "lobby"
    cf.calls.clear()
    sub(cf, "h2", keys[1], 1150)  # solved after giving up: not read, not counted
    clock.t = 1060
    await svc.tick()
    assert "h2" not in cf.calls
    done = await finish_contest(svc, cf, clock, await svc.view(mid), {})
    assert done.mashup.status == "finished"
    by_user = {p.user_id: p.delta for p in done.players}
    assert by_user[2] > 0 and by_user[1] < 0 and by_user[3] < 0  # h2 still wins with 1 solve
    assert "1. h2 - 1 solved" in mashup_embed(done, {}).description
    assert await store.rating(GUILD, 2) == 800 + by_user[2]


async def test_contest_ends_when_everyone_still_in_solved_everything(env):
    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    mid, ends_at = view.mashup.id, view.mashup.ends_at
    keys = [k for k, _ in view.problems]
    for handle in ("h1", "h2"):
        for key in keys:
            sub(cf, handle, key, 1100)
    await svc.give_up(mid, 3)
    clock.t = 1060
    await svc.tick()
    done = await svc.view(mid)
    assert done.mashup.status == "finished" and done.mashup.ends_at == 1060 < ends_at
    assert {p.user_id: p.delta for p in done.players}[3] < 0  # the one who left is ranked last


async def test_contest_ends_when_everyone_gave_up(env):
    svc, store, cf, clock = env
    view = await started(svc, 2)
    await svc.give_up(view.mashup.id, 1)
    await svc.give_up(view.mashup.id, 2)
    clock.t = 1060
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished"
    assert [p.delta for p in done.players] == [0, 0]  # nobody solved: a tie


async def test_contest_log_has_every_teams_solve_once_with_the_solver(env):
    from src.ui.embeds import log_board_embed, log_message

    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    mid = view.mashup.id
    keys = [k for k, _ in view.problems]
    sub(cf, "h1", keys[0], 1100)
    sub(cf, "h2", keys[0], 1200, verdict="WRONG_ANSWER")
    sub(cf, "h2", keys[0], 1300)
    sub(cf, "h2", keys[1], 1400)
    clock.t = 1500
    for uid in (1, 2, 3):
        view = await svc.check(mid, uid)
    assert await svc.claims_to_log(view) == []  # no thread yet: nothing is marked
    await svc.set_thread(mid, 5)
    view = await svc.view(mid)
    entries = await svc.claims_to_log(view)
    assert [(e.at, e.team, e.key, e.user, e.wrong) for e in entries] == [
        (1100, 1, keys[0], 1, 0),
        (1300, 2, keys[0], 2, 1),  # team 2 solves the same problem: logged for its own team
        (1400, 2, keys[1], 2, 0),
    ]
    assert await svc.claims_to_log(await svc.view(mid)) == []  # once each
    by_key = {p.key: p for p in PROBLEMS}
    assert log_message(view, entries[0], by_key).startswith("<t:1100:T> (+1m) <@1> solved A. [")
    assert log_message(view, entries[1], by_key).endswith(", 1 wrong before")
    assert "h2" in log_board_embed(view).description  # the standings


async def test_ff_leaves_a_lobby_gives_up_a_running_contest_or_match(env):
    from types import SimpleNamespace

    from src.cogs.match import MatchCog

    svc, store, cf, clock = env
    edited, said = [], []

    class Mashups:
        async def refresh(self, view):
            edited.append(view.mashup.status)

        async def log_gave_up(self, view, user_id):
            edited.append(f"gave up {user_id}")

    bot = SimpleNamespace(
        services=SimpleNamespace(matches=svc.matches, mashups=svc, db=store),
        get_cog=lambda name: Mashups(),
        get_partial_messageable=lambda *a, **kw: None,
    )
    cog = MatchCog(bot)

    async def ff(uid):
        async def defer(**kw):
            pass

        async def send(text, **kw):
            said.append(text)

        interaction = SimpleNamespace(
            guild_id=GUILD,
            user=SimpleNamespace(id=uid),
            response=SimpleNamespace(defer=defer),
            followup=SimpleNamespace(send=send),
        )
        await MatchCog.ff.callback(cog, interaction)

    with pytest.raises(BotError):
        await ff(1)  # in nothing
    view = await lobby(svc, host=1)
    await svc.join(view.mashup.id, 2)
    await ff(2)  # leaves the contest lobby
    assert said[-1] == "You left the contest lobby." and edited == ["lobby"]
    assert [p.user_id for p in (await svc.view(view.mashup.id)).players] == [1]
    await svc.join(view.mashup.id, 3)
    await svc.start(view.mashup.id, 1)
    await ff(3)  # gives up the running contest
    assert said[-1] == "You gave up the contest." and edited[-2:] == ["active", "gave up 3"]
    assert await store.active_mashup_for(GUILD, 3) is None
    await ff(1)
    await svc.tick()
    assert (await svc.view(view.mashup.id)).mashup.status == "finished"  # everyone is out
    duel = await svc.matches.create(
        guild_id=GUILD, channel_id=5, host_id=4, mode="duel", max_players=2
    )
    await ff(4)  # the host of a match lobby cancels it
    assert said[-1] == "You left the match lobby."
    assert (await store.get_match(duel.match.id)).status == "cancelled"


async def test_final_message_names_the_winner_a_tie_or_nobody(env):
    from src.ui.embeds import final_message

    svc, store, cf, clock = env
    view = await started(svc, 2)
    done = await finish_contest(svc, cf, clock, view, {})
    assert final_message(done) == "The contest is over. Nobody solved anything."
    view = await started(svc, 3, host=4)
    at = view.mashup.started_at + 100
    done = await finish_contest(svc, cf, clock, view, {"h4": [(0, at)], "h3": [(0, at)]})
    assert final_message(done).startswith("The contest is over. Tied for first: <@4>, <@3>")
    view = await started(svc, 2, host=1)
    done = await finish_contest(svc, cf, clock, view, {"h1": [(0, view.mashup.started_at + 100)]})
    assert final_message(done) == "The contest is over. Winner: <@1>. Ratings changed."
    lobby_ = await lobby(svc, host=4)
    await svc.leave(lobby_.mashup.id, 4)
    assert final_message(await svc.view(lobby_.mashup.id)) == "This contest was cancelled."


async def test_give_up_is_logged_to_the_thread(env):
    from types import SimpleNamespace

    from src.cogs.mashup import MashupCog
    from src.ui.embeds import give_up_message

    svc, store, cf, clock = env
    view = await started(svc, 2)
    await svc.set_thread(view.mashup.id, 9)
    view = await svc.give_up(view.mashup.id, 2)
    assert give_up_message(view, 2) == "<@2> gave up."
    sent = []

    class Thread:
        async def send(self, text, **kw):
            sent.append(text)

    cog = MashupCog(SimpleNamespace(get_partial_messageable=lambda *a, **kw: Thread()))
    await cog.log_gave_up(view, 2)
    assert sent == ["<@2> gave up."]
    team = await team_lobby(svc, host=3)  # teams show the team too
    await svc.join(team.mashup.id, 4, 2)
    assert give_up_message(await svc.view(team.mashup.id), 4) == "Team 2 (<@4>) gave up."
    await cog.log_gave_up(
        SimpleNamespace(mashup=SimpleNamespace(thread_id=None)), 2
    )  # no thread: nothing happens
    assert len(sent) == 1


async def test_start_pings_every_player_in_the_thread(env):
    from types import SimpleNamespace

    from src.cogs.mashup import MashupCog
    from src.ui.embeds import start_message

    svc, store, cf, clock = env
    view = await started(svc, 2, 3)
    text = start_message(view)
    assert text.startswith("<@1> <@2> <@3>\nThe contest has started and ends ")
    assert f"<t:{view.mashup.ends_at}:R>" in text
    sent = []

    class Thread:
        async def send(self, text, **kw):
            sent.append((text, kw["allowed_mentions"].users))

    cog = MashupCog(SimpleNamespace(get_partial_messageable=lambda *a, **kw: Thread()))
    await cog.log_started(view)  # no thread yet
    assert sent == []
    await svc.set_thread(view.mashup.id, 9)
    await cog.log_started(await svc.view(view.mashup.id))
    assert sent == [(text, True)]


async def test_wins_count_finished_contests_won_against_someone(env):
    svc, store, cf, clock = env
    view = await started(svc, 2)
    await finish_contest(svc, cf, clock, view, {"h1": [(0, view.mashup.started_at + 60)]})
    assert await svc.wins(GUILD, 1) == 1 and await svc.wins(GUILD, 2) == 0
    lone = await started(svc, host=3)
    await finish_contest(svc, cf, clock, lone, {"h3": [(0, lone.mashup.started_at + 60)]})
    assert await svc.wins(GUILD, 3) == 0  # nobody to beat
    tie = await started(svc, 2, host=4)
    await finish_contest(svc, cf, clock, tie, {})
    assert await svc.wins(GUILD, 4) == 0  # nothing solved
