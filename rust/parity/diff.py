"""Compare two runner outputs: diff.py <py.jsonl> <rust.jsonl> <corpus dir or files...>.
Prints "N of M identical", then differences grouped by fn with one example each. Compares the parsed JSON
canonically (int 1 and float 1.0 differ). No normalization is applied; regex detail text after
"Invalid regex: " is shown separately as contract-allowed (PARITY.md, Acceptable differences, Regex item)."""

import json
import sys
from collections import defaultdict
from pathlib import Path

PREFIX = "Invalid regex: "


def load(path):
    with open(path, encoding="utf-8") as f:
        return {r["id"]: r for r in map(json.loads, filter(str.strip, f))}


def canon(row):
    return json.dumps({k: v for k, v in row.items() if k != "id"}, sort_keys=True, ensure_ascii=True)


def allowed_regex(a, b):
    ta, tb = a.get("err", {}), b.get("err", {})
    return ta.get("kind") == tb.get("kind") == "bot" and ta["text"].startswith(PREFIX) and tb["text"].startswith(PREFIX)


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
    for cid, case in fns.items():
        a, b = py.get(cid), rs.get(cid)
        if a is not None and b is not None and canon(a) == canon(b):
            same += 1
        elif a is not None and b is not None and allowed_regex(a, b):
            allowed[case["fn"]].append((case, a, b))
        else:
            bad[case["fn"]].append((case, a, b))
    total = len(fns)
    print(f"{same} of {total} identical")
    for title, groups in (("contract-allowed (regex detail text)", allowed), ("DIFFERENT", bad)):
        n = sum(map(len, groups.values()))
        if not n:
            continue
        print(f"\n{title}: {n}")
        for fn, items in sorted(groups.items()):
            case, a, b = items[0]
            print(f"- {fn}: {len(items)} diff, e.g. {case['id']} args={json.dumps(case['args'], ensure_ascii=False)[:300]}")
            print(f"    python: {json.dumps(a, ensure_ascii=False)[:300]}")
            print(f"    rust:   {json.dumps(b, ensure_ascii=False)[:300]}")
    sys.exit(1 if bad else 0)


main()
