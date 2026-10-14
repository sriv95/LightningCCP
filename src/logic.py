import math
from collections.abc import Iterable, Sequence

from src.cf.models import Contest, Problem, Submission
from src.errors import BotError

LINK_WINDOW_SECONDS = 300
LINK_MAX_RATING = 1200
RUNNING_PHASES = frozenset({"CODING", "PENDING_SYSTEM_TEST", "SYSTEM_TEST"})
MAX_REMINDER_MINUTES = 10080
MAX_REMINDER_COUNT = 5
CHOICE_MAX_LEN = 100
LOBBY_TTL_SECONDS = 600
MATCH_TTL_SECONDS = 7200
MATCH_MAX_PLAYERS = 10


def parse_tags(raw: str | None) -> frozenset[str]:
    if not raw:
        return frozenset()
    return frozenset(t.strip().lower() for t in raw.split(",") if t.strip())


def tag_suggestions(current: str, all_tags: Iterable[str], limit: int = 25) -> list[str]:
    head, sep, last = current.rpartition(",")
    chosen = parse_tags(head)
    prefix = f"{head.strip()}," if sep else ""
    needle = last.strip().lower()
    out = []
    for tag in sorted(all_tags):
        if tag in chosen or needle not in tag:
            continue
        value = prefix + tag
        if len(value) <= CHOICE_MAX_LEN:
            out.append(value)
        if len(out) == limit:
            break
    return out


def filter_problems(
    problems: Iterable[Problem],
    *,
    min_rating: int | None = None,
    max_rating: int | None = None,
    tags: frozenset[str] = frozenset(),
    exclude_tags: frozenset[str] = frozenset(),
    solved: frozenset[str] = frozenset(),
) -> list[Problem]:
    out = []
    for p in problems:
        if p.rating is None:  # unrated problems are not useful for recommendations
            continue
        if min_rating is not None and p.rating < min_rating:
            continue
        if max_rating is not None and p.rating > max_rating:
            continue
        ptags = set(p.tags)
        if not tags <= ptags or ptags & exclude_tags:
            continue
        if p.key in solved:
            continue
        out.append(p)
    return out


def solved_keys(subs: Iterable[Submission]) -> frozenset[str]:
    return frozenset(s.problem_key for s in subs if s.verdict == "OK")


def round_rating(rating: int | None) -> int:
    if rating is None:
        return 800
    return max(800, min(3500, (rating + 50) // 100 * 100))


def problem_points(rating: int) -> int:
    return max(1, (rating - 500) // 100)


def earliest_solve(subs: Iterable[Submission], problem_key: str, since: int) -> int | None:
    times = [
        s.created
        for s in subs
        if s.problem_key == problem_key and s.verdict == "OK" and s.created >= since
    ]
    return min(times) if times else None


def match_rating(rating_arg: int | None, ratings: Sequence[int | None]) -> int:
    if rating_arg is not None:
        return rating_arg
    known = [800 if r is None else r for r in ratings]
    return round_rating(sum(known) // len(known))


def rank_groups(entries: Sequence[tuple[int, int | None]]) -> list[list[int]]:
    """Order players by solve time; players who never solved form one tied group at the end."""
    solvers = sorted((t, i, uid) for i, (uid, t) in enumerate(entries) if t is not None)
    groups = [[uid] for _, _, uid in solvers]
    rest = [uid for uid, t in entries if t is None]
    if rest:
        groups.append(rest)
    return groups


def _round_away(x: float) -> int:
    return int(math.copysign(math.floor(abs(x) + 0.5), x))


def placement_deltas(groups: Sequence[Sequence[int]], points: int) -> dict[int, int]:
    """Top half gains points (1st gets all), bottom half loses (last loses all), middle gets 0.

    Players in the same group share the average of the positions they occupy.
    """
    n = sum(len(g) for g in groups)
    if n < 2:
        raise ValueError("a placement needs at least 2 players")
    h = n // 2

    def at(k: int) -> float:  # k is the 1-based position
        if k <= h:
            return points * (h - k + 1) / h
        if k > n - h:
            return -points * (k - (n - h)) / h
        return 0.0

    out: dict[int, int] = {}
    pos = 1
    for group in groups:
        avg = sum(at(k) for k in range(pos, pos + len(group))) / len(group)
        for uid in group:
            out[uid] = _round_away(avg)
        pos += len(group)
    return out


def has_compile_error(
    subs: Iterable[Submission],
    problem_key: str,
    issued_at: int,
    window: int = LINK_WINDOW_SECONDS,
) -> bool:
    return any(
        s.problem_key == problem_key
        and s.verdict == "COMPILATION_ERROR"
        and issued_at <= s.created <= issued_at + window
        for s in subs
    )


def select_contests(contests: Iterable[Contest], kind: str, limit: int = 50) -> list[Contest]:
    if kind == "upcoming":
        picked = [c for c in contests if c.phase == "BEFORE" and c.start_time is not None]
        return sorted(picked, key=lambda c: c.start_time)
    if kind == "running":
        picked = [c for c in contests if c.phase in RUNNING_PHASES]
        return sorted(picked, key=lambda c: c.start_time or 0)
    if kind == "recent":
        picked = [c for c in contests if c.phase == "FINISHED" and c.start_time is not None]
        return sorted(picked, key=lambda c: c.start_time, reverse=True)[:limit]
    raise ValueError(f"unknown contest kind: {kind}")


def refresh_minutes(contests: Iterable[Contest]) -> int:
    return 5 if any(c.phase in RUNNING_PHASES for c in contests) else 30


def parse_minutes(raw: str) -> tuple[int, ...]:
    usage = "Use up to 5 comma-separated whole minutes between 1 and 10080, e.g. 10,60."
    try:
        values = {int(part) for part in raw.split(",")}
    except ValueError:
        raise BotError(usage) from None
    if not values or len(values) > MAX_REMINDER_COUNT:
        raise BotError(usage)
    if any(v < 1 or v > MAX_REMINDER_MINUTES for v in values):
        raise BotError(usage)
    return tuple(sorted(values, reverse=True))


def reminders_due(
    contests: Iterable[Contest],
    before_mins: Iterable[int],
    sent: set[tuple[int, int]],
    now: int,
) -> list[tuple[Contest, int]]:
    mins = tuple(before_mins)
    due = []
    for c in contests:
        if c.phase != "BEFORE" or c.start_time is None or now >= c.start_time:
            continue
        for b in mins:
            if now >= c.start_time - b * 60 and (c.id, b) not in sent:
                due.append((c, b))
    return due


def chunked[T](seq: Sequence[T], size: int) -> list[list[T]]:
    return [list(seq[i : i + size]) for i in range(0, len(seq), size)]
