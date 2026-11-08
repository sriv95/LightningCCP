//! Port of tests/test_bingo.py (logic tests; the MashupService ones belong to the service stage) and the
//! mashup tests of tests/test_logic.py. Picks are checked for validity, not equality with Python's pick.
//! Expected values of the extra cases come from `.venv/bin/python -I`.

#![allow(clippy::type_complexity, clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};

use lightningccp::cf::models::Problem;
use lightningccp::logic::mashup::*;
use rand::SeedableRng;
use rand::rngs::StdRng;

fn p(cid: i64, idx: &str, rating: Option<i64>, tags: &[&str]) -> Problem {
    Problem { contest_id: cid, index: idx.into(), name: format!("p{cid}{idx}"), rating, tags: tags.iter().map(|t| t.to_string()).collect() }
}

fn set(items: &[&str]) -> HashSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn solves(items: &[(i64, &[(&str, (i64, i64))])]) -> Solves {
    items.iter().map(|(u, v)| (*u, v.iter().map(|(k, f)| (k.to_string(), *f)).collect())).collect()
}

fn mashup_pool() -> Vec<Problem> {
    let mut v: Vec<Problem> = (800..3600).step_by(100).enumerate().map(|(i, r)| p(i as i64 + 1, "A", Some(r), &[])).collect();
    v.push(p(99, "A", Some(800), &[]));
    v.push(p(97, "A", None, &[]));
    v
}

fn pick(pool: &[Problem], min: i64, max: i64, count: i64, solved: &HashSet<String>, seed: u64, tags: &[&str], ex: &[&str]) -> Option<Vec<Problem>> {
    pick_mashup(pool, min, max, count, solved, &mut StdRng::seed_from_u64(seed), &set(tags), &set(ex)).unwrap()
}

#[test]
fn chunked_splits_and_rejects_zero() {
    assert_eq!(chunked(&[1, 2, 3, 4, 5], 2).unwrap(), vec![vec![1, 2], vec![3, 4], vec![5]]);
    assert!(chunked::<i32>(&[], 3).unwrap().is_empty());
    assert!(chunked(&[1], 0).is_err()); // Python: ValueError from range(step 0)
}

#[test]
fn pick_mashup_spreads_ratings_easy_to_hard() {
    for seed in 0..20 {
        let picked = pick(&mashup_pool(), 800, 1400, 4, &HashSet::new(), seed, &[], &[]).unwrap();
        assert_eq!(picked.iter().map(|p| p.rating.unwrap()).collect::<Vec<_>>(), [800, 1000, 1200, 1400]);
        assert_eq!(picked.iter().map(|p| p.key()).collect::<HashSet<_>>().len(), 4);
    }
}

#[test]
fn pick_mashup_repeated_rating_picks_distinct_problems() {
    for seed in 0..10 {
        let picked = pick(&mashup_pool(), 800, 800, 2, &HashSet::new(), seed, &[], &[]).unwrap();
        assert_eq!(picked.iter().map(|p| p.key()).collect::<HashSet<_>>(), set(&["1A", "99A"]));
    }
}

#[test]
fn pick_mashup_single_problem_aims_at_the_middle_and_skips_solved() {
    for seed in 0..10 {
        assert_eq!(pick(&mashup_pool(), 800, 1200, 1, &HashSet::new(), seed, &[], &[]).unwrap()[0].rating, Some(1000));
        let r = pick(&mashup_pool(), 800, 1200, 1, &set(&["3A"]), seed, &[], &[]).unwrap()[0].rating.unwrap();
        assert!(r == 900 || r == 1100);
    }
}

#[test]
fn pick_mashup_fails_when_the_range_is_too_small() {
    let pool = vec![p(1, "A", Some(800), &[]), p(2, "A", Some(900), &[])];
    let none = HashSet::new();
    assert!(pick(&pool, 800, 900, 3, &none, 1, &[], &[]).is_none());
    assert!(pick(&pool, 800, 900, 2, &set(&["1A"]), 1, &[], &[]).is_none());
    assert!(pick(&pool, 1000, 1100, 1, &none, 1, &[], &[]).is_none());
}

#[test]
fn pick_mashup_respects_tags_and_never_picks_solved() {
    let pool: Vec<Problem> = (0..6).map(|i| p(i, "A", Some(800 + 100 * i), &[if i % 2 == 1 { "dp" } else { "math" }])).collect();
    let none = HashSet::new();
    for seed in 0..10 {
        let picked = pick(&pool, 800, 1300, 3, &none, seed, &["dp"], &[]).unwrap();
        assert!(picked.iter().all(|p| p.contest_id % 2 == 1));
        let picked = pick(&pool, 800, 1300, 3, &none, seed, &[], &["dp"]).unwrap();
        assert!(picked.iter().all(|p| p.contest_id % 2 == 0));
        let picked = pick(&pool, 800, 1300, 1, &set(&["0A", "2A"]), seed, &[], &["dp"]);
        assert_eq!(picked.unwrap().iter().map(|p| p.key()).collect::<Vec<_>>(), ["4A"]);
    }
    assert!(pick(&pool, 800, 1300, 4, &none, 1, &["dp"], &[]).is_none()); // only 3 have dp
}

#[test]
fn pick_mashup_count_zero_is_empty_and_overflow_is_an_error() {
    // Python: no targets, so []
    assert_eq!(pick(&mashup_pool(), 800, 900, 0, &HashSet::new(), 1, &[], &[]), Some(vec![]));
    let mut rng = StdRng::seed_from_u64(1);
    let e = pick_mashup(&mashup_pool(), i64::MIN, i64::MAX, 3, &HashSet::new(), &mut rng, &set(&[]), &set(&[]));
    assert!(e.is_err());
}

#[test]
fn team_solves_keeps_the_first_solver_and_empty_teams() {
    let members = [(1, 1), (2, 1), (3, 2), (9, 3)];
    let s = solves(&[(1, &[("A", (50, 2)), ("B", (70, 0))]), (2, &[("A", (50, 1)), ("B", (80, 0))]), (3, &[("C", (5, 0))])]);
    // from Python: {1: {A: (50,1), B: (70,0)}, 2: {C: (5,0)}, 3: {}}
    let want = solves(&[(1, &[("A", (50, 1)), ("B", (70, 0))]), (2, &[("C", (5, 0))]), (3, &[])]);
    assert_eq!(team_solves(&members, &s), want);
}

fn players(v: &[(i64, &str, i64)]) -> Vec<(i64, String, i64)> {
    v.iter().map(|(a, b, c)| (*a, b.to_string(), *c)).collect()
}

#[test]
fn mashup_standings_rank_solved_then_time_with_shared_ranks() {
    let pl = players(&[(1, "a", 0), (2, "b", 1), (3, "c", 2), (4, "d", 3), (5, "e", 4)]);
    let s = solves(&[
        (1, &[("A", (160, 0)), ("B", (400, 0))]),
        (2, &[("A", (150, 0)), ("B", (410, 0))]),
        (3, &[("A", (100, 0))]),
        (4, &[("A", (100, 0)), ("B", (1000, 0)), ("C", (1100, 0))]),
    ]);
    let rows = mashup_standings(&pl, &s, 100, 0).unwrap();
    let got: Vec<_> = rows.iter().map(|r| (r.user_id, r.rank, r.solved, r.total_seconds)).collect();
    assert_eq!(got, [(4, 1, 3, 1900), (1, 2, 2, 360), (2, 2, 2, 360), (3, 4, 1, 0), (5, 5, 0, 0)]);
    assert_eq!(rows[1].times, HashMap::from([("A".to_string(), 60), ("B".to_string(), 300)]));
}

#[test]
fn mashup_standings_penalty_per_wrong_attempt() {
    let pl = players(&[(1, "a", 0), (2, "b", 1)]);
    let s = solves(&[(1, &[("A", (160, 2))]), (2, &[("A", (200, 0))])]);
    let rows = mashup_standings(&pl, &s, 100, 20).unwrap();
    assert_eq!(rows.iter().map(|r| (r.handle.as_str(), r.total_seconds)).collect::<Vec<_>>(), [("b", 100), ("a", 60 + 2 * 20 * 60)]);
    assert_eq!(rows[1].wrong, HashMap::from([("A".to_string(), 2)]));
    let free = mashup_standings(&pl, &s, 100, 0).unwrap();
    assert_eq!(free.iter().map(|r| r.handle.as_str()).collect::<Vec<_>>(), ["a", "b"]);
}

#[test]
fn mashup_standings_clamp_early_solves_and_break_ties_by_join_time() {
    // from Python: b and a tie (1 solved, 50 s) and keep join order b, a; c solved before the start: 0 s + 180 s penalty
    let pl = players(&[(1, "a", 5), (2, "b", 3), (3, "c", 9), (4, "d", 1)]);
    let s = solves(&[(1, &[("A", (150, 0))]), (2, &[("A", (150, 0))]), (3, &[("A", (50, 1))]), (4, &[])]);
    let rows = mashup_standings(&pl, &s, 100, 3).unwrap();
    let got: Vec<_> = rows.iter().map(|r| (r.user_id, r.rank, r.total_seconds)).collect();
    assert_eq!(got, [(2, 1, 50), (1, 1, 50), (3, 3, 180), (4, 4, 0)]);
    assert_eq!(rows[2].times["A"], 0);
    // overflow is an error, not a wrap
    let big = solves(&[(1, &[("A", (i64::MAX, 0))])]);
    assert!(mashup_standings(&players(&[(1, "a", 0)]), &big, i64::MIN, 0).is_err());
}

#[test]
fn bingo_lines_cover_rows_columns_and_both_diagonals() {
    let lines = bingo_lines(3);
    assert_eq!(lines.len(), 8);
    for l in [vec![0, 1, 2], vec![0, 3, 6], vec![0, 4, 8], vec![2, 4, 6]] {
        assert!(lines.contains(&l));
    }
    assert!(bingo_lines(7).len() == 16 && bingo_lines(7).contains(&vec![6, 12, 18, 24, 30, 36, 42]));
    // from Python: size 4 and size 1 (four copies of cell 0)
    assert_eq!(bingo_lines(4)[8..], [vec![0, 5, 10, 15], vec![3, 6, 9, 12]]);
    assert_eq!(bingo_lines(1), vec![vec![0]; 4]);
}

fn cells(n: i64) -> Vec<(String, i64, i64)> {
    (0..n).map(|i| (format!("{i}A"), 800 + 100 * i, i)).collect()
}

fn claims(v: &[(&str, (i64, i64))]) -> HashMap<String, (i64, i64)> {
    v.iter().map(|(k, c)| (k.to_string(), *c)).collect()
}

#[test]
fn classic_board_stops_at_the_first_full_line() {
    let s = solves(&[
        (1, &[("0A", (10, 0)), ("1A", (20, 0)), ("2A", (30, 0))]),
        (2, &[("3A", (5, 0)), ("4A", (6, 0)), ("5A", (40, 0)), ("6A", (35, 0))]),
    ]);
    let b = bingo_board(3, "classic", "line", &cells(9), &s).unwrap();
    assert!(b.over && b.lines == HashMap::from([(1, 1)]));
    assert!(!b.claims.contains_key("6A") && !b.claims.contains_key("5A"));
    assert_eq!(b.squares[0], ("0A".to_string(), 800, Some(1)));
    assert_eq!(b.squares[8], ("8A".to_string(), 1600, None));
}

#[test]
fn the_earliest_solve_claims_a_square() {
    let s = solves(&[(1, &[("0A", (20, 0))]), (2, &[("0A", (10, 3))])]);
    assert_eq!(bingo_board(3, "classic", "line", &cells(9), &s).unwrap().claims, claims(&[("0A", (2, 10))]));
}

#[test]
fn continuous_board_ends_only_when_full_and_counts_lines() {
    let mut s = solves(&[(1, &[("0A", (0, 0)), ("1A", (1, 0)), ("2A", (2, 0))]), (2, &[("3A", (13, 0)), ("4A", (14, 0))])]);
    let b = bingo_board(3, "classic", "continuous", &cells(9), &s).unwrap();
    assert!(!b.over && b.lines == HashMap::from([(1, 1)]));
    for i in 5..9 {
        s[1].1.push((format!("{i}A"), (20 + i, 0)));
    }
    assert!(bingo_board(3, "classic", "continuous", &cells(9), &s).unwrap().over);
}

fn replace_rows() -> Vec<(String, i64, i64)> {
    let mut r = cells(9);
    r.push(("9A".into(), 900, 0)); // cell 0 was replaced by 9A
    r
}

#[test]
fn replace_board_lets_another_team_take_a_square_over() {
    let s = solves(&[(1, &[("0A", (10, 0)), ("1A", (12, 0))]), (2, &[("9A", (50, 1)), ("0A", (60, 0))])]);
    let b = bingo_board(3, "replace", "line", &replace_rows(), &s).unwrap();
    assert!(!b.over);
    assert_eq!(b.claims, claims(&[("0A", (1, 10)), ("1A", (1, 12)), ("9A", (2, 50))]));
    assert_eq!(b.holders, HashMap::from([(0, (2, 50)), (1, (1, 12))]));
    assert_eq!(b.squares[0], ("9A".to_string(), 900, Some(2)));
    assert_eq!(b.squares[1], ("1A".to_string(), 900, Some(1)));
    assert_eq!(b.squares[2], ("2A".to_string(), 1000, None));
}

#[test]
fn replace_line_must_be_held_when_the_last_square_is_claimed() {
    let mut s = solves(&[(1, &[("0A", (10, 0)), ("1A", (11, 0)), ("2A", (30, 0))]), (2, &[("9A", (20, 0))])]);
    let b = bingo_board(3, "replace", "line", &replace_rows(), &s).unwrap();
    assert!(!b.over && b.lines.is_empty());
    s[0].1.push(("9A".into(), (15, 0)));
    assert!(bingo_board(3, "replace", "line", &replace_rows(), &s).unwrap().over);
}

#[test]
fn bingo_board_errors_on_a_missing_cell_and_team_zero_holds_empty_cells() {
    // Python: KeyError from latest[c]
    assert!(bingo_board(3, "classic", "line", &cells(8), &Vec::new()).is_err());
    // from Python: team 0 "holds" every line through the unheld cells, so lines == {0: 8} and, with win "line", over
    let s = solves(&[(0, &[("0A", (5, 0))])]);
    let b = bingo_board(3, "classic", "continuous", &cells(9), &s).unwrap();
    assert_eq!((b.lines, b.over), (HashMap::from([(0, 8)]), false));
    assert!(bingo_board(3, "classic", "line", &cells(9), &s).unwrap().over);
}

#[test]
fn bingo_board_same_second_tie_follows_insertion_order() {
    // from Python (.venv/bin/python -I): claims == {2A, 0A, 5A, 1A} all (team 1, t 10); key order would stop at 2A and skip 5A
    let s = solves(&[(1, &[("2A", (10, 0)), ("0A", (10, 0)), ("5A", (10, 0)), ("1A", (10, 0))])]);
    let b = bingo_board(3, "classic", "line", &cells(9), &s).unwrap();
    assert_eq!(b.claims, claims(&[("2A", (1, 10)), ("0A", (1, 10)), ("5A", (1, 10)), ("1A", (1, 10))]));
    assert!(b.over);
}

#[test]
fn bingo_standings_rank_by_lines_then_squares_then_time() {
    let s = solves(&[(1, &[("0A", (10, 0)), ("1A", (20, 0))]), (2, &[("3A", (5, 0)), ("4A", (15, 0))]), (3, &[]), (4, &[])]);
    let b = bingo_board(3, "classic", "continuous", &cells(9), &s).unwrap();
    let ranks = bingo_standings(&players(&[(1, "a", 1), (2, "b", 2), (3, "c", 3), (4, "d", 4)]), &b, 0).unwrap();
    let got: Vec<_> = ranks.iter().map(|r| (r.user_id, r.rank, r.solved)).collect();
    assert_eq!(got, [(2, 1, 2), (1, 2, 2), (3, 3, 0), (4, 3, 0)]);
    assert_eq!(ranks[0].total_seconds, 15);
}

#[test]
fn bingo_standings_tie_on_score_shares_rank_and_clamps_time() {
    // from Python: teams 3 (t=8, before start 20), then 2 and 1 tied (t=10, join order), then 4 without squares
    let s = solves(&[(1, &[("0A", (10, 0))]), (2, &[("1A", (10, 0))]), (3, &[("2A", (8, 0))])]);
    let b = bingo_board(3, "classic", "continuous", &cells(9), &s).unwrap();
    let r = bingo_standings(&players(&[(1, "a", 5), (2, "b", 3), (3, "c", 9), (4, "d", 1)]), &b, 20).unwrap();
    let got: Vec<_> = r.iter().map(|r| (r.user_id, r.rank, r.solved, r.total_seconds, r.lines)).collect();
    assert_eq!(got, [(3, 1, 1, 0, 0), (2, 2, 1, 0, 0), (1, 2, 1, 0, 0), (4, 4, 0, 0, 0)]);
}

fn pool3() -> Vec<Problem> {
    vec![p(1, "A", Some(900), &[]), p(2, "A", Some(1100), &[]), p(3, "A", Some(3500), &[])]
}

fn repl(pool: &[Problem], rating: i64, used: &[&str], max: i64, seed: u64, tags: &[&str], ex: &[&str]) -> Option<String> {
    pick_replacement(pool, rating, &set(used), max, &mut StdRng::seed_from_u64(seed), &set(tags), &set(ex)).map(|p| p.key())
}

#[test]
fn pick_replacement_goes_100_up_then_higher_then_same_at_the_top() {
    let pool = pool3();
    let top = 3500;
    assert_eq!(repl(&pool, 800, &[], top, 1, &[], &[]).as_deref(), Some("1A"));
    assert_eq!(repl(&pool, 900, &[], top, 1, &[], &[]).as_deref(), Some("2A")); // 1000 has none
    assert_eq!(repl(&pool, 1100, &["2A"], top, 1, &[], &[]).as_deref(), Some("3A"));
    assert_eq!(repl(&pool, 3500, &[], top, 1, &[], &[]).as_deref(), Some("3A")); // the top: the same rating
    assert_eq!(repl(&pool, 3500, &["3A"], top, 1, &[], &[]), None);
    // Never above max_rating: 1100 is too high for a 1000 cap, so the same rating again.
    assert_eq!(repl(&pool, 900, &[], 1000, 1, &[], &[]).as_deref(), Some("1A"));
    assert_eq!(repl(&pool, 900, &["1A"], 1000, 1, &[], &[]), None);
}

#[test]
fn pick_replacement_picks_from_the_pool_and_honours_tags() {
    let pool: Vec<Problem> = (0..10).map(|i| p(i, "A", Some(1000), &[if i % 2 == 0 { "dp" } else { "math" }])).collect();
    let mut seen = HashSet::new();
    for seed in 0..40 {
        let k = repl(&pool, 900, &["0A", "1A"], 3500, seed, &["dp"], &["geometry"]).unwrap();
        assert!(["2A", "4A", "6A", "8A"].contains(&k.as_str()));
        seen.insert(k);
        let k = repl(&pool, 900, &[], 3500, seed, &[], &["dp"]).unwrap();
        assert!(k.len() == 2 && k.starts_with(['1', '3', '5', '7', '9']));
    }
    assert!(seen.len() > 1);
    assert_eq!(repl(&pool, 900, &[], 3500, 1, &["geometry"], &[]), None);
}
