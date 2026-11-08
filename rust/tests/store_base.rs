//! Port of the core tests of `tests/test_store.py` (handles, pending links, reminders, recaps, weekly,
//! new-contest, caches and migrations). Expected values come from running the Python tests.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use lightningccp::cf::models::{Contest, Problem};
use lightningccp::errors::BotError;
use lightningccp::store::base::{MIGRATIONS, Store, is_constraint_violation};
use rusqlite::Connection;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-store-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Store on `<tmp>/sub/t.db`: also covers the parent directory being created.
async fn store() -> (TempDir, Store) {
    let dir = TempDir::new();
    let s = Store::connect(dir.0.join("sub").join("t.db")).await.unwrap();
    (dir, s)
}

/// An old database at `user_version = n` (the first `n` migrations), with `setup` run on it.
fn old_db(dir: &TempDir, n: usize, setup: &str) -> PathBuf {
    let path = dir.0.join("old.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(&format!("{}\nPRAGMA user_version = {n};", MIGRATIONS[..n].join("\n"))).unwrap();
    db.execute_batch(setup).unwrap();
    path
}

fn query<T: rusqlite::types::FromSql>(path: &PathBuf, sql: &str) -> Vec<T> {
    let db = Connection::open(path).unwrap();
    let mut stmt = db.prepare(sql).unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

fn contest(id: i64, name: &str, kind: &str, phase: &str, start: Option<i64>, duration: i64) -> Contest {
    Contest { id, name: name.into(), kind: kind.into(), phase: phase.into(), start_time: start, duration }
}

fn problem(contest_id: i64, index: &str, name: &str, rating: Option<i64>, tags: &[&str]) -> Problem {
    Problem { contest_id, index: index.into(), name: name.into(), rating, tags: tags.iter().map(|t| t.to_string()).collect() }
}

#[tokio::test]
async fn migrations_idempotent_on_reconnect() {
    let dir = TempDir::new();
    let path = dir.0.join("t.db");
    let s = Store::connect(&path).await.unwrap();
    s.link_handle(1, 2, "Tourist", Some(3800)).await.unwrap();
    s.close().await.unwrap();
    let s2 = Store::connect(&path).await.unwrap();
    assert_eq!(s2.get_handle(1, 2).await.unwrap().unwrap().handle, "Tourist");
    s2.close().await.unwrap();
    assert_eq!(query::<i64>(&path, "PRAGMA user_version"), vec![24]);
}

#[tokio::test]
async fn link_and_get_handle() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "Tourist", Some(3800)).await.unwrap();
    let linked = s.get_handle(1, 10).await.unwrap().unwrap();
    assert_eq!((linked.handle.as_str(), linked.rating), ("Tourist", Some(3800)));
    assert!(s.get_handle(1, 11).await.unwrap().is_none());
    assert!(s.get_handle(2, 10).await.unwrap().is_none()); // guild scoped
}

#[tokio::test]
async fn relink_same_user_updates() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "old", Some(1000)).await.unwrap();
    s.link_handle(1, 10, "new", Some(1100)).await.unwrap();
    assert_eq!(s.get_handle(1, 10).await.unwrap().unwrap().handle, "new");
}

#[tokio::test]
async fn link_handle_case_insensitive_conflict() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "Tourist", Some(3800)).await.unwrap();
    let err = s.link_handle(1, 11, "tourist", Some(3800)).await.unwrap_err();
    assert_eq!(
        err.downcast_ref::<BotError>(),
        Some(&BotError::HandleTaken("tourist is already linked to another member.".into()))
    );
    assert_eq!(s.get_handle(1, 10).await.unwrap().unwrap().handle, "Tourist");
}

#[tokio::test]
async fn same_handle_in_other_guild_ok() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "Tourist", None).await.unwrap();
    s.link_handle(2, 11, "Tourist", None).await.unwrap();
}

#[tokio::test]
async fn user_for_handle_case_insensitive() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "Tourist", None).await.unwrap();
    assert_eq!(s.user_for_handle(1, "TOURIST").await.unwrap(), Some(10));
    assert_eq!(s.user_for_handle(1, "nobody").await.unwrap(), None);
}

#[tokio::test]
async fn unlink() {
    let (_d, s) = store().await;
    s.link_handle(1, 10, "a", None).await.unwrap();
    assert!(s.unlink(1, 10).await.unwrap());
    assert!(!s.unlink(1, 10).await.unwrap());
}

#[tokio::test]
async fn linked_handles_lists_one_guild() {
    let (_d, s) = store().await;
    s.link_handle(1, 2, "b", None).await.unwrap();
    s.link_handle(1, 1, "a", None).await.unwrap();
    s.link_handle(2, 3, "c", None).await.unwrap();
    let mut got = s.linked_handles(1).await.unwrap();
    got.sort();
    assert_eq!(got, vec![(1, "a".to_string()), (2, "b".to_string())]);
}

#[tokio::test]
async fn pending_roundtrip_and_replace() {
    let (_d, s) = store().await;
    s.save_pending(1, 10, "a", "1A", 100).await.unwrap();
    s.save_pending(1, 10, "a", "2B", 200).await.unwrap();
    let p = s.get_pending(1, 10).await.unwrap().unwrap();
    assert_eq!((p.handle.as_str(), p.problem_key.as_str(), p.issued_at), ("a", "2B", 200));
    s.delete_pending(1, 10).await.unwrap();
    assert!(s.get_pending(1, 10).await.unwrap().is_none());
}

#[tokio::test]
async fn reminder_crud() {
    let (_d, s) = store().await;
    assert!(s.get_reminder(1).await.unwrap().is_none());
    s.set_reminder(1, Some(99), None, &[60, 10], "").await.unwrap();
    let r = s.get_reminder(1).await.unwrap().unwrap();
    assert_eq!((r.channel_id, r.role_id, r.before_mins), (Some(99), None, vec![60, 10]));
    s.set_reminder(1, Some(100), Some(5), &[30], "").await.unwrap();
    assert_eq!(s.get_reminder(1).await.unwrap().unwrap().role_id, Some(5));
    assert_eq!(s.all_reminders().await.unwrap().len(), 1);
    assert!(s.clear_reminder(1).await.unwrap());
    assert!(!s.clear_reminder(1).await.unwrap());
    assert!(s.all_reminders().await.unwrap().is_empty());
}

#[tokio::test]
async fn clear_reminder_keeps_role_minutes_and_exclude() {
    let (_d, s) = store().await;
    s.set_reminder(1, Some(99), Some(5), &[30], "div").await.unwrap();
    s.clear_reminder(1).await.unwrap();
    let r = s.get_reminder(1).await.unwrap().unwrap();
    assert_eq!((r.channel_id, r.role_id, r.before_mins, r.exclude.as_str()), (None, Some(5), vec![30], "div"));
}

#[tokio::test]
async fn reminder_minutes_are_stored_like_python_json() {
    let (d, s) = store().await;
    s.set_reminder(1, Some(99), None, &[60, 10], "").await.unwrap();
    assert_eq!(query::<String>(&d.0.join("sub").join("t.db"), "SELECT before_mins FROM reminder"), ["[60, 10]"]);
}

#[tokio::test]
async fn recap_channel_crud() {
    let (_d, s) = store().await;
    assert_eq!(s.get_recap(1).await.unwrap(), None);
    s.set_recap(1, 7).await.unwrap();
    s.set_recap(1, 8).await.unwrap();
    assert_eq!(s.get_recap(1).await.unwrap(), Some(8));
    assert_eq!(s.all_recaps().await.unwrap(), vec![(1, 8)]);
    assert!(s.clear_recap(1).await.unwrap());
    assert!(!s.clear_recap(1).await.unwrap());
}

#[tokio::test]
async fn migration_17_gives_old_channel_to_reminder_and_recap() {
    let dir = TempDir::new();
    let path = old_db(&dir, 16, "INSERT INTO reminder VALUES (1, 99, 5, '[60, 10]')");
    let s = Store::connect(&path).await.unwrap();
    let r = s.get_reminder(1).await.unwrap().unwrap();
    assert_eq!((r.channel_id, r.role_id, r.before_mins, r.exclude.as_str()), (Some(99), Some(5), vec![60, 10], ""));
    assert_eq!(s.get_recap(1).await.unwrap(), Some(99));
    s.close().await.unwrap();
}

#[tokio::test]
async fn mark_sent_is_once() {
    let (_d, s) = store().await;
    assert!(s.mark_sent(1, 500, 60).await.unwrap());
    assert!(!s.mark_sent(1, 500, 60).await.unwrap());
    assert!(s.mark_sent(1, 500, 10).await.unwrap());
    assert_eq!(s.sent_for_guild(1).await.unwrap(), [(500, 60), (500, 10)].into());
}

#[tokio::test]
async fn clear_reminder_forgets_sent() {
    let (_d, s) = store().await;
    s.set_reminder(1, Some(99), None, &[10], "").await.unwrap();
    s.mark_sent(1, 500, 10).await.unwrap();
    s.clear_reminder(1).await.unwrap();
    assert!(s.sent_for_guild(1).await.unwrap().is_empty());
}

#[tokio::test]
async fn contest_cache_roundtrip_replaces() {
    let (_d, s) = store().await;
    let a = contest(1, "A", "CF", "BEFORE", Some(1000), 7200);
    let b = contest(2, "B", "ICPC", "FINISHED", None, 60);
    s.replace_contests(&[a.clone(), b.clone()]).await.unwrap();
    let mut got = s.load_contests().await.unwrap();
    got.sort_by_key(|c| c.id);
    assert_eq!(got, vec![a.clone(), b]);
    s.replace_contests(std::slice::from_ref(&a)).await.unwrap();
    assert_eq!(s.load_contests().await.unwrap(), vec![a]);
}

#[tokio::test]
async fn problem_cache_roundtrip() {
    let (_d, s) = store().await;
    let p = problem(1700, "A", "Foo", Some(800), &["math", "dp"]);
    let q = problem(1700, "B", "Bar", None, &[]);
    s.replace_problems(&[p.clone(), q.clone()]).await.unwrap();
    let mut got = s.load_problems().await.unwrap();
    got.sort_by_key(|x| x.key());
    assert_eq!(got, vec![p, q]);
}

#[tokio::test]
async fn failed_contest_replace_keeps_cache() {
    let (_d, s) = store().await;
    let a = contest(1, "A", "CF", "BEFORE", Some(1000), 7200);
    let b = contest(2, "B", "ICPC", "FINISHED", None, 60);
    s.replace_contests(&[a.clone(), b.clone()]).await.unwrap();
    let err = s.replace_contests(&[a.clone(), a.clone()]).await.unwrap_err(); // duplicate id
    assert!(is_constraint_violation(&err));
    s.link_handle(1, 1, "a", None).await.unwrap(); // unrelated write must not commit the DELETE
    let mut got = s.load_contests().await.unwrap();
    got.sort_by_key(|c| c.id);
    assert_eq!(got, vec![a, b]);
}

#[tokio::test]
async fn failed_problem_replace_keeps_cache() {
    let (_d, s) = store().await;
    let p = problem(1700, "A", "Foo", Some(800), &["math", "dp"]);
    let q = problem(1700, "B", "Bar", None, &[]);
    s.replace_problems(&[p.clone(), q.clone()]).await.unwrap();
    let err = s.replace_problems(&[p.clone(), p.clone()]).await.unwrap_err(); // duplicate (contest_id, idx)
    assert!(is_constraint_violation(&err));
    s.link_handle(1, 1, "a", None).await.unwrap();
    let mut got = s.load_problems().await.unwrap();
    got.sort_by_key(|x| x.key());
    assert_eq!(got, vec![p, q]);
}

#[tokio::test]
async fn failed_replace_does_not_discard_concurrent_write() {
    let (_d, s) = store().await;
    let seed = problem(1, "B", "ok", Some(800), &[]);
    s.replace_problems(std::slice::from_ref(&seed)).await.unwrap();
    let dup = [problem(1, "A", "x", Some(800), &[]), problem(1, "A", "y", Some(800), &[])];
    let (a, b) = tokio::join!(s.replace_problems(&dup), s.link_handle(1, 1, "a", None));
    assert!(is_constraint_violation(&a.unwrap_err()));
    b.unwrap();
    assert_eq!(s.get_handle(1, 1).await.unwrap().unwrap().handle, "a");
    assert_eq!(s.load_problems().await.unwrap(), vec![seed]);
}

#[tokio::test]
async fn migration_2_drops_challenge_and_adds_match_tables() {
    let dir = TempDir::new();
    let path = old_db(&dir, 1, "INSERT INTO challenge (guild_id, user_id, problem_key, rating, issued_at) VALUES (1, 1, '1A', 800, 1)");
    Store::connect(&path).await.unwrap().close().await.unwrap();
    let names: Vec<String> = query(&path, "SELECT name FROM sqlite_master WHERE type = 'table'");
    assert!(!names.contains(&"challenge".to_string()));
    assert!(names.contains(&"matches".to_string()) && names.contains(&"match_players".to_string()));
    assert_eq!(query::<i64>(&path, "PRAGMA user_version"), vec![MIGRATIONS.len() as i64]);
}

#[tokio::test]
async fn migration_3_resets_old_points() {
    // Python also asserts Store.rating == 800 and stats; those methods belong to the match stage.
    let dir = TempDir::new();
    let path = old_db(
        &dir,
        2,
        "INSERT INTO matches (id, guild_id, channel_id, host_id, mode, max_players, status, created_at) \
         VALUES (1, 1, 1, 1, 'solo', 1, 'finished', 1); \
         INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, delta, active) \
         VALUES (1, 1, 1, 'h1', 1, 9, 0);",
    );
    Store::connect(&path).await.unwrap().close().await.unwrap();
    assert_eq!(query::<i64>(&path, "SELECT delta FROM match_players"), vec![0]);
    assert_eq!(query::<i64>(&path, "SELECT COUNT(*) FROM matches WHERE status = 'finished'"), vec![1]);
}

#[tokio::test]
async fn weekly_channel_marks_each_week_once_and_lists_events() {
    let (d, s) = store().await;
    assert_eq!(s.get_weekly(1).await.unwrap(), None);
    s.set_weekly(1, 7, 1000).await.unwrap();
    s.set_weekly(1, 8, 2000).await.unwrap(); // a new channel keeps last_sent
    assert_eq!(s.get_weekly(1).await.unwrap(), Some(8));
    assert_eq!(s.all_weeklies().await.unwrap(), vec![(1, 8, 1000)]);
    assert!(s.mark_weekly(1, 2000).await.unwrap());
    assert!(!s.mark_weekly(1, 2000).await.unwrap());
    assert!(s.clear_weekly(1).await.unwrap() && !s.clear_weekly(1).await.unwrap());
    // a finished solo match (raw rows; the match methods are not in this stage) and a lobby that is not counted
    Connection::open(d.0.join("sub").join("t.db"))
        .unwrap()
        .execute_batch(
            "INSERT INTO matches (id, guild_id, channel_id, host_id, mode, max_players, status, created_at, finished_at) \
             VALUES (1, 1, 5, 1, 'solo', 1, 'finished', 100, 500), (2, 1, 5, 1, 'duel', 2, 'lobby', 100, NULL); \
             INSERT INTO match_players (match_id, guild_id, user_id, handle, joined_at, delta, active) VALUES (1, 1, 1, 'h1', 100, 5, 0), (2, 1, 1, 'h1', 100, NULL, 1);",
        )
        .unwrap();
    s.link_handle(1, 1, "h1", Some(1000)).await.unwrap();
    assert_eq!(s.rating_events(1).await.unwrap(), vec![(1, "h1".to_string(), 500, 5)]);
    assert!(s.rating_events(2).await.unwrap().is_empty());
}

#[tokio::test]
async fn migration_5_rebuilds_the_mashup_tables() {
    let dir = TempDir::new();
    let path = old_db(&dir, 4, "INSERT INTO mashups (guild_id, channel_id, host_id, started_at, ends_at, status) VALUES (1, 1, 1, 1, 2, 'active')");
    Store::connect(&path).await.unwrap().close().await.unwrap();
    assert_eq!(query::<i64>(&path, "SELECT COUNT(*) FROM mashups"), vec![0]); // rebuilt empty
    // the new shape accepts a lobby mashup (Python: create_mashup then get_mashup status 'lobby')
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "INSERT INTO mashups (guild_id, channel_id, host_id, min_rating, max_rating, count, duration_seconds, penalty_minutes, status, created_at) \
         VALUES (1, 1, 1, 800, 1500, 5, 7200, 0, 'lobby', 10)",
    )
    .unwrap();
    assert_eq!(query::<String>(&path, "SELECT status FROM mashups"), vec!["lobby".to_string()]);
}

#[tokio::test]
async fn recap_marks_once_and_clear_recap_resets() {
    let (_d, s) = store().await;
    assert!(s.mark_recapped(1, 7).await.unwrap());
    assert!(!s.mark_recapped(1, 7).await.unwrap());
    assert_eq!(s.recapped(1).await.unwrap(), [7].into());
    assert!(s.recapped(2).await.unwrap().is_empty());
    s.set_recap(1, 99).await.unwrap();
    s.clear_recap(1).await.unwrap();
    assert!(s.recapped(1).await.unwrap().is_empty());
}

#[tokio::test]
async fn migration_8_adds_tag_columns_with_empty_defaults() {
    let dir = TempDir::new();
    let path = old_db(
        &dir,
        7,
        "INSERT INTO matches (guild_id, channel_id, host_id, mode, max_players, status, created_at) VALUES (1, 2, 3, 'solo', 1, 'lobby', 4)",
    );
    Store::connect(&path).await.unwrap().close().await.unwrap();
    assert_eq!(query::<String>(&path, "SELECT tags || '|' || exclude_tags FROM matches"), vec!["|".to_string()]);
}

const MASHUP_ROW: &str = "INSERT INTO mashups (guild_id, channel_id, host_id, min_rating, max_rating, count, duration_seconds, penalty_minutes, status, created_at) \
                          VALUES (1, 2, 3, 800, 1200, 3, 7200, 0, '";

#[tokio::test]
async fn migration_16_turns_individual_contests_into_teams_of_one() {
    let dir = TempDir::new();
    let mut setup = format!("{MASHUP_ROW}lobby', 4); {MASHUP_ROW}finished', 4);");
    for (mashup_id, user_id, at) in [(1, 3, 4), (2, 5, 9), (2, 6, 7), (2, 4, 7)] {
        setup += &format!("INSERT INTO mashup_players (mashup_id, guild_id, user_id, handle, joined_at) VALUES ({mashup_id}, 1, {user_id}, 'h', {at});");
    }
    let path = old_db(&dir, 8, &setup);
    Store::connect(&path).await.unwrap().close().await.unwrap();
    let rows = |sql: &str| -> Vec<String> { query(&path, sql) };
    assert_eq!(rows("SELECT id || ':' || teams || ':' || team_size FROM mashups ORDER BY id"), ["1:20:1", "2:3:1"]);
    assert_eq!(rows("SELECT user_id || ':' || team FROM mashup_players WHERE mashup_id = 1"), ["3:1"]);
    // numbered in joining order (joined_at, then insertion order)
    assert_eq!(rows("SELECT user_id || ':' || team FROM mashup_players WHERE mashup_id = 2 ORDER BY team"), ["6:1", "4:2", "5:3"]);
}

#[tokio::test]
async fn migration_10_defaults_the_player_cap_to_20() {
    let dir = TempDir::new();
    let path = old_db(&dir, 9, &format!("{MASHUP_ROW}lobby', 4)"));
    Store::connect(&path).await.unwrap().close().await.unwrap();
    assert_eq!(query::<i64>(&path, "SELECT max_players FROM mashups"), vec![20]);
}

#[tokio::test]
async fn a_failing_migration_changes_nothing_and_can_be_retried() {
    let dir = TempDir::new();
    let path = dir.0.join("t.db");
    Store::connect(&path).await.unwrap().close().await.unwrap();
    let broken = "ALTER TABLE reminder ADD COLUMN extra INTEGER; SELECT * FROM no_such_table;";
    let mut list: Vec<&str> = MIGRATIONS.to_vec();
    list.push(broken);
    let err = Store::connect_with(&path, &list).await.err().expect("the broken migration must fail");
    assert!(matches!(err.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(..))), "{err:?}");
    assert_eq!(query::<i64>(&path, "PRAGMA user_version"), vec![MIGRATIONS.len() as i64]);
    let columns: Vec<String> = query(&path, "SELECT name FROM pragma_table_info('reminder')");
    assert!(!columns.contains(&"extra".to_string())); // the first statement was rolled back too
}

#[tokio::test]
async fn new_contest_channel_posts_each_contest_once_and_remembers_askers() {
    let (_d, s) = store().await;
    assert_eq!(s.get_newcontest(1).await.unwrap(), None);
    s.set_newcontest(1, 7, &[10, 11]).await.unwrap();
    s.set_newcontest(1, 8, &[11, 12]).await.unwrap(); // a new channel keeps what was posted
    assert_eq!(s.get_newcontest(1).await.unwrap(), Some(8));
    assert_eq!(s.all_newcontests().await.unwrap(), vec![(1, 8)]);
    assert_eq!(s.posted(1).await.unwrap(), [10, 11, 12].into());
    assert!(s.mark_posted(1, 13).await.unwrap() && !s.mark_posted(1, 13).await.unwrap());
    assert!(s.add_contest_remind(1, 13, 5).await.unwrap());
    assert!(!s.add_contest_remind(1, 13, 5).await.unwrap());
    s.add_contest_remind(1, 13, 4).await.unwrap();
    assert_eq!(s.contest_reminders(1).await.unwrap(), vec![(13, vec![5, 4])]);
    assert!(s.clear_newcontest(1).await.unwrap() && !s.clear_newcontest(1).await.unwrap());
    assert!(s.posted(1).await.unwrap().is_empty());
}

#[tokio::test]
async fn soon_post_is_marked_once_per_contest() {
    let (_d, s) = store().await;
    s.set_newcontest(1, 7, &[10]).await.unwrap();
    assert!(s.soon_posted(1).await.unwrap().is_empty());
    assert!(s.mark_soon(1, 10).await.unwrap() && !s.mark_soon(1, 10).await.unwrap());
    assert!(!s.mark_soon(1, 99).await.unwrap()); // never announced: no row
    assert_eq!(s.soon_posted(1).await.unwrap(), [10].into());
}

#[tokio::test]
async fn contest_reminders_keep_first_seen_contest_order() {
    let (_d, s) = store().await;
    for (contest, user) in [(30, 1), (20, 2), (30, 3), (10, 4), (20, 5)] {
        s.add_contest_remind(1, contest, user).await.unwrap();
    }
    // expected order from Python: list((await store.contest_reminders(1)).items()) on the same rows
    assert_eq!(s.contest_reminders(1).await.unwrap(), vec![(30, vec![1, 3]), (20, vec![2, 5]), (10, vec![4])]);
}
