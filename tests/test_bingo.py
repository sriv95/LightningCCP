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
from src.mashup import MashupService, MashupView
from src.matches import MatchService
from src.ui.embeds import mashup_embed
from tests.test_mashup import Clock, FakeCF, FakeProblems, sub

GUILD = 1
# Five problems at each rating 800-1700.
BINGO_PROBLEMS = [Problem(i, "A", f"Name{i}", 800 + 100 * (i % 10), ()) for i in range(1, 51)]


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


def test_replace_board_lets_another_team_take_a_square_over():
    rows = cells(9) + [("9A", 900, 0)]  # cell 0 was replaced by 9A
    solves = {1: {"0A": (10, 0), "1A": (12, 0)}, 2: {"9A": (50, 1), "0A": (60, 0)}}
    board = bingo_board(3, "replace", "line", rows, solves)
    assert not board.over  # a replace board is never "full"
    assert board.claims == {"0A": (1, 10), "1A": (1, 12), "9A": (2, 50)}  # 0A: only the first
    assert board.holders == {0: (2, 50), 1: (1, 12)}
    assert board.squares[0] == ("9A", 900, 2) and board.squares[1] == ("1A", 900, 1)
    assert board.squares[2] == ("2A", 1000, None)


def test_replace_line_must_be_held_when_the_last_square_is_claimed():
    rows = cells(9) + [("9A", 900, 0)]
    solves = {
        1: {"0A": (10, 0), "1A": (11, 0), "2A": (30, 0)},
        2: {"9A": (20, 0)},  # steals cell 0 before team 1 completes the row
    }
    board = bingo_board(3, "replace", "line", rows, solves)
    assert not board.over and board.lines == {}
    solves[1]["9A"] = (15, 0)  # team 1 takes it first instead
    assert bingo_board(3, "replace", "line", rows, solves).over


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
    assert ranks[0].total_seconds == 15


def test_pick_replacement_goes_100_up_then_higher_then_same_at_the_top():
    pool = [
        Problem(1, "A", "x", 900, ()),
        Problem(2, "A", "y", 1100, ()),
        Problem(3, "A", "z", 3500, ()),
    ]
    rng = random.Random(1)
    assert pick_replacement(pool, 800, frozenset(), rng=rng).key == "1A"
    assert pick_replacement(pool, 900, frozenset(), rng=rng).key == "2A"  # 1000 has none
    assert pick_replacement(pool, 1100, frozenset({"2A"}), rng=rng).key == "3A"
    assert (
        pick_replacement(pool, 3500, frozenset(), rng=rng).key == "3A"
    )  # the top: the same rating
    assert pick_replacement(pool, 3500, frozenset({"3A"}), rng=rng) is None
    # Never above max_rating: 1100 is too high for a 1000 cap, so the same rating again.
    assert pick_replacement(pool, 900, frozenset(), 1000, rng=rng).key == "1A"
    assert pick_replacement(pool, 900, frozenset({"1A"}), 1000, rng=rng) is None


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


async def test_replace_gives_a_claimed_square_a_problem_100_higher_to_steal(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace")
    first = next(c for c in view.cells if c.rating == 800)  # shuffled; not the top rating
    sub(cf, "h1", first.key, 1010)
    clock.t = 1020
    after = await svc.check(view.mashup.id, 1)
    newest = [c for c in after.cells if c.cell == first.cell][-1]
    assert newest.idx > first.idx and newest.key != first.key
    assert newest.rating == first.rating + 100 and newest.since == 1010
    assert after.board.squares[first.cell] == (newest.key, newest.rating, 1)  # still held by team 1
    # Checking again does not stack a second replacement on the same cell.
    again = await svc.check(view.mashup.id, 1)
    assert len([c for c in again.cells if c.cell == first.cell]) == 2
    # A solve before the claim does not count for the new problem; a later one takes the square.
    sub(cf, "h2", newest.key, 1005)
    clock.t = 1100
    assert (await svc.check(view.mashup.id, 2)).board.squares[first.cell][2] == 1
    sub(cf, "h2", newest.key, 1060)
    third = await svc.check(view.mashup.id, 2)
    assert third.board.squares[first.cell][2] == 2
    assert len([c for c in third.cells if c.cell == first.cell]) == 3
    ranks = third.standings
    assert [(r.handle, r.solved) for r in ranks] == [("h2", 1), ("h1", 0)]


async def test_replace_never_picks_a_problem_someone_in_the_lobby_solved(env):
    svc, store, cf, clock = env
    solved_1100 = [p for p in BINGO_PROBLEMS if p.rating == 1100]
    for p in solved_1100:  # h1 solved every problem rated 1100
        sub(cf, "h1", p.key, 10)
    view = await bingo(svc, 2, mode="replace", min_rating=800, max_rating=1200)
    assert await store.mashup_excluded(view.mashup.id) >= {p.key for p in solved_1100}
    assert 1100 not in {c.rating for c in view.cells}
    top = next(c for c in view.cells if c.rating == 1000)
    sub(cf, "h2", top.key, 1010)
    clock.t = 1020
    after = await svc.check(view.mashup.id, 2)
    newest = [c for c in after.cells if c.cell == top.cell][-1]
    assert newest.rating == 1200  # 1100 is all solved, so the next rating up


async def test_replace_ends_at_the_first_line_and_continuous_runs_on(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace")
    cells_by = {c.cell: c.key for c in view.cells}
    for i, at in zip((0, 1, 2), (1010, 1020, 1030), strict=True):
        sub(cf, "h1", cells_by[i], at)
    clock.t = 1040
    await svc.check(view.mashup.id, 1)  # stores the solves and adds the replacements
    await svc.tick()
    done = await svc.view(view.mashup.id)
    assert done.mashup.status == "finished"
    assert [(r.handle, r.rank, r.solved, r.lines) for r in done.standings] == [
        ("h1", 1, 3, 1),
        ("h2", 2, 0, 0),
    ]


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


async def test_replace_never_goes_above_max_rating(env):
    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace", min_rating=800, max_rating=1300)
    top = next(c for c in view.cells if c.rating == 1300)
    sub(cf, "h1", top.key, 1010)
    clock.t = 1020
    after = await svc.check(view.mashup.id, 1)
    newest = [c for c in after.cells if c.cell == top.cell][-1]
    assert newest.rating == 1300 and newest.key != top.key  # the cap: the same rating again


async def test_solve_log_names_the_team_and_the_player(env):
    svc, store, cf, clock = env
    lobby = await svc.create(
        guild_id=GUILD, channel_id=5, host_id=1, min_rating=800, max_rating=1700, count=0,
        duration_seconds=7200, mode="classic", size=3, teams=2, team_size=2,
    )  # fmt: skip
    await svc.join(lobby.mashup.id, 2, 1)
    await svc.join(lobby.mashup.id, 3, 2)
    view = await svc.start(lobby.mashup.id, 1)
    cells_by = {c.cell: c for c in view.cells}
    sub(cf, "h2", cells_by[0].key, 1010)  # team 1, the second member
    sub(cf, "h3", cells_by[1].key, 1020)  # team 2
    sub(cf, "h1", cells_by[1].key, 1030)  # team 1, too late for the square
    clock.t = 1040
    for uid in (1, 2, 3):
        view = await svc.check(lobby.mashup.id, uid)
    assert view.log == [
        (1010, 1, cells_by[0].key, cells_by[0].rating, 2, 0),
        (1020, 2, cells_by[1].key, cells_by[1].rating, 3, 0),
    ]
    field = next(f for f in mashup_embed(view, {}).fields if f.name == "Solve log")
    lines = field.value.splitlines()
    assert lines[0].endswith("- Team 2 (<@3>)") and lines[1].endswith("- Team 1 (<@2>)")


def test_a_full_size_bingo_embed_fits_discords_limit():
    from src.db.store import MashupCell, MashupPlayer, MashupRow

    m = MashupRow(
        1, 1, 5, 9, 1000, 800, 1500, 49, 7200, 0, "active", 0, 0, 7200, teams=10, max_players=50,
        team_size=5, mode="replace", size=7, tags="implementation,math", exclude_tags="geometry",
    )  # fmt: skip
    cells = [MashupCell(i, f"{1000 + i}B2", 3500, i, 0) for i in range(49)]
    players = [MashupPlayer(1000 + i, f"handle{i}_long_name", i, i % 10 + 1) for i in range(50)]
    solves = {1000 + i: {cells[i].key: (100 + i, 0)} for i in range(49)}
    view = MashupView(m, [], players, solves, None, cells)
    by_key = {c.key: Problem(1000 + i, "B2", "x" * 40, 3500, ()) for i, c in enumerate(cells)}
    assert len(mashup_embed(view, by_key)) <= 6000
    assert len(mashup_embed(view, by_key).description) <= 4096


async def test_claims_are_logged_to_the_thread_once_and_only_the_newest_shows_the_board(env):
    from types import SimpleNamespace

    import discord

    from src.cogs.mashup import MashupCog

    svc, store, cf, clock = env
    view = await bingo(svc, 2, mode="replace")
    mid = view.mashup.id
    assert await svc.claims_to_log(await svc.view(mid)) == []  # no thread yet
    await svc.set_thread(mid, 777)
    first, second = [c for c in view.cells if c.rating < 1700][:2]
    sub(cf, "h2", first.key, 1010)
    clock.t = 1020
    after = await svc.check(mid, 2)
    assert after.mashup.thread_id == 777

    sent, edits = [], []

    class Thread:
        async def send(self, text, embed=discord.utils.MISSING, **kw):
            sent.append((text, embed))
            return SimpleNamespace(id=100 + len(sent))

        def get_partial_message(self, message_id):
            async def edit(**kw):
                edits.append((message_id, kw))

            return SimpleNamespace(edit=edit)

    bot = SimpleNamespace(
        get_partial_messageable=lambda *a, **kw: Thread(),
        services=SimpleNamespace(
            mashups=svc, db=store, problems=SimpleNamespace(problems=BINGO_PROBLEMS)
        ),
    )
    cog = MashupCog(bot)
    await cog._log(after)
    await cog._log(await svc.view(mid))  # nothing new the second time
    assert len(sent) == 1 and edits == []
    text, embed = sent[0]
    assert text.startswith("<t:1010:T> (+0m) <@2> claimed [")
    assert f"({first.rating})" in text and "New problem there:" in text
    assert f"({first.rating + 100})" in text
    assert "[2]" in embed.description  # the board shows team 2 holding the square
    assert (await store.get_mashup(mid)).log_board_id == 101

    sub(cf, "h1", second.key, 1030)
    clock.t = 1040
    await svc.check(mid, 1)
    await cog._log(await svc.view(mid))
    assert len(sent) == 2 and sent[1][1].description.startswith("```")
    assert edits == [(101, {"embeds": []})]  # the older message loses its board
    assert (await store.get_mashup(mid)).log_board_id == 102


async def test_a_finished_bingo_posts_its_closing_message_once(env):
    from types import SimpleNamespace

    import discord

    from src.cogs.mashup import MashupCog

    svc, store, cf, clock = env
    view = await bingo(svc, 2)
    mid = view.mashup.id
    await svc.set_thread(mid, 777)
    keys = [c.key for c in sorted(view.cells, key=lambda c: c.cell)]
    for i, at in zip((0, 1, 2), (1010, 1020, 1030), strict=True):
        sub(cf, "h1", keys[i], at)
    clock.t = 1040
    await svc.tick()
    done = await svc.view(mid)
    assert done.mashup.status == "finished"

    sent, edits = [], []

    class Thread:
        async def send(self, text, embed=discord.utils.MISSING, **kw):
            sent.append((text, embed))
            return SimpleNamespace(id=100 + len(sent))

        def get_partial_message(self, message_id):
            async def edit(**kw):
                edits.append(message_id)

            return SimpleNamespace(edit=edit)

    bot = SimpleNamespace(
        get_partial_messageable=lambda *a, **kw: Thread(),
        services=SimpleNamespace(
            mashups=svc, db=store, problems=SimpleNamespace(problems=BINGO_PROBLEMS)
        ),
    )
    cog = MashupCog(bot)
    await cog._log(done)
    await cog._log(await svc.view(mid))  # nothing twice
    assert len(sent) == 4  # three claims, then the closing message
    assert sent[2][1] is discord.utils.MISSING  # the claims before it carry no board
    text, embed = sent[3]
    assert text == "The bingo is over. Winner: <@1>. Ratings changed."
    assert "1. [1]" in embed.description and "rating +" in embed.description
    assert edits == [] and (await store.get_mashup(mid)).log_board_id == 104
