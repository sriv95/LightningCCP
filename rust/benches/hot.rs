//! Per-item cost of the hot pure functions. The same inputs, built by the same formulas, are in
//! `rust/bench/py_hot.py`; keep the two in step.
use std::collections::{HashMap, HashSet};
use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lightningccp::cf::models::{Contest, Problem};
use lightningccp::cogs::contests::{check_regex, not_excluded};
use lightningccp::logic::base::{ProblemFilter, casefold, contest_ics, filter_problems, match_deltas};
use lightningccp::logic::mashup::{Solves, bingo_board, mashup_standings};
use lightningccp::store::base::Store;
use lightningccp::ui::embeds::a::{RankEntry, contest_pages, esc, ranklist_pages};
use rand::seq::IndexedRandom;
use rand::{SeedableRng, rngs::StdRng};

const TAGS: [&str; 8] = ["dp", "math", "greedy", "graphs", "geometry", "strings", "trees", "sortings"];

fn problems(n: usize) -> Vec<Problem> {
    (0..n)
        .map(|i| Problem {
            contest_id: 1 + (i / 6) as i64,
            index: ["A", "B", "C", "D", "E", "F"][i % 6].to_string(),
            name: format!("Problem {i}"),
            rating: if i % 13 == 0 { None } else { Some(800 + ((i as i64 * 7 + i as i64 / 28) % 28) * 100) },
            tags: (0..1 + i % 3).map(|k| TAGS[(i * (k + 1) + k) % 8].to_string()).collect(),
        })
        .collect()
}

fn contests(n: usize) -> Vec<Contest> {
    (0..n)
        .map(|i| Contest {
            id: 1000 + i as i64,
            name: match i % 4 {
                0 => format!("Codeforces Round #{i} (Div. {})", 1 + i % 2),
                1 => format!("Educational Codeforces Round {i}"),
                2 => format!("Kotlin Heroes: Episode {i}"),
                _ => format!("CodeTON Round {i}, Div. 1 + Div. 2; rated"),
            },
            kind: "CF".into(),
            phase: "BEFORE".into(),
            start_time: Some(1_700_000_000 + i as i64 * 86400),
            duration: 7200,
        })
        .collect()
}

fn solves(players: i64, n_problems: i64) -> Solves {
    (1..=players)
        .map(|u| {
            let mine = (0..n_problems).filter(|p| (u + p) % 3 != 0).map(|p| (format!("{}A", 1000 + p), (1_000_000 + u * 37 + p * 600, (u + p) % 3))).collect();
            (u, mine)
        })
        .collect()
}

fn hot(c: &mut Criterion) {
    let typical = "Codeforces Round #900 (Div. 2) *bold* _under_ [x]";
    let long = "Привет мир_ *звезда* `код` ~tilde~ https://example.com/a_b_c ‖ 日本語 ".repeat(50);
    c.bench_function("esc typical", |b| b.iter(|| esc(black_box(typical))));
    c.bench_function("esc long unicode", |b| b.iter(|| esc(black_box(&long))));

    let list = contests(200);
    let ics = Contest { name: "CodeTON Round 7, Div. 1 + Div. 2; rated".into(), ..list[3].clone() };
    c.bench_function("contest_ics", |b| b.iter(|| contest_ics(black_box(&ics), 1_699_000_000, 10).unwrap()));

    let keep: HashSet<i64> = [1000, 1001].into();
    // cold: validate and compile on every call (a /remind exclude command); the loop never does this
    let pat = r"  div\. ?[12]|kotlin|^educational  ";
    c.bench_function("regex cold: check_regex + not_excluded (200 contests)", |b| {
        b.iter(|| {
            let p = check_regex(black_box(pat)).unwrap();
            not_excluded(black_box(&list), &p, &keep).unwrap()
        })
    });
    // warm: the reminder loop compiles once (LoopState.excludes) and only matches names afterwards. The compiled
    // regex type is private to cogs::contests, so build the same one here (the pattern needs no translation).
    let rx = fancy_regex::RegexBuilder::new(pat.trim()).case_insensitive(true).build().unwrap();
    assert_eq!(
        list.iter().filter(|c| keep.contains(&c.id) || !rx.is_match(&c.name).unwrap_or(false)).map(|c| c.id).collect::<Vec<_>>(),
        not_excluded(&list, pat.trim(), &keep).unwrap().iter().map(|c| c.id).collect::<Vec<_>>()
    );
    c.bench_function("regex warm: not_excluded, compiled once (200 contests)", |b| {
        b.iter(|| {
            black_box(&list).iter().filter(|c| keep.contains(&c.id) || !rx.is_match(&c.name).unwrap_or(false)).cloned().collect::<Vec<_>>()
        })
    });

    let text = "Straße ΣΊΣΥΦΟΣ ǅ Hello World ß İstanbul";
    c.bench_function("casefold", |b| b.iter(|| casefold(black_box(text))));

    let set = problems(10_000);
    let f = ProblemFilter {
        min_rating: Some(1200),
        max_rating: Some(1600),
        tags: ["dp".to_string()].into(),
        exclude_tags: ["geometry".to_string()].into(),
        solved: set.iter().step_by(20).map(Problem::key).collect(),
        contest_id: None,
    };
    c.bench_function("filter_problems (10k)", |b| b.iter(|| filter_problems(black_box(&set), &f)));
    let mut rng = StdRng::seed_from_u64(7);
    c.bench_function("gimme: filter + pick (10k)", |b| {
        b.iter(|| filter_problems(black_box(&set), &f).choose(&mut rng).cloned())
    });

    c.bench_function("contest_pages (50 contests)", |b| b.iter(|| contest_pages(black_box(&list[..50]), "Upcoming", 10).unwrap()));
    let entries: Vec<RankEntry> = (0..50).map(|i| (i + 1, 100 + i, format!("user_{i}*"), 1000.0 - i as f64 * 7.5, i % 4 * 10)).collect();
    c.bench_function("ranklist_pages (50 entries)", |b| b.iter(|| ranklist_pages(black_box(&entries), "Ranklist", 10).unwrap()));

    let players: Vec<(i64, String, i64)> = (1..=30).map(|u| (u, format!("player{u}"), 1_000_000 + u)).collect();
    let mash = solves(30, 8);
    c.bench_function("mashup_standings (30 players, 8 problems)", |b| {
        b.iter(|| mashup_standings(black_box(&players), &mash, 1_000_000, 5).unwrap())
    });
    let cells: Vec<(String, i64, i64)> = (0..25).map(|i| (format!("{}A", 1000 + i), 800 + i * 100, i)).collect();
    let bingo = solves(6, 25);
    c.bench_function("bingo_board (5x5, 6 teams)", |b| {
        b.iter(|| bingo_board(5, "classic", "line", black_box(&cells), &bingo).unwrap())
    });

    let groups: Vec<Vec<i64>> = vec![vec![1], vec![2, 3], vec![4], vec![5, 6], vec![7], vec![8]];
    let ratings: HashMap<i64, i64> = (1..=8).map(|u| (u, 800 + u * 150)).collect();
    c.bench_function("match_deltas (8 players)", |b| b.iter(|| match_deltas(black_box(&groups), &ratings, 1.5).unwrap()));

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let dir = std::env::temp_dir().join(format!("lccp-bench-{}", std::process::id()));
    let store = rt.block_on(Store::connect(dir.join("bench.db"))).unwrap();
    c.bench_function("store link_handle + get_handle", |b| {
        b.iter(|| {
            rt.block_on(async {
                store.link_handle(1, 42, "tourist", Some(3800)).await.unwrap();
                store.get_handle(1, 42).await.unwrap()
            })
        })
    });
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

criterion_group!(benches, hot);
criterion_main!(benches);
