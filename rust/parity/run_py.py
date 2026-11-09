"""Parity harness runner (Python side), twin of rust/src/bin/parity.rs. Run from the repo root:
    .venv/bin/python -I rust/parity/run_py.py rust/parity/corpus/*.jsonl > out.jsonl
Same protocol: JSONL {"id","fn","args"} in; {"id","out"} or {"id","err":{"kind":"bot","text"}|{"kind":"other"}} out.
Add a function: one entry in FNS here and one arm in the Rust runner."""

import asyncio
import dataclasses
import json
import sys

import discord

sys.path.insert(0, ".")

from src import logic as lg  # noqa: E402
from src.cf.client import CodeforcesClient  # noqa: E402
from src.cf.models import Contest, Problem, Submission, User  # noqa: E402  # noqa: E402
from src.cogs.contests import _check_regex, _not_excluded  # noqa: E402
from src.daily import DailyView  # noqa: E402
from src.db.store import DailyRow, MashupCell, MashupPlayer, MashupRow, MatchPlayer, MatchRow  # noqa: E402
from src.errors import BotError  # noqa: E402
from src.logic import WeeklyRow  # noqa: E402
from src.mashup import LogEntry, MashupRecord, MashupView  # noqa: E402
from src.matches import MatchView  # noqa: E402
from src.ui import embeds as em  # noqa: E402
from src.ui.embeds import esc  # noqa: E402

I64 = (-(2**63), 2**63 - 1)


def contest(d):
    return Contest(d["id"], d["name"], d["type"], d["phase"], d["start_time"], d["duration"])


def cj(c):
    return {
        "id": c.id,
        "name": c.name,
        "type": c.type,
        "phase": c.phase,
        "start_time": c.start_time,
        "duration": c.duration,
    }


def contests(v):
    return [contest(c) for c in v]


def subs(v):
    return [Submission(x["id"], x["created"], x["problem_key"], x["verdict"]) for x in v]


def py_int(s):
    # Rust returns Option<i64> and saturates a magnitude past i64::MAX to it (so -2^63 gives -(2^63-1)); every call
    # site range-checks, so a saturated value is rejected like the huge Python int. Same adapter here.
    try:
        v = int(s)
    except ValueError:
        return None
    return max(-I64[1], min(I64[1], v))


def parse_digits(s):
    import re

    return py_int(s) if re.fullmatch(r"\d+", s) else None


def user(u):
    return {"handle": u.handle, "rating": u.rating, "max_rating": u.max_rating, "rank": u.rank}


class _Resp:
    def __init__(self, payload):
        self.payload = payload

    async def __aenter__(self):
        return self

    async def __aexit__(self, *exc):
        return False

    async def json(self, content_type=None):
        return self.payload


class _Session:
    def __init__(self, payload):
        self.payload = payload

    def get(self, url, params=None, timeout=None):
        return _Resp(self.payload)


async def _nosleep(_):
    pass


def cf(payload, method, *args):
    client = CodeforcesClient(
        _Session(payload), interval=0.0, retries=3, retry_delay=0.0, sleep=_nosleep
    )
    return asyncio.run(getattr(client, method)(*args))


def standing(r):
    # Rust holds points as f64; an int from JSON is the same number (formatted `:g`), so compare it as a float.
    pts = float(r.points) if type(r.points) is int else r.points
    return {"rank": r.rank, "handles": list(r.handles), "points": pts, "penalty": r.penalty}


def flt(v):
    return float(v)


def emb(e):
    """The full embed as the wire dict (to_dict drops an empty title, description and url, like the Rust EmbedData)."""
    d = e.to_dict()
    return {
        "title": d.get("title"),
        "description": d.get("description"),
        "url": d.get("url"),
        "colour": d.get("color"),
        "fields": [
            {"name": f["name"], "value": f["value"], "inline": f.get("inline", False)}
            for f in d.get("fields", [])
        ],
        "footer": d.get("footer", {}).get("text"),
        "author": d.get("author", {}).get("name"),
        "timestamp": d.get("timestamp"),
        "thumbnail": d.get("thumbnail", {}).get("url"),
    }


def embs(v):
    return [emb(e) for e in v]


def prob(d):
    return (
        None
        if d is None
        else Problem(d["contest_id"], d["index"], d["name"], d["rating"], tuple(d["tags"]))
    )


def daily_view(a):
    r = a[0]
    row = DailyRow(
        1, 1, r["day"], r["problem_key"], r["rating"], r["issued_at"], r["solved_at"], None
    )
    return DailyView(row, prob(a[1]), a[2], a[3])


class _Member:
    def __init__(self, name):
        self.display_name = name


EMBED_FNS = {
    "embed.problem": lambda a: emb(em.problem_embed(prob(a[0]), footer=a[1])),
    "embed.daily": lambda a: emb(em.daily_embed(daily_view(a))),
    "embed.profile": lambda a: emb(
        em.profile_embed(
            User(a[0]["handle"], a[0]["rating"], a[0]["max_rating"], a[0]["rank"]), _Member(a[1])
        )
    ),
    "embed.member_profile": lambda a: emb(
        em.member_profile_embed(
            _Member(a[0]),
            rating=a[1],
            rank=a[2],
            ranked=a[3],
            matches=a[4],
            contests=a[5],
            changes=a[6],
            peak=a[7],
            streak=a[8],
            codeforces=a[9],
            contest_wins=a[10],
        )
    ),
    "embed.contest_pages": lambda a: embs(em.contest_pages(contests(a[0]), a[1], a[2])),
    "embed.new_contest": lambda a: emb(em.new_contest_embed(contest(a[0]), a[1])),
    "embed.weekly": lambda a: emb(em.weekly_embed([WeeklyRow(*r) for r in a[0]])),
    "embed.stats_pages": lambda a: embs(
        em.stats_pages([tuple(r) for r in a[0]], a[1], {u: n for u, n in a[2]}, a[3], a[4])
    ),
    "embed.ranklist": lambda a: embs(
        em.ranklist_pages([(e[0], e[1], e[2], flt(e[3]), e[4]) for e in a[0]], a[1], a[2])
    ),
    "embed.handle_list": lambda a: embs(em.handle_list_pages([tuple(e) for e in a[0]], a[1])),
    "embed.recap": lambda a: emb(
        em.recap_embed(contest(a[0]), [(e[0], e[1], e[2], flt(e[3]), e[4]) for e in a[1]])
    ),
    "embed.stalk": lambda a: embs(em.stalk_pages(a[0], [(k, prob(p)) for k, p in a[1]], a[2])),
}


def match_record(r):
    return (MatchRow(**r[0]), [MatchPlayer(**p) for p in r[1]])


def by_key(v):
    ps = [prob(p) for p in v]
    return {p.key: p for p in ps}


def mashup_view(d):
    """`{mashup, problems, players, solves, note, cells}` as the Rust runner reads it; solves are [[user, [[key, [time, wrong]], ...]], ...]."""
    return MashupView(
        MashupRow(**d["mashup"]),
        [tuple(p) for p in d["problems"]],
        [MashupPlayer(**p) for p in d["players"]],
        {u: {k: tuple(v) for k, v in mine} for u, mine in d["solves"]},
        d["note"],
        [MashupCell(**c) for c in d["cells"]],
    )


def timestamp_embed(a):
    import datetime

    return emb(
        discord.Embed(title=a[0], timestamp=datetime.datetime.fromtimestamp(a[1], datetime.UTC))
    )


EMBED_FNS.update(
    {
        "embed.match": lambda a: emb(
            em.match_embed(
                MatchView(
                    MatchRow(**a[0]),
                    [MatchPlayer(**p) for p in a[1]],
                    prob(a[2]),
                    None if a[3] is None else {u: r for u, r in a[3]},
                    a[4],
                )
            )
        ),
        "embed.history_pages": lambda a: embs(
            em.history_pages(
                a[0],
                [match_record(r) for r in a[1]],
                a[2],
                by_key(a[3]),
                [
                    MashupRecord(
                        MashupRow(**r["mashup"]),
                        r["rank"],
                        r["entrants"],
                        r["solved"],
                        r["team"],
                        r["delta"],
                    )
                    for r in a[4]
                ],
            )
        ),
        "embed.head_to_head": lambda a: embs(
            em.head_to_head_pages(a[0], [match_record(r) for r in a[1]], a[2], a[3], by_key(a[4]))
        ),
        "embed.number_pages": lambda a: embs(
            em.number_pages([discord.Embed(title=t, description=d) for t, d in a[0]])
        ),
        "embed.timestamp": timestamp_embed,
        "mashup.start_message": lambda a: em.start_message(mashup_view(a[0])),
        "mashup.give_up_message": lambda a: em.give_up_message(mashup_view(a[0]), a[1]),
        "mashup.log_message": lambda a: em.log_message(
            mashup_view(a[0]), LogEntry(*a[1]), by_key(a[2])
        ),
        "mashup.log_board_embed": lambda a: emb(em.log_board_embed(mashup_view(a[0]))),
        "mashup.final_message": lambda a: em.final_message(mashup_view(a[0])),
        "mashup.embed": lambda a: emb(em.mashup_embed(mashup_view(a[0]), by_key(a[1]))),
    }
)


FNS = {
    "esc": lambda a: esc(a[0]),
    "fmt_g": lambda a: f"{flt(a[0]):g}",
    "casefold": lambda a: a[0].casefold(),
    "py_int": lambda a: py_int(a[0]),
    "parse_digits": lambda a: parse_digits(a[0]),
    "parse_tags": lambda a: sorted(lg.parse_tags(a[0])),
    "normalize_tags": lambda a: lg.normalize_tags(a[0]),
    "tag_suggestions": lambda a: lg.tag_suggestions(a[0], a[1], a[2]),
    "format_hm": lambda a: lg.format_hm(a[0]),
    "ordinal": lambda a: lg.ordinal(a[0]),
    "placement": lambda a: lg.placement(a[0], a[1]),
    "rating_series": lambda a: [list(p) for p in lg.rating_series(a[0])],
    "round_rating": lambda a: lg.round_rating(a[0]),
    "floor_rating": lambda a: lg.floor_rating(a[0]),
    "utc_day": lambda a: lg.utc_day(a[0]),
    "week_start": lambda a: lg.week_start(a[0]),
    "daily_streak": lambda a: lg.daily_streak(a[0], a[1]),
    "match_rating": lambda a: lg.match_rating(a[0], a[1]),
    "rank_groups": lambda a: lg.rank_groups([tuple(p) for p in a[0]]),
    "solo_delta": lambda a: lg.solo_delta(a[0], a[1], a[2]),
    "duel_gain": lambda a: lg.duel_gain(a[0]),
    "match_deltas": lambda a: [
        list(p) for p in lg.match_deltas(a[0], {u: r for u, r in a[1]}, flt(a[2])).items()
    ],
    "has_compile_error": lambda a: lg.has_compile_error(subs(a[0]), a[1], a[2], a[3]),
    "solved_keys": lambda a: sorted(lg.solved_keys(subs(a[0]))),
    "unsolved_keys": lambda a: lg.unsolved_keys(subs(a[0])),
    "weekly_board": lambda a: [list(r) for r in lg.weekly_board([tuple(e) for e in a[0]], a[1])],
    "select_contests": lambda a: [cj(c) for c in lg.select_contests(contests(a[0]), a[1], a[2])],
    "refresh_minutes": lambda a: lg.refresh_minutes(contests(a[0])),
    "parse_minutes": lambda a: list(lg.parse_minutes(a[0])),
    "reminders_due": lambda a: [
        [cj(c), b]
        for c, b in lg.reminders_due(contests(a[0]), a[1], {tuple(p) for p in a[2]}, a[3])
    ],
    "new_contests": lambda a: [cj(c) for c in lg.new_contests(contests(a[0]), set(a[1]), a[2])],
    "soon_contests": lambda a: [
        cj(c) for c in lg.soon_contests(contests(a[0]), set(a[1]), a[2], a[3])
    ],
    "recaps_due": lambda a: [cj(c) for c in lg.recaps_due(contests(a[0]), set(a[1]), a[2])],
    "contest_ics": lambda a: lg.contest_ics(contest(a[0]), a[1], a[2]),
    "check_regex": lambda a: _check_regex(a[0]),
    "not_excluded": lambda a: [c.id for c in _not_excluded(contests(a[0]), a[1], set(a[2]))],
    "cf.user_info": lambda a: user(cf(a[1], "user_info", a[0])),
    "cf.users_info": lambda a: [user(u) for u in cf(a[1], "users_info", a[0])],
    "cf.problemset": lambda a: [
        {
            "contest_id": p.contest_id,
            "index": p.index,
            "name": p.name,
            "rating": p.rating,
            "tags": list(p.tags),
        }
        for p in cf(a[0], "problemset")
    ],
    "cf.contests": lambda a: [cj(c) for c in cf(a[0], "contests")],
    "cf.user_status": lambda a: [
        {"id": s.id, "created": s.created, "problem_key": s.problem_key, "verdict": s.verdict}
        for s in cf(a[3], "user_status", a[0], a[1], a[2])
    ],
    "cf.standings": lambda a: [standing(r) for r in cf(a[2], "standings", a[0], a[1])],
    "cf.rating_history": lambda a: [
        {"time": r.time, "rating": r.rating} for r in cf(a[1], "rating_history", a[0])
    ],
}


def _store_norm(v):
    """Store results as plain JSON: dataclasses as dicts, sets sorted, dicts as [key, value] pairs, tuples as lists."""
    if isinstance(v, (set, frozenset)):
        return sorted(_store_norm(x) for x in v)
    if isinstance(v, dict):
        return [[k, _store_norm(x)] for k, x in v.items()]
    if isinstance(v, (list, tuple)):
        return [_store_norm(x) for x in v]
    if hasattr(v, "__dataclass_fields__"):
        return {k: _store_norm(x) for k, x in dataclasses.asdict(v).items()}
    return v


def _pairs(v):
    return {k: x for k, x in v}


# Args that Python takes as mappings travel as [key, value] pairs; methods with keyword-only parameters take one object.
STORE_ARGS = {
    "record_solves": lambda a: [a[0], _pairs(a[1])],
    "finish_match": lambda a: [a[0], a[1], _pairs(a[2]), a[3]],
    "record_mashup_solves": lambda a: [a[0], a[1], {k: tuple(x) for k, x in a[2]}],
    "finish_mashup": lambda a: [a[0], None if a[1] is None else _pairs(a[1]), a[2]],
    "start_mashup": lambda a: [a[0], [tuple(p) for p in a[1]], *a[2:]],
}


async def _store_call(st, op):
    args = STORE_ARGS.get(op[0], lambda a: a)(op[1:])
    if len(args) == 1 and isinstance(args[0], dict):
        return await getattr(st, op[0])(**args[0])
    return await getattr(st, op[0])(*args)


def _dump(path):
    import sqlite3

    db = sqlite3.connect(path)
    names = [
        r[0]
        for r in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]
    out = {
        t: [
            [("blob:" + x.hex()) if isinstance(x, bytes) else x for x in r]
            # mashup_excluded is filled from a set in both stores (unspecified order), so it is ordered by content.
            for r in db.execute(
                f'SELECT * FROM "{t}" ORDER BY '
                + ("mashup_id, problem_key" if t == "mashup_excluded" else "rowid")
            )
        ]
        for t in names
    }
    db.close()
    return out


def store_run(ops):
    """Same scenario protocol as store_run in parity.rs: results per op, then the dump of every table."""
    import sqlite3
    import tempfile
    from pathlib import Path

    from src.db.store import Store

    async def go(path):
        st = Store(path)
        await st.connect()
        results = []
        for op in ops:
            try:
                results.append({"ok": _store_norm(await _store_call(st, op))})
            except BotError as e:
                results.append({"err": "bot", "text": e.user_message})
            except sqlite3.IntegrityError:
                results.append({"err": "constraint"})
            except Exception:
                results.append({"err": "other"})
        await st.close()
        return results

    with tempfile.TemporaryDirectory() as d:
        path = Path(d) / "p.db"
        results = asyncio.run(go(path))
        return {"results": results, "dump": _dump(path)}


FNS["store.run"] = lambda a: store_run(a[0])
FNS.update(EMBED_FNS)


def run(case):
    row = {"id": case["id"]}
    try:
        row["out"] = FNS[case["fn"]](case["args"])
    except BotError as e:
        row["err"] = {"kind": "bot", "text": e.user_message}
    except Exception:
        row["err"] = {"kind": "other"}
    return row


def main():
    paths = sys.argv[1:]
    files = [open(p, encoding="utf-8") for p in paths] if paths else [sys.stdin]
    out = sys.stdout
    for f in files:
        for line in f:
            if line.strip():
                out.write(json.dumps(run(json.loads(line)), ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
