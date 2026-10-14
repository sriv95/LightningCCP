import pytest

from src.cf.models import Contest, Problem, Submission
from src.errors import BotError
from src.logic import (
    chunked,
    duel_gain,
    earliest_solve,
    filter_problems,
    first_solve,
    format_hm,
    has_compile_error,
    mashup_standings,
    match_deltas,
    match_rating,
    ordinal,
    parse_minutes,
    parse_tags,
    pick_mashup,
    placement,
    rank_groups,
    rating_series,
    refresh_minutes,
    reminders_due,
    round_rating,
    select_contests,
    solo_delta,
    solved_keys,
    tag_suggestions,
    unsolved_keys,
)


def P(cid, idx, rating, *tags):
    return Problem(cid, idx, f"p{cid}{idx}", rating, tuple(tags))


def S(key, verdict, created):
    return Submission(1, created, key, verdict)


def C(cid, phase, start, duration=7200):
    return Contest(cid, f"c{cid}", "CF", phase, start, duration)


def test_parse_tags():
    assert parse_tags(None) == frozenset()
    assert parse_tags("") == frozenset()
    assert parse_tags(" DP, Graphs ,, ") == {"dp", "graphs"}


def test_tag_suggestions_prefix_and_dedupe():
    tags = {"dp", "dfs and similar", "graphs", "greedy"}
    assert tag_suggestions("g", tags) == ["graphs", "greedy"]
    assert tag_suggestions("dp,g", tags) == ["dp,graphs", "dp,greedy"]
    assert tag_suggestions("dp,df", tags) == ["dp,dfs and similar"]
    assert "dp,dp" not in tag_suggestions("dp,d", tags)  # already chosen


def test_tag_suggestions_limit_and_length():
    # Review Focus 5: Discord allows 25 choices of at most 100 characters.
    many = {f"tag{i:03d}" for i in range(60)}
    assert len(tag_suggestions("", many)) == 25
    fits = tag_suggestions("a,b,c,d,e,x", {"x" * 90})  # prefix 10 + 90 = 100
    assert [len(v) for v in fits] == [100]
    assert tag_suggestions("a,b,c,d,e,x", {"x" * 95}) == []  # would be 105


def test_filter_problems_rating_range_and_unrated_excluded():
    ps = [P(1, "A", 800), P(1, "B", 1200), P(1, "C", None), P(1, "D", 1600)]
    assert [p.key for p in filter_problems(ps)] == ["1A", "1B", "1D"]
    assert [p.key for p in filter_problems(ps, min_rating=1000, max_rating=1500)] == ["1B"]
    assert [p.key for p in filter_problems(ps, max_rating=800)] == ["1A"]


def test_filter_problems_by_contest():
    ps = [P(1, "A", 800), P(2, "A", 900), P(2, "B", None), P(2, "C", 1000)]
    assert [p.key for p in filter_problems(ps, contest_id=2)] == ["2A", "2C"]  # unrated stays out
    assert filter_problems(ps, contest_id=3) == []
    assert [p.key for p in filter_problems(ps, contest_id=2, min_rating=950)] == ["2C"]


def test_filter_problems_tags_and_solved():
    ps = [P(1, "A", 800, "dp", "math"), P(1, "B", 800, "dp"), P(1, "C", 800, "math")]
    assert [p.key for p in filter_problems(ps, tags=frozenset({"dp", "math"}))] == ["1A"]
    assert [p.key for p in filter_problems(ps, exclude_tags=frozenset({"math"}))] == ["1B"]
    assert [p.key for p in filter_problems(ps, solved=frozenset({"1A"}))] == ["1B", "1C"]


def test_filter_problems_no_match_is_empty_list():
    assert filter_problems([P(1, "A", 800)], min_rating=3000) == []


def test_solved_keys_only_ok():
    subs = [S("1A", "OK", 1), S("1B", "WRONG_ANSWER", 2), S("1C", None, 3)]
    assert solved_keys(subs) == {"1A"}


def test_round_rating():
    assert round_rating(None) == 800
    assert round_rating(1849) == 1800
    assert round_rating(1850) == 1900
    assert round_rating(100) == 800
    assert round_rating(9999) == 3500


def test_has_compile_error_in_window():
    subs = [S("1A", "COMPILATION_ERROR", 1100)]
    assert has_compile_error(subs, "1A", issued_at=1000)


def test_has_compile_error_before_issue_does_not_count():
    # Review Focus 3
    assert not has_compile_error([S("1A", "COMPILATION_ERROR", 999)], "1A", issued_at=1000)


def test_has_compile_error_after_window_does_not_count():
    assert not has_compile_error([S("1A", "COMPILATION_ERROR", 1301)], "1A", issued_at=1000)
    assert has_compile_error([S("1A", "COMPILATION_ERROR", 1300)], "1A", issued_at=1000)


def test_has_compile_error_wrong_verdict_or_problem():
    assert not has_compile_error([S("1A", "WRONG_ANSWER", 1100)], "1A", issued_at=1000)
    assert not has_compile_error([S("1B", "COMPILATION_ERROR", 1100)], "1A", issued_at=1000)


def test_select_contests():
    cs = [
        C(1, "BEFORE", 3000),
        C(2, "BEFORE", 2000),
        C(3, "CODING", 900),
        C(4, "FINISHED", 100),
        C(5, "FINISHED", 200),
        C(6, "BEFORE", None),
    ]
    assert [c.id for c in select_contests(cs, "upcoming")] == [2, 1]
    assert [c.id for c in select_contests(cs, "running")] == [3]
    assert [c.id for c in select_contests(cs, "recent")] == [5, 4]
    assert [c.id for c in select_contests(cs, "recent", limit=1)] == [5]
    with pytest.raises(ValueError):
        select_contests(cs, "bogus")


def test_refresh_minutes():
    assert refresh_minutes([C(1, "BEFORE", 10)]) == 30
    assert refresh_minutes([C(1, "BEFORE", 10), C(2, "SYSTEM_TEST", 5)]) == 5


def test_parse_minutes():
    assert parse_minutes("10, 60,10") == (60, 10)
    for bad in ["", "abc", "0", "-5", "10081", "1,2,3,4,5,6"]:
        with pytest.raises(BotError):
            parse_minutes(bad)


def test_reminders_due_window_and_sent():
    c = C(1, "BEFORE", start=10_000)
    # 61 minutes before start: nothing due
    assert reminders_due([c], (60, 10), set(), now=10_000 - 61 * 60) == []
    # 59 minutes before: the 60 minute reminder is due
    assert reminders_due([c], (60, 10), set(), now=10_000 - 59 * 60) == [(c, 60)]
    # already sent: skip
    assert reminders_due([c], (60, 10), {(1, 60)}, now=10_000 - 59 * 60) == []
    # 5 minutes before with nothing sent: both due (bot was down)
    assert {b for _, b in reminders_due([c], (60, 10), set(), now=10_000 - 300)} == {60, 10}


def test_reminders_due_never_after_start_or_for_non_upcoming():
    # Review Focus 4
    c = C(1, "BEFORE", start=10_000)
    assert reminders_due([c], (10,), set(), now=10_000) == []
    assert reminders_due([c], (10,), set(), now=10_001) == []
    running = C(2, "CODING", start=10_000)
    assert reminders_due([running], (10,), set(), now=9_900) == []
    unscheduled = C(3, "BEFORE", start=None)
    assert reminders_due([unscheduled], (10,), set(), now=9_900) == []


def test_chunked():
    assert chunked([1, 2, 3, 4, 5], 2) == [[1, 2], [3, 4], [5]]
    assert chunked([], 3) == []


def test_earliest_solve():
    subs = [
        S("1A", "OK", 700),
        S("1A", "OK", 500),
        S("1A", "WRONG_ANSWER", 450),
        S("1B", "OK", 400),
    ]
    assert earliest_solve(subs, "1A", since=500) == 500
    assert earliest_solve(subs, "1A", since=501) == 700
    assert earliest_solve(subs, "1A", since=701) is None
    assert earliest_solve(subs, "1C", since=0) is None


def test_match_rating():
    assert match_rating(1400, [800, 3000]) == 1400
    assert match_rating(None, [1000]) == 1000
    assert match_rating(None, [1000, 1900]) == 1500  # average 1450 rounds up to the nearest 100
    assert match_rating(None, [800]) == 800
    assert match_rating(None, [740]) == 800  # never below the lowest problem rating
    assert match_rating(None, [3500, 3500, 3500]) == 3500


def test_rank_groups_orders_by_solve_time_then_ties_non_solvers():
    entries = [(1, None), (2, 300), (3, 200), (4, None)]
    assert rank_groups(entries) == [[3], [2], [1, 4]]
    assert rank_groups([(1, 5), (2, 5)]) == [[1], [2]]  # equal times: join order
    assert rank_groups([(1, None), (2, None)]) == [[1, 2]]


def test_solo_delta_is_linear_in_the_gap():
    assert [solo_delta(1000, p, True) for p in (400, 800, 900, 1000, 1100, 1200, 1400, 2000)] == [
        1,
        6,
        8,
        10,
        16,
        22,
        33,
        33,
    ]  # +1 at 400 or more below, +10 equal, +33 at 400 or more above
    assert solo_delta(800, 800, False) == 0  # failing never costs rating


def test_duel_gain_staircase():
    steps = {-500: 16, -400: 16, -399: 33, -200: 33, -199: 49, 0: 49, 199: 49, 200: 67, 399: 67}
    assert {gap: duel_gain(gap) for gap in steps} == steps
    assert duel_gain(400) == duel_gain(900) == 95


def test_match_deltas_duel_is_zero_sum_staircase():
    assert match_deltas([[1], [2]], {1: 800, 2: 800}) == {1: 49, 2: -49}
    assert match_deltas([[1], [2]], {1: 800, 2: 1200}) == {1: 95, 2: -95}  # upset
    assert match_deltas([[1], [2]], {1: 1200, 2: 800}) == {1: 16, 2: -16}


def test_match_deltas_averages_over_opponents():
    r = dict.fromkeys(range(1, 5), 800)
    assert match_deltas([[1], [2], [3]], r) == {1: 49, 2: 0, 3: -49}
    assert match_deltas([[1], [2], [3], [4]], r) == {1: 49, 2: 16, 3: -16, 4: -49}


def test_match_deltas_ties_move_nothing_between_the_tied():
    r = dict.fromkeys(range(1, 4), 800)
    assert match_deltas([[1], [2, 3]], r) == {1: 49, 2: -25, 3: -25}  # -24.5 rounds away from 0
    assert match_deltas([[1, 2]], {1: 800, 2: 800}) == {1: 0, 2: 0}


def test_match_deltas_equal_ratings_sum_to_zero():
    for n in range(2, 11):
        deltas = match_deltas([[i] for i in range(n)], dict.fromkeys(range(n), 1500))
        assert len(deltas) == n and sum(deltas.values()) == 0
        assert deltas[0] == 49 and deltas[n - 1] == -49


def test_match_deltas_needs_two_players():
    with pytest.raises(ValueError):
        match_deltas([[1]], {1: 800})


def test_unsolved_keys_attempted_not_solved_newest_first():
    subs = [
        S("1C", "WRONG_ANSWER", 5),
        S("1A", "OK", 4),
        S("1B", "WRONG_ANSWER", 3),
        S("1C", "TIME_LIMIT_EXCEEDED", 2),
        S("1A", "WRONG_ANSWER", 1),
    ]
    assert unsolved_keys(subs) == ["1C", "1B"]
    assert unsolved_keys([]) == []


def test_rating_series_accumulates_from_the_start_rating():
    assert rating_series([(10, 49), (20, -16), (30, 0)]) == [(10, 849), (20, 833), (30, 833)]
    assert rating_series([]) == []


def test_ordinal():
    assert [ordinal(n) for n in (1, 2, 3, 4, 11, 12, 13, 21, 22, 102, 111)] == [
        "1st",
        "2nd",
        "3rd",
        "4th",
        "11th",
        "12th",
        "13th",
        "21st",
        "22nd",
        "102nd",
        "111th",
    ]


def test_placement_by_rating_change():
    deltas = [49, 16, -16, -49]
    assert [placement(deltas, d) for d in deltas] == [1, 2, 3, 4]
    assert placement([-25, 49, -25], -25) == 2  # ties share a place
    assert placement([0, 0], 0) is None  # nobody solved


def test_first_solve_counts_wrong_attempts_before_the_accept_inside_the_window():
    subs = [
        S("1A", "WRONG_ANSWER", 90),  # before the start
        S("1A", "WRONG_ANSWER", 150),
        S("1A", "COMPILATION_ERROR", 160),  # not a wrong attempt
        S("1A", "TIME_LIMIT_EXCEEDED", 170),
        S("1A", None, 175),  # still being judged
        S("1A", "OK", 200),
        S("1A", "WRONG_ANSWER", 210),  # after the accept
        S("1A", "OK", 300),
        S("2A", "OK", 5000),  # after the end
    ]
    assert first_solve(subs, "1A", since=100, until=1000) == (200, 2)
    assert first_solve(subs, "2A", since=100, until=1000) is None
    assert first_solve(subs, "1A", since=100, until=150) is None  # ended before the accept
    assert first_solve([S("1A", "OK", 100)], "1A", since=100, until=100) == (100, 0)


def test_format_hm():
    assert [format_hm(s) for s in (0, 59, 60, 3599, 3600, 7260)] == [
        "0:00",
        "0:00",
        "0:01",
        "0:59",
        "1:00",
        "2:01",
    ]


def mashup_pool():
    return [P(i, "A", r) for i, r in enumerate(range(800, 3600, 100), start=1)] + [
        P(99, "A", 800),
        P(97, "A", None),
    ]


def test_pick_mashup_spreads_ratings_easy_to_hard():
    picked = pick_mashup(mashup_pool(), 800, 1400, 4)  # targets 800, 1000, 1200, 1400
    assert [p.rating for p in picked] == [800, 1000, 1200, 1400]
    assert len({p.key for p in picked}) == 4


def test_pick_mashup_repeated_rating_picks_distinct_problems():
    picked = pick_mashup(mashup_pool(), 800, 800, 2)  # two problems rated 800 exist
    assert {p.key for p in picked} == {"1A", "99A"}


def test_pick_mashup_single_problem_aims_at_the_middle_and_skips_solved():
    assert pick_mashup(mashup_pool(), 800, 1200, 1)[0].rating == 1000
    solved = frozenset({"3A"})  # the only 1000 problem
    assert pick_mashup(mashup_pool(), 800, 1200, 1, solved)[0].rating in (900, 1100)


def test_pick_mashup_fails_when_the_range_is_too_small():
    pool = [P(1, "A", 800), P(2, "A", 900)]
    assert pick_mashup(pool, 800, 900, 3) is None
    assert pick_mashup(pool, 800, 900, 2, frozenset({"1A"})) is None
    assert pick_mashup(pool, 1000, 1100, 1) is None


def test_mashup_standings_rank_solved_then_time_with_shared_ranks():
    players = [(1, "a", 0), (2, "b", 1), (3, "c", 2), (4, "d", 3), (5, "e", 4)]
    solves = {
        1: {"A": (160, 0), "B": (400, 0)},
        2: {"A": (150, 0), "B": (410, 0)},  # same total as player 1, so the same rank
        3: {"A": (100, 0)},
        4: {"A": (100, 0), "B": (1000, 0), "C": (1100, 0)},
    }
    rows = mashup_standings(players, solves, started_at=100)
    assert [(r.user_id, r.rank, r.solved, r.total_seconds) for r in rows] == [
        (4, 1, 3, 1900),
        (1, 2, 2, 360),
        (2, 2, 2, 360),
        (3, 4, 1, 0),
        (5, 5, 0, 0),
    ]
    assert rows[1].times == {"A": 60, "B": 300}


def test_mashup_standings_penalty_per_wrong_attempt():
    players = [(1, "a", 0), (2, "b", 1)]
    solves = {1: {"A": (160, 2)}, 2: {"A": (200, 0)}}  # a solved first but with 2 wrong attempts
    rows = mashup_standings(players, solves, started_at=100, penalty_minutes=20)
    assert [(r.handle, r.total_seconds) for r in rows] == [("b", 100), ("a", 60 + 2 * 20 * 60)]
    assert rows[1].wrong == {"A": 2}
    free = mashup_standings(players, solves, started_at=100)  # no penalty: a is first
    assert [r.handle for r in free] == ["a", "b"]


def test_recaps_due_window_and_sent():
    from src.logic import RECAP_WINDOW_SECONDS, recaps_due

    now = 1_000_000
    done = Contest(1, "a", "CF", "FINISHED", now - 7200 - 60, 7200)  # ended 60 s ago
    older = Contest(2, "b", "CF", "FINISHED", now - 7200 - 600, 7200)
    stale = Contest(3, "c", "CF", "FINISHED", now - 7200 - RECAP_WINDOW_SECONDS - 1, 7200)
    running = Contest(4, "d", "CF", "CODING", now - 100, 7200)
    assert [c.id for c in recaps_due([done, older, stale, running], set(), now)] == [2, 1]
    assert [c.id for c in recaps_due([done, older], {2}, now)] == [1]


def test_pick_mashup_respects_tags():
    pool = [Problem(i, "A", "x", 800 + 100 * i, ("dp",) if i % 2 else ("math",)) for i in range(6)]
    picked = pick_mashup(pool, 800, 1300, 3, tags=frozenset({"dp"}))
    assert {p.contest_id % 2 for p in picked} == {1}
    assert pick_mashup(pool, 800, 1300, 4, tags=frozenset({"dp"})) is None  # only 3 have dp
    picked = pick_mashup(pool, 800, 1300, 3, exclude_tags=frozenset({"dp"}))
    assert {p.contest_id % 2 for p in picked} == {0}
