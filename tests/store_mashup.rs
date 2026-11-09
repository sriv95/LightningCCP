//! Tests of the mashup store methods. Expected values (`EXPECTED`, one `[step, value]` per line) come from
//! running the same scenario through the Python original's `Store`, now frozen here (the original is gone).
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lightningccp::errors::BotError;
use lightningccp::store::base::Store;
use lightningccp::store::mashup::{MashupCell, MashupPlayer, MashupRow, NewMashup};
use serde_json::{Value, json};

const EXPECTED: &str = r#"
["create1",1]
["get1",{"id":1,"guild_id":1,"channel_id":10,"message_id":null,"host_id":100,"min_rating":800,"max_rating":1600,"count":3,"duration_seconds":7200,"penalty_minutes":10,"status":"lobby","created_at":1000,"started_at":null,"ends_at":null,"teams":2,"max_players":20,"rated":0,"team_size":2,"tags":"dp,graphs","exclude_tags":"math","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":null,"log_board_id":null}]
["get_missing",null]
["host_again","AlreadyInMatch: You are already in a match or contest. Finish it, give up, or leave the lobby first."]
["add_bob","ok"]
["add_bob_again","AlreadyInMatch: You are already in a match or contest. Finish it, give up, or leave the lobby first."]
["add_carol","ok"]
["add_dave_team_full","MatchFull: That team is full."]
["add_dave_cap","MatchFull: This match is full."]
["add_closed","MatchClosed: This contest is no longer open."]
["move_full","MatchFull: That team is full."]
["move_ok","ok"]
["move_closed","MatchClosed: This contest is no longer open."]
["players1",[{"user_id":100,"handle":"alice","joined_at":1000,"team":1,"delta":null,"active":true},{"user_id":200,"handle":"bob","joined_at":1001,"team":2,"delta":null,"active":true},{"user_id":300,"handle":"carol","joined_at":1001,"team":2,"delta":null,"active":true}]]
["active_for_alice",1]
["active_for_nobody",null]
["remove_bob",true]
["remove_bob_again",false]
["start_wrong",false]
["start_ok",true]
["start_again",false]
["remove_active",false]
["problems",[["1A",800],["2B",900],["3C",1000]]]
["cells",[{"idx":0,"key":"1A","rating":800,"cell":0,"since":2000},{"idx":1,"key":"2B","rating":900,"cell":1,"since":2000},{"idx":2,"key":"3C","rating":1000,"cell":2,"since":2000}]]
["excluded",["1A","9Z"]]
["excluded_none",[]]
["replace_stale",false]
["replace_ok",true]
["replace_nocell",false]
["replace_nomashup",false]
["cells2",[{"idx":0,"key":"1A","rating":800,"cell":0,"since":2000},{"idx":1,"key":"2B","rating":900,"cell":1,"since":2000},{"idx":2,"key":"3C","rating":1000,"cell":2,"since":2000},{"idx":3,"key":"4D","rating":1100,"cell":1,"since":3000}]]
["problems2",[["1A",800],["2B",900],["3C",1000],["4D",1100]]]
["log1",true]
["log2",false]
["solves_rec",2]
["solves_rec2",1]
["solves_rec3",1]
["solves_empty",0]
["solves",{"100":{"1A":[2200,3]},"300":{"1A":[2100,0],"2B":[2500,1],"3C":[2700,2]}}]
["solves_none",{}]
["gave_up",true]
["gave_up_again",false]
["active_after_giveup",null]
["players_after_giveup",[{"user_id":100,"handle":"alice","joined_at":1000,"team":1,"delta":null,"active":false},{"user_id":300,"handle":"carol","joined_at":1001,"team":2,"delta":null,"active":true}]]
["set_msg",null]
["set_thread",null]
["set_log_board",null]
["in_active",[1]]
["in_none",[]]
["finish_ok",true]
["finish_again",false]
["get1_done",{"id":1,"guild_id":1,"channel_id":11,"message_id":555,"host_id":100,"min_rating":800,"max_rating":1600,"count":3,"duration_seconds":7200,"penalty_minutes":10,"status":"finished","created_at":1000,"started_at":2000,"ends_at":5000,"teams":2,"max_players":20,"rated":0,"team_size":2,"tags":"dp,graphs","exclude_tags":"math","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":666,"log_board_id":777}]
["players_done",[{"user_id":100,"handle":"alice","joined_at":1000,"team":1,"delta":-5,"active":false},{"user_id":300,"handle":"carol","joined_at":1001,"team":2,"delta":12,"active":false}]]
["active_done",null]
["create2",2]
["cancel",true]
["cancel_again",false]
["get2",{"id":2,"guild_id":1,"channel_id":10,"message_id":null,"host_id":100,"min_rating":800,"max_rating":1600,"count":2,"duration_seconds":3600,"penalty_minutes":5,"status":"cancelled","created_at":6000,"started_at":null,"ends_at":null,"teams":0,"max_players":20,"rated":1,"team_size":0,"tags":"","exclude_tags":"","contest_id":1234,"mode":"bingo","size":3,"win":"continuous","thread_id":null,"log_board_id":null}]
["players2",[{"user_id":100,"handle":"alice","joined_at":6000,"team":null,"delta":null,"active":false}]]
["create3",3]
["start3",true]
["finish3",true]
["get3",{"id":3,"guild_id":1,"channel_id":10,"message_id":null,"host_id":100,"min_rating":800,"max_rating":1600,"count":1,"duration_seconds":3600,"penalty_minutes":5,"status":"finished","created_at":7000,"started_at":7100,"ends_at":10700,"teams":0,"max_players":20,"rated":1,"team_size":0,"tags":"","exclude_tags":"","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":null,"log_board_id":null}]
["finished_100",[{"id":3,"guild_id":1,"channel_id":10,"message_id":null,"host_id":100,"min_rating":800,"max_rating":1600,"count":1,"duration_seconds":3600,"penalty_minutes":5,"status":"finished","created_at":7000,"started_at":7100,"ends_at":10700,"teams":0,"max_players":20,"rated":1,"team_size":0,"tags":"","exclude_tags":"","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":null,"log_board_id":null},{"id":1,"guild_id":1,"channel_id":11,"message_id":555,"host_id":100,"min_rating":800,"max_rating":1600,"count":3,"duration_seconds":7200,"penalty_minutes":10,"status":"finished","created_at":1000,"started_at":2000,"ends_at":5000,"teams":2,"max_players":20,"rated":0,"team_size":2,"tags":"dp,graphs","exclude_tags":"math","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":666,"log_board_id":777}]]
["finished_100_lim1",[{"id":3,"guild_id":1,"channel_id":10,"message_id":null,"host_id":100,"min_rating":800,"max_rating":1600,"count":1,"duration_seconds":3600,"penalty_minutes":5,"status":"finished","created_at":7000,"started_at":7100,"ends_at":10700,"teams":0,"max_players":20,"rated":1,"team_size":0,"tags":"","exclude_tags":"","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":null,"log_board_id":null}]]
["finished_300",[{"id":1,"guild_id":1,"channel_id":11,"message_id":555,"host_id":100,"min_rating":800,"max_rating":1600,"count":3,"duration_seconds":7200,"penalty_minutes":10,"status":"finished","created_at":1000,"started_at":2000,"ends_at":5000,"teams":2,"max_players":20,"rated":0,"team_size":2,"tags":"dp,graphs","exclude_tags":"math","contest_id":null,"mode":"contest","size":0,"win":"line","thread_id":666,"log_board_id":777}]]
["finished_other_guild",[]]
["in_all",[1,2,3]]
"#;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-mashup-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn expected() -> BTreeMap<String, Value> {
    EXPECTED.lines().filter(|l| !l.is_empty()).map(|l| serde_json::from_str::<(String, Value)>(l).unwrap()).collect()
}

fn row(m: &MashupRow) -> Value {
    json!({
        "id": m.id, "guild_id": m.guild_id, "channel_id": m.channel_id, "message_id": m.message_id, "host_id": m.host_id,
        "min_rating": m.min_rating, "max_rating": m.max_rating, "count": m.count, "duration_seconds": m.duration_seconds,
        "penalty_minutes": m.penalty_minutes, "status": m.status, "created_at": m.created_at, "started_at": m.started_at,
        "ends_at": m.ends_at, "teams": m.teams, "max_players": m.max_players, "rated": m.rated, "team_size": m.team_size,
        "tags": m.tags, "exclude_tags": m.exclude_tags, "contest_id": m.contest_id, "mode": m.mode, "size": m.size,
        "win": m.win, "thread_id": m.thread_id, "log_board_id": m.log_board_id,
    })
}

fn rows(v: Vec<MashupRow>) -> Value {
    Value::Array(v.iter().map(row).collect())
}

fn opt_row(v: Option<MashupRow>) -> Value {
    v.as_ref().map_or(Value::Null, row)
}

/// Python's `"ClassName: message"` for a `BotError`, `"ok"` for success.
fn err(r: anyhow::Result<()>) -> Value {
    match r {
        Ok(()) => json!("ok"),
        Err(e) => {
            let class = match e.downcast_ref::<BotError>().expect("not a BotError") {
                BotError::AlreadyInMatch => "AlreadyInMatch",
                BotError::MatchFull(_) => "MatchFull",
                BotError::MatchClosed(_) => "MatchClosed",
                other => panic!("unexpected {other:?}"),
            };
            json!(format!("{class}: {e}"))
        }
    }
}

fn players(v: Vec<MashupPlayer>) -> Value {
    json!(
        v.iter()
            .map(|p| json!({"user_id": p.user_id, "handle": p.handle, "joined_at": p.joined_at, "team": p.team, "delta": p.delta, "active": p.active}))
            .collect::<Vec<_>>()
    )
}

fn cells(v: Vec<MashupCell>) -> Value {
    json!(v.iter().map(|c| json!({"idx": c.idx, "key": c.key, "rating": c.rating, "cell": c.cell, "since": c.since})).collect::<Vec<_>>())
}

#[allow(clippy::type_complexity)]
fn solves(v: Vec<(i64, Vec<(String, (i64, i64))>)>) -> Value {
    let mut out = serde_json::Map::new();
    for (user, per) in v {
        let inner: serde_json::Map<String, Value> = per.into_iter().map(|(k, (t, w))| (k, json!([t, w]))).collect();
        out.insert(user.to_string(), Value::Object(inner));
    }
    Value::Object(out)
}

fn ids(v: Vec<MashupRow>) -> Value {
    json!(v.iter().map(|m| m.id).collect::<Vec<_>>())
}

fn sorted(set: HashSet<String>) -> Value {
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    json!(v)
}

fn new(host_id: i64, now: i64) -> NewMashup {
    NewMashup {
        guild_id: 1,
        channel_id: 10,
        host_id,
        host_handle: "alice".into(),
        min_rating: 800,
        max_rating: 1600,
        count: 3,
        duration_seconds: 7200,
        penalty_minutes: 10,
        now,
        ..Default::default()
    }
}

fn s(x: &str) -> String {
    x.to_string()
}

/// A second connection for direct SQL; foreign keys off like the Store (bundled SQLite defaults them on).
fn raw(dir: &TempDir) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(dir.0.join("t.db")).unwrap();
    c.execute_batch("PRAGMA foreign_keys = OFF").unwrap();
    c
}

async fn store(dir: &TempDir) -> Store {
    Store::connect(dir.0.join("t.db")).await.unwrap()
}

/// The scenario of the Python run, step by step, in the same order.
async fn scenario(st: &Store) -> Vec<(String, Value)> {
    let mut out: Vec<(String, Value)> = Vec::new();
    macro_rules! p {
        ($name:expr, $v:expr) => {{
            let v: Value = $v;
            out.push(($name.to_string(), v));
        }};
    }
    let first = NewMashup { teams: 2, host_team: Some(1), tags: s("dp,graphs"), exclude_tags: s("math"), team_size: 2, rated: false, ..new(100, 1000) };
    p!("create1", json!(st.create_mashup(first).await.unwrap()));
    p!("get1", opt_row(st.get_mashup(1).await.unwrap()));
    p!("get_missing", opt_row(st.get_mashup(99).await.unwrap()));
    p!("host_again", err(st.create_mashup(new(100, 1000)).await.map(|_| ())));
    p!("add_bob", err(st.add_mashup_player(1, 1, 200, "bob", 1001, 20, Some(2), 2).await));
    p!("add_bob_again", err(st.add_mashup_player(1, 1, 200, "bob", 1002, 20, Some(2), 2).await));
    p!("add_carol", err(st.add_mashup_player(1, 1, 300, "carol", 1001, 20, Some(1), 2).await));
    p!("add_dave_team_full", err(st.add_mashup_player(1, 1, 400, "dave", 1003, 20, Some(1), 2).await));
    p!("add_dave_cap", err(st.add_mashup_player(1, 1, 400, "dave", 1003, 3, Some(2), 2).await));
    p!("add_closed", err(st.add_mashup_player(77, 1, 400, "dave", 1003, 3, None, 0).await));
    p!("move_full", err(st.move_mashup_player(1, 300, 2, 1).await));
    p!("move_ok", err(st.move_mashup_player(1, 300, 2, 3).await));
    p!("move_closed", err(st.move_mashup_player(77, 300, 2, 0).await));
    p!("players1", players(st.mashup_players(1).await.unwrap()));
    p!("active_for_alice", json!(st.active_mashup_for(1, 100).await.unwrap()));
    p!("active_for_nobody", json!(st.active_mashup_for(1, 999).await.unwrap()));
    p!("remove_bob", json!(st.remove_mashup_player(1, 200).await.unwrap()));
    p!("remove_bob_again", json!(st.remove_mashup_player(1, 200).await.unwrap()));
    p!("start_wrong", json!(st.start_mashup(1, &[(s("1A"), 800)], 2000, 9200, &[100], &[]).await.unwrap()));
    let three = [(s("1A"), 800), (s("2B"), 900), (s("3C"), 1000)];
    p!("start_ok", json!(st.start_mashup(1, &three, 2000, 9200, &[300, 100], &[s("9Z"), s("1A"), s("9Z")]).await.unwrap()));
    p!("start_again", json!(st.start_mashup(1, &[(s("1A"), 800)], 2000, 9200, &[100, 300], &[]).await.unwrap()));
    p!("remove_active", json!(st.remove_mashup_player(1, 300).await.unwrap()));
    p!("problems", json!(st.mashup_problems(1).await.unwrap()));
    p!("cells", cells(st.mashup_cells(1).await.unwrap()));
    p!("excluded", sorted(st.mashup_excluded(1).await.unwrap()));
    p!("excluded_none", sorted(st.mashup_excluded(5).await.unwrap()));
    p!("replace_stale", json!(st.replace_cell(1, 1, 0, "4D", 1100, 3000).await.unwrap()));
    p!("replace_ok", json!(st.replace_cell(1, 1, 1, "4D", 1100, 3000).await.unwrap()));
    p!("replace_nocell", json!(st.replace_cell(1, 7, 0, "5E", 1100, 3000).await.unwrap()));
    p!("replace_nomashup", json!(st.replace_cell(55, 0, 0, "5E", 1100, 3000).await.unwrap()));
    p!("cells2", cells(st.mashup_cells(1).await.unwrap()));
    p!("problems2", json!(st.mashup_problems(1).await.unwrap()));
    p!("log1", json!(st.mark_logged(1, "1A").await.unwrap()));
    p!("log2", json!(st.mark_logged(1, "1A").await.unwrap()));
    p!("solves_rec", json!(st.record_mashup_solves(1, 300, &[(s("2B"), (2500, 1)), (s("1A"), (2100, 0))]).await.unwrap()));
    p!("solves_rec2", json!(st.record_mashup_solves(1, 300, &[(s("2B"), (2600, 5)), (s("3C"), (2700, 2))]).await.unwrap()));
    p!("solves_rec3", json!(st.record_mashup_solves(1, 100, &[(s("1A"), (2200, 3))]).await.unwrap()));
    p!("solves_empty", json!(st.record_mashup_solves(1, 100, &[]).await.unwrap()));
    p!("solves", solves(st.mashup_solves(1).await.unwrap()));
    p!("solves_none", solves(st.mashup_solves(5).await.unwrap()));
    p!("gave_up", json!(st.mark_mashup_gave_up(1, 100).await.unwrap()));
    p!("gave_up_again", json!(st.mark_mashup_gave_up(1, 100).await.unwrap()));
    p!("active_after_giveup", json!(st.active_mashup_for(1, 100).await.unwrap()));
    p!("players_after_giveup", players(st.mashup_players(1).await.unwrap()));
    st.set_mashup_message(1, 11, 555).await.unwrap();
    p!("set_msg", Value::Null);
    st.set_mashup_thread(1, 666).await.unwrap();
    p!("set_thread", Value::Null);
    st.set_log_board(1, 777).await.unwrap();
    p!("set_log_board", Value::Null);
    p!("in_active", ids(st.mashups_in(&["active"]).await.unwrap()));
    p!("in_none", ids(st.mashups_in(&[]).await.unwrap()));
    p!("finish_ok", json!(st.finish_mashup(1, &[(100, -5), (300, 12), (999, 1)], Some(5000)).await.unwrap()));
    p!("finish_again", json!(st.finish_mashup(1, &[(100, 1)], None).await.unwrap()));
    p!("get1_done", opt_row(st.get_mashup(1).await.unwrap()));
    p!("players_done", players(st.mashup_players(1).await.unwrap()));
    p!("active_done", json!(st.active_mashup_for(1, 300).await.unwrap()));
    let bingo = NewMashup { count: 2, duration_seconds: 3600, penalty_minutes: 5, mode: s("bingo"), size: 3, win: s("continuous"), contest_id: Some(1234), ..new(100, 6000) };
    p!("create2", json!(st.create_mashup(bingo).await.unwrap()));
    p!("cancel", json!(st.cancel_mashup(2).await.unwrap()));
    p!("cancel_again", json!(st.cancel_mashup(2).await.unwrap()));
    p!("get2", opt_row(st.get_mashup(2).await.unwrap()));
    p!("players2", players(st.mashup_players(2).await.unwrap()));
    let third = NewMashup { count: 1, duration_seconds: 3600, penalty_minutes: 5, ..new(100, 7000) };
    p!("create3", json!(st.create_mashup(third).await.unwrap()));
    p!("start3", json!(st.start_mashup(3, &[(s("7G"), 1200)], 7100, 10700, &[100], &[]).await.unwrap()));
    p!("finish3", json!(st.finish_mashup(3, &[], Some(99999)).await.unwrap()));
    p!("get3", opt_row(st.get_mashup(3).await.unwrap()));
    p!("finished_100", rows(st.finished_mashups(1, 100, 50).await.unwrap()));
    p!("finished_100_lim1", rows(st.finished_mashups(1, 100, 1).await.unwrap()));
    p!("finished_300", rows(st.finished_mashups(1, 300, 50).await.unwrap()));
    p!("finished_other_guild", rows(st.finished_mashups(2, 100, 50).await.unwrap()));
    p!("in_all", ids(st.mashups_in(&["active", "finished", "cancelled"]).await.unwrap()));
    out
}

#[tokio::test]
async fn every_method_matches_python() {
    let dir = TempDir::new();
    let st = store(&dir).await;
    let got = scenario(&st).await;
    let want = expected();
    assert_eq!(got.len(), want.len());
    for (name, value) in got {
        assert_eq!(Some(&value), want.get(&name), "step {name}");
    }
    st.close().await.unwrap();
}

#[tokio::test]
async fn one_active_mashup_index_blocks_a_second_active_row() {
    let dir = TempDir::new();
    let st = store(&dir).await;
    st.create_mashup(new(100, 1)).await.unwrap();
    let insert = "INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, active) VALUES (2, 1, 100, 'a', 1, ?)";
    let raw = raw(&dir);
    let clash = raw.execute(insert, [1]).unwrap_err();
    assert!(matches!(clash, rusqlite::Error::SqliteFailure(e, _) if e.code == rusqlite::ErrorCode::ConstraintViolation));
    // a player who gave up (active = 0) does not count
    raw.execute(insert, [0]).unwrap();
    // create_mashup turns the clash into AlreadyInMatch
    let again = st.create_mashup(new(100, 2)).await.unwrap_err();
    assert_eq!(again.downcast_ref::<BotError>(), Some(&BotError::AlreadyInMatch));
    st.close().await.unwrap();
}

#[tokio::test]
async fn an_active_match_player_cannot_host_or_join() {
    // Python on the same input: AlreadyInMatch, no mashup row, AlreadyInMatch, one player left in the lobby.
    let dir = TempDir::new();
    let st = store(&dir).await;
    raw(&dir)
        .execute("INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, active) VALUES (1, 1, 100, 'a', 1, 1)", [])
        .unwrap();
    let e = st.create_mashup(new(100, 1)).await.unwrap_err();
    assert_eq!(e.downcast_ref::<BotError>(), Some(&BotError::AlreadyInMatch));
    assert_eq!(st.get_mashup(1).await.unwrap(), None);
    st.create_mashup(new(5, 1)).await.unwrap();
    let e = st.add_mashup_player(1, 1, 100, "a", 2, 20, None, 0).await.unwrap_err();
    assert_eq!(e.downcast_ref::<BotError>(), Some(&BotError::AlreadyInMatch));
    assert_eq!(st.mashup_players(1).await.unwrap().len(), 1);
    st.close().await.unwrap();
}
