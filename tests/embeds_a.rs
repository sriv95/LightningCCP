//! Parity tests for src/ui/embeds/a.rs against src/ui/embeds.py lines 1-460. VECTORS were produced by running the
//! Python builders with the same inputs (`discord.Embed.to_dict()` minus the constant `type` and `flags` keys, `escape_markdown`, `f"{x:g}"`); the first ten
//! tests port tests/test_match_embeds.py. Regenerate by rerunning the Python builders on the inputs below.
use std::collections::HashMap;
use std::sync::LazyLock;

use lightningccp::cf::models::{Contest, ContestResult, Problem, User};
use lightningccp::logic::base::WeeklyRow;
use lightningccp::store::mashup::MashupRow;
use lightningccp::store::matches::{DailyRow, MatchPlayer, MatchRow};
use lightningccp::ui::embeds::a::*;

/// `match_embed` for matches the fixtures build with a known mode.
fn match_embed(v: &MatchView) -> EmbedData {
    lightningccp::ui::embeds::a::match_embed(v).unwrap()
}
use serde_json::{Map, Value, json};
use serenity::all::CreateEmbed;

mod common;
const FIXTURE: &str = "embeds_a_vectors.json";
static VEC: LazyLock<Value> = LazyLock::new(|| common::load(FIXTURE));

/// Python `Embed.to_dict()` of our value: empty title, description and url are dropped.
fn dict(e: &EmbedData) -> Value {
    let mut m = Map::new();
    if let Some(c) = e.colour {
        m.insert("color".into(), json!(c));
    }
    for (k, v) in [("title", &e.title), ("description", &e.description), ("url", &e.url)] {
        if let Some(v) = v.as_ref().filter(|v| !v.is_empty()) {
            m.insert(k.into(), json!(v));
        }
    }
    if !e.fields.is_empty() {
        let f: Vec<Value> = e.fields.iter().map(|f| json!({"name": f.name, "value": f.value, "inline": f.inline})).collect();
        m.insert("fields".into(), json!(f));
    }
    if let Some(t) = &e.footer {
        m.insert("footer".into(), json!({"text": t}));
    }
    if let Some(t) = &e.author {
        m.insert("author".into(), json!({"name": t}));
    }
    if let Some(t) = e.timestamp {
        m.insert("timestamp".into(), json!(t.unix_timestamp()));
    }
    Value::Object(m)
}

fn check(name: &str, got: &[EmbedData]) {
    let want = &VEC["embeds"][name];
    assert!(want.is_array(), "no vector {name}");
    let got: Vec<Value> = got.iter().map(dict).collect();
    if common::regen() {
        return common::put(FIXTURE, &format!("/embeds/{name}"), Value::Array(got));
    }
    assert_eq!(&Value::Array(got), want, "{name}");
}

fn s(x: &str) -> String {
    x.to_string()
}

fn problem(c: i64, i: &str, name: &str, rating: Option<i64>, tags: &[&str]) -> Problem {
    Problem { contest_id: c, index: s(i), name: s(name), rating, tags: tags.iter().map(|t| s(t)).collect() }
}

fn p1() -> Problem {
    problem(1700, "A", "Foo [x] _bar_", Some(1000), &["dp", "math"])
}
fn p2() -> Problem {
    problem(1, "B", "No rating", None, &[])
}
fn p3() -> Problem {
    problem(2, "C", "Zero", Some(0), &["a"])
}

fn contest(id: i64, name: &str, start: Option<i64>, duration: i64) -> Contest {
    Contest { id, name: s(name), kind: s("CF"), phase: s("BEFORE"), start_time: start, duration }
}
fn c1() -> Contest {
    contest(1, "Round [Div. 2] _x_ \u{0e44}\u{0e17}\u{0e22}", Some(1700000000), 7500)
}
fn c2() -> Contest {
    contest(2, "NoTime", None, 0)
}
fn c3() -> Contest {
    contest(3, "Zero start", Some(0), 360060)
}

fn drow(f: impl FnOnce(&mut DailyRow)) -> DailyRow {
    let mut r = DailyRow { guild_id: 1, user_id: 1, day: 20000, problem_key: Some(s("1700A")), rating: Some(1000), issued_at: Some(1), solved_at: None, match_id: None };
    f(&mut r);
    r
}
fn daily(row: DailyRow, problem: Option<Problem>, delta: Option<i64>, streak: i64) -> EmbedData {
    daily_embed(&DailyView { row, problem, delta, streak })
}

fn mrow(f: impl FnOnce(&mut MatchRow)) -> MatchRow {
    let mut r = MatchRow {
        id: 1, guild_id: 1, channel_id: 5, message_id: None, host_id: 10, mode: s("duel"), max_players: 2, invited_id: None,
        rating_arg: None, rating: None, problem_key: None, status: s("lobby"), created_at: 1000, started_at: None,
        expires_at: None, finished_at: None, tags: s(""), exclude_tags: s(""),
    };
    f(&mut r);
    r
}

fn pl(uid: i64, f: impl FnOnce(&mut MatchPlayer)) -> MatchPlayer {
    let mut p = MatchPlayer { match_id: 1, guild_id: 1, user_id: uid, handle: format!("h{uid}"), joined_at: 1000, solved_at: None, gave_up: false, delta: None, active: true };
    f(&mut p);
    p
}
fn p(uid: i64) -> MatchPlayer {
    pl(uid, |_| {})
}
fn solved(uid: i64, at: i64, delta: Option<i64>) -> MatchPlayer {
    pl(uid, |p| {
        p.solved_at = Some(at);
        p.delta = delta;
    })
}
fn delta(uid: i64, d: i64) -> MatchPlayer {
    pl(uid, |p| p.delta = Some(d))
}

fn view(m: MatchRow, players: Vec<MatchPlayer>, problem: Option<Problem>, ratings: Option<&[(i64, i64)]>, note: Option<&str>) -> MatchView {
    MatchView { r#match: m, players, problem, ratings: ratings.map(|r| r.iter().copied().collect()), note: note.map(s) }
}
fn menbed(m: MatchRow, players: Vec<MatchPlayer>) -> EmbedData {
    match_embed(&view(m, players, None, None, None))
}

fn text(e: &EmbedData) -> String {
    let mut parts = vec![e.title.clone().unwrap_or_default(), e.description.clone().unwrap_or_default(), e.footer.clone().unwrap_or_default()];
    parts.extend(e.fields.iter().map(|f| format!("{} {}", f.name, f.value)));
    parts.join("\n")
}

fn one(pages: Vec<EmbedData>) -> EmbedData {
    pages.into_iter().next().unwrap()
}

// ---- ports of tests/test_match_embeds.py ----

fn base(f: impl FnOnce(&mut MatchRow)) -> MatchRow {
    mrow(f)
}
fn test_problem() -> Problem {
    problem(1700, "A", "Foo", Some(1000), &["dp"])
}

#[test]
fn lobby_embed_lists_tag_filters() {
    let m = base(|m| {
        m.tags = s("dp,math");
        m.exclude_tags = s("greedy");
    });
    let out = text(&menbed(m, vec![p(10)]));
    assert!(out.contains("Tags: dp, math") && out.contains("Excluding: greedy"));
    assert!(!text(&menbed(mrow(|_| {}), vec![p(10)])).contains("Tags:"));
}

#[test]
fn lobby_embed() {
    let out = text(&menbed(mrow(|m| m.invited_id = Some(11)), vec![p(10)]));
    assert!(out.to_lowercase().contains("lobby") && out.contains("1/2"));
    assert!(out.contains("<@10>") && out.contains("h10"));
    assert!(out.contains("<@11>"));
    assert!(out.contains(&format!("<t:{}:R>", 1000 + 600)));
}

#[test]
fn active_embed_shows_problem_status_and_expiry() {
    let m = mrow(|m| {
        m.status = s("active");
        m.problem_key = Some(s("1700A"));
        m.rating = Some(1000);
        m.started_at = Some(2000);
        m.expires_at = Some(9200);
    });
    let pr = test_problem();
    let players = vec![solved(10, 2100, None), pl(11, |p| p.gave_up = true), p(12)];
    let e = match_embed(&view(m, players, Some(pr.clone()), None, None));
    let out = text(&e);
    assert!(out.contains("1700A") && e.url.as_deref() == Some(pr.url().as_str()));
    assert!(out.contains("solved") && out.contains("gave up") && out.contains("solving"));
    assert!(out.contains("<t:9200:R>"));
}

#[test]
fn active_embed_without_cached_problem_still_renders() {
    let m = mrow(|m| {
        m.status = s("active");
        m.problem_key = Some(s("1700A"));
        m.rating = Some(1000);
        m.expires_at = Some(9200);
    });
    assert!(text(&menbed(m, vec![p(10)])).contains("1700A"));
}

fn fin(f: impl FnOnce(&mut MatchRow)) -> MatchRow {
    mrow(|m| {
        m.status = s("finished");
        m.problem_key = Some(s("1700A"));
        m.rating = Some(1000);
        f(m);
    })
}

#[test]
fn finished_embed_lists_signed_deltas_and_ratings() {
    let v = view(fin(|_| {}), vec![solved(10, 2100, Some(5)), delta(11, -5)], Some(test_problem()), Some(&[(10, 812), (11, 797)]), None);
    let out = text(&match_embed(&v));
    assert!(out.contains("+5") && out.contains("-5"));
    assert!(out.contains("rating 812") && out.contains("rating 797"));
    assert!(out.contains("1. <@10> +5") && out.contains("2. <@11> -5"));
}

fn custom_fin() -> MatchRow {
    fin(|m| {
        m.mode = s("custom");
        m.max_players = 3;
        m.finished_at = Some(2500);
    })
}

#[test]
fn finished_custom_embed_names_the_winner_without_places() {
    let m = custom_fin();
    let players = vec![solved(10, 2200, Some(-25)), solved(11, 2100, Some(74)), delta(12, -25)];
    let out = text(&match_embed(&view(m.clone(), players.clone(), Some(test_problem()), Some(&[]), None)));
    assert!(out.contains("Winner <@11> +74") && out.contains("<@10> -25") && !out.contains("1. "));
    let rec = [(m, players)];
    let by_key = HashMap::new();
    assert!(history_pages("t", &rec, 10, &by_key, &[]).unwrap()[0].description.as_deref().unwrap().ends_with("lost, -25"));
    assert!(history_pages("t", &rec, 11, &by_key, &[]).unwrap()[0].description.as_deref().unwrap().ends_with("won, +74"));
}

#[test]
fn finished_embed_nobody_solved() {
    let v = view(fin(|_| {}), vec![delta(10, 0), delta(11, 0)], Some(test_problem()), Some(&[(10, 800), (11, 800)]), Some("Time is up."));
    let out = text(&match_embed(&v));
    assert!(out.contains("Nobody solved") && out.contains("Time is up."));
}

#[test]
fn cancelled_embed() {
    let v = view(mrow(|m| m.status = s("cancelled")), vec![], None, None, Some("The host left, so the match was cancelled."));
    let e = match_embed(&v);
    assert!(e.title.unwrap().to_lowercase().contains("cancelled"));
    assert!(e.description.as_deref().unwrap().contains("host left"));
    assert!(e.description.unwrap().contains("Host: <@10> | Duel"));
}

fn record(mode: &str, finished: i64, deltas: &[(i64, i64)], won: &[i64], pk: &str) -> MatchRecord {
    let m = mrow(|m| {
        m.status = s("finished");
        m.mode = s(mode);
        m.problem_key = Some(s(pk));
        m.rating = Some(800);
        m.finished_at = Some(finished);
    });
    let players = deltas.iter().map(|&(u, d)| pl(u, |p| {
        p.delta = Some(d);
        p.solved_at = won.contains(&u).then_some(100);
    })).collect();
    (m, players)
}

fn by_key(ps: &[Problem]) -> HashMap<String, Problem> {
    ps.iter().map(|p| (p.key(), p.clone())).collect()
}

#[test]
fn history_pages_lines() {
    let records = vec![
        record("solo", 2000, &[(1, 10)], &[1], "1700A"),
        record("solo", 1900, &[(1, 0)], &[], "1700A"),
        record("duel", 1800, &[(1, 49), (2, -49)], &[1], "1700A"),
        record("custom", 1700, &[(1, -16), (2, 49), (3, 16)], &[2, 3], "1700A"),
        record("duel", 1600, &[(1, 0), (2, 0)], &[], "1700A"),
    ];
    let pr = test_problem();
    let pages = number_pages(history_pages("T", &records, 1, &by_key(std::slice::from_ref(&pr)), &[]).unwrap());
    let desc = pages[0].description.clone().unwrap();
    let lines: Vec<&str> = desc.lines().collect();
    assert!(lines[0].ends_with("- solved, +10") && lines[0].contains(&format!("[{}]({}) (800)", pr.key(), pr.url())));
    assert!(lines[1].ends_with("- not solved, +0"));
    assert!(lines[2].ends_with("vs <@2> - 1st of 2, +49"));
    assert!(lines[3].ends_with("vs <@2>, <@3> - lost, -16"));
    assert!(lines[4].ends_with("vs <@2> - no result, +0"));
    assert_eq!(pages[0].footer.as_deref(), Some("Page 1/1"));
    assert!(history_pages("T", &[], 1, &HashMap::new(), &[]).unwrap().is_empty());
}

#[test]
fn head_to_head_record_and_lines() {
    let records = vec![
        record("duel", 3000, &[(1, 49), (2, -49)], &[1], "1700A"),
        record("duel", 2000, &[(1, -16), (2, 16)], &[2], "1700A"),
        record("custom", 1500, &[(1, 16), (2, -16), (3, 0)], &[1], "1700A"),
        record("custom", 1400, &[(1, 5), (3, -5)], &[1], "1700A"),
        record("duel", 1300, &[(1, 0), (2, 0)], &[], "1700A"),
    ];
    let page = one(head_to_head_pages("T", &records, 1, 2, &HashMap::new()).unwrap());
    let desc = page.description.unwrap();
    let lines: Vec<&str> = desc.lines().collect();
    assert_eq!(lines[0], "**Wins 2, losses 1, ties 1** (4 matches)");
    assert!(lines.len() == 5 && !desc.contains("vs"));
    let empty = head_to_head_pages("T", &records[3..4], 1, 2, &HashMap::new()).unwrap();
    assert_eq!(empty[0].description.as_deref(), Some("No matches against each other yet."));
}

// ---- byte-equal against Python ----

#[test]
fn esc_vectors() {
    let long: [String; 3] = [s(&"x".repeat(5000)), "[a_](b_)".repeat(700), format!("{}{}", "x".repeat(4096), "_".repeat(100))];
    let mut next_long = long.iter();
    for (i, pair) in VEC["esc"].as_array().unwrap().iter().enumerate() {
        let input = pair[0].as_str().map_or_else(|| next_long.next().unwrap().clone(), s);
        if common::regen() {
            common::put(FIXTURE, &format!("/esc/{i}/1"), json!(esc(&input)));
            continue;
        }
        assert_eq!(esc(&input), pair[1].as_str().unwrap(), "esc case {i}: {input:?}");
    }
    assert!(next_long.next().is_none());
}

#[test]
fn fmt_g_vectors() {
    let xs = [
        0.0, -0.0, 1.0, 0.5, 100000.0, 999999.0, 999999.5, 1000000.0, 1e-4, 1e-5, 12345.678, 123456.7, 1234567.0, 0.000123456789, 5e-324,
        1.7976931348623157e308, 2.5, 3.5, 1e21, 123456.5, 0.1 + 0.2, f64::INFINITY, f64::NEG_INFINITY, f64::NAN, 1e100, 99999.95, 9.999995e5, 1.5e-7, -1234.5,
    ];
    let want = VEC["fmt_g"].as_array().unwrap();
    assert_eq!(xs.len(), want.len());
    for (i, (x, w)) in xs.iter().zip(want).enumerate() {
        if common::regen() {
            common::put(FIXTURE, &format!("/fmt_g/{i}/1"), json!(fmt_g(*x)));
            continue;
        }
        assert_eq!(fmt_g(*x), w[1].as_str().unwrap(), "{}", w[0]);
    }
}

#[test]
fn problem_daily_profile_vectors() {
    check("problem_full", &[problem_embed(&p1(), Some("foot"))]);
    check("problem_unrated_notags", &[problem_embed(&p2(), None)]);
    check("problem_zero_rating_emptyfooter", &[problem_embed(&p3(), Some(""))]);
    check("daily_unsolved_streak", &[daily(drow(|_| {}), Some(p1()), None, 3)]);
    check("daily_unsolved_nostreak_dropped", &[daily(drow(|_| {}), None, None, 0)]);
    check("daily_unsolved_dropped_nokey", &[daily(drow(|r| r.problem_key = None), None, None, 1)]);
    let done = |f: fn(&mut DailyRow)| drow(move |r| {
        r.solved_at = Some(5);
        f(r);
    });
    check("daily_solved_delta", &[daily(done(|_| {}), Some(p1()), Some(20), 1)]);
    check("daily_solved_negdelta", &[daily(done(|_| {}), Some(p1()), Some(-3), 2)]);
    check("daily_solved_zerodelta", &[daily(done(|_| {}), Some(p1()), Some(0), 2)]);
    check("daily_solved_first_solo", &[daily(done(|r| r.problem_key = None), None, None, 0)]);
    check("daily_solved_already", &[daily(done(|_| {}), None, None, 1)]);
    let user = |h: &str, r, m, k: Option<&str>| User { handle: s(h), rating: r, max_rating: m, rank: k.map(s) };
    check("profile_rated", &[profile_embed(&user("tourist", Some(3800), Some(4000), Some("legendary grandmaster")), "Tourist \u{1f600}")]);
    check("profile_unrated", &[profile_embed(&user("new", None, None, None), "n")]);
    check("profile_norank", &[profile_embed(&user("x_y", Some(1200), None, None), "n")]);
    let mp = |f: fn(&mut MemberProfile)| {
        let mut m = MemberProfile { display_name: "Ann", rating: 850, rank: Some(2), ranked: 10, matches: 5, contests: 2, changes: &[5, -3, 0, 7], peak: Some(900), streak: 4, codeforces: "1200 (pupil)", contest_wins: 0 };
        f(&mut m);
        member_profile_embed(&m)
    };
    check("member_full", &[mp(|m| m.contest_wins = 1)]);
    check("member_unranked", &[mp(|m| {
        m.rank = None;
        m.peak = None;
        m.changes = &[];
    })]);
    check("member_rank0_peak0", &[mp(|m| {
        m.rank = Some(0);
        m.peak = Some(0);
    })]);
}

#[test]
fn contest_vectors() {
    check("contest_pages_3", &contest_pages(&[c1(), c2(), c3()], "Upcoming", 10).unwrap());
    check("contest_pages_12", &contest_pages(&vec![c1(); 12], "Upcoming", 10).unwrap());
    check("contest_pages_p1", &contest_pages(&[c1(), c2()], "T", 1).unwrap());
    check("contest_pages_empty", &contest_pages(&[], "T", 10).unwrap());
    check("new_contest", &[new_contest_embed(&c1(), "New contest")]);
    check("new_contest_title", &[new_contest_embed(&c3(), "Soon")]);
    assert!(contest_pages(&[c1()], "T", 0).is_err());
}

#[test]
fn match_embed_vectors() {
    let me = menbed;
    check("m_lobby_full", &[me(mrow(|m| {
        m.invited_id = Some(11);
        m.tags = s("dp,math");
        m.exclude_tags = s("greedy");
    }), vec![p(10), pl(11, |p| p.handle = s("a_b [c]"))])]);
    check("m_lobby_plain", &[me(mrow(|_| {}), vec![p(10)])]);
    check("m_lobby_custom", &[me(mrow(|m| {
        m.mode = s("custom");
        m.max_players = 5;
    }), vec![p(10)])]);
    check("m_cancel_note", &[match_embed(&view(mrow(|m| m.status = s("cancelled")), vec![], None, None, Some("The host left.")))]);
    check("m_cancel_nonote", &[match_embed(&view(mrow(|m| {
        m.status = s("cancelled");
        m.mode = s("custom");
        m.max_players = 3;
    }), vec![], None, None, Some("")))]);
    check("m_cancel_solo", &[me(mrow(|m| {
        m.status = s("cancelled");
        m.mode = s("solo");
    }), vec![])]);
    let act = || mrow(|m| {
        m.status = s("active");
        m.problem_key = Some(s("1700A"));
        m.rating = Some(1000);
        m.started_at = Some(2000);
        m.expires_at = Some(9200);
    });
    check("m_active_problem", &[match_embed(&view(act(), vec![solved(10, 2100, None), pl(11, |p| p.gave_up = true), p(12)], Some(p1()), None, None))]);
    check("m_active_noproblem", &[me(act(), vec![p(10)])]);
    check("m_active_solo_noexp", &[me(mrow(|m| {
        m.status = s("active");
        m.mode = s("solo");
        m.problem_key = Some(s("9Z"));
    }), vec![p(10)])]);
    let f = |players, ratings: Option<&[(i64, i64)]>, note| match_embed(&view(fin(|_| {}), players, Some(p1()), ratings, note));
    check("m_fin_duel", &[f(vec![solved(10, 2100, Some(5)), delta(11, -5)], Some(&[(10, 812), (11, 797)]), None)]);
    check("m_fin_ties_order", &[f(vec![delta(10, 3), delta(11, 3), delta(12, 9)], Some(&[(10, 1)]), None)]);
    check("m_fin_noratings", &[f(vec![solved(10, 1, Some(5))], None, None)]);
    check("m_fin_custom", &[match_embed(&view(custom_fin(), vec![solved(10, 2200, Some(-25)), solved(11, 2100, Some(74)), delta(12, -25)], Some(p1()), Some(&[]), None))]);
    check("m_fin_custom_nosolve_delta", &[match_embed(&view(custom_fin(), vec![delta(10, 3), delta(11, -3)], Some(p1()), Some(&[(10, 5)]), None))]);
    check("m_fin_nobody", &[f(vec![delta(10, 0), p(11)], Some(&[(10, 800), (11, 800)]), Some("Time is up."))]);
    let solo = mrow(|m| {
        m.status = s("finished");
        m.mode = s("solo");
        m.problem_key = Some(s("1700A"));
    });
    check("m_fin_noproblem_solo", &[match_embed(&view(solo, vec![solved(10, 3, Some(11))], None, Some(&[(10, 811)]), None))]);
}

fn wr(rank: i64, user_id: i64, handle: &str, rating: i64, gain: i64, moved: Option<i64>) -> WeeklyRow {
    WeeklyRow { rank, user_id, handle: s(handle), rating, gain, moved }
}

#[test]
fn weekly_and_stats_vectors() {
    check("weekly_rows", &[weekly_embed(&[wr(1, 10, "a_b", 900, 20, Some(2)), wr(2, 11, "c", 800, 0, Some(0)), wr(3, 12, "d", 700, -5, Some(-1)), wr(4, 13, "e", 600, 5, None)])]);
    check("weekly_empty", &[weekly_embed(&[])]);
    let twelve: Vec<WeeklyRow> = (1..=12).map(|i| wr(i, i, &format!("h{i}"), 800 + i, i - 6, (i % 3 != 0).then_some(i - 5))).collect();
    check("weekly_12", &[weekly_embed(&twelve)]);

    let rows: Vec<(i64, String, i64, i64)> = (1..=25).map(|i| (i, format!("h{i}_"), 1000 - i * 10, i)).collect();
    let none = HashMap::new();
    let st = |m: &[(i64, i64)]| m.iter().copied().collect::<HashMap<_, _>>();
    check("stats_all_me", &stats_pages(&rows, 3, &none, false, "all"));
    check("stats_all_notme", &stats_pages(&rows[..4], 99, &none, false, "all"));
    check("stats_contests_streak", &stats_pages(&rows[..3], 2, &st(&[(1, 5), (2, 5)]), false, "contests"));
    check("stats_matches_empty", &stats_pages(&[], 1, &none, false, "matches"));
    check("stats_all_empty", &stats_pages(&[], 1, &none, false, "all"));
    check("stats_by_streak", &stats_pages(&rows[..6], 4, &st(&[(1, 2), (2, 9), (4, 9), (6, 1)]), true, "all"));
    check("stats_by_streak_me_absent", &stats_pages(&rows[..6], 5, &st(&[(1, 2)]), true, "all"));
    check("stats_by_streak_empty", &stats_pages(&rows[..6], 5, &none, true, "all"));
    check("stats_by_streak_zero", &stats_pages(&rows[..3], 5, &st(&[(1, 0)]), true, "contests"));
}

fn entries() -> Vec<RankEntry> {
    let pts = [1500.0, 1234.5, 0.1 + 0.2, 123456789.0, 0.00001234, 1e16, 999999.5, -2.5, 0.0, 100000.0, std::f64::consts::PI];
    let handles = ["a_b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
    let pen = [0, 30, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    (0..11).map(|i| (i as i64 + 1, 10 + i as i64, s(handles[i]), pts[i], pen[i])).collect()
}

fn res(handle: &str, points: f64, penalty: i64, solved: i64, last_ac: Option<i64>, kind: &str) -> ContestResult {
    ContestResult { handle: s(handle), points, penalty, solved, last_ac, participant_type: s(kind) }
}

fn rrow(id: i64, h: &str, result: Option<ContestResult>, rank: Option<i64>) -> RanklistRow {
    RanklistRow { user_id: id, handle: s(h), result, rank }
}

fn rcontest(kind: &str, phase: &str) -> Contest {
    Contest { kind: s(kind), phase: s(phase), ..contest(7, "Round [7] _x_", Some(1700000000), 7200) }
}

/// Deliberately shuffled input: sorting must come from the rules, not the input order.
fn icpc_rows() -> Vec<RanklistRow> {
    let c = |h: &str, pts, pen| Some(res(h, pts, pen, 0, None, "CONTESTANT"));
    vec![
        rrow(1, "zero", c("zero", 0.0, 0), Some(5)),
        rrow(2, "slow", c("slow", 3.0, 200), Some(2)),
        rrow(3, "fast", c("fast", 3.0, 100), Some(1)),
        rrow(4, "nopart", None, None),
        rrow(5, "ooc_", Some(res("ooc_", 9.0, 1, 9, Some(5), "OUT_OF_COMPETITION")), None),
        rrow(6, "top", c("top", 1234.5, 0), Some(3)),
    ]
}

fn cf_rows() -> Vec<RanklistRow> {
    let c = |h: &str, solved, last| Some(res(h, 0.0, 0, solved, last, "CONTESTANT"));
    vec![
        rrow(1, "zero", c("zero", 0, None), Some(4)),
        rrow(2, "late", c("late", 2, Some(900)), Some(2)),
        rrow(3, "early", c("early", 2, Some(300)), Some(1)),
        rrow(4, "one", c("one", 1, Some(10)), Some(3)),
        rrow(5, "ooc_", Some(res("ooc_", 0.0, 0, 5, Some(5), "OUT_OF_COMPETITION")), None),
        rrow(6, "norating", c("norating", 1, Some(20)), None),
    ]
}

#[test]
fn ranklist_modes_vectors() {
    const T: i64 = 1700003600;
    let icpc = rcontest("ICPC", "CODING");
    let cf = rcontest("CF", "FINISHED");
    let many: Vec<RanklistRow> = (1..=12).map(|i| rrow(i, &format!("h{i}"), Some(res(&format!("h{i}"), 0.0, 0, 13 - i, Some(i), "CONTESTANT")), Some(i))).collect();
    let n = RanklistMode::FinishedNoRatings;
    check("ranklist_running_icpc", &ranklist_embeds(&icpc, &icpc_rows(), RanklistMode::Running, T, 10).unwrap());
    check("ranklist_finished_cf_unrated", &ranklist_embeds(&cf, &cf_rows(), n, T, 10).unwrap());
    check("ranklist_finished_icpc_unrated", &ranklist_embeds(&icpc, &icpc_rows(), n, T, 10).unwrap());
    check("ranklist_rated_cf", &ranklist_embeds(&cf, &cf_rows(), RanklistMode::FinishedRated, T, 10).unwrap());
    check("ranklist_rated_p5", &ranklist_embeds(&cf, &many, RanklistMode::FinishedRated, T, 5).unwrap());
    check("ranklist_p5_running", &ranklist_embeds(&cf, &many[..7], RanklistMode::Running, T, 5).unwrap());
    check("ranklist_empty", &ranklist_embeds(&cf, &[], n, T, 10).unwrap());
}

#[test]
fn ranklist_sorting_and_sections() {
    let lines = |p: &[EmbedData]| p.iter().flat_map(|e| e.description.clone().unwrap().lines().map(String::from).collect::<Vec<_>>()).collect::<Vec<_>>();
    let order = |p: &[EmbedData]| lines(p).iter().filter_map(|l| l.split("<@").nth(1).map(|x| x.split('>').next().unwrap().to_string())).collect::<Vec<_>>();
    let icpc = rcontest("ICPC", "FINISHED");
    let p = ranklist_embeds(&icpc, &icpc_rows(), RanklistMode::FinishedNoRatings, 0, 10).unwrap();
    assert_eq!(order(&p), ["6", "3", "2", "1", "5"].map(String::from)); // ICPC: points desc, penalty asc
    assert_eq!(order(&p[..1]), ["6", "3", "2", "1"].map(String::from).to_vec()); // unrated section on its own page
    assert!(lines(&p).iter().all(|l| l.starts_with("?\\.") || l.starts_with("**")));
    let p = ranklist_embeds(&rcontest("CF", "FINISHED"), &cf_rows(), RanklistMode::FinishedNoRatings, 0, 10).unwrap();
    assert_eq!(order(&p), ["3", "2", "4", "6", "1", "5"].map(String::from)); // solved desc, last AC asc, 0 solved last, OOC in Unrated
    let p = ranklist_embeds(&rcontest("CF", "FINISHED"), &cf_rows(), RanklistMode::FinishedRated, 0, 10).unwrap();
    assert_eq!(order(&p), ["3", "2", "4", "1", "5", "6"].map(String::from)); // by real rank; Unrated keeps input order
    assert!(p.iter().all(|e| !e.footer.as_ref().unwrap().contains("Codeforces")));
    assert!(lines(&p).iter().any(|l| l.starts_with("1\\. ")) && lines(&p).iter().filter(|l| l.starts_with("?\\.")).count() == 2);
}

#[test]
fn ranklist_recap_handles_stalk_vectors() {
    let e = entries();
    check("recap_3", &[recap_embed(&c1(), &e[..3]).unwrap()]);
    check("recap_1", &[recap_embed(&c1(), &e[..1]).unwrap()]);
    check("recap_11", &[recap_embed(&c1(), &e).unwrap()]);
    assert!(recap_embed(&c1(), &[]).is_err());

    let mut hl: Vec<(i64, String, Option<i64>, Option<String>)> =
        (1..=12).map(|i| (i, format!("u_{i}"), (i % 3 != 0).then_some(1500 - i), (i % 3 != 0).then(|| s("expert")))).collect();
    hl.push((99, s("z"), Some(1000), None));
    hl.push((98, s("y"), None, Some(s("pupil"))));
    check("handles", &handle_list_pages(&hl, 10).unwrap());
    check("handles_p5", &handle_list_pages(&hl[..7], 5).unwrap());
    check("handles_empty", &handle_list_pages(&[], 10).unwrap());

    let mut items: Vec<(String, Option<Problem>)> = vec![(s("1700A"), Some(p1())), (s("5B"), None), (s("1B"), Some(p2())), (s("2C"), Some(p3()))];
    items.extend((0..20).map(|i| (format!("{i}A"), Some(p1()))));
    check("stalk", &stalk_pages("x_y", &items, 15).unwrap());
    check("stalk_p2", &stalk_pages("h", &items[..3], 2).unwrap());
    check("stalk_empty", &stalk_pages("h", &[], 15).unwrap());
}

fn mr(f: impl FnOnce(&mut MashupRow)) -> MashupRow {
    let mut r = MashupRow {
        id: 1, guild_id: 1, channel_id: 1, message_id: None, host_id: 1, min_rating: 800, max_rating: 1500, count: 4, duration_seconds: 7200,
        penalty_minutes: 0, status: s("finished"), created_at: 1, started_at: Some(2), ends_at: Some(1750), teams: 0, max_players: 20, rated: 1,
        team_size: 0, tags: s(""), exclude_tags: s(""), contest_id: None, mode: s("contest"), size: 0, win: s("line"), thread_id: None, log_board_id: None,
    };
    f(&mut r);
    r
}

#[test]
fn history_vectors() {
    let bk = by_key(&[p1()]);
    let recs = vec![
        record("solo", 2000, &[(1, 10)], &[1], "1700A"),
        record("solo", 1900, &[(1, 0)], &[], "1700A"),
        record("duel", 1800, &[(1, 49), (2, -49)], &[1], "1700A"),
        record("custom", 1700, &[(1, -16), (2, 49), (3, 16)], &[2, 3], "1700A"),
        record("duel", 1600, &[(1, 0), (2, 0)], &[], "1700A"),
        record("custom", 1500, &[(1, 0), (2, 0), (3, 0), (4, 0), (5, 0), (6, 0)], &[], "1700A"),
        record("custom", 1400, &[(1, 5), (2, -1), (3, -1), (4, -1), (5, -1), (6, -1)], &[1], "1700A"),
        record("duel", 1300, &[(1, -49), (2, 49)], &[], "9Z"),
        record("duel", 1200, &[(1, -49), (2, 49), (3, 5)], &[2], "1700A"),
    ];
    check("history_lines", &number_pages(history_pages("T", &recs, 1, &bk, &[]).unwrap()));
    check("history_empty", &history_pages("T", &[], 1, &HashMap::new(), &[]).unwrap());
    check("history_12", &history_pages("T", &[recs.clone(), recs.clone()].concat(), 1, &bk, &[]).unwrap());
    let rec_m = |m: MashupRow, rank, entrants, solved, team, delta| MashupRecord { mashup: m, rank, entrants, solved, team, delta };
    let mrs = vec![
        rec_m(mr(|_| {}), 2, 5, 3, None, Some(12)),
        rec_m(mr(|m| {
            m.ends_at = Some(1800);
            m.mode = s("classic");
            m.team_size = 1;
        }), 1, 3, 0, None, None),
        rec_m(mr(|m| {
            m.ends_at = Some(1800);
            m.team_size = 2;
        }), 1, 3, 2, Some(2), Some(-4)),
        rec_m(mr(|m| {
            m.ends_at = Some(1900);
            m.mode = s("replace");
            m.team_size = 3;
        }), 3, 3, 1, Some(1), Some(0)),
    ];
    check("history_mashups", &history_pages("T", &recs[..3], 1, &bk, &mrs).unwrap());
    check("history_mashups_only", &history_pages("T", &[], 1, &bk, &mrs).unwrap());
    let h2h = vec![
        record("duel", 3000, &[(1, 49), (2, -49)], &[1], "1700A"),
        record("duel", 2000, &[(1, -16), (2, 16)], &[2], "1700A"),
        record("custom", 1500, &[(1, 16), (2, -16), (3, 0)], &[1], "1700A"),
        record("custom", 1400, &[(1, 5), (3, -5)], &[1], "1700A"),
        record("duel", 1300, &[(1, 0), (2, 0)], &[], "1700A"),
    ];
    check("h2h", &head_to_head_pages("T", &h2h, 1, 2, &HashMap::new()).unwrap());
    check("h2h_none", &head_to_head_pages("T", &h2h[3..4], 1, 2, &HashMap::new()).unwrap());
    check("h2h_13", &head_to_head_pages("T", &std::iter::repeat_n(h2h.clone(), 3).flatten().collect::<Vec<_>>(), 1, 2, &bk).unwrap());
    check("number_pages", &number_pages(vec![EmbedData::new("a", None), EmbedData::new("b", None).description("x")]));
    check("number_pages_empty", &number_pages(vec![]));
}

#[test]
fn errors_match_python_exceptions() {
    let (m, mut players) = record("solo", 1, &[(1, 5)], &[1], "1700A");
    // Python: `me.delta:+d` with delta None is a TypeError; a missing player is StopIteration
    players[0].delta = None;
    assert!(history_pages("T", &[(m.clone(), players)], 1, &HashMap::new(), &[]).is_err());
    assert!(history_pages("T", &[(m.clone(), vec![delta(2, 1)])], 1, &HashMap::new(), &[]).is_err());
    let mut open = m;
    open.finished_at = None;
    assert!(history_pages("T", &[(open, vec![delta(1, 1)])], 1, &HashMap::new(), &[]).is_err());
}

#[test]
fn create_embed_conversion_skips_empty_text() {
    let e = EmbedData::new("", Some(BLUE)).description("").footer("f").field("a", "b", false);
    let v = serde_json::to_value(CreateEmbed::from(e)).unwrap();
    assert_eq!(v, json!({"color": BLUE, "fields": [{"name": "a", "value": "b", "inline": false}], "footer": {"text": "f"}, "type": "rich"}));
}

#[test]
fn unknown_match_mode_is_an_error_not_a_panic() {
    for status in ["lobby", "cancelled", "active"] {
        let m = mrow(|m| {
            m.mode = s("weird");
            m.status = s(status);
        });
        let err = lightningccp::ui::embeds::a::match_embed(&view(m, vec![], None, None, None)).unwrap_err();
        assert!(err.to_string().starts_with("KeyError"), "{status}: {err}");
    }
}

/// A saved rank is shown as is (never a standings rank), "?" when absent, and the dot is escaped so Discord
/// does not renumber the list from the first number.
#[test]
fn ranklist_shows_saved_rank_with_escaped_dot() {
    let cf = rcontest("CF", "FINISHED");
    let r = |i, h: &str, rank| rrow(i, h, Some(res(h, 3.0, 0, 3, Some(i), "CONTESTANT")), rank);
    let rows = [r(1, "a", Some(4651)), r(2, "b", Some(5894)), r(3, "c", None)];
    let p = ranklist_embeds(&cf, &rows, RanklistMode::FinishedRated, 0, 10).unwrap();
    let d = p[0].description.clone().unwrap();
    assert!(d.starts_with("4651\\. <@1>") && d.contains("\n5894\\. <@2>"), "{d}");
    assert!(p[1].description.clone().unwrap().contains("?\\. <@3>"));
}
