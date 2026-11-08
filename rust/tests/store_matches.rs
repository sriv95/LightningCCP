//! Port of the match, rating, stats and daily tests of `tests/test_store.py` (plus the daily methods, which Python
//! covers through `tests/test_daily.py`). Expected values come from the Python tests and from running the Python store.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lightningccp::errors::BotError;
use lightningccp::store::base::{MIGRATIONS, Store};
use lightningccp::store::matches::{DailySolve, NewMatch};
use rusqlite::Connection;

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-matches-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn db(&self) -> PathBuf {
        self.0.join("sub").join("t.db")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn store() -> (TempDir, Store) {
    let dir = TempDir::new();
    let s = Store::connect(dir.db()).await.unwrap();
    (dir, s)
}

/// Python `make_match(store, host, mode, max_players, invited, guild, now)`.
async fn make_match(s: &Store, host: i64, mode: &str, max_players: i64, guild: i64, now: i64) -> anyhow::Result<i64> {
    s.link_handle(guild, host, &format!("h{host}"), Some(1000)).await.unwrap();
    s.create_match(NewMatch {
        guild_id: guild,
        channel_id: 5,
        host_id: host,
        host_handle: format!("h{host}"),
        mode: mode.into(),
        max_players,
        now,
        ..Default::default()
    })
    .await
}

async fn duel(s: &Store, host: i64) -> i64 {
    make_match(s, host, "duel", 2, 1, 100).await.unwrap()
}

async fn solo(s: &Store, host: i64, guild: i64) -> i64 {
    make_match(s, host, "solo", 1, guild, 100).await.unwrap()
}

fn is(e: anyhow::Error, want: BotError) -> bool {
    e.downcast_ref::<BotError>() == Some(&want)
}

/// Raw access to the same file for rows the store has no writer for here (mashups).
fn raw(dir: &TempDir) -> Connection {
    Connection::open(dir.db()).unwrap()
}

const MASHUP: &str = "INSERT INTO mashups (id, guild_id, channel_id, host_id, min_rating, max_rating, count, duration_seconds, \
                      penalty_minutes, status, created_at, started_at, ends_at) VALUES (1, 1, 5, 1, 800, 1500, 1, 7200, 0, 'finished', 1, 10, 700);";

#[tokio::test]
async fn create_match_adds_host_player() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    let m = s.get_match(mid).await.unwrap().unwrap();
    assert_eq!((m.status.as_str(), m.mode.as_str(), m.host_id, m.max_players), ("lobby", "duel", 1, 2));
    assert_eq!((m.tags.as_str(), m.exclude_tags.as_str(), m.message_id, m.rating, m.created_at), ("", "", None, None, 100));
    let players = s.get_players(mid).await.unwrap();
    let got: Vec<_> = players.iter().map(|p| (p.user_id, p.handle.as_str(), p.active, p.gave_up)).collect();
    assert_eq!(got, [(1, "h1", true, false)]);
    assert_eq!(s.active_match_for(1, 1).await.unwrap(), Some(mid));
    assert_eq!(s.get_match(999).await.unwrap(), None);
}

#[tokio::test]
async fn one_active_match_per_user() {
    let (_d, s) = store().await;
    duel(&s, 1).await;
    let e = make_match(&s, 1, "duel", 2, 1, 100).await.unwrap_err();
    assert!(is(e, BotError::AlreadyInMatch));
    let other = duel(&s, 2).await;
    let e = s.add_player(other, 1, 1, "h1", 101).await.unwrap_err();
    assert!(is(e, BotError::AlreadyInMatch));
    assert_eq!(s.active_match_for(1, 3).await.unwrap(), None);
    assert_eq!(s.active_match_for(2, 1).await.unwrap(), None); // guild scoped
    // the failed create left no half-written match behind
    let n: i64 = raw(&_d).query_row("SELECT COUNT(*) FROM matches", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn add_player_rules() {
    let (_d, s) = store().await;
    let mid = make_match(&s, 1, "custom", 3, 1, 100).await.unwrap();
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    assert!(is(s.add_player(mid, 1, 2, "h2", 102).await.unwrap_err(), BotError::AlreadyInMatch));
    s.add_player(mid, 1, 3, "h3", 103).await.unwrap();
    assert!(is(s.add_player(mid, 1, 4, "h4", 104).await.unwrap_err(), BotError::match_full()));
    let ids: Vec<_> = s.get_players(mid).await.unwrap().iter().map(|p| p.user_id).collect();
    assert_eq!(ids, [1, 2, 3]);
    assert!(s.cancel_match(mid, 110).await.unwrap());
    assert!(is(s.add_player(mid, 1, 4, "h4", 111).await.unwrap_err(), BotError::match_closed()));
    assert!(is(s.add_player(999, 1, 4, "h4", 111).await.unwrap_err(), BotError::match_closed()));
}

#[tokio::test]
async fn remove_player_only_in_lobby() {
    let (_d, s) = store().await;
    let mid = make_match(&s, 1, "custom", 3, 1, 100).await.unwrap();
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    assert!(s.remove_player(mid, 2).await.unwrap());
    assert!(!s.remove_player(mid, 2).await.unwrap());
    assert_eq!(s.active_match_for(1, 2).await.unwrap(), None);
    s.add_player(mid, 1, 2, "h2", 102).await.unwrap();
    assert!(s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap());
    assert!(!s.remove_player(mid, 2).await.unwrap()); // started: cannot leave
}

#[tokio::test]
async fn cancel_match_frees_everyone_once() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    assert!(s.cancel_match(mid, 150).await.unwrap());
    assert!(!s.cancel_match(mid, 151).await.unwrap());
    let m = s.get_match(mid).await.unwrap().unwrap();
    assert_eq!((m.status.as_str(), m.finished_at), ("cancelled", Some(150)));
    assert_eq!(s.active_match_for(1, 1).await.unwrap(), None);
    assert_eq!(s.active_match_for(1, 2).await.unwrap(), None);
}

#[tokio::test]
async fn start_after_cancel_is_rejected() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.cancel_match(mid, 150).await.unwrap();
    assert!(!s.start_match(mid, 1000, "1700A", 200, 7400, &[1]).await.unwrap());
    assert_eq!(s.get_match(mid).await.unwrap().unwrap().status, "cancelled");
    assert!(!s.start_match(999, 1000, "1700A", 200, 7400, &[1]).await.unwrap());
}

#[tokio::test]
async fn start_match_sets_fields_once() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    assert!(s.start_match(mid, 1000, "1700A", 200, 7400, &[1]).await.unwrap());
    assert!(!s.start_match(mid, 1100, "1700B", 300, 7500, &[1]).await.unwrap());
    let m = s.get_match(mid).await.unwrap().unwrap();
    assert_eq!(
        (m.status.as_str(), m.rating, m.problem_key.as_deref(), m.started_at, m.expires_at),
        ("active", Some(1000), Some("1700A"), Some(200), Some(7400))
    );
}

#[tokio::test]
async fn start_match_requires_the_same_player_set() {
    let (_d, s) = store().await;
    let mid = make_match(&s, 1, "custom", 3, 1, 100).await.unwrap();
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    s.add_player(mid, 1, 3, "h3", 102).await.unwrap();
    assert!(s.remove_player(mid, 3).await.unwrap()); // a player left after the snapshot
    assert!(!s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2, 3]).await.unwrap());
    s.add_player(mid, 1, 3, "h3", 103).await.unwrap(); // a player joined after the snapshot
    assert!(!s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap());
    assert_eq!(s.get_match(mid).await.unwrap().unwrap().status, "lobby");
    assert!(s.start_match(mid, 1000, "1700A", 200, 7400, &[3, 2, 1]).await.unwrap());
}

#[tokio::test]
async fn give_up_frees_slot_once_and_only_when_active() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    assert!(!s.mark_gave_up(mid, 2).await.unwrap()); // lobby
    s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    assert!(s.mark_gave_up(mid, 2).await.unwrap());
    assert!(!s.mark_gave_up(mid, 2).await.unwrap());
    let players = s.get_players(mid).await.unwrap();
    let p2 = players.iter().find(|p| p.user_id == 2).unwrap();
    assert!(p2.gave_up && !p2.active);
    assert_eq!(s.active_match_for(1, 2).await.unwrap(), None);
    assert_eq!(s.active_match_for(1, 1).await.unwrap(), Some(mid));
}

#[tokio::test]
async fn record_solves_sets_time_once() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    s.record_solves(mid, &[(1, 300)]).await.unwrap(); // lobby: ignored
    s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    s.record_solves(mid, &[(1, 300), (2, 400)]).await.unwrap();
    s.record_solves(mid, &[(1, 999)]).await.unwrap();
    let got: Vec<_> = s.get_players(mid).await.unwrap().iter().map(|p| (p.user_id, p.solved_at)).collect();
    assert_eq!(got, [(1, Some(300)), (2, Some(400))]);
    assert!(!s.mark_gave_up(mid, 1).await.unwrap()); // solved players cannot give up
}

#[tokio::test]
async fn finish_match_is_once() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    assert!(s.finish_match(mid, 500, &[(1, 5), (2, -5)], None).await.unwrap());
    assert!(!s.finish_match(mid, 501, &[(1, 50), (2, -50)], None).await.unwrap());
    let got: Vec<_> = s.get_players(mid).await.unwrap().iter().map(|p| (p.user_id, p.delta)).collect();
    assert_eq!(got, [(1, Some(5)), (2, Some(-5))]);
    assert_eq!(s.get_match(mid).await.unwrap().unwrap().status, "finished");
    assert_eq!(s.rating(1, 1).await.unwrap(), 805);
    assert_eq!(s.rating(1, 2).await.unwrap(), 795);
    assert_eq!(s.active_match_for(1, 1).await.unwrap(), None);
}

#[tokio::test]
async fn rating_sums_across_matches_and_guilds() {
    let (_d, s) = store().await;
    for _ in 0..2 {
        let mid = solo(&s, 1, 1).await;
        s.start_match(mid, 1000, "1700A", 200, 7400, &[1]).await.unwrap();
        s.finish_match(mid, 500, &[(1, 5)], None).await.unwrap();
    }
    let other = solo(&s, 1, 2).await;
    s.start_match(other, 1000, "1700A", 200, 7400, &[1]).await.unwrap();
    s.finish_match(other, 500, &[(1, 7)], None).await.unwrap();
    assert_eq!(s.rating(1, 1).await.unwrap(), 810);
    assert_eq!(s.rating(2, 1).await.unwrap(), 807);
    assert_eq!(s.rating(1, 99).await.unwrap(), 800); // everyone starts at 800
}

#[tokio::test]
async fn matches_due() {
    let (_d, s) = store().await;
    let lobby = make_match(&s, 1, "duel", 2, 1, 100).await.unwrap();
    let active = make_match(&s, 2, "duel", 2, 1, 100).await.unwrap();
    s.start_match(active, 1000, "1700A", 200, 7400, &[2]).await.unwrap();
    assert!(s.matches_due(99, 7399).await.unwrap().is_empty());
    let ids: Vec<_> = s.matches_due(100, 7399).await.unwrap().iter().map(|m| m.id).collect();
    assert_eq!(ids, [lobby]);
    let mut due: Vec<_> = s.matches_due(100, 7400).await.unwrap().iter().map(|m| m.id).collect();
    due.sort();
    assert_eq!(due, [lobby, active]);
}

#[tokio::test]
async fn set_message() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.set_message(mid, 77, 88).await.unwrap();
    let m = s.get_match(mid).await.unwrap().unwrap();
    assert_eq!((m.channel_id, m.message_id), (77, Some(88)));
}

#[tokio::test]
async fn match_survives_reconnect() {
    let dir = TempDir::new();
    let path = dir.0.join("t.db");
    let s = Store::connect(&path).await.unwrap();
    let mid = duel(&s, 1).await;
    s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
    s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    s.close().await.unwrap();
    let s2 = Store::connect(&path).await.unwrap();
    assert_eq!(s2.get_match(mid).await.unwrap().unwrap().status, "active");
    assert_eq!(s2.get_players(mid).await.unwrap().len(), 2);
    assert_eq!(s2.active_match_for(1, 2).await.unwrap(), Some(mid));
    s2.close().await.unwrap();
}

#[tokio::test]
async fn stats_orders_and_counts_finished_only() {
    let (_d, s) = store().await;
    s.link_handle(1, 2, "linked2", None).await.unwrap();
    for deltas in [[(1, 5), (2, -5)], [(1, -2), (2, 2)]] {
        let mid = duel(&s, 1).await;
        s.add_player(mid, 1, 2, "h2", 101).await.unwrap();
        s.start_match(mid, 1000, "1700A", 200, 7400, &[1, 2]).await.unwrap();
        s.finish_match(mid, 500, &deltas, None).await.unwrap();
    }
    make_match(&s, 3, "duel", 2, 1, 100).await.unwrap(); // lobby: not counted
    assert_eq!(s.stats(1, "all").await.unwrap(), [(1, "h1".to_string(), 803, 2), (2, "linked2".to_string(), 797, 2)]);
    assert!(s.stats(9, "all").await.unwrap().is_empty());
}

#[tokio::test]
async fn migration_3_resets_old_points_to_start_rating() {
    let dir = TempDir::new();
    let path = dir.0.join("t.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(&format!("{}\n{}\nPRAGMA user_version = 2;", MIGRATIONS[0], MIGRATIONS[1])).unwrap();
    db.execute_batch(
        "INSERT INTO matches (id, guild_id, channel_id, host_id, mode, max_players, status, created_at) VALUES (1, 1, 1, 1, 'solo', 1, 'finished', 1);\
         INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, delta, active) VALUES (1, 1, 1, 'h1', 1, 9, 0);",
    )
    .unwrap();
    drop(db);
    let s = Store::connect(&path).await.unwrap();
    assert_eq!(s.rating(1, 1).await.unwrap(), 800);
    assert_eq!(s.stats(1, "all").await.unwrap(), [(1, "h1".to_string(), 800, 1)]); // still counted as a finished match
    s.close().await.unwrap();
}

#[tokio::test]
async fn rating_changes_lists_finished_matches_oldest_first() {
    let (_d, s) = store().await;
    for (finished, delta) in [(900, 5), (500, -2)] {
        let mid = solo(&s, 1, 1).await;
        s.start_match(mid, 100, "1700A", 200, 7400, &[1]).await.unwrap();
        s.finish_match(mid, finished, &[(1, delta)], None).await.unwrap();
    }
    make_match(&s, 1, "duel", 2, 1, 100).await.unwrap(); // lobby: not counted
    assert_eq!(s.rating_changes(1, 1).await.unwrap(), [(500, -2), (900, 5)]);
    assert!(s.rating_changes(1, 2).await.unwrap().is_empty() && s.rating_changes(2, 1).await.unwrap().is_empty());
}

#[tokio::test]
async fn finished_matches_newest_first_with_players() {
    let (_d, s) = store().await;
    let mut ids = vec![];
    for (finished, delta) in [(500, 10), (900, -3)] {
        let mid = solo(&s, 1, 1).await;
        s.start_match(mid, 100, "1700A", 200, 7400, &[1]).await.unwrap();
        s.finish_match(mid, finished, &[(1, delta)], None).await.unwrap();
        ids.push(mid);
    }
    let d = duel(&s, 2).await;
    s.add_player(d, 1, 1, "h1", 101).await.unwrap();
    s.start_match(d, 100, "1700A", 200, 7400, &[1, 2]).await.unwrap();
    s.finish_match(d, 700, &[(1, 49), (2, -49)], None).await.unwrap();
    make_match(&s, 1, "duel", 2, 1, 100).await.unwrap(); // lobby: not listed
    let records = s.finished_matches(1, 1, 100).await.unwrap();
    assert_eq!(records.iter().map(|(m, _)| m.id).collect::<Vec<_>>(), [ids[1], d, ids[0]]);
    let mut got: Vec<_> = records[1].1.iter().map(|p| (p.user_id, p.delta)).collect();
    got.sort();
    assert_eq!(got, [(1, Some(49)), (2, Some(-49))]);
    assert_eq!(s.finished_matches(1, 1, 2).await.unwrap().len(), 2);
    assert!(s.finished_matches(1, 3, 100).await.unwrap().is_empty() && s.finished_matches(2, 1, 100).await.unwrap().is_empty());
}

// --- daily and mashup-aware methods (Python: tests/test_daily.py, tests/test_mashup.py) ---

#[tokio::test]
async fn issue_daily_replaces_an_unsolved_one_only_for_a_different_rating() {
    let (_d, s) = store().await;
    s.issue_daily(1, 1, 0, "1701A", 900, 1000).await.unwrap();
    s.issue_daily(1, 1, 0, "1799Z", 900, 1005).await.unwrap(); // same rating: kept
    let d = s.get_daily(1, 1, 0).await.unwrap().unwrap();
    assert_eq!((d.problem_key.as_deref(), d.rating, d.issued_at, d.solved_at, d.match_id), (Some("1701A"), Some(900), Some(1000), None, None));
    s.issue_daily(1, 1, 0, "1700A", 800, 1010).await.unwrap();
    let d = s.get_daily(1, 1, 0).await.unwrap().unwrap();
    assert_eq!((d.problem_key.as_deref(), d.rating, d.issued_at), (Some("1700A"), Some(800), Some(1010)));
    assert_eq!(s.get_daily(1, 1, 1).await.unwrap(), None);
}

#[tokio::test]
async fn record_daily_solve_books_a_finished_solo_match_once() {
    let (_d, s) = store().await;
    s.issue_daily(1, 1, 0, "1700A", 800, 1000).await.unwrap();
    let solve = DailySolve { guild_id: 1, channel_id: 5, user_id: 1, handle: "h1".into(), day: 0, solved_at: 1100, delta: 40, now: 1200 };
    assert!(s.record_daily_solve(solve.clone()).await.unwrap());
    assert!(!s.record_daily_solve(solve.clone()).await.unwrap());
    assert!(!s.record_daily_solve(DailySolve { day: 7, ..solve }).await.unwrap()); // nothing issued
    let d = s.get_daily(1, 1, 0).await.unwrap().unwrap();
    assert_eq!(d.solved_at, Some(1200));
    let m = s.get_match(d.match_id.unwrap()).await.unwrap().unwrap();
    assert_eq!(
        (m.mode.as_str(), m.status.as_str(), m.max_players, m.rating, m.problem_key.as_deref(), m.created_at, m.started_at, m.finished_at),
        ("solo", "finished", 1, Some(800), Some("1700A"), 1000, Some(1000), Some(1200))
    );
    let p = &s.get_players(m.id).await.unwrap()[0];
    assert_eq!((p.joined_at, p.solved_at, p.delta, p.active), (1000, Some(1100), Some(40), false));
    assert_eq!(s.rating(1, 1).await.unwrap(), 840);
    assert_eq!(s.active_match_for(1, 1).await.unwrap(), None);
    assert_eq!(s.daily_days(1, 1).await.unwrap(), [0]); // solved match and claimed daily, one day
}

#[tokio::test]
async fn solo_finish_with_bonus_day_doubles_the_first_claim_only() {
    let (_d, s) = store().await;
    s.issue_daily(1, 1, 0, "1700A", 800, 10).await.unwrap(); // an issued, unsolved daily is claimed
    let a = solo(&s, 1, 1).await;
    s.start_match(a, 800, "1700B", 20, 7400, &[1]).await.unwrap();
    s.record_solves(a, &[(1, 30)]).await.unwrap();
    assert!(s.finish_match(a, 40, &[(1, 20)], Some(0)).await.unwrap());
    assert_eq!(s.get_players(a).await.unwrap()[0].delta, Some(40));
    let d = s.get_daily(1, 1, 0).await.unwrap().unwrap();
    assert_eq!((d.solved_at, d.match_id, d.problem_key.as_deref()), (Some(40), Some(a), Some("1700A")));
    let b = solo(&s, 1, 1).await; // second solve of the day pays normally
    s.start_match(b, 800, "1700C", 50, 7400, &[1]).await.unwrap();
    s.finish_match(b, 60, &[(1, 20)], Some(0)).await.unwrap();
    assert_eq!(s.get_players(b).await.unwrap()[0].delta, Some(20));
    let c = solo(&s, 1, 1).await; // no gain: no claim, no doubling
    s.start_match(c, 800, "1700D", 70, 7400, &[1]).await.unwrap();
    s.finish_match(c, 80, &[(1, 0)], Some(1)).await.unwrap();
    assert_eq!(s.get_daily(1, 1, 1).await.unwrap(), None);
    let e = solo(&s, 2, 1).await; // no daily issued: the claim row has no problem
    s.start_match(e, 800, "1700E", 70, 7400, &[2]).await.unwrap();
    s.finish_match(e, 90, &[(2, 5)], Some(0)).await.unwrap();
    assert_eq!(s.get_players(e).await.unwrap()[0].delta, Some(10));
    assert_eq!(s.get_daily(1, 2, 0).await.unwrap().unwrap().problem_key, None);
    assert_eq!(s.rating(1, 1).await.unwrap(), 800 + 40 + 20 + 0);
}

#[tokio::test]
async fn bonus_day_with_two_deltas_is_an_error_that_rolls_back() {
    let (_d, s) = store().await;
    let mid = duel(&s, 1).await;
    s.start_match(mid, 800, "1700A", 20, 7400, &[1]).await.unwrap();
    assert!(s.finish_match(mid, 40, &[(1, 5), (2, -5)], Some(0)).await.is_err());
    assert_eq!(s.get_match(mid).await.unwrap().unwrap().status, "active");
}

#[tokio::test]
async fn daily_days_and_stats_include_contests() {
    let (d, s) = store().await;
    raw(&d)
        .execute_batch(&format!(
            "{MASHUP} INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, active, delta) VALUES (1, 1, 1, 'm1', 1, 0, 12), (1, 1, 2, 'm2', 1, 0, -12);\
             INSERT INTO mashup_solves (mashup_id, user_id, problem_key, solved_at) VALUES (1, 1, '1700A', {}), (1, 2, '1700A', {});",
            86400 * 3 + 5,
            86400 * 3 + 9
        ))
        .unwrap();
    let mid = solo(&s, 1, 1).await;
    s.start_match(mid, 800, "1700A", 20, 7400, &[1]).await.unwrap();
    s.record_solves(mid, &[(1, 86400 * 5 + 1)]).await.unwrap();
    s.finish_match(mid, 86400 * 5 + 2, &[(1, 10)], None).await.unwrap();
    assert_eq!(s.daily_days(1, 1).await.unwrap(), [5, 3]);
    assert_eq!(s.daily_days(1, 2).await.unwrap(), [3]);
    assert!(s.daily_days(1, 3).await.unwrap().is_empty() && s.daily_days(2, 1).await.unwrap().is_empty());
    let by_user = s.daily_days_by_user(1).await.unwrap();
    assert_eq!(by_user.len(), 2);
    assert!(by_user.contains(&(1, vec![5, 3])) && by_user.contains(&(2, vec![3])));
    assert!(s.daily_days_by_user(2).await.unwrap().is_empty());
    assert_eq!(s.rating(1, 1).await.unwrap(), 822);
    assert_eq!(s.rating(1, 2).await.unwrap(), 788);
    assert_eq!(s.stats(1, "all").await.unwrap(), [(1, "h1".to_string(), 822, 2), (2, "m2".to_string(), 788, 1)]);
    assert_eq!(s.stats(1, "matches").await.unwrap(), [(1, "h1".to_string(), 810, 1)]);
    assert_eq!(s.stats(1, "contests").await.unwrap(), [(1, "h1".to_string(), 812, 1), (2, "m2".to_string(), 788, 1)]);
    assert_eq!(s.stats(1, "nonsense").await.unwrap().len(), 2); // unknown kind reads both, like Python
    assert_eq!(s.rating_changes(1, 1).await.unwrap(), [(700, 12), (86400 * 5 + 2, 10)]);
}

#[tokio::test]
async fn an_active_mashup_player_cannot_create_or_join_a_match() {
    let (d, s) = store().await;
    raw(&d)
        .execute_batch(&format!("{MASHUP} UPDATE mashups SET status = 'active'; INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at, active) VALUES (1, 1, 1, 'm1', 1, 1);"))
        .unwrap();
    assert!(is(make_match(&s, 1, "duel", 2, 1, 100).await.unwrap_err(), BotError::AlreadyInMatch));
    let other = duel(&s, 2).await;
    assert!(is(s.add_player(other, 1, 1, "h1", 101).await.unwrap_err(), BotError::AlreadyInMatch));
    assert!(make_match(&s, 1, "duel", 2, 2, 100).await.is_ok()); // other guild is free
}

#[tokio::test]
async fn a_bad_mode_is_reported_as_already_in_match_like_python() {
    // Python wraps every sqlite3.IntegrityError (a CHECK failure too) in AlreadyInMatch.
    let (_d, s) = store().await;
    assert!(is(make_match(&s, 1, "bogus", 2, 1, 100).await.unwrap_err(), BotError::AlreadyInMatch));
}
