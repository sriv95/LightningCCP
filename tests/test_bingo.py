import random

import pytest

from src.cf.models import Problem
from src.db.store import Store
from src.errors import BotError
from src.logic import (
    bingo_board,
    bingo_lines,
    bingo_standings,
    pick_replacement,
)
from src.mashup import MashupService
from src.matches import MatchService
from src.ui.embeds import mashup_embed
from tests.test_mashup import Clock, FakeCF, FakeProblems, sub

GUILD = 1
# Three problems at each rating 800-1700.
BINGO_PROBLEMS = [Problem(i, "A", f"Name{i}", 800 + 100 * (i % 10), ()) for i in range(1, 31)]


def test_bingo_lines_cover_rows_columns_and_both_diagonals():
    lines = bingo_lines(3)
    assert len(lines) == 8
    assert (0, 1, 2) in lines and (0, 3, 6) in lines
    assert (0, 4, 8) in lines and (2, 4, 6) in lines
    assert len(bingo_lines(7)) == 16 and (6, 12, 18, 24, 30, 36, 42) in bingo_lines(7)


def cells(n):
    return [(f"{i}A", 800 + 100 * i, i) for i in range(n)]


def test_classic_board_stops_at_the_first_full_line():
    # team 1 owns the top row at time 30, team 2 owns the middle row only at 40 (too late)
    solves = {
        1: {"0A": (10, 0), "1A": (20, 0), "2A": (30, 0)},
        2: {"3A": (5, 0), "4A": (6, 0), "5A": (40, 0), "6A": (35, 0)},
    }
    board = bingo_board(3, "classic", "line", cells(9), solves)
    assert board.over and board.lines == {1: 1}
    assert "6A" not in board.claims and "5A" not in board.claims  # after the winning claim
    assert board.squares[0] == ("0A", 800, 1) and board.squares[8] == ("8A", 1600, None)


def test_the_earliest_solve_claims_a_square():
    solves = {1: {"0A": (20, 0)}, 2: {"0A": (10, 3)}}
    assert bingo_board(3, "classic", "line", cells(9), solves).claims == {"0A": (2, 10)}


def test_continuous_board_ends_only_when_full_and_counts_lines():
    solves = {1: {f"{i}A": (i, 0) for i in (0, 1, 2)}, 2: {f"{i}A": (10 + i, 0) for i in (3, 4)}}
    board = bingo_board(3, "classic", "continuous", cells(9), solves)
    assert not board.over and board.lines == {1: 1}
    solves[2].update({f"{i}A": (20 + i, 0) for i in (5, 6, 7, 8)})
    assert bingo_board(3, "classic", "continuous", cells(9), solves).over


def test_replace_board_scores_every_claim_and_shows_the_newest_problem():
    rows = cells(9) + [("9A", 900, 0)]  # cell 0 was replaced by 9A
    solves = {1: {"0A": (10, 0), "9A": (50, 1)}, 2: {"1A": (20, 0)}}
    board = bingo_board(3, "replace", "line", rows, solves)
    assert not board.over and board.lines == {}
    assert board.claims == {"0A": (1, 10), "9A": (1, 50), "1A": (2, 20)}
    assert board.squares[0] == ("9A", 900, 1) and board.squares[1] == ("1A", 900, 2)


def test_bingo_standings_rank_by_lines_then_squares_then_time():
    solves = {1: {"0A": (10, 0), "1A": (20, 0)}, 2: {"3A": (5, 0), "4A": (15, 0)}, 3: {}, 4: {}}
    board = bingo_board(3, "classic", "continuous", cells(9), solves)
    ranks = bingo_standings([(1, "a", 1), (2, "b", 2), (3, "c", 3), (4, "d", 4)], board, 0)
    assert [(r.user_id, r.rank, r.solved) for r in ranks] == [
        (2, 1, 2),
        (1, 2, 2),
        (3, 3, 0),
        (4, 3, 0),
    ]
    assert ranks[0].total_seconds == 15 and ranks[0].times == {"3A": 5, "4A": 15}


def test_pick_replacement_goes_100_up_then_higher_then_same_at_the_top():
    pool = [
        Problem(1, "A", "x", 900, ()),
        Problem(2, "A", "y", 1100, ()),
        Problem(3, "A", "z", 3500, ()),
    ]
    rng = random.Random(1)
    assert pick_replacement(pool, 800, frozenset(), rng).key == "1A"
    assert pick_replacement(pool, 900, frozenset(), rng).key == "2A"  # 1000 has none
    assert pick_replacement(pool, 1100, frozenset({"2A"}), rng).key == "3A"
    assert pick_replacement(pool, 3500, frozenset(), rng).key == "3A"  # the top: the same rating
    assert pick_replacement(pool, 3500, frozenset({"3A"}), rng) is None


@pytest.fixture
async def env(tmp_path):
    store = Store(tmp_path / "t.db")
    await store.connect()
    cf, clock = FakeCF(), Clock()
    clock.t = 1000
    problems = FakeProblems(BINGO_PROBLEMS)
    svc = MashupService(store, cf, problems, clock=clock)
    for uid in range(1, 5):
        await store.link_handle(GUILD, uid, f"h{uid}", None)
    svc.matches = MatchService(store, cf, problems, clock=clock)
    yield svc, store, cf, clock
    await store.close()


async def bingo(svc, *joiners, mode="classic", win="line", size=3, **kw):
    args = dict(
        guild_id=GUILD,
        channel_id=5,
        host_id=1,
        min_rating=800,
        max_rating=1700,
        count=0,
        duration_seconds=7200,
        mode=mode,
        size=size,
        win=win,
    )
    args.update(kw)
    view = await svc.create(**args)
    for uid in joiners:
        await svc.join(view.mashup.id, uid)
    return await svc.start(view.mashup.id, 1)


async def test_create_validates_size_and_sets_the_board_count(env):
    svc, *_ = env
    with pytest.raises(BotError):
        await svc.create(
            guild_id=GUILD, channel_id=5, host_id=1, min_rating=800, max_rating=900,
            count=0, duration_seconds=60, mode="classic", size=8,
        )  # fmt: skip
    view = await bingo(svc, 2, size=4)
    assert view.mashup.count == 16 and len(view.cells) == 16
    assert sorted(c.cell for c in view.cells) == list(range(16))


async def test_classic_line_win_ends_the_bingo_and_rates_it(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2)
    keys = [c.key for c in sorted(view.cells, key=lambda c: c.cell)]
    for i, at in zip((0, 1, 2), (1010, 1020, 1030), strict=True):
        sub(cf, "h1", keys[i], at)
    sub(cf, "h2", keys[3], 1015)
    sub(cf, "h2", keys[4], 1025)
    clock.t = 1040
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished" and done.mashup.ends_at == 1040
    ranks = done.standings
    assert [(r.handle, r.rank, r.solved, r.lines) for r in ranks] == [
        ("h1", 1, 3, 1),
        ("h2", 2, 2, 0),
    ]
    deltas = {p.handle: p.delta for p in done.players}
    assert deltas["h1"] > 0 > deltas["h2"]
    assert "[1]" in mashup_embed(done, {}).description  # the board shows the owner


async def test_a_line_waits_for_an_earlier_claim_of_a_player_not_read_yet(env, monkeypatch):
    svc, store, cf, clock = env
    view = await bingo(svc, 2)
    keys = [c.key for c in sorted(view.cells, key=lambda c: c.cell)]
    for i, at in zip((0, 1, 2), (1010, 1020, 1030), strict=True):
        sub(cf, "h1", keys[i], at)
    for i, at in zip(
        (3, 4, 5), (1005, 1006, 1007), strict=True
    ):  # h2's line is earlier but not read yet
        sub(cf, "h2", keys[i], at)
    monkeypatch.setattr("src.mashup.MASHUP_READS_PER_SECOND", 0.0001)  # one read per tick
    clock.t = 1040
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished"
    assert [(r.handle, r.rank) for r in done.standings] == [("h2", 1), ("h1", 2)]


async def test_continuous_bingo_keeps_going_after_a_line(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, win="continuous")
    keys = [c.key for c in sorted(view.cells, key=lambda c: c.cell)]
    for i, at in zip((0, 1, 2), (1010, 1020, 1030), strict=True):
        sub(cf, "h1", keys[i], at)
    sub(cf, "h2", keys[3], 1015)
    clock.t = 1040
    await svc.tick()
    assert (await svc.view(view.mashup.id)).mashup.status == "active"
    clock.t = view.mashup.ends_at
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished"
    assert [(r.handle, r.lines) for r in done.standings] == [("h1", 1), ("h2", 0)]


async def test_a_full_classic_board_ends_the_bingo(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, win="continuous")
    keys = [c.key for c in sorted(view.cells, key=lambda c: c.cell)]
    for i, key in enumerate(keys):
        sub(cf, "h1" if i % 2 else "h2", key, 1010 + i)
    clock.t = 1100
    for _ in range(3):  # reads share a budget, so a few ticks
        await svc.tick()
    assert (await svc.view(view.mashup.id)).mashup.status == "finished"


async def test_replace_gives_a_solved_square_a_problem_100_higher(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace")
    first = next(c for c in view.cells if c.cell == 0)
    sub(cf, "h1", first.key, 1010)
    clock.t = 1020
    after = await svc.check(view.mashup.id, 1)
    newest = [c for c in after.cells if c.cell == 0][-1]
    assert newest.idx > first.idx and newest.key != first.key
    assert newest.rating == first.rating + 100 and newest.since == 1010
    assert after.board.squares[0][:2] == (newest.key, newest.rating)
    assert after.board.squares[0][2] is None  # open again
    # Checking again does not stack a second replacement on the same cell.
    again = await svc.check(view.mashup.id, 1)
    assert len([c for c in again.cells if c.cell == 0]) == 2
    # Only a solve after the claim counts for the new problem; a later one claims it.
    sub(cf, "h2", newest.key, 1005)
    sub(cf, "h2", again.cells[-1].key, 1500, verdict="WRONG_ANSWER")
    clock.t = 1100
    assert (await svc.check(view.mashup.id, 2)).board.claims.keys() == {first.key}
    sub(cf, "h2", newest.key, 1060)
    third = await svc.check(view.mashup.id, 2)
    assert third.board.claims[newest.key] == (2, 1060)
    assert len([c for c in third.cells if c.cell == 0]) == 3
    ranks = third.standings
    assert [(r.handle, r.solved, r.lines) for r in ranks] == [("h1", 1, 0), ("h2", 1, 0)]
    assert ranks[0].rank == 1 and ranks[1].rank == 2  # equal squares: h1 got there first


async def test_replace_runs_until_time_is_up_and_ranks_by_squares(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace")
    for c in view.cells[:2]:
        sub(cf, "h2", c.key, 1010)
    clock.t = 1100
    await svc.tick()
    assert (await svc.view(view.mashup.id)).mashup.status == "active"
    clock.t = view.mashup.ends_at
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished"
    assert [(r.handle, r.solved) for r in done.standings] == [("h2", 2), ("h1", 0)]


async def test_bingo_embeds_and_rematch(env):
    svc, store, cf, clock = env
    lobby = await svc.create(
        guild_id=GUILD, channel_id=5, host_id=1, min_rating=800, max_rating=1700, count=0,
        duration_seconds=3600, mode="classic", size=3, win="line",
    )  # fmt: skip
    e = mashup_embed(lobby, {})
    assert (
        e.title == "Bingo lobby" and "3x3 classic bingo, the first full line wins" in e.description
    )
    await svc.join(lobby.mashup.id, 2)
    view = await svc.start(lobby.mashup.id, 1)
    by_key = {p.key: p for p in BINGO_PROBLEMS}
    e = mashup_embed(view, by_key)
    assert e.title == "Bingo"
    assert [f.name for f in e.fields] == ["Row 1", "Row 2", "Row 3"]
    assert "codeforces.com" in e.fields[0].value
    clock.t = view.mashup.ends_at
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert mashup_embed(done, by_key).title == "Bingo - final standings"
    again = await svc.rematch(view.mashup.id, 2, 5)
    m = again.mashup
    assert (m.mode, m.size, m.win, m.count) == ("classic", 3, "line", 9)
    assert mashup_embed(await bingo_lobby_replace(svc), {}).description.count("replace bingo") == 1


async def bingo_lobby_replace(svc):
    return await svc.create(
        guild_id=GUILD, channel_id=5, host_id=3, min_rating=800, max_rating=1700, count=0,
        duration_seconds=3600, mode="replace", size=4,
    )  # fmt: skip
