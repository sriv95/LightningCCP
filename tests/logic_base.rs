//! Port of the non-mashup tests of tests/test_logic.py plus edge cases whose expected values come from
//! running the Python original (`.venv/bin/python -I`).

use std::collections::{HashMap, HashSet};

use lightningccp::cf::models::{Contest, Problem, Submission};
use lightningccp::errors::BotError;
use lightningccp::logic::base::*;

fn p(cid: i64, idx: &str, rating: Option<i64>, tags: &[&str]) -> Problem {
    Problem {
        contest_id: cid,
        index: idx.into(),
        name: format!("p{cid}{idx}"),
        rating,
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

fn s(key: &str, verdict: Option<&str>, created: i64) -> Submission {
    Submission { id: 1, created, problem_key: key.into(), verdict: verdict.map(String::from) }
}

fn c(cid: i64, phase: &str, start: Option<i64>) -> Contest {
    cn(cid, &format!("c{cid}"), phase, start, 7200)
}

fn cn(cid: i64, name: &str, phase: &str, start: Option<i64>, duration: i64) -> Contest {
    Contest { id: cid, name: name.into(), kind: "CF".into(), phase: phase.into(), start_time: start, duration }
}

fn set<T: std::hash::Hash + Eq, const N: usize>(v: [T; N]) -> HashSet<T> {
    v.into_iter().collect()
}

fn strs<const N: usize>(v: [&str; N]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn keys(ps: &[Problem]) -> Vec<String> {
    ps.iter().map(Problem::key).collect()
}

fn ids(cs: &[Contest]) -> Vec<i64> {
    cs.iter().map(|c| c.id).collect()
}

fn ratings(v: &[(i64, i64)]) -> HashMap<i64, i64> {
    v.iter().copied().collect()
}

fn deltas(groups: &[Vec<i64>], r: &HashMap<i64, i64>, scale: f64) -> Vec<(i64, i64)> {
    match_deltas(groups, r, scale).unwrap()
}

fn flat(range: std::ops::RangeInclusive<i64>, rating: i64) -> HashMap<i64, i64> {
    range.map(|i| (i, rating)).collect()
}

#[test]
fn parse_tags_cases() {
    assert_eq!(parse_tags(None), set([]));
    assert_eq!(parse_tags(Some("")), set([]));
    assert_eq!(parse_tags(Some(" DP, Graphs ,, ")), set(["dp".to_string(), "graphs".to_string()]));
    // Python: sorted(parse_tags(" \x1cA  ,Σ")) == ['a', 'σ']
    assert_eq!(parse_tags(Some(" \x1cA\u{a0} ,Σ")), set(["a".to_string(), "σ".to_string()]));
    assert_eq!(normalize_tags(Some(" DP, graphs ,,Dp")), "dp,graphs");
}

#[test]
fn tag_suggestions_prefix_and_dedupe() {
    let tags = strs(["dp", "dfs and similar", "graphs", "greedy"]);
    assert_eq!(tag_suggestions("g", &tags, 25), ["graphs", "greedy"]);
    assert_eq!(tag_suggestions("dp,g", &tags, 25), ["dp,graphs", "dp,greedy"]);
    assert_eq!(tag_suggestions("dp,df", &tags, 25), ["dp,dfs and similar"]);
    assert!(!tag_suggestions("dp,d", &tags, 25).contains(&"dp,dp".to_string())); // already chosen
    // Python: tag_suggestions("DP , G", ...) == ['DP,graphs', 'DP,greedy']; limit=1 on "dp," -> ['dp,a']
    assert_eq!(tag_suggestions("DP , G", &strs(["dp", "graphs", "greedy"]), 25), ["DP,graphs", "DP,greedy"]);
    assert_eq!(tag_suggestions("dp,", &strs(["dp", "a", "b"]), 1), ["dp,a"]);
}

#[test]
fn tag_suggestions_limit_and_length() {
    let many: Vec<String> = (0..60).map(|i| format!("tag{i:03}")).collect();
    assert_eq!(tag_suggestions("", &many, 25).len(), 25);
    let fits = tag_suggestions("a,b,c,d,e,x", &["x".repeat(90)], 25); // prefix 10 + 90 = 100
    assert_eq!(fits.iter().map(|v| v.len()).collect::<Vec<_>>(), [100]);
    assert!(tag_suggestions("a,b,c,d,e,x", &["x".repeat(95)], 25).is_empty()); // would be 105
    // length counts code points, not bytes
    assert_eq!(tag_suggestions("", &["ж".repeat(100)], 25).len(), 1);
    assert!(tag_suggestions("", &["ж".repeat(101)], 25).is_empty());
}

#[test]
fn filter_problems_rating_range_and_unrated_excluded() {
    let ps = [p(1, "A", Some(800), &[]), p(1, "B", Some(1200), &[]), p(1, "C", None, &[]), p(1, "D", Some(1600), &[])];
    assert_eq!(keys(&filter_problems(&ps, &ProblemFilter::default())), ["1A", "1B", "1D"]);
    let f = ProblemFilter { min_rating: Some(1000), max_rating: Some(1500), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["1B"]);
    let f = ProblemFilter { max_rating: Some(800), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["1A"]);
}

#[test]
fn filter_problems_by_contest() {
    let ps = [p(1, "A", Some(800), &[]), p(2, "A", Some(900), &[]), p(2, "B", None, &[]), p(2, "C", Some(1000), &[])];
    let f = ProblemFilter { contest_id: Some(2), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["2A", "2C"]); // unrated stays out
    assert!(filter_problems(&ps, &ProblemFilter { contest_id: Some(3), ..Default::default() }).is_empty());
    let f = ProblemFilter { contest_id: Some(2), min_rating: Some(950), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["2C"]);
}

#[test]
fn filter_problems_tags_and_solved() {
    let ps = [p(1, "A", Some(800), &["dp", "math"]), p(1, "B", Some(800), &["dp"]), p(1, "C", Some(800), &["math"])];
    let f = ProblemFilter { tags: set(["dp".into(), "math".into()]), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["1A"]);
    let f = ProblemFilter { exclude_tags: set(["math".into()]), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["1B"]);
    let f = ProblemFilter { solved: set(["1A".into()]), ..Default::default() };
    assert_eq!(keys(&filter_problems(&ps, &f)), ["1B", "1C"]);
}

#[test]
fn filter_problems_no_match_is_empty_list() {
    let f = ProblemFilter { min_rating: Some(3000), ..Default::default() };
    assert!(filter_problems(&[p(1, "A", Some(800), &[])], &f).is_empty());
}

#[test]
fn solved_keys_only_ok() {
    let subs = [s("1A", Some("OK"), 1), s("1B", Some("WRONG_ANSWER"), 2), s("1C", None, 3)];
    assert_eq!(solved_keys(&subs), set(["1A".to_string()]));
}

#[test]
fn round_rating_cases() {
    assert_eq!(round_rating(None), 800);
    assert_eq!(round_rating(Some(1849)), 1800);
    assert_eq!(round_rating(Some(1850)), 1900);
    assert_eq!(round_rating(Some(100)), 800);
    assert_eq!(round_rating(Some(9999)), 3500);
    // Python: negatives floor then clamp, [round_rating(x) for x in (-1,-50,-51,-49,-100,49,50,0,3549,3550)]
    let got: Vec<i64> = [-1, -50, -51, -49, -100, 49, 50, 0, 3549, 3550].map(|x| round_rating(Some(x))).to_vec();
    assert_eq!(got, [800, 800, 800, 800, 800, 800, 800, 800, 3500, 3500]);
    // Python: [floor_rating(x) for x in (-1,-100,-101,99,100,799,3600)]
    let got: Vec<i64> = [-1, -100, -101, 99, 100, 799, 3600].map(floor_rating).to_vec();
    assert_eq!(got, [800, 800, 800, 800, 800, 800, 3500]);
    assert_eq!(floor_rating(1999), 1900);
}

#[test]
fn has_compile_error_cases() {
    let ce = |t| [s("1A", Some("COMPILATION_ERROR"), t)];
    assert!(has_compile_error(&ce(1100), "1A", 1000, LINK_WINDOW_SECONDS));
    assert!(!has_compile_error(&ce(999), "1A", 1000, LINK_WINDOW_SECONDS)); // before issue
    assert!(!has_compile_error(&ce(1301), "1A", 1000, LINK_WINDOW_SECONDS));
    assert!(has_compile_error(&ce(1300), "1A", 1000, LINK_WINDOW_SECONDS));
    assert!(!has_compile_error(&[s("1A", Some("WRONG_ANSWER"), 1100)], "1A", 1000, LINK_WINDOW_SECONDS));
    assert!(!has_compile_error(&[s("1B", Some("COMPILATION_ERROR"), 1100)], "1A", 1000, LINK_WINDOW_SECONDS));
}

#[test]
fn select_contests_cases() {
    let cs = [
        c(1, "BEFORE", Some(3000)),
        c(2, "BEFORE", Some(2000)),
        c(3, "CODING", Some(900)),
        c(4, "FINISHED", Some(100)),
        c(5, "FINISHED", Some(200)),
        c(6, "BEFORE", None),
    ];
    assert_eq!(ids(&select_contests(&cs, "upcoming", 50).unwrap()), [2, 1]);
    assert_eq!(ids(&select_contests(&cs, "running", 50).unwrap()), [3]);
    assert_eq!(ids(&select_contests(&cs, "recent", 50).unwrap()), [5, 4]);
    assert_eq!(ids(&select_contests(&cs, "recent", 1).unwrap()), [5]);
    assert!(select_contests(&cs, "bogus", 50).is_err());
    // equal start times keep input order in a descending sort (Python sorted(reverse=True) is stable)
    let tied = [c(7, "FINISHED", Some(10)), c(8, "FINISHED", Some(10))];
    assert_eq!(ids(&select_contests(&tied, "recent", 50).unwrap()), [7, 8]);
}

#[test]
fn refresh_minutes_cases() {
    assert_eq!(refresh_minutes(&[c(1, "BEFORE", Some(10))]), 30);
    assert_eq!(refresh_minutes(&[c(1, "BEFORE", Some(10)), c(2, "SYSTEM_TEST", Some(5))]), 5);
}

#[test]
fn parse_minutes_cases() {
    assert_eq!(parse_minutes("10, 60,10").unwrap(), [60, 10]);
    for bad in ["", "abc", "0", "-5", "10081", "1,2,3,4,5,6"] {
        assert!(matches!(parse_minutes(bad), Err(BotError::Plain(_))), "{bad:?}");
    }
    // Python int() edge cases, run with .venv/bin/python -I
    assert_eq!(parse_minutes(" +5 ,1_0").unwrap(), [10, 5]);
    assert_eq!(parse_minutes("10080,1").unwrap(), [10080, 1]);
    for bad in ["1__0", "_1", "1_", "+-1", "1.5", " ", "0x10", "99999999999999999999999"] {
        assert!(parse_minutes(bad).is_err(), "{bad:?}");
    }
    // Python: parse_minutes(s) for the same strings under .venv/bin/python -I (3.12, Unicode 15.0)
    for (s, want) in [
        ("١٠", vec![10]),
        ("١٠, ٥", vec![10, 5]),
        ("５,1_０", vec![10, 5]),
        ("\u{a0}5\u{2003}", vec![5]),
        ("\u{2028}\u{2029}5", vec![5]),
        ("٠٥", vec![5]),
        ("+٥", vec![5]),
        ("٥_٥", vec![55]),
        ("５\u{661}", vec![51]),
        ("𝟓", vec![5]),
        ("𝟏𝟎", vec![10]),
        ("٥\u{660}", vec![50]),
        (" 1 , 2 ,\t3\n", vec![3, 2, 1]),
        (&format!("{}5", "0".repeat(4297)), vec![5]),
    ] {
        assert_eq!(parse_minutes(s).unwrap(), want, "{s:?}");
    }
    // BotError in Python: U+001C..U+001F are not stripped by int(); 4301+ digits raise ValueError
    for bad in ["\x1c5\x1f", "1٫0", "-٥", "²", "½", "1,,2", &format!("{}5", "0".repeat(4300)), &format!("1_{}", "0".repeat(4300))] {
        assert!(matches!(parse_minutes(bad), Err(BotError::Plain(_))), "{:?}", &bad[..bad.len().min(12)]);
    }
    assert_eq!(
        parse_minutes("x").unwrap_err().to_string(),
        "Use up to 5 comma-separated whole minutes between 1 and 10080, e.g. 10,60."
    );
}

#[test]
fn reminders_due_window_and_sent() {
    let ct = c(1, "BEFORE", Some(10_000));
    let none = HashSet::new();
    assert!(reminders_due(std::slice::from_ref(&ct), &[60, 10], &none, 10_000 - 61 * 60).is_empty());
    assert_eq!(reminders_due(std::slice::from_ref(&ct), &[60, 10], &none, 10_000 - 59 * 60), [(ct.clone(), 60)]);
    assert!(reminders_due(std::slice::from_ref(&ct), &[60, 10], &set([(1, 60)]), 10_000 - 59 * 60).is_empty());
    let both: HashSet<i64> = reminders_due(std::slice::from_ref(&ct), &[60, 10], &none, 10_000 - 300).iter().map(|x| x.1).collect();
    assert_eq!(both, set([60, 10]));
}

#[test]
fn reminders_due_never_after_start_or_for_non_upcoming() {
    let none = HashSet::new();
    let ct = c(1, "BEFORE", Some(10_000));
    assert!(reminders_due(std::slice::from_ref(&ct), &[10], &none, 10_000).is_empty());
    assert!(reminders_due(&[ct], &[10], &none, 10_001).is_empty());
    assert!(reminders_due(&[c(2, "CODING", Some(10_000))], &[10], &none, 9_900).is_empty());
    assert!(reminders_due(&[c(3, "BEFORE", None)], &[10], &none, 9_900).is_empty());
}

#[test]
fn earliest_solve_cases() {
    let subs = [s("1A", Some("OK"), 700), s("1A", Some("OK"), 500), s("1A", Some("WRONG_ANSWER"), 450), s("1B", Some("OK"), 400)];
    assert_eq!(earliest_solve(&subs, "1A", 500), Some(500));
    assert_eq!(earliest_solve(&subs, "1A", 501), Some(700));
    assert_eq!(earliest_solve(&subs, "1A", 701), None);
    assert_eq!(earliest_solve(&subs, "1C", 0), None);
}

#[test]
fn match_rating_cases() {
    assert_eq!(match_rating(Some(1400), &[800, 3000]).unwrap(), 1400);
    assert_eq!(match_rating(None, &[1000]).unwrap(), 1000);
    assert_eq!(match_rating(None, &[1000, 1900]).unwrap(), 1500); // average 1450 rounds up to the nearest 100
    assert_eq!(match_rating(None, &[800]).unwrap(), 800);
    assert_eq!(match_rating(None, &[740]).unwrap(), 800);
    assert_eq!(match_rating(None, &[3500, 3500, 3500]).unwrap(), 3500);
    // Python: [match_rating(None, r) for r in ([-5000,100],[-1],[1000,1001],[-1000,0,1])] == [800,800,1000,800]
    assert_eq!(match_rating(None, &[-5000, 100]).unwrap(), 800);
    assert_eq!(match_rating(None, &[1000, 1001]).unwrap(), 1000);
    // Python: match_rating(None, []) raises ZeroDivisionError
    assert!(match_rating(None, &[]).is_err());
    assert_eq!(match_rating(Some(1400), &[]).unwrap(), 1400);
}

#[test]
fn rank_groups_orders_by_solve_time_then_ties_non_solvers() {
    assert_eq!(rank_groups(&[(1, None), (2, Some(300)), (3, Some(200)), (4, None)]), vec![vec![3], vec![2], vec![1, 4]]);
    assert_eq!(rank_groups(&[(1, Some(5)), (2, Some(5))]), vec![vec![1], vec![2]]); // equal times: join order
    assert_eq!(rank_groups(&[(1, None), (2, None)]), vec![vec![1, 2]]);
}

#[test]
fn solo_delta_is_linear_in_the_gap() {
    let got: Vec<i64> = [400, 800, 900, 1000, 1100, 1200, 1400, 2000].map(|pr| solo_delta(1000, pr, true)).to_vec();
    assert_eq!(got, [1, 6, 8, 10, 16, 22, 33, 33]);
    assert_eq!(solo_delta(800, 800, false), 0);
    // Python: [solo_delta(1000, p, True) for p in range(600, 1401, 25)]
    let want = [1, 2, 2, 3, 3, 4, 4, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 11, 13, 14, 16, 17, 19, 20, 22, 23, 24, 26, 27, 29, 30, 32, 33];
    let got: Vec<i64> = (600..=1400).step_by(25).map(|pr| solo_delta(1000, pr, true)).collect();
    assert_eq!(got, want);
    // Python: [solo_delta(r, 800, True) for r in (800, 1000, 2000, 500)]
    assert_eq!([800, 1000, 2000, 500].map(|r| solo_delta(r, 800, true)), [10, 6, 1, 27]);
}

#[test]
fn duel_gain_staircase() {
    let steps = [(-500, 16), (-400, 16), (-399, 33), (-200, 33), (-199, 49), (0, 49), (199, 49), (200, 67), (399, 67), (400, 95), (900, 95)];
    for (gap, want) in steps {
        assert_eq!(duel_gain(gap), want, "gap {gap}");
    }
}

#[test]
fn match_deltas_duel_is_zero_sum_staircase() {
    let g = [vec![1], vec![2]];
    assert_eq!(deltas(&g, &ratings(&[(1, 800), (2, 800)]), 1.0), [(1, 49), (2, -49)]);
    assert_eq!(deltas(&g, &ratings(&[(1, 800), (2, 1200)]), 1.0), [(1, 95), (2, -95)]); // upset
    assert_eq!(deltas(&g, &ratings(&[(1, 1200), (2, 800)]), 1.0), [(1, 16), (2, -16)]);
}

#[test]
fn match_deltas_averages_over_opponents() {
    let r = flat(1..=4, 800);
    assert_eq!(deltas(&[vec![1], vec![2], vec![3]], &r, 1.0), [(1, 49), (2, 0), (3, -49)]);
    assert_eq!(deltas(&[vec![1], vec![2], vec![3], vec![4]], &r, 1.0), [(1, 49), (2, 16), (3, -16), (4, -49)]);
}

#[test]
fn match_deltas_ties_move_nothing_between_the_tied() {
    let r = flat(1..=3, 800);
    assert_eq!(deltas(&[vec![1], vec![2, 3]], &r, 1.0), [(1, 49), (2, -25), (3, -25)]); // -24.5 rounds away from 0
    assert_eq!(deltas(&[vec![1, 2]], &flat(1..=2, 800), 1.0), [(1, 0), (2, 0)]);
}

#[test]
fn match_deltas_equal_ratings_sum_to_zero() {
    for n in 2..=10i64 {
        let groups: Vec<Vec<i64>> = (0..n).map(|i| vec![i]).collect();
        let d = deltas(&groups, &flat(0..=n - 1, 1500), 1.0);
        assert_eq!(d.len() as i64, n);
        assert_eq!(d.iter().map(|x| x.1).sum::<i64>(), 0);
        assert_eq!(d[0].1, 49);
        assert_eq!(d[(n - 1) as usize].1, -49);
    }
}

#[test]
fn match_deltas_missing_rating_is_an_error_not_a_panic() {
    // Python: KeyError
    assert!(match_deltas(&[vec![1], vec![2]], &ratings(&[(1, 800)]), 1.0).is_err());
}

#[test]
fn match_deltas_needs_two_players() {
    assert!(match_deltas(&[vec![1]], &ratings(&[(1, 800)]), 1.0).is_err());
}

#[test]
fn match_deltas_scale_multiplies_before_rounding() {
    let r = flat(1..=3, 800);
    assert_eq!(deltas(&[vec![1], vec![2, 3]], &r, CUSTOM_SCALE), [(1, 74), (2, -37), (3, -37)]); // 73.5, -36.75
    assert_eq!(deltas(&[vec![1], vec![2]], &r, CUSTOM_SCALE), [(1, 74), (2, -74)]);
    assert_eq!(deltas(&[vec![1], vec![2]], &r, 1.0), [(1, 49), (2, -49)]);
    // Python: match_deltas([[1],[2,3],[4]], {1:800,2:900,3:1200,4:700}, 1.5)
    let r = ratings(&[(1, 800), (2, 900), (3, 1200), (4, 700)]);
    assert_eq!(deltas(&[vec![1], vec![2, 3], vec![4]], &r, 1.5), [(1, 97), (2, -8), (3, -40), (4, -49)]);
    // Python: match_deltas([[1],[2],[3]], {1:800,2:1300,3:2000})
    let r = ratings(&[(1, 800), (2, 1300), (3, 2000)]);
    assert_eq!(deltas(&[vec![1], vec![2], vec![3]], &r, 1.0), [(1, 95), (2, 0), (3, -95)]);
    // Python: match_deltas([[1,2],[3],[4],[5]], {1:800,2:900,3:1200,4:700,5:1100}, 1.5)
    let r = ratings(&[(1, 800), (2, 900), (3, 1200), (4, 700), (5, 1100)]);
    assert_eq!(deltas(&[vec![1, 2], vec![3], vec![4], vec![5]], &r, 1.5), [(1, 79), (2, 63), (3, -36), (4, -1), (5, -104)]);
}

#[test]
fn unsolved_keys_attempted_not_solved_newest_first() {
    let subs = [
        s("1C", Some("WRONG_ANSWER"), 5),
        s("1A", Some("OK"), 4),
        s("1B", Some("WRONG_ANSWER"), 3),
        s("1C", Some("TIME_LIMIT_EXCEEDED"), 2),
        s("1A", Some("WRONG_ANSWER"), 1),
    ];
    assert_eq!(unsolved_keys(&subs), ["1C", "1B"]);
    assert!(unsolved_keys(&[]).is_empty());
}

#[test]
fn rating_series_accumulates_from_the_start_rating() {
    assert_eq!(rating_series(&[(10, 49), (20, -16), (30, 0)]), [(10, 849), (20, 833), (30, 833)]);
    assert!(rating_series(&[]).is_empty());
}

#[test]
fn ordinal_cases() {
    let got: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 102, 111].map(ordinal).to_vec();
    assert_eq!(got, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "102nd", "111th"]);
    // Python (floor modulo): [ordinal(n) for n in (-1,-2,-11,-12,-21,-13,0,20,100,110,120)]
    let got: Vec<String> = [-1, -2, -11, -12, -21, -13, 0, 20, 100, 110, 120].map(ordinal).to_vec();
    assert_eq!(got, ["-1th", "-2th", "-11th", "-12th", "-21th", "-13th", "0th", "20th", "100th", "110th", "120th"]);
}

#[test]
fn placement_by_rating_change() {
    let d = [49, 16, -16, -49];
    assert_eq!(d.map(|x| placement(&d, x)), [Some(1), Some(2), Some(3), Some(4)]);
    assert_eq!(placement(&[-25, 49, -25], -25), Some(2)); // ties share a place
    assert_eq!(placement(&[0, 0], 0), None); // nobody solved
}

#[test]
fn first_solve_counts_wrong_attempts_before_the_accept_inside_the_window() {
    let subs = [
        s("1A", Some("WRONG_ANSWER"), 90),
        s("1A", Some("WRONG_ANSWER"), 150),
        s("1A", Some("COMPILATION_ERROR"), 160),
        s("1A", Some("TIME_LIMIT_EXCEEDED"), 170),
        s("1A", None, 175),
        s("1A", Some("OK"), 200),
        s("1A", Some("WRONG_ANSWER"), 210),
        s("1A", Some("OK"), 300),
        s("2A", Some("OK"), 5000),
    ];
    assert_eq!(first_solve(&subs, "1A", 100, 1000), Some((200, 2)));
    assert_eq!(first_solve(&subs, "2A", 100, 1000), None);
    assert_eq!(first_solve(&subs, "1A", 100, 150), None);
    assert_eq!(first_solve(&[s("1A", Some("OK"), 100)], "1A", 100, 100), Some((100, 0)));
}

#[test]
fn format_hm_cases() {
    let got: Vec<String> = [0, 59, 60, 3599, 3600, 7260].map(format_hm).to_vec();
    assert_eq!(got, ["0:00", "0:00", "0:01", "0:59", "1:00", "2:01"]);
    // Python (floor division): [format_hm(s) for s in (-1,-60,-3600,-3601,359999)]
    let got: Vec<String> = [-1, -60, -3600, -3601, 359999].map(format_hm).to_vec();
    assert_eq!(got, ["-1:59", "-1:59", "-1:00", "-2:59", "99:59"]);
}

#[test]
fn recaps_due_window_and_sent() {
    let now = 1_000_000;
    let done = cn(1, "a", "FINISHED", Some(now - 7200 - 60), 7200); // ended 60 s ago
    let older = cn(2, "b", "FINISHED", Some(now - 7200 - 600), 7200);
    let stale = cn(3, "c", "FINISHED", Some(now - 7200 - RECAP_WINDOW_SECONDS - 1), 7200);
    let running = cn(4, "d", "CODING", Some(now - 100), 7200);
    let all = [done.clone(), older.clone(), stale, running];
    assert_eq!(ids(&recaps_due(&all, &set([]), now)), [2, 1]);
    assert_eq!(ids(&recaps_due(&[done, older], &set([2]), now)), [1]);
}

#[test]
fn contest_suggestions_lists_newest_finished_known_contests() {
    let cs = [
        cn(1, "Round One", "FINISHED", Some(100), 7200),
        cn(2, "Educational Two", "FINISHED", Some(300), 7200),
        cn(3, "Round Three", "BEFORE", Some(900), 7200),
        cn(4, "Round Four", "FINISHED", Some(200), 7200),
        cn(5, "Unknown problems", "FINISHED", Some(400), 7200),
    ];
    let known = set([1, 2, 3, 4]);
    let pair = |i: i64, n: &str| (format!("{i} - {n}"), i);
    assert_eq!(
        contest_suggestions(&cs, &known, "", 25),
        [pair(2, "Educational Two"), pair(4, "Round Four"), pair(1, "Round One")]
    );
    assert_eq!(contest_suggestions(&cs, &known, "ROUND", 25), [pair(4, "Round Four"), pair(1, "Round One")]);
    assert_eq!(contest_suggestions(&cs, &known, "2", 25), [pair(2, "Educational Two")]);
    assert_eq!(contest_suggestions(&cs, &known, "", 1), [pair(2, "Educational Two")]);
    // the label is cut at 100 code points
    let long = [cn(7, &"ж".repeat(150), "FINISHED", Some(1), 1)];
    let got = contest_suggestions(&long, &set([7]), "", 25);
    assert_eq!(got[0].0.chars().count(), 100);
}

#[test]
fn week_start_is_sunday_midnight_utc() {
    let sunday = 1_700_000_000 / 86400 * 86400 - 2 * 86400; // 2023-11-12
    assert_eq!(week_start(sunday), sunday);
    assert_eq!(week_start(sunday + 6 * 86400 + 86399), sunday);
    assert_eq!(week_start(sunday + 7 * 86400), sunday + 7 * 86400);
    // Python, negative times (floor division): week_start of (-1,-86400,-4*86400,-4*86400-1,-7*86400,0,3*86400,4*86400,4*86400+86399)
    let got = [-1, -86400, -4 * 86400, -4 * 86400 - 1, -7 * 86400, 0, 3 * 86400, 4 * 86400, 4 * 86400 + 86399].map(week_start);
    assert_eq!(got, [-345600, -345600, -345600, -950400, -950400, -345600, 259200, 259200, 259200]);
    assert_eq!([-1, -86400, -86401].map(utc_day), [-1, -1, -2]);
}

#[test]
fn weekly_board_ranks_gain_and_moves() {
    let week = 14 * DAY_SECONDS;
    let ev = |u: i64, h: &str, day: i64, d: i64| (u, h.to_string(), day * DAY_SECONDS, d);
    let events = [ev(1, "a", 1, 10), ev(2, "b", 2, 5), ev(2, "b", 8, 20), ev(3, "c", 9, 1), ev(1, "a", 14, 99)];
    let rows = weekly_board(&events, week);
    let got: Vec<_> = rows.iter().map(|r| (r.rank, r.user_id, r.rating, r.gain, r.moved)).collect();
    assert_eq!(
        got,
        [
            (1, 2, START_RATING + 25, 20, Some(1)),
            (2, 1, START_RATING + 10, 0, Some(-1)),
            (3, 3, START_RATING + 1, 1, None)
        ]
    );
    assert!(weekly_board(&[], week).is_empty());
    // full tie on rating, games and handle: first-seen user id ranks first (Python stable sort over dict order)
    let tie = [ev(9, "z", 1, 5), ev(4, "z", 1, 5)];
    assert_eq!(weekly_board(&tie, week).iter().map(|r| r.user_id).collect::<Vec<_>>(), [9, 4]);
}

#[test]
fn daily_streak_cases() {
    // Python: daily_streak([10,8,7,3],10), ([10,9,6],12), ([10],13), ([],1), ([5,2],5) -> 3 2 0 0 1
    assert_eq!(daily_streak(&[10, 8, 7, 3], 10), 3);
    assert_eq!(daily_streak(&[10, 9, 6], 12), 2);
    assert_eq!(daily_streak(&[10], 13), 0);
    assert_eq!(daily_streak(&[], 1), 0);
    assert_eq!(daily_streak(&[5, 2], 5), 1);
}

#[test]
fn new_contests_skips_posted_started_and_finished() {
    let cs = [
        c(1, "BEFORE", Some(900)),
        c(2, "BEFORE", Some(700)),
        c(3, "BEFORE", Some(500)),
        c(4, "FINISHED", Some(900)),
        c(5, "BEFORE", Some(100)),
        c(6, "BEFORE", None),
    ];
    assert_eq!(ids(&new_contests(&cs, &set([3]), 200)), [2, 1]);
}

#[test]
fn soon_contests_only_within_window_and_not_done() {
    const H: i64 = 3600;
    let cs = [
        c(1, "BEFORE", Some(100 + 47 * H)),
        c(2, "BEFORE", Some(100 + 49 * H)),
        c(3, "BEFORE", Some(100 + 2 * H)),
        c(4, "BEFORE", Some(100)),
        c(5, "FINISHED", Some(100 + H)),
    ];
    assert_eq!(ids(&soon_contests(&cs, &set([3]), 100, NEW_CONTEST_SOON_SECONDS)), [1]);
}

#[test]
fn contest_ics_has_utc_times_alarm_and_escaped_text() {
    let ics = contest_ics(&cn(9, "A, B; C", "BEFORE", Some(1_700_000_000), 7200), 1_699_990_000, 10).unwrap();
    let lines: Vec<&str> = ics.split("\r\n").collect();
    assert_eq!(lines[0], "BEGIN:VCALENDAR");
    assert_eq!(lines[lines.len() - 2], "END:VCALENDAR");
    assert_eq!(lines[lines.len() - 1], "");
    for want in ["DTSTART:20231114T221320Z", "DTEND:20231115T001320Z", "SUMMARY:A\\, B\\; C", "TRIGGER:-PT10M"] {
        assert!(lines.contains(&want), "{want}");
    }
}

/// Expected values from Python 3.12 `str.casefold()`.
#[test]
fn casefold_matches_python() {
    assert_eq!(casefold("ẞtraße"), "sstrasse");
    assert_eq!(casefold("ΑΣ"), "ασ"); // lower() would give the final sigma "ας"
    assert_eq!(casefold("ꭰ"), "Ꭰ"); // Cherokee folds to upper case
    assert_eq!(casefold("İ"), "i\u{307}");
    assert_eq!(casefold("ﬃ"), "ffi");
    assert_eq!(casefold("ǅ"), "ǆ");
    assert_eq!(casefold("Tourist_99"), "tourist_99");
}

/// Python `re.fullmatch(r"\d+", s)` then `int(s)`.
#[test]
fn parse_digits_matches_python() {
    assert_eq!(parse_digits("٣٤"), Some(34));
    assert_eq!(parse_digits("१२३"), Some(123));
    assert_eq!(parse_digits("１２"), Some(12));
    assert_eq!(parse_digits("9".repeat(30).as_str()), Some(i64::MAX)); // past i64: saturates
    for bad in ["٣a", "", "+5", " 5", "5_0", "a"] {
        assert_eq!(parse_digits(bad), None, "{bad:?}");
    }
}
