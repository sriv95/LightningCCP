import math
import random
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass

from src.cf.models import Contest, Problem, Submission
from src.errors import BotError

LINK_WINDOW_SECONDS = 300
LINK_MAX_RATING = 1200
RUNNING_PHASES = frozenset({"CODING", "PENDING_SYSTEM_TEST", "SYSTEM_TEST"})
MAX_REMINDER_MINUTES = 10080
MAX_REMINDER_COUNT = 5
RECAP_WINDOW_SECONDS = 86400  # only contests that ended this recently get a recap
CHOICE_MAX_LEN = 100
LOBBY_TTL_SECONDS = 600
MATCH_TTL_SECONDS = 7200
MATCH_MAX_PLAYERS = 10
MASHUP_MAX_PLAYERS = 20
MASHUP_MAX_PROBLEMS = 10
START_RATING = 800
SOLO_MIN = 1
SOLO_EQUAL = 10
SOLO_MAX = 33
SOLO_RANGE = 400
DAILY_MULTIPLIER = 2
DAY_SECONDS = 86400
STREAK_GAP = 2  # days between claims that keep a streak alive: one missed day is forgiven


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


def format_hm(seconds: int) -> str:
    return f"{seconds // 3600}:{seconds % 3600 // 60:02d}"


def ordinal(n: int) -> str:
    suffix = "th" if 10 <= n % 100 <= 20 else {1: "st", 2: "nd", 3: "rd"}.get(n % 10, "th")
    return f"{n}{suffix}"


def placement(deltas: Sequence[int], mine: int) -> int | None:
    """1-based place of a player among a match's rating changes (ties share a place); None when
    nobody's rating moved, which means nobody solved."""
    if not any(deltas):
        return None
    return 1 + sum(d > mine for d in deltas)


def rating_series(changes: Iterable[tuple[int, int]]) -> list[tuple[int, int]]:
    """(time, rating change) pairs, oldest first, to (time, rating after) from the start rating."""
    rating = START_RATING
    series = []
    for time, delta in changes:
        rating += delta
        series.append((time, rating))
    return series


def unsolved_keys(subs: Iterable[Submission]) -> list[str]:
    """Problems attempted but never solved, in order of first appearance (newest attempt first)."""
    solved = solved_keys(subs)
    return list(dict.fromkeys(s.problem_key for s in subs if s.problem_key not in solved))


def round_rating(rating: int | None) -> int:
    if rating is None:
        return 800
    return max(800, min(3500, (rating + 50) // 100 * 100))


def floor_rating(rating: int) -> int:
    return max(800, min(3500, rating // 100 * 100))


def utc_day(now: int) -> int:
    """Days since the epoch; the daily problem resets when this changes (midnight UTC)."""
    return now // DAY_SECONDS


def daily_streak(days: Sequence[int], today: int) -> int:
    """Claimed daily days (newest first) in a row, allowing one missed day between claims. Missing
    two days in a row ends the streak, so it is already over once the newest claim is 3+ days old."""
    if not days or today - days[0] > STREAK_GAP:
        return 0
    streak = 1
    for newer, older in zip(days, days[1:], strict=False):
        if newer - older > STREAK_GAP:
            break
        streak += 1
    return streak


def earliest_solve(subs: Iterable[Submission], problem_key: str, since: int) -> int | None:
    times = [
        s.created
        for s in subs
        if s.problem_key == problem_key and s.verdict == "OK" and s.created >= since
    ]
    return min(times) if times else None


def first_solve(
    subs: Iterable[Submission], problem_key: str, since: int, until: int
) -> tuple[int, int] | None:
    """(time of the earliest accepted submission, wrong attempts before it) within since..until.

    Compilation errors and submissions still being judged are not wrong attempts."""
    mine = [s for s in subs if s.problem_key == problem_key and since <= s.created <= until]
    solved_at = min((s.created for s in mine if s.verdict == "OK"), default=None)
    if solved_at is None:
        return None
    wrong = sum(
        1
        for s in mine
        if s.created < solved_at and s.verdict not in (None, "OK", "COMPILATION_ERROR")
    )
    return solved_at, wrong


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


def solo_delta(rating: int, problem_rating: int, solved: bool) -> int:
    """Linear in the gap to the problem, +1 at 400 or more below you, +10 at equal, +33 at 400
    or more above. Failing a solo match never costs rating."""
    if not solved:
        return 0
    gap = max(-SOLO_RANGE, min(SOLO_RANGE, problem_rating - rating))
    if gap < 0:
        return _round_away(SOLO_MIN + (SOLO_EQUAL - SOLO_MIN) * (gap + SOLO_RANGE) / SOLO_RANGE)
    return _round_away(SOLO_EQUAL + (SOLO_MAX - SOLO_EQUAL) * gap / SOLO_RANGE)


def duel_gain(gap: int) -> int:
    """Rating a winner gains, by how far the loser is above (positive gap) or below the winner."""
    if gap <= -400:
        return 16
    if gap <= -200:
        return 33
    if gap < 200:
        return 49
    return 67 if gap < 400 else 95


def match_deltas(groups: Sequence[Sequence[int]], ratings: Mapping[int, int]) -> dict[int, int]:
    """Each pair of players is a duel: the winner gains duel_gain, the loser loses the same, a tie
    moves nothing. A player's change is the average over their opponents.

    groups is ordered best first; players in one group tie.
    """
    n = sum(len(g) for g in groups)
    if n < 2:
        raise ValueError("a rated match needs at least 2 players")
    place = {uid: i for i, g in enumerate(groups) for uid in g}
    out: dict[int, int] = {}
    for uid, mine in place.items():
        total = 0
        for other, theirs in place.items():
            if mine < theirs:
                total += duel_gain(ratings[other] - ratings[uid])
            elif mine > theirs:
                total -= duel_gain(ratings[uid] - ratings[other])
        out[uid] = _round_away(total / (n - 1))
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


def recaps_due(contests: Iterable[Contest], sent: set[int], now: int) -> list[Contest]:
    """Finished contests that ended within the recap window and have no recap yet, oldest first."""
    due = [
        c
        for c in contests
        if c.phase == "FINISHED"
        and c.start_time is not None
        and c.id not in sent
        and 0 <= now - (c.start_time + c.duration) <= RECAP_WINDOW_SECONDS
    ]
    return sorted(due, key=lambda c: c.start_time)


def chunked[T](seq: Sequence[T], size: int) -> list[list[T]]:
    return [list(seq[i : i + size]) for i in range(0, len(seq), size)]


def pick_mashup(
    problems: Sequence[Problem],
    min_rating: int,
    max_rating: int,
    count: int,
    solved: frozenset[str] = frozenset(),
    rng: random.Random | None = None,
) -> list[Problem] | None:
    """`count` distinct unsolved problems, easy to hard, as close as possible to ratings spread
    evenly over min_rating..max_rating. None when the range cannot supply that many."""
    rng = rng or random.Random()
    if count == 1:
        targets = [(min_rating + max_rating) / 2]
    else:
        targets = [min_rating + (max_rating - min_rating) * i / (count - 1) for i in range(count)]
    chosen: list[Problem] = []
    taken = set(solved)
    for target in targets:
        pool = filter_problems(
            problems, min_rating=min_rating, max_rating=max_rating, solved=frozenset(taken)
        )
        if not pool:
            return None
        nearest = min(abs(p.rating - target) for p in pool)
        pick = rng.choice([p for p in pool if abs(p.rating - target) == nearest])
        chosen.append(pick)
        taken.add(pick.key)
    return chosen


@dataclass(frozen=True)
class MashupRank:
    rank: int
    user_id: int
    handle: str
    solved: int
    total_seconds: int  # solve times plus the penalty of every wrong attempt
    times: dict[str, int]  # problem key -> seconds after the start, for solved problems
    wrong: dict[str, int]  # problem key -> wrong attempts before the solve


def mashup_standings(
    players: Iterable[tuple[int, str, int]],
    solves: dict[int, dict[str, tuple[int, int]]],
    started_at: int,
    penalty_minutes: int = 0,
) -> list[MashupRank]:
    """Most problems solved first, then the lowest total time; equal scores share a rank.

    players are (user_id, handle, joined_at); solves maps user id to {problem key: (solve time,
    wrong attempts)}. Every wrong attempt before a solve adds penalty_minutes to the total."""
    rows = []
    for user_id, handle, joined_at in players:
        mine = solves.get(user_id, {})
        times = {k: max(0, t - started_at) for k, (t, _) in mine.items()}
        wrong = {k: w for k, (_, w) in mine.items()}
        total = sum(times.values()) + sum(wrong.values()) * penalty_minutes * 60
        rows.append((-len(times), total, joined_at, user_id, handle, times, wrong))
    rows.sort(key=lambda r: r[:3])
    ranked: list[MashupRank] = []
    for i, (neg, total, _, user_id, handle, times, wrong) in enumerate(rows):
        same = ranked and (ranked[-1].solved, ranked[-1].total_seconds) == (-neg, total)
        rank = ranked[-1].rank if same else i + 1
        ranked.append(MashupRank(rank, user_id, handle, -neg, total, times, wrong))
    return ranked
