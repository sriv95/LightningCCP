import pytest

from src.cf.models import Contest, Problem, Submission
from src.errors import BotError
from src.logic import (
    chunked,
    earliest_solve,
    filter_problems,
    has_compile_error,
    match_rating,
    parse_minutes,
    parse_tags,
    placement_deltas,
    problem_points,
    rank_groups,
    refresh_minutes,
    reminders_due,
    round_rating,
    select_contests,
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


def test_problem_points():
    assert problem_points(800) == 3
    assert problem_points(1400) == 9
    assert problem_points(0) == 1


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
    assert match_rating(None, [None, 1000]) == 900  # unrated counts as 800
    assert match_rating(None, [None]) == 800
    assert match_rating(None, [3500, 3500, 3500]) == 3500


def test_rank_groups_orders_by_solve_time_then_ties_non_solvers():
    entries = [(1, None), (2, 300), (3, 200), (4, None)]
    assert rank_groups(entries) == [[3], [2], [1, 4]]
    assert rank_groups([(1, 5), (2, 5)]) == [[1], [2]]  # equal times: join order
    assert rank_groups([(1, None), (2, None)]) == [[1, 2]]


def test_placement_deltas_duel():
    assert placement_deltas([[1], [2]], 9) == {1: 9, 2: -9}


def test_placement_deltas_four_players_round_half_away_from_zero():
    assert placement_deltas([[1], [2], [3], [4]], 9) == {1: 9, 2: 5, 3: -5, 4: -9}


def test_placement_deltas_odd_count_middle_gets_zero():
    assert placement_deltas([[1], [2], [3]], 9) == {1: 9, 2: 0, 3: -9}
    assert placement_deltas([[1], [2], [3], [4], [5]], 9) == {1: 9, 2: 5, 3: 0, 4: -5, 5: -9}


def test_placement_deltas_ties_share_average():
    # Review Focus 4
    assert placement_deltas([[1], [2, 3, 4]], 9) == {1: 9, 2: -3, 3: -3, 4: -3}
    assert placement_deltas([[1], [2, 3]], 9) == {1: 9, 2: -5, 3: -5}  # (0 + -9) / 2 = -4.5


def test_placement_deltas_sum_to_zero():
    # Review Focus 4: without ties the top and bottom halves mirror exactly, so rounding keeps the sum at 0.
    for n in range(2, 11):
        groups = [[i] for i in range(n)]
        for points in (1, 5, 9, 30):
            deltas = placement_deltas(groups, points)
            assert len(deltas) == n
            assert sum(deltas.values()) == 0
            assert deltas[0] == points and deltas[n - 1] == -points


def test_placement_deltas_ten_players_exact():
    deltas = placement_deltas([[i] for i in range(10)], 10)
    assert [deltas[i] for i in range(10)] == [10, 8, 6, 4, 2, -2, -4, -6, -8, -10]


def test_placement_deltas_needs_two_players():
    with pytest.raises(ValueError):
        placement_deltas([[1]], 5)


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
