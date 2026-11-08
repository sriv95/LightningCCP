//! Port of `tests/test_caches.py` (5 tests) plus the refresh-interval rules of `src/cache/*.py`, run on a
//! paused tokio clock through a fake `Http` under the real `CodeforcesClient`.
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightningccp::cache::contests::{ContestCache, refresh_delay};
use lightningccp::cache::problems::ProblemCache;
use lightningccp::cf::client::{BoxFuture, CodeforcesClient, Http, Params};
use lightningccp::cf::models::{Contest, Problem};
use lightningccp::errors::BotError;
use lightningccp::store::base::Store;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-cache-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Answers `problemset.problems` and `contest.list` from mutable data; `down` makes every call fail.
#[derive(Default)]
struct FakeHttp {
    problems: Mutex<Vec<Problem>>,
    contests: Mutex<Vec<Contest>>,
    down: Mutex<bool>,
    calls: Mutex<Vec<Instant>>,
}

impl Http for FakeHttp {
    fn get<'a>(&'a self, url: &'a str, _params: &'a Params) -> BoxFuture<'a, Result<Value, ()>> {
        self.calls.lock().unwrap().push(Instant::now());
        let r = if *self.down.lock().unwrap() {
            Err(())
        } else if url.ends_with("problemset.problems") {
            let problems: Vec<Value> = self.problems.lock().unwrap().iter().map(|p| {
                json!({"contestId": p.contest_id, "index": p.index, "name": p.name, "rating": p.rating, "tags": p.tags})
            }).collect();
            Ok(json!({"status": "OK", "result": {"problems": problems, "problemStatistics": []}}))
        } else {
            let contests: Vec<Value> = self.contests.lock().unwrap().iter().map(|c| {
                json!({"id": c.id, "name": c.name, "type": c.kind, "phase": c.phase,
                       "startTimeSeconds": c.start_time, "durationSeconds": c.duration})
            }).collect();
            Ok(json!({"status": "OK", "result": contests}))
        };
        Box::pin(async move { r })
    }
}

impl FakeHttp {
    fn set_problems(&self, p: &[Problem]) {
        *self.problems.lock().unwrap() = p.to_vec();
    }
    fn set_contests(&self, c: &[Contest]) {
        *self.contests.lock().unwrap() = c.to_vec();
    }
    fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }
    /// Seconds from `t0` of each call so far.
    fn call_secs(&self, t0: Instant) -> Vec<u64> {
        self.calls.lock().unwrap().iter().map(|t| t.duration_since(t0).as_secs()).collect()
    }
}

struct Env {
    _dir: TempDir,
    http: Arc<FakeHttp>,
    cf: Arc<CodeforcesClient>,
    store: Arc<Store>,
}

async fn env() -> Env {
    let dir = TempDir::new();
    let store = Arc::new(Store::connect(dir.0.join("t.db")).await.unwrap());
    let http = Arc::new(FakeHttp::default());
    let cf = Arc::new(CodeforcesClient::with_timing(http.clone(), Duration::ZERO, 3, Duration::ZERO));
    Env { _dir: dir, http, cf, store }
}

fn problem(tags: &[&str]) -> Problem {
    Problem { contest_id: 1, index: "A".into(), name: "x".into(), rating: Some(800), tags: tags.iter().map(|t| t.to_string()).collect() }
}

fn contest(phase: &str) -> Contest {
    Contest { id: 1, name: "A".into(), kind: "CF".into(), phase: phase.into(), start_time: Some(1000), duration: 7200 }
}

const MIN: u64 = 60;
const HOUR: u64 = 3600;

fn secs(s: u64) -> Duration {
    Duration::from_secs(s)
}

// --- tests/test_caches.py ---

#[tokio::test]
async fn require_raises_when_empty() {
    let e = env().await;
    let cache = ProblemCache::new(e.cf, e.store);
    assert_eq!(cache.require().unwrap_err(), BotError::CacheNotReady);
    assert_eq!(BotError::CacheNotReady.to_string(), "Codeforces data is still loading. Try again in a minute.");
}

#[tokio::test]
async fn problem_refresh_sets_tags_and_persists() {
    let e = env().await;
    let p = Problem { tags: vec!["dp".into(), "math".into()], ..problem(&[]) };
    e.http.set_problems(&[p.clone()]);
    let cache = ProblemCache::new(e.cf.clone(), e.store.clone());
    cache.refresh().await.unwrap();
    assert_eq!(*cache.require().unwrap(), vec![p.clone()]);
    assert_eq!(*cache.tags(), ["dp", "math"].iter().map(|s| s.to_string()).collect());
    let fresh = ProblemCache::new(e.cf, e.store);
    fresh.load().await.unwrap(); // restart: data comes back from the DB
    assert_eq!(*fresh.require().unwrap(), vec![p]);
}

#[tokio::test]
async fn contest_refresh_and_load() {
    let e = env().await;
    let c = contest("BEFORE");
    e.http.set_contests(&[c.clone()]);
    let cache = ContestCache::new(e.cf.clone(), e.store.clone());
    cache.refresh().await.unwrap();
    assert_eq!(*cache.contests(), vec![c.clone()]);
    let fresh = ContestCache::new(e.cf, e.store);
    fresh.load().await.unwrap();
    assert_eq!(*fresh.contests(), vec![c]);
}

#[tokio::test]
async fn problem_refresh_empty_keeps_old_data() {
    let e = env().await;
    let p = problem(&["dp"]);
    e.http.set_problems(&[p.clone()]);
    let cache = ProblemCache::new(e.cf.clone(), e.store.clone());
    cache.refresh().await.unwrap();
    e.http.set_problems(&[]);
    let err = cache.refresh().await.unwrap_err();
    assert_eq!(err.to_string(), "Codeforces returned no problems");
    assert_eq!(*cache.require().unwrap(), vec![p.clone()]);
    assert_eq!(e.store.load_problems().await.unwrap(), vec![p]);
}

#[tokio::test]
async fn contest_refresh_empty_keeps_old_data() {
    let e = env().await;
    let c = contest("BEFORE");
    e.http.set_contests(&[c.clone()]);
    let cache = ContestCache::new(e.cf.clone(), e.store.clone());
    cache.refresh().await.unwrap();
    e.http.set_contests(&[]);
    let err = cache.refresh().await.unwrap_err();
    assert_eq!(err.to_string(), "Codeforces returned no contests");
    assert_eq!(*cache.contests(), vec![c.clone()]);
    assert_eq!(e.store.load_contests().await.unwrap(), vec![c]);
}

// --- intervals ---

#[test]
fn contest_delay_is_5_min_while_running_else_30() {
    assert_eq!(refresh_delay(&[]), secs(30 * MIN));
    for phase in ["BEFORE", "FINISHED"] {
        assert_eq!(refresh_delay(&[contest(phase)]), secs(30 * MIN), "{phase}");
    }
    for phase in ["CODING", "PENDING_SYSTEM_TEST", "SYSTEM_TEST"] {
        assert_eq!(refresh_delay(&[contest("BEFORE"), contest(phase)]), secs(5 * MIN), "{phase}");
    }
}

#[tokio::test(start_paused = true)]
async fn problem_tick_delay_6h_ok_5min_on_failure_or_empty() {
    let e = env().await;
    let cache = ProblemCache::new(e.cf.clone(), e.store.clone());
    e.http.set_problems(&[problem(&["dp"])]);
    assert_eq!(cache.tick().await, secs(6 * HOUR));
    e.http.set_problems(&[]);
    assert_eq!(cache.tick().await, secs(5 * MIN));
    e.http.set_problems(&[problem(&["dp"])]);
    e.http.set_down(true);
    assert_eq!(cache.tick().await, secs(5 * MIN));
    assert_eq!(cache.require().unwrap().len(), 1); // old data kept
}

#[tokio::test(start_paused = true)]
async fn contest_tick_delay_follows_phase_and_failure() {
    let e = env().await;
    let cache = ContestCache::new(e.cf.clone(), e.store.clone());
    e.http.set_contests(&[contest("BEFORE")]);
    assert_eq!(cache.tick().await, secs(30 * MIN));
    e.http.set_contests(&[contest("CODING")]);
    assert_eq!(cache.tick().await, secs(5 * MIN));
    e.http.set_contests(&[contest("FINISHED")]);
    assert_eq!(cache.tick().await, secs(30 * MIN));
    e.http.set_contests(&[]);
    assert_eq!(cache.tick().await, secs(5 * MIN));
    e.http.set_contests(&[contest("FINISHED")]);
    e.http.set_down(true);
    assert_eq!(cache.tick().await, secs(5 * MIN));
}

// --- the loops on a fake clock ---

/// Move the paused clock to `t0 + s` seconds, then give the tick that may have started (store thread,
/// real time) time to finish. Without the pause the idle runtime would auto-advance past it.
async fn at(t0: Instant, s: u64) {
    tokio::time::sleep_until(t0 + secs(s)).await;
    std::thread::sleep(Duration::from_millis(30));
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(start_paused = true)]
async fn problem_loop_codeforces_down_at_startup_keeps_db_data_and_retries_in_5_min() {
    let e = env().await;
    let old = problem(&["old"]);
    e.store.replace_problems(&[old.clone()]).await.unwrap();
    e.http.set_down(true);
    let cache = Arc::new(ProblemCache::new(e.cf.clone(), e.store.clone()));
    let t0 = Instant::now();
    cache.start().await.unwrap(); // no abort
    at(t0, 1).await;
    assert_eq!(e.http.call_secs(t0), [0]); // first tick runs immediately
    assert_eq!(*cache.require().unwrap(), vec![old]);
    e.http.set_down(false);
    let new = problem(&["new"]);
    e.http.set_problems(&[new.clone()]);
    at(t0, 5 * MIN + 1).await;
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN]);
    assert_eq!(*cache.require().unwrap(), vec![new]);
    at(t0, 5 * MIN + 6 * HOUR - 1).await; // no tick yet
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN]);
    at(t0, 5 * MIN + 6 * HOUR + 1).await;
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN, 5 * MIN + 6 * HOUR]);
    cache.stop();
    at(t0, 20 * HOUR).await;
    assert_eq!(e.http.call_secs(t0).len(), 3); // stopped
}

#[tokio::test(start_paused = true)]
async fn contest_loop_5_min_while_running_then_30_min_and_5_min_after_failure() {
    let e = env().await;
    e.http.set_contests(&[contest("CODING")]);
    let cache = Arc::new(ContestCache::new(e.cf.clone(), e.store.clone()));
    let t0 = Instant::now();
    cache.start().await.unwrap();
    at(t0, 1).await; // one tick per step: a step over several would outrun the store thread
    at(t0, 5 * MIN + 1).await;
    at(t0, 10 * MIN + 1).await;
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN, 10 * MIN]);
    e.http.set_contests(&[contest("FINISHED")]);
    at(t0, 15 * MIN - 1).await;
    assert_eq!(e.http.call_secs(t0).len(), 3);
    at(t0, 15 * MIN + 1).await; // the 15 min tick sees FINISHED: next in 30 min
    at(t0, 45 * MIN - 1).await;
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN, 10 * MIN, 15 * MIN]);
    e.http.set_down(true);
    at(t0, 45 * MIN + 1).await; // the 45 min tick fails: retry in 5 min
    assert_eq!(e.http.call_secs(t0), [0, 5 * MIN, 10 * MIN, 15 * MIN, 45 * MIN]);
    at(t0, 50 * MIN + 1).await;
    assert_eq!(e.http.call_secs(t0).last().copied(), Some(50 * MIN));
    cache.stop();
}
