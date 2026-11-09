"""Writes the fake Codeforces payloads of the --bench modes (`lightningccp --bench` reads them).

Usage: python3 bench/gen_fixtures.py [out_dir]   (stdlib only; default: bench/fixtures next to this file)
Deterministic: fixed seed, fixed BASE time. Each file is a full API envelope {"status":"OK","result":...}.
Contest times are relative to BASE (1_800_000_000); the bench modes run their clock from BASE too.
"""

import json
import random
import sys
from pathlib import Path

BASE = 1_800_000_000
N_USERS = 60
HANDLES = [f"bench_user_{i:03d}" for i in range(1, N_USERS + 1)]
TAGS = [
    "implementation",
    "math",
    "greedy",
    "dp",
    "data structures",
    "brute force",
    "constructive algorithms",
    "graphs",
    "sortings",
    "binary search",
    "dfs and similar",
    "trees",
    "strings",
    "number theory",
    "combinatorics",
    "geometry",
    "bitmasks",
    "two pointers",
    "dsu",
    "shortest paths",
    "probabilities",
    "divide and conquer",
    "hashing",
    "games",
    "flows",
    "interactive",
    "matrices",
    "fft",
    "graph matchings",
]
RANKS = [
    (0, "newbie"),
    (1200, "pupil"),
    (1400, "specialist"),
    (1600, "expert"),
    (1900, "candidate master"),
    (2100, "master"),
    (2300, "international master"),
    (2400, "grandmaster"),
]
VERDICTS = (
    ["OK"] * 6
    + ["WRONG_ANSWER"] * 3
    + ["TIME_LIMIT_EXCEEDED", "COMPILATION_ERROR", "RUNTIME_ERROR"]
)


def env(result):
    return {"status": "OK", "result": result}


def problemset(rng, n_contests=2000):
    problems = []
    for cid in range(1, n_contests + 1):
        for index in "ABCDEF"[: rng.choice((4, 5, 6, 6))]:
            r = 800 + 100 * min(27, int(rng.expovariate(0.2)) + "ABCDEF".index(index) * 3)
            p = {
                "contestId": cid,
                "index": index,
                "name": f"Problem {cid}{index} {rng.randrange(10**6):06d}",
                "type": "PROGRAMMING",
                "points": float(500 * (1 + "ABCDEF".index(index))),
                "tags": rng.sample(TAGS, rng.randrange(0, 5)),
            }
            if rng.random() < 0.95:  # a few unrated, like the real set
                p["rating"] = min(3500, r)
            problems.append(p)
    return env(
        {
            "problems": problems,
            "problemStatistics": [
                {
                    "contestId": p["contestId"],
                    "index": p["index"],
                    "solvedCount": rng.randrange(10, 50000),
                }
                for p in problems
            ],
        }
    )


def contests(rng):
    out = []
    for i in range(60):  # upcoming: one about every 40 minutes from BASE + 15 min
        out.append((2000 + i, "BEFORE", BASE + 900 + i * 2400))
    for i in range(3):
        out.append((1990 + i, "CODING", BASE - 1800 - i * 600))
    for i in range(300):  # finished, newest first by id
        out.append((1989 - i, "FINISHED", BASE - 7200 - i * 86400 // 2))
    names = [
        "Codeforces Round {n} (Div. 1)",
        "Codeforces Round {n} (Div. 2)",
        "Codeforces Round {n} (Div. 3)",
        "Educational Codeforces Round {n}",
        "Kotlin Heroes: Episode {n}",
        "Good Bye {n}",
        "Hello {n}",
        "CodeTON Round {n}",
        "Codeforces Global Round {n}",
        "Codeforces Round {n} (Div. 4)",
    ]
    result = []
    for cid, phase, start in out:
        c = {
            "id": cid,
            "name": rng.choice(names).format(n=cid % 400),
            "type": rng.choice(["CF", "CF", "ICPC"]),
            "phase": phase,
            "frozen": False,
            "durationSeconds": rng.choice((7200, 7200, 9000, 10800)),
            "startTimeSeconds": start,
            "relativeTimeSeconds": BASE - start,
        }
        result.append(c)
    return env(result)


def users(rng):
    result = []
    for i, h in enumerate(HANDLES):
        rating = 800 + (i * 37) % 1700
        rank = [name for lo, name in RANKS if rating >= lo][-1]
        u = {
            "handle": h,
            "rating": rating,
            "maxRating": rating + rng.randrange(0, 200),
            "rank": rank,
            "maxRank": rank,
            "contribution": 0,
            "friendOfCount": rng.randrange(0, 50),
        }
        if i % 9 == 8:  # an unrated member
            del u["rating"], u["maxRating"], u["rank"], u["maxRank"]
        result.append(u)
    return env(result)


def status(rng, problems, n=1500):
    rows = rng.sample(problems, n)
    out = []
    for i, p in enumerate(rows):  # newest first, all before BASE
        out.append(
            {
                "id": 300_000_000 - i,
                "contestId": p["contestId"],
                "creationTimeSeconds": BASE - 3600 - i * 4000,
                "relativeTimeSeconds": 2147483647,
                "problem": p,
                "verdict": rng.choice(VERDICTS),
                "programmingLanguage": "C++20 (GCC 13-64)",
                "passedTestCount": rng.randrange(0, 40),
                "timeConsumedMillis": rng.randrange(0, 2000),
                "memoryConsumedBytes": rng.randrange(0, 10**8),
            }
        )
    return env(out)


def standings(rng, n=2000):
    rows = []
    at = {
        rng.randrange(1, n): h for h in HANDLES
    }  # our members at random ranks (a few may collide: skipped)
    for rank in range(1, n + 1):
        h = at.get(rank) or f"someone_{rng.randrange(10**7):07d}"
        rows.append(
            {
                "party": {
                    "contestId": 1,
                    "members": [{"handle": h}],
                    "participantType": "CONTESTANT",
                    "ghost": False,
                    "startTimeSeconds": BASE - 86400,
                },
                "rank": rank,
                "points": float(max(0, 5000 - rank * 2)),
                "penalty": rng.randrange(0, 600),
                "successfulHackCount": 0,
                "unsuccessfulHackCount": 0,
                "problemResults": [{"points": 500.0, "rejectedAttemptCount": 0, "type": "FINAL"}]
                * 5,
            }
        )
    return env(
        {
            "contest": {"id": 1, "name": "Fixture", "type": "CF", "phase": "FINISHED"},
            "problems": [{"index": c} for c in "ABCDE"],
            "rows": rows,
        }
    )


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent / "fixtures"
    out.mkdir(parents=True, exist_ok=True)
    rng = random.Random(20260101)
    ps = problemset(rng)
    files = {
        "problemset.json": ps,
        "contest_list.json": contests(rng),
        "user_info.json": users(rng),
        "user_status.json": status(rng, ps["result"]["problems"]),
        "contest_standings.json": standings(rng),
    }
    for name, payload in files.items():
        (out / name).write_text(json.dumps(payload, separators=(",", ":")) + "\n")


if __name__ == "__main__":
    main()
