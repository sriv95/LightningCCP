//! Parity tests for src/ui/embeds/b.rs against src/ui/embeds.py lines 463-743. VECTORS were produced by running the
//! Python builders (`mashup_embed`, `log_board_embed`, `start_message`, `give_up_message`, `final_message`,
//! `log_message`, and `MashupView.log`/`standings`) on the scenario views embedded with them (`discord.Embed.to_dict()`
//! minus the constant `type` and `flags` keys, `len(embed)`); `{"err": ...}` is where Python raised. The last tests port
//! the embed parts of tests/test_mashup.py and tests/test_bingo.py. Regenerate by rerunning the Python builders on the
//! scenario views (each entry's "view" and "by_key" are their inputs).
use std::collections::HashMap;
use std::sync::LazyLock;

use lightningccp::cf::models::Problem;
use lightningccp::store::mashup::{MashupCell, MashupPlayer, MashupRow};
use lightningccp::ui::embeds::a::EmbedData;
use lightningccp::ui::embeds::b::*;
use serde_json::{Map, Value, json};


mod common;
const FIXTURE: &str = "embeds_b_vectors.json";
static VEC: LazyLock<Value> = LazyLock::new(|| common::load(FIXTURE));

fn i(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("not an int: {v}"))
}
fn oi(v: &Value) -> Option<i64> {
    (!v.is_null()).then(|| i(v))
}
fn st(v: &Value) -> String {
    v.as_str().expect("string").to_string()
}

fn row_of(m: &Value) -> MashupRow {
    MashupRow {
        id: i(&m["id"]),
        guild_id: i(&m["guild_id"]),
        channel_id: i(&m["channel_id"]),
        message_id: oi(&m["message_id"]),
        host_id: i(&m["host_id"]),
        min_rating: i(&m["min_rating"]),
        max_rating: i(&m["max_rating"]),
        count: i(&m["count"]),
        duration_seconds: i(&m["duration_seconds"]),
        penalty_minutes: i(&m["penalty_minutes"]),
        status: st(&m["status"]),
        created_at: i(&m["created_at"]),
        started_at: oi(&m["started_at"]),
        ends_at: oi(&m["ends_at"]),
        teams: i(&m["teams"]),
        max_players: i(&m["max_players"]),
        rated: i(&m["rated"]),
        team_size: i(&m["team_size"]),
        tags: st(&m["tags"]),
        exclude_tags: st(&m["exclude_tags"]),
        contest_id: oi(&m["contest_id"]),
        mode: st(&m["mode"]),
        size: i(&m["size"]),
        win: st(&m["win"]),
        thread_id: oi(&m["thread_id"]),
        log_board_id: oi(&m["log_board_id"]),
    }
}

fn view_of(v: &Value) -> MashupView {
    MashupView {
        mashup: row_of(&v["mashup"]),
        problems: v["problems"].as_array().unwrap().iter().map(|p| (st(&p[0]), i(&p[1]))).collect(),
        players: v["players"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| MashupPlayer {
                user_id: i(&p["user_id"]),
                handle: st(&p["handle"]),
                joined_at: i(&p["joined_at"]),
                team: oi(&p["team"]),
                delta: oi(&p["delta"]),
                active: p["active"].as_bool().unwrap(),
            })
            .collect(),
        solves: v["solves"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| (i(&u[0]), u[1].as_array().unwrap().iter().map(|s| (st(&s[0]), (i(&s[1]), i(&s[2])))).collect()))
            .collect(),
        note: v["note"].as_str().map(str::to_string),
        cells: v["cells"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| MashupCell { idx: i(&c["idx"]), key: st(&c["key"]), rating: i(&c["rating"]), cell: oi(&c["cell"]), since: i(&c["since"]) })
            .collect(),
    }
}

fn by_key_of(v: &Value) -> HashMap<String, Problem> {
    v.as_object()
        .unwrap()
        .iter()
        .map(|(k, p)| {
            (k.clone(), Problem { contest_id: i(&p["contest_id"]), index: st(&p["index"]), name: st(&p["name"]), rating: oi(&p["rating"]), tags: vec![] })
        })
        .collect()
}

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
    Value::Object(m)
}

/// Python `len(embed)`: title, description, footer, author and field text, in characters.
fn embed_len(e: &EmbedData) -> usize {
    let n = |s: &Option<String>| s.as_ref().map_or(0, |s| s.chars().count());
    n(&e.title) + n(&e.description) + n(&e.footer) + n(&e.author) + e.fields.iter().map(|f| f.name.chars().count() + f.value.chars().count()).sum::<usize>()
}

fn scenarios() -> Vec<(&'static String, MashupView, HashMap<String, Problem>, &'static Value)> {
    VEC.as_object().unwrap().iter().map(|(n, s)| (n, view_of(&s["view"]), by_key_of(&s["by_key"]), &s["out"])).collect()
}

/// Compares a Python result (`{"text": ..}`, `{"embed": ..}` or `{"err": ..}`) with ours.
/// `ptr` locates `want` in the fixture; under UPDATE_VECTORS=1 it is rewritten from `got` (a Python `err` entry stays when we also fail).
fn same(name: &str, what: &str, got: anyhow::Result<Value>, ptr: &str) {
    let want = VEC.pointer(ptr).unwrap_or_else(|| panic!("no vector {ptr}"));
    if common::regen() {
        let new = match got {
            Err(_) if want.get("err").is_some() => return,
            Err(e) => json!({"err": e.to_string()}),
            Ok(v) if v.is_string() => json!({"text": v}),
            Ok(v) => v,
        };
        return common::put(FIXTURE, ptr, new);
    }
    let got = got.map(|v| v.get("embed").cloned().unwrap_or(v));
    match (got, want.get("err")) {
        (Err(_), Some(_)) => {}
        (Err(e), None) => panic!("{name} {what}: unexpected error {e}"),
        (Ok(v), Some(err)) => panic!("{name} {what}: Python raised {err}, we returned {v}"),
        (Ok(v), None) => assert_eq!(&v, want.get("text").or(want.get("embed")).expect("text or embed"), "{name} {what}"),
    }
}

fn emb(r: anyhow::Result<EmbedData>) -> anyhow::Result<Value> {
    r.map(|e| json!({"embed": dict(&e), "len": embed_len(&e)}))
}

#[test]
fn mashup_embed_matches_python() {
    let all = scenarios();
    assert!(all.len() > 40);
    for (name, view, by_key, out) in &all {
        let got = mashup_embed(view, by_key);
        if let (false, Ok(e), Some(n)) = (common::regen(), &got, out["embed"].get("len")) {
            assert_eq!(embed_len(e) as i64, i(n), "{name} len");
        }
        same(name, "embed", emb(got), &format!("/{name}/out/embed"));
    }
}

#[test]
fn thread_messages_match_python() {
    for (name, view, _, out) in &scenarios() {
        same(name, "start", Ok(json!(start_message(view))), &format!("/{name}/out/start"));
        same(name, "final", final_message(view).map(Value::from), &format!("/{name}/out/final"));
        same(name, "log_board", emb(log_board_embed(view)), &format!("/{name}/out/log_board"));
        for uid in out["give_up"].as_object().unwrap().keys() {
            same(name, "give_up", give_up_message(view, uid.parse().unwrap()).map(Value::from), &format!("/{name}/out/give_up/{uid}"));
        }
    }
}

#[test]
fn log_entries_and_solve_messages_match_python() {
    for (name, view, by_key, out) in &scenarios() {
        let log = view.log();
        if common::regen() {
            let new = log.as_ref().map(|l| l.iter().map(|e| json!([e.at, e.team, e.key, e.rating, e.user, e.wrong])).collect::<Vec<_>>());
            if let Ok(new) = new {
                common::put(FIXTURE, &format!("/{name}/out/log"), json!(new));
            }
        }
        let out = &VEC[name.as_str()]["out"];
        let Some(want) = out["log"].as_array() else {
            assert!(log.is_err(), "{name}: Python raised in log");
            continue;
        };
        let log = log.unwrap_or_else(|e| panic!("{name}: {e}"));
        let got: Vec<Value> = log.iter().map(|e| json!([e.at, e.team, e.key, e.rating, e.user, e.wrong])).collect();
        assert_eq!(&got, want, "{name} log");
        let empty = HashMap::new();
        for (k, e) in log.iter().enumerate() {
            same(name, "log_message", log_message(view, e, by_key).map(Value::from), &format!("/{name}/out/log_messages/{k}"));
            same(name, "log_message (no problems)", log_message(view, e, &empty).map(Value::from), &format!("/{name}/out/log_messages_empty/{k}"));
        }
    }
}

#[test]
fn standings_match_python() {
    for (name, view, _, out) in &scenarios() {
        let got = view.standings();
        if common::regen() {
            if let Ok(g) = &got {
                let new: Vec<Value> = g.iter().map(|r| json!([r.rank, r.user_id, r.handle, r.solved, r.total_seconds, r.lines])).collect();
                common::put(FIXTURE, &format!("/{name}/out/standings"), json!(new));
            }
            continue;
        }
        let Some(want) = out["standings"].as_array() else {
            assert!(got.is_err(), "{name}: Python raised in standings");
            continue;
        };
        let got: Vec<Value> =
            got.unwrap_or_else(|e| panic!("{name}: {e}")).iter().map(|r| json!([r.rank, r.user_id, r.handle, r.solved, r.total_seconds, r.lines])).collect();
        assert_eq!(&got, want, "{name} standings");
    }
}

// ---- ports of the embed parts of tests/test_mashup.py and tests/test_bingo.py ----

fn scenario(name: &str) -> (MashupView, HashMap<String, Problem>) {
    let s = &VEC[name];
    (view_of(&s["view"]), by_key_of(&s["by_key"]))
}

fn desc(e: &EmbedData) -> &str {
    e.description.as_deref().unwrap()
}

#[test]
fn contest_embed_lobby_active_cancelled() {
    // test_embed_lobby_active_cancelled
    let (view, by_key) = scenario("c_lobby_solo");
    let e = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(e.title.as_deref(), Some("Mashup lobby"));
    assert!(desc(&e).contains("problems rated 800-1200") && desc(&e).contains("20 min per wrong attempt") && desc(&e).contains("<@1000> ([h"));
    let (view, by_key) = scenario("c_active");
    let text = mashup_embed(&view, &by_key).unwrap().description.unwrap();
    assert!(text.contains("A. [1A Name") && text.contains("C. [3A Name") && text.contains("(1200)"));
    assert_eq!(text.lines().last().unwrap(), "4\\. [h3](https://codeforces.com/profile/h3) - 0 solved, 0:00 (gave up)");
    assert_eq!(mashup_embed(&scenario("c_finished").0, &by_key).unwrap().title.as_deref(), Some("Mashup contest - final standings"));
    assert_eq!(mashup_embed(&scenario("c_cancelled_nonote").0, &by_key).unwrap().title.as_deref(), Some("Mashup cancelled"));
}

#[test]
fn team_contest_embeds() {
    // test_team_contest_embeds_and_refresh_skips_finished_teams
    let (view, by_key) = scenario("c_team_active");
    let e = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(e.title.as_deref(), Some("Team mashup"));
    assert!(desc(&e).contains("1\\. Team 1 (<@1>, <@3>) - 2 solved"));
    let (view, by_key) = scenario("c_lobby_team");
    assert!(desc(&mashup_embed(&view, &by_key).unwrap()).contains("Team 1: <@1000> ([a](https://codeforces.com/profile/a)), <@3> ([c](https://codeforces.com/profile/c))\nTeam 3: <@2> ([b\\_](https://codeforces.com/profile/b_))\nTeam 4: <@4> ([d](https://codeforces.com/profile/d))\n"));
}

#[test]
fn bingo_embeds() {
    // test_bingo_embeds_and_rematch
    let (view, by_key) = scenario("b_lobby_classic_line");
    let e = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(e.title.as_deref(), Some("Bingo lobby"));
    assert!(desc(&e).contains("3x3 classic bingo, the first full line wins"));
    let (view, by_key) = scenario("b_active_empty");
    let e = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(e.title.as_deref(), Some("Bingo"));
    assert_eq!(e.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Row 1", "Row 2", "Row 3"]);
    assert!(e.fields[0].value.contains("codeforces.com"));
    let (view, by_key) = scenario("b_finished_classic");
    let e = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(e.title.as_deref(), Some("Bingo - final standings"));
    assert!(e.fields.iter().all(|f| f.name == "Solve log"));
    let (view, by_key) = scenario("b_lobby_replace_tags");
    assert_eq!(desc(&mashup_embed(&view, &by_key).unwrap()).matches("replace bingo").count(), 1);
    // test_classic_line_win_ends_the_bingo_and_rates_it: the board shows the owner
    let (view, by_key) = scenario("b_full_line_end");
    assert!(desc(&mashup_embed(&view, &by_key).unwrap()).contains("[1]"));
}

#[test]
fn solve_log_names_the_team_and_the_player() {
    // test_solve_log_names_the_team_and_the_player
    let (view, by_key) = scenario("b_team_active");
    let e = mashup_embed(&view, &by_key).unwrap();
    let field = e.fields.iter().find(|f| f.name == "Solve log").unwrap();
    let lines: Vec<&str> = field.value.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].ends_with("- Team 2 (<@3>)") && lines[1].ends_with("- Team 2 (<@3>)") && lines[2].ends_with("- Team 1 (<@5>)"));
}

#[test]
fn full_size_bingo_embed_fits_discords_limit() {
    // test_a_full_size_bingo_embed_fits_discords_limit
    for name in ["b_full_size", "b_full_size_finished"] {
        let (view, by_key) = scenario(name);
        let e = mashup_embed(&view, &by_key).unwrap();
        assert!(embed_len(&e) <= 6000, "{name}");
        assert!(desc(&e).chars().count() <= 4096, "{name}");
    }
}

#[test]
fn a_player_without_a_team_is_an_error_where_python_would_use_none() {
    let (mut view, by_key) = scenario("c_active");
    view.players[0].team = None;
    assert!(mashup_embed(&view, &by_key).is_err() && view.log().is_err() && view.standings().is_err());
    // the lobby shows such a player in no team, like Python
    let (mut view, by_key) = scenario("c_lobby_team");
    view.players[0].team = None;
    assert!(mashup_embed(&view, &by_key).is_ok());
}

/// The scenario with 50 players that have 24-character handles (Codeforces' longest).
fn fifty_long_handles(name: &str) -> (MashupView, HashMap<String, Problem>) {
    let (mut view, by_key) = scenario(name);
    let p = view.players[0].clone();
    view.players = (1..=50)
        .map(|n| MashupPlayer { user_id: 100_000_000_000_000_000 + n, handle: format!("{}{n:02}", "a".repeat(22)), team: Some(n), ..p.clone() })
        .collect();
    view.solves.clear();
    (view, by_key)
}

fn plain_and_short(name: &str) {
    let (view, by_key) = fifty_long_handles(name);
    let d = mashup_embed(&view, &by_key).unwrap().description.unwrap();
    assert!(d.chars().count() <= 4096 && !d.contains("codeforces.com/profile") && d.contains(&"a".repeat(22)), "{name}");
}

#[test]
fn a_lobby_over_the_limit_falls_back_to_plain_handles() {
    plain_and_short("c_lobby_solo");
}

#[test]
fn contest_standings_over_the_limit_fall_back_to_plain_handles() {
    plain_and_short("c_active_nosolves");
}

#[test]
fn bingo_standings_over_the_limit_fall_back_to_plain_handles() {
    plain_and_short("b_active_classic");
}
