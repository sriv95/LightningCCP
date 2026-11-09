"""Python side of rust/benches/hot.rs: the same functions, same inputs, ns per op.

Run from the repo root: .venv/bin/python -I rust/bench/py_hot.py
timeit autorange, best of 5 repeats. Keep the input builders in step with benches/hot.rs.
"""

import asyncio
import random
import shutil
import sys
import tempfile
import timeit
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))  # -I drops the script and cwd dirs

from src.cf.models import Contest, Problem  # noqa: E402
from src.cogs.contests import _check_regex, _not_excluded  # noqa: E402
from src.db.store import Store  # noqa: E402
from src.logic import (  # noqa: E402
    bingo_board,
    contest_ics,
    filter_problems,
    match_deltas,
    mashup_standings,
)
from src.ui.embeds import contest_pages, esc, ranklist_pages  # noqa: E402

TAGS = ["dp", "math", "greedy", "graphs", "geometry", "strings", "trees", "sortings"]


def problems(n):
    return [
        Problem(
            1 + i // 6,
            "ABCDEF"[i % 6],
            f"Problem {i}",
            None if i % 13 == 0 else 800 + ((i * 7 + i // 28) % 28) * 100,
            tuple(TAGS[(i * (k + 1) + k) % 8] for k in range(1 + i % 3)),
        )
        for i in range(n)
    ]


def contests(n):
    def name(i):
        return (
            f"Codeforces Round #{i} (Div. {1 + i % 2})",
            f"Educational Codeforces Round {i}",
            f"Kotlin Heroes: Episode {i}",
            f"CodeTON Round {i}, Div. 1 + Div. 2; rated",
        )[i % 4]

    return [Contest(1000 + i, name(i), "CF", "BEFORE", 1_700_000_000 + i * 86400, 7200) for i in range(n)]


def solves(players, n_problems):
    return {
        u: {
            f"{1000 + p}A": (1_000_000 + u * 37 + p * 600, (u + p) % 3)
            for p in range(n_problems)
            if (u + p) % 3 != 0
        }
        for u in range(1, players + 1)
    }


typical = "Codeforces Round #900 (Div. 2) *bold* _under_ [x]"
long = "Привет мир_ *звезда* `код` ~tilde~ https://example.com/a_b_c ‖ 日本語 " * 50
lst = contests(200)
ics = Contest(lst[3].id, "CodeTON Round 7, Div. 1 + Div. 2; rated", "CF", "BEFORE", lst[3].start_time, 7200)
keep = {1000, 1001}
text = "Straße ΣΊΣΥΦΟΣ ǅ Hello World ß İstanbul"
pset = problems(10_000)
solved = frozenset(p.key for p in pset[::20])
kw = dict(min_rating=1200, max_rating=1600, tags=frozenset({"dp"}), exclude_tags=frozenset({"geometry"}), solved=solved)
rng = random.Random(7)
entries = [(i + 1, 100 + i, f"user_{i}*", 1000.0 - i * 7.5, i % 4 * 10) for i in range(50)]
players = [(u, f"player{u}", 1_000_000 + u) for u in range(1, 31)]
mash = solves(30, 8)
cells = [(f"{1000 + i}A", 800 + i * 100, i) for i in range(25)]
bingo = solves(6, 25)
groups = [[1], [2, 3], [4], [5, 6], [7], [8]]
ratings = {u: 800 + u * 150 for u in range(1, 9)}

tmp = Path(tempfile.mkdtemp(prefix="lccp-pybench-"))
loop = asyncio.new_event_loop()
store = Store(tmp / "bench.db")
loop.run_until_complete(store.connect())


async def store_op():
    await store.link_handle(1, 42, "tourist", 3800)
    return await store.get_handle(1, 42)


CASES = [
    ("esc typical", lambda: esc(typical)),
    ("esc long unicode", lambda: esc(long)),
    ("contest_ics", lambda: contest_ics(ics, 1_699_000_000, 10)),
    (
        "check_regex + not_excluded (200 contests)",
        lambda: _not_excluded(lst, _check_regex(r"  div\. ?[12]|kotlin|^educational  "), keep),
    ),
    ("casefold", lambda: text.casefold()),
    ("filter_problems (10k)", lambda: filter_problems(pset, **kw)),
    ("gimme: filter + pick (10k)", lambda: rng.choice(filter_problems(pset, **kw))),
    ("contest_pages (50 contests)", lambda: contest_pages(lst[:50], "Upcoming", 10)),
    ("ranklist_pages (50 entries)", lambda: ranklist_pages(entries, "Ranklist", 10)),
    ("mashup_standings (30 players, 8 problems)", lambda: mashup_standings(players, mash, 1_000_000, 5)),
    ("bingo_board (5x5, 6 teams)", lambda: bingo_board(5, "classic", "line", cells, bingo)),
    ("match_deltas (8 players)", lambda: match_deltas(groups, ratings, 1.5)),
    ("store link_handle + get_handle", lambda: loop.run_until_complete(store_op())),
]

for label, fn in CASES:
    t = timeit.Timer(fn)
    n, _ = t.autorange()
    best = min(t.repeat(repeat=5, number=n)) / n
    print(f"{label}\t{best * 1e9:.0f}")

loop.run_until_complete(store.close())
loop.close()
shutil.rmtree(tmp, ignore_errors=True)
