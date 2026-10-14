import math
from collections.abc import Iterable, Mapping, Sequence

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
START_RATING = 800
ELO_K = 32


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


def unsolved_keys(subs: Iterable[Submission]) -> list[str]:
    """Problems attempted but never solved, in order of first appearance (newest attempt first)."""
    solved = solved_keys(subs)
    return list(dict.fromkeys(s.problem_key for s in subs if s.problem_key not in solved))


def round_rating(rating: int | None) -> int:
    if rating is None:
        return 800
    return max(800, min(3500, (rating + 50) // 100 * 100))


def earliest_solve(subs: Iterable[Submission], problem_key: str, since: int) -> int | None:
    times = [
        s.created
        for s in subs
        if s.problem_key == problem_key and s.verdict == "OK" and s.created >= since
    ]
    return min(times) if times else None


def match_rating(rating_arg: int | None, ratings: Sequence[int]) -> int:
    """Problem rating for a match: the host's choice, else the players' average match rating."""
    if rating_arg is not None:
        return rating_arg
    return round_rating(sum(ratings) // len(ratings))


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


def _expected(rating: float, opponent: float) -> float:
    return 1 / (1 + 10 ** ((opponent - rating) / 400))


def solo_delta(rating: int, problem_rating: int, solved: bool) -> int:
    """Elo against the problem. Failing a solo match never costs rating."""
    return _round_away(ELO_K * (1 - _expected(rating, problem_rating))) if solved else 0


def elo_deltas(groups: Sequence[Sequence[int]], ratings: Mapping[int, int]) -> dict[int, int]:
    """Pairwise Elo: each player is scored against every other player, then averaged.

    groups is ordered best first; players in one group tie.
    """
    n = sum(len(g) for g in groups)
    if n < 2:
        raise ValueError("a rated match needs at least 2 players")
    place = {uid: i for i, g in enumerate(groups) for uid in g}
    out: dict[int, int] = {}
    for uid, mine in place.items():
        total = 0.0
        for other, theirs in place.items():
            if other != uid:
                result = 1.0 if mine < theirs else 0.5 if mine == theirs else 0.0
                total += result - _expected(ratings[uid], ratings[other])
        out[uid] = _round_away(ELO_K * total / (n - 1))
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
