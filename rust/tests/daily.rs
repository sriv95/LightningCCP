//! Port of `tests/test_daily.py` (13 tests). The solo-win tests drive `Store::finish_match` with the one delta and bonus day
//! that `MatchService.check` passes in Python (the match service itself is stage s11). Plus rng injection.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Problem, Submission, User};
use lightningccp::daily::{CfApi, DailyService, ProblemSource};
use lightningccp::errors::BotError;
use lightningccp::logic::base::{daily_streak, floor_rating, utc_day};
use lightningccp::store::base::Store;
use lightningccp::store::matches::NewMatch;
use lightningccp::store::mashup::NewMashup;
use lightningccp::ui::embeds::a::daily_embed;
use rand::SeedableRng;
use rand::rngs::StdRng;

const GUILD: i64 = 1;

fn problem(contest: i64, name: &str, rating: i64) -> Problem {
    Problem { contest_id: contest, index: "A".into(), name: name.into(), rating: Some(rating), tags: vec![] }
}

#[derive(Default)]
struct FakeCf {
    subs: Mutex<Vec<Submission>>,
    counts: Mutex<Vec<Option<i64>>>,
}

impl CfApi for FakeCf {
    async fn user_info(&self, _: &str) -> anyhow::Result<User> {
        unreachable!()
    }
    async fn users_info(&self, _: &[String]) -> anyhow::Result<Vec<User>> {
        unreachable!()
    }
    async fn user_status(&self, _: &str, count: Option<i64>) -> anyhow::Result<Vec<Submission>> {
        self.counts.lock().unwrap().push(count);
        Ok(self.subs.lock().unwrap().clone())
    }
}

struct FakeProblems(Mutex<Vec<Problem>>);

impl ProblemSource for FakeProblems {
    fn problems(&self) -> Arc<Vec<Problem>> {
        Arc::new(self.0.lock().unwrap().clone())
    }
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        Ok(self.problems())
    }
    fn tags(&self) -> Arc<HashSet<String>> {
        Arc::default()
    }
}

struct Env {
    dir: PathBuf,
    svc: DailyService<FakeCf, FakeProblems>,
    store: Arc<Store>,
    cf: Arc<FakeCf>,
    clock: Arc<AtomicI64>,
    problems: Arc<FakeProblems>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sub(id: i64, created: i64, key: &str, verdict: &str) -> Submission {
    Submission { id, created, problem_key: key.into(), verdict: Some(verdict.into()) }
}

async fn env_with(problems: Vec<Problem>, seed: u64) -> Env {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("lccp-daily-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::connect(dir.join("t.db")).await.unwrap());
    store.link_handle(GUILD, 1, "h1", Some(1000)).await.unwrap();
    let (cf, problems) = (Arc::new(FakeCf::default()), Arc::new(FakeProblems(Mutex::new(problems))));
    let clock = Arc::new(AtomicI64::new(1000));
    let c = clock.clone();
    let svc = DailyService::with(
        store.clone(),
        cf.clone(),
        problems.clone(),
        Box::new(move || c.load(Ordering::SeqCst) as f64),
        StdRng::seed_from_u64(seed),
    );
    Env { dir, svc, store, cf, clock, problems }
}

async fn env() -> Env {
    env_with(vec![problem(1700, "Foo", 800)], 1).await
}

/// `finish_match` of a solo match won at `at`, as `MatchService.check` books it.
async fn solo_win(e: &Env, at: i64) {
    let mid = e
        .store
        .create_match(NewMatch { guild_id: GUILD, channel_id: 5, host_id: 1, host_handle: "h1".into(), mode: "solo".into(), max_players: 1, now: 1000, ..Default::default() })
        .await
        .unwrap();
    e.store.start_match(mid, 800, "1700A", 1000, 9999, &[1]).await.unwrap();
    // solo_delta(800, 800) is 10; the first claim of the day doubles it
    assert!(e.store.finish_match(mid, at, &[(1, 10)], Some(utc_day(at))).await.unwrap());
}

#[test]
fn floor_rating_and_utc_day_values() {
    let floored: Vec<i64> = [500, 800, 899, 1099, 4000].into_iter().map(floor_rating).collect();
    assert_eq!(floored, [800, 800, 800, 1000, 3500]);
    assert!(utc_day(86399) == 0 && utc_day(86400) == 1);
}

#[tokio::test]
async fn issue_then_solve_doubles_and_counts_toward_rating() {
    let e = env().await;
    let first = e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert!(first.problem == Some(problem(1700, "Foo", 800)) && first.row.rating == Some(800) && first.row.solved_at.is_none());
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().row.problem_key.as_deref(), Some("1700A")); // stable within the day
    e.cf.subs.lock().unwrap().push(sub(1100, 1100, "1700A", "OK"));
    let done = e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert!(done.delta == Some(20) && done.row.solved_at.is_some());
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 820);
    let changes: Vec<i64> = e.store.rating_changes(GUILD, 1).await.unwrap().into_iter().map(|c| c.1).collect();
    assert_eq!(changes, [20]);
    let again = e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert!(again.delta.is_none() && again.row.solved_at.is_some());
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 820); // no second payout
}

#[tokio::test]
async fn solve_before_issue_does_not_count() {
    let e = env().await;
    e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    e.cf.subs.lock().unwrap().push(sub(1, 999, "1700A", "OK")); // before issued_at (1000)
    assert!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().row.solved_at.is_none());
}

#[tokio::test]
async fn new_day_new_problem_from_floored_rating() {
    let e = env().await;
    e.problems.0.lock().unwrap().push(problem(1701, "Bar", 900));
    e.store.link_handle(GUILD, 2, "h2", Some(1000)).await.unwrap();
    let mid = e
        .store
        .create_match(NewMatch { guild_id: GUILD, channel_id: 5, host_id: 2, host_handle: "h2".into(), mode: "solo".into(), max_players: 1, now: 1, ..Default::default() })
        .await
        .unwrap();
    e.store.start_match(mid, 800, "1700A", 1, 99, &[2]).await.unwrap();
    e.store.finish_match(mid, 2, &[(2, 150)], None).await.unwrap(); // match rating 950 floors to 900 (rounding would give 1000)
    assert_eq!(e.svc.claim(GUILD, 5, 2, None).await.unwrap().row.rating, Some(900));
    e.clock.fetch_add(86400, Ordering::SeqCst);
    assert_eq!(e.svc.claim(GUILD, 5, 2, None).await.unwrap().row.day, 1);
}

#[tokio::test]
async fn solo_win_first_claims_the_day() {
    let e = env().await;
    solo_win(&e, 1100).await;
    let done = e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert!(done.row.problem_key.is_none() && done.row.solved_at.is_some());
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 820); // /daily adds nothing on top
}

#[tokio::test]
async fn daily_after_issue_then_solo_win_is_not_paid_twice() {
    let e = env().await;
    e.svc.claim(GUILD, 5, 1, None).await.unwrap(); // issued, unsolved
    solo_win(&e, 1100).await;
    e.cf.subs.lock().unwrap().push(sub(1100, 1100, "1700A", "OK"));
    assert!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().delta.is_none());
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 820);
}

#[tokio::test]
async fn contest_solve_counts_for_streak() {
    let e = env().await;
    let mid = e
        .store
        .create_mashup(NewMashup { guild_id: GUILD, channel_id: 5, host_id: 1, host_handle: "h1".into(), min_rating: 800, max_rating: 1500, count: 1, duration_seconds: 7200, penalty_minutes: 0, now: 10, ..Default::default() })
        .await
        .unwrap();
    e.store.record_mashup_solves(mid, 1, &[("1700A".into(), (86400 * 3 + 5, 0))]).await.unwrap();
    assert_eq!(e.store.daily_days(GUILD, 1).await.unwrap(), [3]);
    assert_eq!(e.store.daily_days_by_user(GUILD).await.unwrap(), [(1, vec![3])]);
    assert!(e.store.daily_days(GUILD, 2).await.unwrap().is_empty());
}

#[tokio::test]
async fn errors() {
    let e = env().await;
    let err = e.svc.claim(GUILD, 5, 99, None).await.unwrap_err();
    assert_eq!(err.downcast_ref::<BotError>(), Some(&BotError::not_linked()));
    e.cf.subs.lock().unwrap().push(sub(1, 1, "1700A", "OK")); // the only 800 problem is already solved
    let err = e.svc.claim(GUILD, 5, 1, None).await.unwrap_err();
    assert_eq!(err.downcast_ref::<BotError>(), Some(&BotError::Plain("You have solved every problem rated 800.".into())));
}

#[test]
fn daily_streak_forgives_one_missed_day() {
    assert_eq!(daily_streak(&[], 10), 0);
    assert_eq!(daily_streak(&[10, 9, 8], 10), 3);
    assert_eq!(daily_streak(&[10, 8, 6], 10), 3); // one missed day between claims keeps it alive
    assert_eq!(daily_streak(&[10, 7, 6], 10), 1); // two missed days in a row end it
    assert_eq!(daily_streak(&[8], 10), 1); // one day missed so far, still claimable
    assert_eq!(daily_streak(&[7], 10), 0); // two days missed: over
}

#[tokio::test]
async fn streak_counts_claimed_days_and_shows_in_embed() {
    let e = env().await;
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().streak, 0);
    e.cf.subs.lock().unwrap().push(sub(1100, 1100, "1700A", "OK"));
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().streak, 1);
    assert_eq!(e.store.daily_days(GUILD, 1).await.unwrap(), [0]);
    e.clock.fetch_add(2 * 86400, Ordering::SeqCst); // day 2, day 1 missed
    e.cf.subs.lock().unwrap().clear();
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().streak, 1); // new problem issued, streak alive
    let t = e.clock.load(Ordering::SeqCst);
    e.cf.subs.lock().unwrap().push(sub(t, t + 1, "1700A", "OK"));
    let view = e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert_eq!(view.streak, 2);
    let fields: Vec<(String, String)> = daily_embed(&view).fields.into_iter().map(|f| (f.name, f.value)).collect();
    assert_eq!(fields, [("Streak".to_string(), "2 days".to_string())]);
    e.clock.fetch_add(3 * 86400, Ordering::SeqCst); // days 3, 4 missed
    e.cf.subs.lock().unwrap().clear();
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().streak, 0);
}

#[tokio::test]
async fn daily_days_by_user() {
    let e = env().await;
    e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    e.cf.subs.lock().unwrap().push(sub(1100, 1100, "1700A", "OK"));
    e.svc.claim(GUILD, 5, 1, None).await.unwrap();
    assert_eq!(e.store.daily_days_by_user(GUILD).await.unwrap(), [(1, vec![0])]);
    assert!(e.store.daily_days_by_user(2).await.unwrap().is_empty());
}

#[tokio::test]
async fn chosen_rating_and_reroll_on_a_different_rating() {
    let e = env().await;
    e.problems.0.lock().unwrap().push(problem(1701, "Bar", 900));
    let first = e.svc.claim(GUILD, 5, 1, Some(900)).await.unwrap();
    assert!(first.row.rating == Some(900) && first.problem == Some(problem(1701, "Bar", 900)));
    assert_eq!(e.svc.claim(GUILD, 5, 1, Some(900)).await.unwrap().row.problem_key.as_deref(), Some("1701A")); // same rating: kept
    assert_eq!(e.svc.claim(GUILD, 5, 1, None).await.unwrap().row.problem_key.as_deref(), Some("1701A")); // no rating: kept
    e.clock.fetch_add(10, Ordering::SeqCst);
    let swapped = e.svc.claim(GUILD, 5, 1, Some(800)).await.unwrap();
    assert!(swapped.row.problem_key.as_deref() == Some("1700A") && swapped.row.issued_at == Some(1010));
    assert_eq!(swapped.row.rating, Some(800));
    // a plain check reads the 100 newest submissions, a new problem all of them (src/daily.py:73)
    assert_eq!(*e.cf.counts.lock().unwrap(), [None, Some(100), Some(100), None]);
}

#[tokio::test]
async fn solved_daily_is_claimed_before_a_reroll_and_stays_solved() {
    let e = env().await;
    e.problems.0.lock().unwrap().push(problem(1701, "Bar", 900));
    e.svc.claim(GUILD, 5, 1, Some(900)).await.unwrap();
    e.cf.subs.lock().unwrap().push(sub(1100, 1100, "1701A", "OK"));
    let done = e.svc.claim(GUILD, 5, 1, Some(800)).await.unwrap(); // solved: claim it, ignore the new rating
    assert!(done.row.solved_at.is_some() && done.delta.is_some());
    let again = e.svc.claim(GUILD, 5, 1, Some(800)).await.unwrap();
    assert!(again.row.problem_key.as_deref() == Some("1701A") && again.delta.is_none());
}

/// PARITY: the pick comes from the injected rng, is always a problem of the asked rating, and the same seed repeats it.
#[tokio::test]
async fn pick_is_valid_and_follows_the_seed() {
    let pool: Vec<Problem> = (0..20).map(|i| problem(1800 + i, "P", 800)).collect();
    let mut seen = HashSet::new();
    for seed in 0..12 {
        let e = env_with(pool.clone(), seed).await;
        let key = e.svc.claim(GUILD, 5, 1, None).await.unwrap().row.problem_key.unwrap();
        assert!(pool.iter().any(|p| p.key() == key));
        let again = env_with(pool.clone(), seed).await;
        assert_eq!(again.svc.claim(GUILD, 5, 1, None).await.unwrap().row.problem_key.unwrap(), key);
        seen.insert(key);
    }
    assert!(seen.len() > 1, "different seeds should not all pick the same problem");
}
