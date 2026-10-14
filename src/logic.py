import math
import random
from collections.abc import Container, Iterable, Mapping, Sequence
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
CUSTOM_SCALE = 1.5  # a custom match pays the duel table this many times
MASHUP_MAX_PLAYERS = 50
# Codeforces reads per second for running contests; commands get the rest of the 1 per second
MASHUP_READS_PER_SECOND = 0.5
MASHUP_MIN_INTERVAL = 10  # seconds between rounds when few players wait for a read
MASHUP_MAX_INTERVAL = 60
MASHUP_MAX_PROBLEMS = 10
MASHUP_MAX_TEAMS = 10
MASHUP_MAX_TEAM_SIZE = 5
BINGO_MIN_SIZE = 3
BINGO_MAX_SIZE = 7
BINGO_REPLACE_STEP = 100  # a solved square of a replace bingo gets a problem rated this much higher
MAX_PROBLEM_RATING = 3500
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


def normalize_tags(raw: str | None) -> str:
    """Canonical stored form of a tag list: sorted, lower case, comma-separated."""
    return ",".join(sorted(parse_tags(raw)))


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


def contest_suggestions(
    contests: Iterable[Contest], known_ids: Container[int], current: str, limit: int = 25
) -> list[tuple[str, int]]:
    """(label, contest id) of the newest finished contests whose problems are known, whose id or
    name contains what the user typed."""
    needle = current.strip().lower()
    newest = sorted(
        (c for c in contests if c.phase == "FINISHED" and c.id in known_ids),
        key=lambda c: c.start_time or 0,
        reverse=True,
    )
    out = []
    for c in newest:
        label = f"{c.id} - {c.name}"
        if needle in label.lower():
            out.append((label[:CHOICE_MAX_LEN], c.id))
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
    contest_id: int | None = None,
) -> list[Problem]:
    out = []
    for p in problems:
        if p.rating is None:  # unrated problems are not useful for recommendations
            continue
        if contest_id is not None and p.contest_id != contest_id:
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


def match_deltas(
    groups: Sequence[Sequence[int]], ratings: Mapping[int, int], scale: float = 1.0
) -> dict[int, int]:
    """Each pair of players is a duel: the winner gains duel_gain, the loser loses the same, a tie
    moves nothing. A player's change is the average over their opponents, times scale.

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
        out[uid] = _round_away(total * scale / (n - 1))
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
    tags: frozenset[str] = frozenset(),
    exclude_tags: frozenset[str] = frozenset(),
) -> list[Problem] | None:
    """`count` distinct unsolved problems, easy to hard, as close as possible to ratings spread
    evenly over min_rating..max_rating, all with `tags` and none with `exclude_tags`. None when
    that cannot supply that many."""
    rng = rng or random.Random()
    if count == 1:
        targets = [(min_rating + max_rating) / 2]
    else:
        targets = [min_rating + (max_rating - min_rating) * i / (count - 1) for i in range(count)]
    chosen: list[Problem] = []
    taken = set(solved)
    for target in targets:
        pool = filter_problems(
            problems,
            min_rating=min_rating,
            max_rating=max_rating,
            tags=tags,
            exclude_tags=exclude_tags,
            solved=frozenset(taken),
        )
        if not pool:
            return None
        nearest = min(abs(p.rating - target) for p in pool)
        pick = rng.choice([p for p in pool if abs(p.rating - target) == nearest])
        chosen.append(pick)
        taken.add(pick.key)
    return chosen


def team_solves(
    members: Iterable[tuple[int, int]], solves: dict[int, dict[str, tuple[int, int]]]
) -> dict[int, dict[str, tuple[int, int]]]:
    """Team number -> problem -> (solve time, wrong attempts) of the teammate who solved it
    first. members are (user_id, team); a problem counts once for the team."""
    out: dict[int, dict[str, tuple[int, int]]] = {}
    for user_id, team in members:
        mine = out.setdefault(team, {})
        for key, found in solves.get(user_id, {}).items():
            if key not in mine or found < mine[key]:
                mine[key] = found
    return out


@dataclass(frozen=True)
class MashupRank:
    rank: int
    user_id: int  # the team number in a team contest
    handle: str
    solved: int
    total_seconds: int  # solve times plus the penalty of every wrong attempt
    times: dict[str, int]  # problem key -> seconds after the start, for solved problems
    wrong: dict[str, int]  # problem key -> wrong attempts before the solve
    lines: int = 0  # full bingo lines (always 0 in a contest)


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


def pick_replacement(
    problems: Sequence[Problem],
    rating: int,
    used: frozenset[str],
    rng: random.Random | None = None,
    tags: frozenset[str] = frozenset(),
    exclude_tags: frozenset[str] = frozenset(),
) -> Problem | None:
    """A random unused problem rated BINGO_REPLACE_STEP above `rating`, else the next rating up that
    has one; at the top of the scale (nothing higher), one of the same rating. None when no
    problem fits."""
    rng = rng or random.Random()
    for r in [*range(rating + BINGO_REPLACE_STEP, MAX_PROBLEM_RATING + 1, 100), rating]:
        pool = filter_problems(
            problems,
            min_rating=r,
            max_rating=r,
            tags=tags,
            exclude_tags=exclude_tags,
            solved=used,
        )
        if pool:
            return rng.choice(pool)
    return None


def bingo_lines(size: int) -> list[tuple[int, ...]]:
    """Every row, column and the two diagonals of a size x size board, as cell numbers."""
    rows = [tuple(range(r * size, (r + 1) * size)) for r in range(size)]
    cols = [tuple(range(c, size * size, size)) for c in range(size)]
    diagonals = [
        tuple(i * (size + 1) for i in range(size)),
        tuple((i + 1) * (size - 1) for i in range(size)),
    ]
    return rows + cols + diagonals


@dataclass(frozen=True)
class BingoBoard:
    squares: list[tuple[str, int, int | None]]  # per cell: (problem key, rating, owning team)
    claims: dict[str, tuple[int, int]]  # claimed problem key -> (team, solve time)
    lines: dict[int, int]  # team -> full lines (classic only)
    over: bool  # classic: a team holds a line (first-line win) or every square is claimed


def bingo_board(
    size: int,
    mode: str,
    win: str,
    cells: Sequence[tuple[str, int, int]],
    solves: dict[int, dict[str, tuple[int, int]]],
) -> BingoBoard:
    """The state of a shared bingo board. `cells` are (problem key, rating, cell) in the order
    they were put on the board (a replace bingo has several problems per cell, the last is the
    current one); `solves` is team -> problem key -> (solve time, wrong attempts).

    The team with the earliest solve of a problem claims it. In a classic board the claimed
    problem fills its cell and, with win == "line", the claims stop at the one that gives a team
    its first full line (that team wins). A replace board has no lines: every claim scores."""
    first: dict[str, tuple[int, int]] = {}  # problem key -> (solve time, team)
    for team, mine in solves.items():
        for key, (at, _) in mine.items():
            if key not in first or (at, team) < first[key]:
                first[key] = (at, team)
    cell_of = {key: cell for key, _, cell in cells}
    owner: dict[int, int] = {}
    kept: list[tuple[str, int, int]] = []
    over = False
    lines = bingo_lines(size)
    for key, (at, team) in sorted(first.items(), key=lambda kv: kv[1]):
        if key not in cell_of:
            continue
        kept.append((key, team, at))
        if mode != "classic":
            continue
        owner[cell_of[key]] = team
        if win == "line" and any(
            cell_of[key] in line and all(owner.get(c) == team for c in line) for line in lines
        ):
            over = True
            break
    if mode == "classic":
        over = over or len(kept) == size * size
    claims = {key: (team, at) for key, team, at in kept}
    held = {
        team: n
        for team in set(owner.values())
        if (n := sum(all(owner.get(c) == team for c in line) for line in lines))
    }
    latest = {cell: (key, rating) for key, rating, cell in cells}
    squares = [
        (*latest[c], claims[latest[c][0]][0] if latest[c][0] in claims else None)
        for c in range(size * size)
    ]
    return BingoBoard(squares, claims, held, over)


def bingo_standings(
    players: Iterable[tuple[int, str, int]], board: BingoBoard, started_at: int
) -> list[MashupRank]:
    """Teams by full lines, then claimed squares, then who reached that score first; equal
    scores share a rank. players are (team, handle, joined_at)."""
    rows = []
    for team, handle, joined_at in players:
        mine = {k: at for k, (t, at) in board.claims.items() if t == team}
        held = board.lines.get(team, 0)
        rows.append(
            (
                (-held, -len(mine), max(mine.values(), default=0), joined_at),
                team,
                handle,
                mine,
                held,
            )
        )
    rows.sort(key=lambda r: r[0])
    ranked: list[MashupRank] = []
    for i, (score, team, handle, mine, held) in enumerate(rows):
        same = ranked and rows[i - 1][0][:3] == score[:3]
        times = {k: max(0, at - started_at) for k, at in mine.items()}
        ranked.append(
            MashupRank(
                ranked[-1].rank if same else i + 1,
                team,
                handle,
                len(mine),
                max(times.values(), default=0),
                times,
                {},
                held,
            )
        )
    return ranked
