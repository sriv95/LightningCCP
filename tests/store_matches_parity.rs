//! Parity check from PARITY.md for the match tables: the same scenario run through the Rust store must read back
//! exactly what the Python store read after running it (frozen in tests/fixtures/store_matches_reads.json, made by
//! the Python original, which is gone), and the `one_active_match` index must reject a second active match with
//! `AlreadyInMatch`.
use lightningccp::errors::BotError;
use lightningccp::store::base::Store;
use lightningccp::store::matches::{DailyRow, DailySolve, MatchPlayer, MatchRow, NewMatch};
use serde_json::{Value, json};

/// Mashup rows added after the scenario (users 1 and 2 solved a contest on day 3).
const MASHUP: &str = "INSERT INTO mashups (id, guild_id, channel_id, host_id, min_rating, max_rating, count, duration_seconds, \
    penalty_minutes, status, created_at, started_at, ends_at) VALUES (1, 1, 5, 1, 800, 1500, 1, 7200, 0, 'finished', 1, 10, 700);\
    INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, active, delta) VALUES (1, 1, 1, 'm1', 1, 0, 12), (1, 1, 2, 'm2', 1, 0, -12);\
    INSERT INTO mashup_solves (mashup_id, user_id, problem_key, solved_at) VALUES (1, 1, '1700A', 259205), (1, 2, '1700A', 259209);";

fn mk(host: i64, mode: &str, n: i64, tags: &str) -> NewMatch {
    NewMatch { guild_id: 1, channel_id: 5, host_id: host, host_handle: format!("h{host}"), mode: mode.into(), max_players: n, now: 100, tags: tags.into(), ..Default::default() }
}

async fn scenario(s: &Store) {
    let m = s.create_match(mk(1, "duel", 2, "")).await.unwrap();
    s.add_player(m, 1, 2, "h2", 101).await.unwrap();
    s.start_match(m, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    s.record_solves(m, &[(1, 300)]).await.unwrap();
    s.finish_match(m, 500, &[(1, 5), (2, -5)], None).await.unwrap();
    s.issue_daily(1, 1, 0, "1701A", 900, 1000).await.unwrap();
    let solve = DailySolve { guild_id: 1, channel_id: 5, user_id: 1, handle: "h1".into(), day: 0, solved_at: 1100, delta: 40, now: 1200 };
    s.record_daily_solve(solve).await.unwrap();
    let solo = s.create_match(mk(3, "solo", 1, "")).await.unwrap();
    s.start_match(solo, 800, "1700B", 20, 7400, &[3]).await.unwrap();
    s.record_solves(solo, &[(3, 86400 * 2 + 5)]).await.unwrap();
    s.finish_match(solo, 86400 * 2 + 9, &[(3, 10)], Some(2)).await.unwrap();
    let a = s.create_match(mk(4, "duel", 2, "")).await.unwrap();
    s.start_match(a, 700, "1700C", 50, 8000, &[4]).await.unwrap();
    let lobby = s.create_match(mk(5, "custom", 3, "dp")).await.unwrap();
    s.add_player(lobby, 1, 6, "h6", 101).await.unwrap();
    s.set_message(lobby, 77, 88).await.unwrap();
    let c = s.create_match(mk(7, "duel", 2, "")).await.unwrap();
    s.cancel_match(c, 150).await.unwrap();
}

fn match_json(m: &MatchRow) -> Value {
    json!({"id": m.id, "guild_id": m.guild_id, "channel_id": m.channel_id, "message_id": m.message_id, "host_id": m.host_id,
        "mode": m.mode, "max_players": m.max_players, "invited_id": m.invited_id, "rating_arg": m.rating_arg, "rating": m.rating,
        "problem_key": m.problem_key, "status": m.status, "created_at": m.created_at, "started_at": m.started_at,
        "expires_at": m.expires_at, "finished_at": m.finished_at, "tags": m.tags, "exclude_tags": m.exclude_tags})
}

fn player_json(p: &MatchPlayer) -> Value {
    json!({"match_id": p.match_id, "guild_id": p.guild_id, "user_id": p.user_id, "handle": p.handle, "joined_at": p.joined_at,
        "solved_at": p.solved_at, "gave_up": p.gave_up, "delta": p.delta, "active": p.active})
}

fn daily_json(d: &Option<DailyRow>) -> Value {
    match d {
        None => Value::Null,
        Some(d) => json!({"guild_id": d.guild_id, "user_id": d.user_id, "day": d.day, "problem_key": d.problem_key, "rating": d.rating,
            "issued_at": d.issued_at, "solved_at": d.solved_at, "match_id": d.match_id}),
    }
}

async fn reads(s: &Store) -> Value {
    let mut matches = vec![];
    for i in 1..8 {
        matches.extend(s.get_match(i).await.unwrap().iter().map(match_json));
    }
    let mut players = vec![];
    let mut rating = vec![];
    let mut active = vec![];
    for i in 1..8 {
        players.push(s.get_players(i).await.unwrap().iter().map(player_json).collect::<Vec<_>>());
        rating.push(s.rating(1, i).await.unwrap());
        active.push(s.active_match_for(1, i).await.unwrap());
    }
    let mut stats = vec![];
    for k in ["all", "matches", "contests"] {
        stats.push(s.stats(1, k).await.unwrap());
    }
    let mut changes = vec![];
    for u in 1..4 {
        changes.push(s.rating_changes(1, u).await.unwrap());
    }
    let mut days = s.daily_days_by_user(1).await.unwrap();
    days.sort();
    let daily: Vec<Value> = vec![daily_json(&s.get_daily(1, 1, 0).await.unwrap()), daily_json(&s.get_daily(1, 1, 2).await.unwrap())];
    let finished: Vec<Value> = s
        .finished_matches(1, 1, 100)
        .await
        .unwrap()
        .iter()
        .map(|(m, ps)| json!([match_json(m), ps.iter().map(player_json).collect::<Vec<_>>()]))
        .collect();
    let mut due: Vec<i64> = s.matches_due(100, 8000).await.unwrap().iter().map(|m| m.id).collect();
    due.sort();
    let dup = match s.create_match(mk(4, "duel", 2, "")).await {
        Ok(_) => "none".to_string(),
        Err(e) if e.downcast_ref::<BotError>() == Some(&BotError::AlreadyInMatch) => "AlreadyInMatch".to_string(),
        Err(e) => format!("{e}"),
    };
    json!({"matches": matches, "players": players, "rating": rating, "stats": stats, "changes": changes, "days": days,
        "daily": daily, "finished": finished, "due": due, "active": active, "dup": dup})
}

#[tokio::test]
async fn rust_reads_what_python_read_after_the_same_scenario() {
    let dir = std::env::temp_dir().join(format!("lccp-matches-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rs_db = dir.join("rs.db");

    let s = Store::connect(&rs_db).await.unwrap();
    scenario(&s).await;
    s.close().await.unwrap();
    rusqlite::Connection::open(&rs_db).unwrap().execute_batch(MASHUP).unwrap();

    let s = Store::connect(&rs_db).await.unwrap();
    let from_rust = reads(&s).await;
    s.close().await.unwrap();

    let from_python: Value = serde_json::from_str(include_str!("fixtures/store_matches_reads.json")).unwrap();
    assert_eq!(from_python, from_rust, "Rust reads differ from the frozen Python reads");
    assert_eq!(from_rust["dup"], "AlreadyInMatch");
    assert_eq!(from_rust["rating"][2], 820); // user 3: solo gain 10 doubled by the daily bonus
    let _ = std::fs::remove_dir_all(&dir);
}
