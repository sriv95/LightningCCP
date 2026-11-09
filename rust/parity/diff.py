"""Compare two runner outputs: diff.py <py.jsonl> <rust.jsonl> <corpus dir or files...>.
Prints "N of M identical", then differences grouped by fn with one example each. Compares the parsed JSON
canonically (int 1 and float 1.0 differ). No normalization is applied; regex detail text after
"Invalid regex: " is shown separately as contract-allowed (PARITY.md, Acceptable differences, Regex item).
Other contract-allowed categories (CATEGORIES below) are listed apart from the real differences, each citing its
PARITY.md line; everything else is DIFFERENT and makes the exit code 1."""

import json
import sys
from collections import defaultdict
from datetime import datetime
from pathlib import Path

PREFIX = "Invalid regex: "


def load(path):
    with open(path, encoding="utf-8") as f:
        return {r["id"]: r for r in map(json.loads, filter(str.strip, f))}


def canon(row):
    return json.dumps(
        {k: v for k, v in row.items() if k != "id"}, sort_keys=True, ensure_ascii=True
    )


def allowed_regex(a, b):
    ta, tb = a.get("err", {}), b.get("err", {})
    return (
        ta.get("kind") == tb.get("kind") == "bot"
        and ta["text"].startswith(PREFIX)
        and tb["text"].startswith(PREFIX)
    )


def is_err(r, kind=None):
    return "err" in r and (kind is None or r["err"]["kind"] == kind)


def malformed_cf(case, a, b):
    # Python read a wrong-typed or malformed Codeforces payload, Rust rejected it with the generic error. This includes a
    # missing field in a standings row no wanted handle matches (Python filters first and never reads it, Rust parses every row).
    return case["fn"].startswith("cf.") and "out" in a and is_err(b, "other")


def regex_text(case):
    return json.dumps(case["args"], ensure_ascii=False)


def regex_engine(case, a, b):
    if case["fn"] not in ("check_regex", "not_excluded"):
        return False
    pat = case["args"][0] if case["fn"] == "check_regex" else case["args"][1]
    names = [] if case["fn"] == "check_regex" else [c["name"] for c in case["args"][0]]
    if "\\N{" in pat:  # name table
        return True
    if any(ch in regex_text(case) for ch in "İı"):  # case-insensitive folding of dotted/dotless i
        return True
    if "\\B" in pat and "" in names:  # \B on an empty string
        return True
    return (
        case["fn"] == "check_regex"
        and "out" in a
        and is_err(b, "bot")
        and "error building NFA" in b["err"]["text"]
    )  # compile-size limit


def same_instant(case, a, b):
    # Only the timestamp text form differs (`+00:00` vs `.000Z`); the rest of the embed and the instant are equal.
    if case["fn"] != "embed.timestamp" or "out" not in a or "out" not in b:
        return False
    oa, ob = dict(a["out"]), dict(b["out"])
    ta, tb = oa.pop("timestamp", None), ob.pop("timestamp", None)
    if oa != ob or not (isinstance(ta, str) and isinstance(tb, str)):
        return False
    try:
        return datetime.fromisoformat(ta) == datetime.fromisoformat(tb)
    except ValueError:
        return False


def team_none_player(case, a, b):
    # Python rendered a mashup view that has a player with team None, Rust rejects it with the generic error.
    v = case["args"][0] if case["args"] else None
    players = v.get("players", []) if isinstance(v, dict) else []
    return (
        case["fn"].startswith("mashup.")
        and any(p.get("team") is None for p in players)
        and "out" in a
        and is_err(b, "other")
    )


CATEGORIES = [
    ("regex detail text after the prefix", "Acceptable differences, Regex item", None),
    (
        "malformed Codeforces payload rejected by Rust",
        "Acceptable differences, 2026-11-10 (a)",
        malformed_cf,
    ),
    (
        "regex-engine: \\N{name}, i/dotless-i folding, \\B on empty, compile-size limit",
        "Acceptable differences, 2026-11-10 (b)",
        regex_engine,
    ),
    (
        "embed timestamp text form (+00:00 vs .000Z, same instant)",
        "Acceptable differences, 2026-11-10 (c)",
        same_instant,
    ),
    (
        "mashup player with team None: error in Rust",
        "Acceptable differences, 2026-11-10 (d)",
        team_none_player,
    ),
]


def classify(case, a, b):
    if allowed_regex(a, b):
        return CATEGORIES[0][0]
    for name, _, pred in CATEGORIES[1:]:
        if pred(case, a, b):
            return name
    return None


def main():
    py, rs = load(sys.argv[1]), load(sys.argv[2])
    fns = {}
    for p in sys.argv[3:]:
        for f in sorted(Path(p).glob("*.jsonl")) if Path(p).is_dir() else [Path(p)]:
            for line in f.read_text(encoding="utf-8").splitlines():
                if line.strip():
                    c = json.loads(line)
                    fns[c["id"]] = c
    same, allowed, bad = 0, defaultdict(list), defaultdict(list)
    by_domain = defaultdict(lambda: [0, 0])  # domain -> [identical, total]
    for cid, case in fns.items():
        a, b = py.get(cid), rs.get(cid)
        dom = cid.rsplit("-", 1)[0]
        by_domain[dom][1] += 1
        if a is not None and b is not None and canon(a) == canon(b):
            same += 1
            by_domain[dom][0] += 1
        elif a is not None and b is not None and (cat := classify(case, a, b)):
            allowed[cat].append((case, a, b))
        else:
            bad[case["fn"]].append((case, a, b))
    total = len(fns)
    print(f"{same} of {total} identical")
    for dom, (n, t) in sorted(by_domain.items()):
        print(f"  {dom}: {n} of {t} identical")
    refs = {name: ref for name, ref, _ in CATEGORIES}
    for cat, items in allowed.items():
        print(f"\ncontract-allowed, {cat}: {len(items)} (PARITY.md {refs[cat]})")
        case, a, b = items[0]
        print(f"  e.g. {case['id']} args={json.dumps(case['args'], ensure_ascii=False)[:200]}")
    for title, groups in (("DIFFERENT", bad),):
        n = sum(map(len, groups.values()))
        if not n:
            continue
        print(f"\n{title}: {n}")
        for fn, items in sorted(groups.items()):
            case, a, b = items[0]
            print(
                f"- {fn}: {len(items)} diff, e.g. {case['id']} args={json.dumps(case['args'], ensure_ascii=False)[:300]}"
            )
            print(f"    python: {json.dumps(a, ensure_ascii=False)[:300]}")
            print(f"    rust:   {json.dumps(b, ensure_ascii=False)[:300]}")
    sys.exit(1 if bad else 0)


main()
