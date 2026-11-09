//! Port of `tests/test_matches.py` (44 tests) over a fake Codeforces client and a real store on a temp file,
//! plus seeded-pick tests. Expected values come from the Python tests.
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Problem, Submission};
use lightningccp::errors::BotError;
use lightningccp::logic::base::{LOBBY_TTL_SECONDS, MATCH_TTL_SECONDS};
use lightningccp::matches::{CfApi, Clock, CreateMatch, MatchService, MatchView, ProblemSource};
use lightningccp::store::base::Store;
use rand::SeedableRng;
use rand::rngs::StdRng;

const GUILD: i64 = 1;

type Hook = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn hook<F, Fut>(f: F) -> Hook
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |h| Box::pin(f(h)))
}

fn problem(contest: i64, index: &str, name: &str, rating: i64, tags: &[&str]) -> Problem {
    Problem {
        contest_id: contest,
        index: index.into(),
        name: name.into(),
        rating: Some(rating),
        tags: tags.iter().map(|t| t.to_string()).collect(),
    }
}

fn base_problem() -> Problem {
    problem(1700, "A", "Foo", 800, &[])
}

#[derive(Default)]
struct FakeCf {
    subs: Mutex<HashMap<String, Vec<Submission>>>,
    errors: Mutex<HashMap<String, BotError>>,
    before_status: Mutex<Option<Hook>>,
    since: Mutex<Option<i64>>,
}

impl CfApi for FakeCf {
    async fn user_info(&self, _: &str) -> anyhow::Result<lightningccp::cf::models::User> {
        unreachable!()
    }
    async fn users_info(&self, _: &[String]) -> anyhow::Result<Vec<lightningccp::cf::models::User>> {
        unreachable!()
    }
    fn user_status(
        &self,
        handle: &str,
        _count: Option<i64>,
        since: Option<i64>,
    ) -> impl Future<Output = anyhow::Result<Vec<Submission>>> + Send {
        let handle = handle.to_string();
        async move {
            tokio::task::yield_now().await; // so concurrent requests interleave like real network calls
            *self.since.lock().unwrap() = since;
            let hook = self.before_status.lock().unwrap().clone();
            if let Some(h) = hook {
                h(handle.clone()).await;
            }
            if let Some(e) = self.errors.lock().unwrap().get(&handle) {
                return Err(e.clone().into());
            }
            Ok(self.subs.lock().unwrap().get(&handle).cloned().unwrap_or_default())
        }
    }
}

impl FakeCf {
    fn solve(&self, handle: &str, at: i64) {
        self.solve_key(handle, at, "1700A", "OK");
    }
    fn solve_key(&self, handle: &str, at: i64, key: &str, verdict: &str) {
        let s = Submission { id: at, created: at, problem_key: key.into(), verdict: Some(verdict.into()) };
        self.subs.lock().unwrap().entry(handle.into()).or_default().push(s);
    }
}

struct FakeProblems(Mutex<Arc<Vec<Problem>>>);

impl FakeProblems {
    fn set(&self, p: Vec<Problem>) {
        *self.0.lock().unwrap() = Arc::new(p);
    }
}

impl ProblemSource for FakeProblems {
    fn tags(&self) -> Arc<std::collections::HashSet<String>> {
        Arc::default()
    }
    fn problems(&self) -> Arc<Vec<Problem>> {
        self.0.lock().unwrap().clone()
    }
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        let p = self.problems();
        if p.is_empty() { Err(BotError::CacheNotReady) } else { Ok(p) }
    }
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-svc-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Svc = MatchService<FakeCf, FakeProblems>;

struct Env {
    svc: Arc<Svc>,
    store: Arc<Store>,
    cf: Arc<FakeCf>,
    problems: Arc<FakeProblems>,
    t: Arc<AtomicI64>,
    _dir: TempDir,
}

async fn env_with(problems: Vec<Problem>, users: i64, seed: u64) -> Env {
    let dir = TempDir::new();
    let store = Arc::new(Store::connect(dir.0.join("t.db")).await.unwrap());
    let cf = Arc::new(FakeCf::default());
    let problems = Arc::new(FakeProblems(Mutex::new(Arc::new(problems))));
    let t = Arc::new(AtomicI64::new(1000));
    let t2 = t.clone();
    let clock: Clock = Arc::new(move || t2.load(SeqCst) as f64);
    let svc = Arc::new(MatchService::with(
        store.clone(),
        cf.clone(),
        problems.clone(),
        clock,
        StdRng::seed_from_u64(seed),
    ));
    for uid in 1..=users {
        store.link_handle(GUILD, uid, &format!("h{uid}"), Some(1000)).await.unwrap();
    }
    Env { svc, store, cf, problems, t, _dir: dir }
}

async fn env() -> Env {
    env_with(vec![base_problem()], 6, 1).await
}

impl Env {
    fn advance(&self, secs: i64) {
        self.t.fetch_add(secs, SeqCst);
    }
    fn now(&self) -> i64 {
        self.t.load(SeqCst)
    }
    async fn make(&self, host: i64, mode: &str, max_players: i64) -> anyhow::Result<MatchView> {
        self.svc.create(cm(host, mode, max_players)).await
    }
    async fn ok(&self, host: i64, mode: &str, max_players: i64) -> MatchView {
        self.make(host, mode, max_players).await.unwrap()
    }
    /// A lobby of `mode` hosted by 1 with players 2..=n joined.
    async fn lobby(&self, mode: &str, max_players: i64, n: i64) -> i64 {
        let id = self.ok(1, mode, max_players).await.r#match.id;
        for uid in 2..=n {
            self.svc.join(id, uid).await.unwrap();
        }
        id
    }
}

fn cm(host: i64, mode: &str, max_players: i64) -> CreateMatch {
    CreateMatch { guild_id: GUILD, channel_id: 5, host_id: host, mode: mode.into(), max_players, ..Default::default() }
}

fn deltas(v: &MatchView) -> HashMap<i64, Option<i64>> {
    v.players.iter().map(|p| (p.user_id, p.delta)).collect()
}

fn d(pairs: &[(i64, i64)]) -> HashMap<i64, Option<i64>> {
    pairs.iter().map(|&(u, x)| (u, Some(x))).collect()
}

fn ratings(pairs: &[(i64, i64)]) -> Option<HashMap<i64, i64>> {
    Some(pairs.iter().copied().collect())
}

fn err(r: anyhow::Result<MatchView>) -> BotError {
    r.unwrap_err().downcast_ref::<BotError>().expect("a BotError").clone()
}

fn closed(e: &BotError) -> bool {
    matches!(e, BotError::MatchClosed(_))
}

fn plain(e: &BotError) -> bool {
    matches!(e, BotError::Plain(_))
}

#[tokio::test]
async fn solo_solve_raises_rating() {
    let e = env().await;
    let view = e.ok(1, "solo", 1).await;
    assert_eq!(view.r#match.status, "active");
    assert_eq!((view.r#match.problem_key.as_deref(), view.r#match.rating), (Some("1700A"), Some(800)));
    assert_eq!(view.problem, Some(base_problem()));
    assert_eq!(e.svc.check(view.r#match.id, 1).await.unwrap().r#match.status, "active");
    e.cf.solve("h1", 1100);
    let done = e.svc.check(view.r#match.id, 1).await.unwrap();
    assert_eq!(done.r#match.status, "finished");
    assert_eq!(deltas(&done), d(&[(1, 20)])); // 10 for an 800 problem at rating 800, doubled: first solo today
    assert_eq!(done.ratings, ratings(&[(1, 820)]));
    assert!(done.note.unwrap().contains("Daily bonus"));
    assert!(closed(&err(e.svc.check(view.r#match.id, 1).await)));
    assert!(e.svc.current(GUILD, 1).await.unwrap().is_none());
}

#[tokio::test]
async fn only_first_solo_win_of_the_day_is_doubled() {
    let e = env().await;
    let first = e.ok(1, "solo", 1).await;
    e.cf.solve("h1", 1100);
    assert_eq!(deltas(&e.svc.check(first.r#match.id, 1).await.unwrap()), d(&[(1, 20)]));
    e.cf.subs.lock().unwrap().clear(); // PROBLEM is the only one: solve it again after the first match
    let second = e.ok(1, "solo", 1).await;
    e.cf.solve("h1", 1200);
    let again = e.svc.check(second.r#match.id, 1).await.unwrap();
    assert_eq!(deltas(&again), d(&[(1, 10)]));
    assert!(again.note.is_none());
    e.advance(86400); // the next UTC day doubles again
    e.cf.subs.lock().unwrap().clear();
    let third = e.ok(1, "solo", 1).await;
    e.cf.solve("h1", e.now() + 100);
    assert_eq!(deltas(&e.svc.check(third.r#match.id, 1).await.unwrap()), d(&[(1, 18)])); // rating 830: a 9 gain, doubled
}

#[tokio::test]
async fn failed_solo_does_not_use_up_the_bonus() {
    let e = env().await;
    let lost = e.ok(1, "solo", 1).await;
    assert_eq!(deltas(&e.svc.give_up(lost.r#match.id, 1).await.unwrap()), d(&[(1, 0)]));
    assert!(e.store.get_daily(GUILD, 1, 0).await.unwrap().is_none());
}

#[tokio::test]
async fn solo_give_up_scores_zero_and_frees_player() {
    let e = env().await;
    let view = e.ok(1, "solo", 1).await;
    let done = e.svc.give_up(view.r#match.id, 1).await.unwrap();
    assert_eq!(done.r#match.status, "finished");
    assert_eq!(deltas(&done), d(&[(1, 0)]));
    e.ok(1, "solo", 1).await; // free to play again
}

#[tokio::test]
async fn solo_without_free_problem_is_cancelled() {
    let e = env().await;
    e.cf.solve("h1", 1);
    assert!(plain(&err(e.make(1, "solo", 1).await)));
    assert!(e.store.active_match_for(GUILD, 1).await.unwrap().is_none());
}

#[tokio::test]
async fn create_requires_linked_handle() {
    let e = env().await;
    assert!(matches!(err(e.make(99, "solo", 1).await), BotError::NotLinked(_)));
}

#[tokio::test]
async fn create_while_in_match() {
    let e = env().await;
    let lobby = e.ok(1, "duel", 2).await;
    assert_eq!(err(e.make(1, "solo", 1).await), BotError::AlreadyInMatch);
    let current = e.svc.current(GUILD, 1).await.unwrap().unwrap();
    assert_eq!(current.r#match.id, lobby.r#match.id);
}

#[tokio::test]
async fn duel_cannot_invite_self() {
    let e = env().await;
    let r = e.svc.create(CreateMatch { invited_id: Some(1), ..cm(1, "duel", 2) }).await;
    assert_eq!(err(r), BotError::Plain("You cannot duel yourself.".into()));
}

#[tokio::test]
async fn join_reserved_duel() {
    let e = env().await;
    let lobby = e.svc.create(CreateMatch { invited_id: Some(2), ..cm(1, "duel", 2) }).await.unwrap();
    let id = lobby.r#match.id;
    assert_eq!(err(e.svc.join(id, 3).await), BotError::Plain("This duel is reserved for <@2>.".into()));
    let joined = e.svc.join(id, 2).await.unwrap();
    assert_eq!(joined.players.iter().map(|p| p.user_id).collect::<Vec<_>>(), [1, 2]);
}

#[tokio::test]
async fn join_full_lobby() {
    let e = env().await;
    let id = e.ok(1, "duel", 2).await.r#match.id;
    e.svc.join(id, 2).await.unwrap();
    assert!(matches!(err(e.svc.join(id, 3).await), BotError::MatchFull(_)));
}

#[tokio::test]
async fn join_while_in_other_match() {
    let e = env().await;
    e.ok(1, "duel", 2).await;
    let other = e.ok(2, "custom", 3).await;
    assert_eq!(err(e.svc.join(other.r#match.id, 1).await), BotError::AlreadyInMatch);
}

#[tokio::test]
async fn join_requires_linked_handle() {
    let e = env().await;
    let id = e.ok(1, "duel", 2).await.r#match.id;
    assert!(matches!(err(e.svc.join(id, 99).await), BotError::NotLinked(_)));
}

#[tokio::test]
async fn start_rules() {
    let e = env().await;
    let id = e.ok(1, "duel", 2).await.r#match.id;
    assert!(plain(&err(e.svc.start(id, 1).await))); // only one player
    e.svc.join(id, 2).await.unwrap();
    assert!(plain(&err(e.svc.start(id, 2).await))); // not the host
    assert_eq!(e.svc.start(id, 1).await.unwrap().r#match.status, "active");
    assert!(closed(&err(e.svc.start(id, 1).await)));
}

#[tokio::test]
async fn start_without_free_problem_keeps_lobby() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.cf.solve("h2", 5);
    assert!(plain(&err(e.svc.start(id, 1).await)));
    assert_eq!(e.store.get_match(id).await.unwrap().unwrap().status, "lobby");
}

#[tokio::test]
async fn start_on_cancelled_lobby_fails() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.leave(id, 1).await.unwrap(); // host leaves: cancelled
    assert!(closed(&err(e.svc.start(id, 1).await)));
}

#[tokio::test]
async fn leave_rules() {
    let e = env().await;
    let id = e.lobby("custom", 4, 3).await;
    let left = e.svc.leave(id, 2).await.unwrap();
    assert_eq!(left.players.iter().map(|p| p.user_id).collect::<Vec<_>>(), [1, 3]);
    e.ok(2, "solo", 1).await; // player 2 is free again
    assert!(plain(&err(e.svc.leave(id, 4).await))); // not in the match
    let cancelled = e.svc.leave(id, 1).await.unwrap(); // host leaves
    assert_eq!(cancelled.r#match.status, "cancelled");
    assert!(cancelled.note.unwrap().to_lowercase().contains("host"));
    assert!(e.svc.current(GUILD, 3).await.unwrap().is_none());
    assert!(e.svc.current(GUILD, 1).await.unwrap().is_none());
}

#[tokio::test]
async fn leave_after_start_is_rejected() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    assert!(closed(&err(e.svc.leave(id, 2).await)));
}

#[tokio::test]
async fn leave_message_depends_on_status() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    assert!(err(e.svc.leave(id, 2).await).to_string().contains("Give Up"));
    e.cf.solve("h2", 1500);
    e.svc.check(id, 2).await.unwrap(); // finished
    let msg = err(e.svc.leave(id, 2).await);
    assert!(closed(&msg));
    assert!(!msg.to_string().contains("Give Up"));
}

#[tokio::test]
async fn duel_winner_is_earliest_solve_not_first_click() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.solve("h2", 1100);
    e.cf.solve("h1", 1200);
    let done = e.svc.check(id, 1).await.unwrap(); // player 1 clicks, player 2 solved first
    assert_eq!(deltas(&done), d(&[(2, 49), (1, -49)]));
    assert_eq!(done.ratings, ratings(&[(2, 849), (1, 751)]));
}

#[tokio::test]
async fn duel_check_without_solves_keeps_running() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    assert_eq!(e.svc.check(id, 1).await.unwrap().r#match.status, "active");
}

#[tokio::test]
async fn solve_before_start_does_not_count() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap(); // started_at = 1000
    e.cf.solve("h1", 999);
    assert_eq!(e.svc.check(id, 1).await.unwrap().r#match.status, "active");
}

#[tokio::test]
async fn duel_give_up_is_a_forfeit() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    let done = e.svc.give_up(id, 1).await.unwrap();
    assert_eq!(done.r#match.status, "finished");
    assert_eq!(deltas(&done), d(&[(1, -49), (2, 49)]));
}

#[tokio::test]
async fn give_up_errors() {
    let e = env().await;
    let id = e.lobby("custom", 3, 3).await;
    assert!(closed(&err(e.svc.give_up(id, 2).await))); // lobby: use Leave
    e.svc.start(id, 1).await.unwrap();
    assert_eq!(e.svc.give_up(id, 2).await.unwrap().r#match.status, "active"); // player 3 still plays
    assert!(plain(&err(e.svc.give_up(id, 2).await))); // already gave up
    assert!(plain(&err(e.svc.check(id, 2).await))); // gave up: cannot check
    e.cf.solve("h1", 1100);
    e.svc.check(id, 1).await.unwrap();
    assert!(closed(&err(e.svc.give_up(id, 1).await))); // the first solve ended the match
}

#[tokio::test]
async fn custom_ends_at_the_first_solve_and_pays_the_duel_table_one_and_a_half_times() {
    let e = env().await;
    let id = e.lobby("custom", 4, 4).await;
    e.svc.start(id, 1).await.unwrap();
    assert_eq!(e.svc.give_up(id, 3).await.unwrap().r#match.status, "active");
    e.cf.solve("h2", 1100);
    e.cf.solve("h1", 1200); // solved too, but later: no placement among the others
    let done = e.svc.check(id, 1).await.unwrap();
    assert_eq!(done.r#match.status, "finished");
    // 3 duels at 49: 147 * 1.5 / 3 = 73.5 for the winner, -49 * 1.5 / 3 = -24.5 for each loser
    assert_eq!(deltas(&done), d(&[(2, 74), (1, -25), (3, -25), (4, -25)]));
    assert_eq!(done.ratings, ratings(&[(2, 874), (1, 775), (3, 775), (4, 775)]));
}

#[tokio::test]
async fn custom_everyone_gave_up_moves_no_ratings() {
    let e = env().await;
    let id = e.lobby("custom", 3, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.svc.give_up(id, 1).await.unwrap();
    let done = e.svc.give_up(id, 2).await.unwrap();
    assert_eq!(done.r#match.status, "finished");
    assert_eq!(deltas(&done), d(&[(1, 0), (2, 0)]));
}

#[tokio::test]
async fn expiry_without_solvers_moves_no_ratings() {
    let e = env().await;
    let id = e.lobby("custom", 3, 3).await;
    e.svc.start(id, 1).await.unwrap();
    e.advance(MATCH_TTL_SECONDS - 1);
    assert!(e.svc.expire_due().await.unwrap().is_empty());
    e.advance(1);
    let views = e.svc.expire_due().await.unwrap();
    assert_eq!(views.len(), 1);
    let view = &views[0];
    assert_eq!(view.r#match.status, "finished");
    assert_eq!(deltas(view), d(&[(1, 0), (2, 0), (3, 0)]));
    assert!(view.note.as_ref().unwrap().to_lowercase().contains("time"));
    assert!(e.svc.current(GUILD, 1).await.unwrap().is_none());
    assert!(e.svc.expire_due().await.unwrap().is_empty());
}

#[tokio::test]
async fn custom_expiry_after_a_solve_scores_it() {
    let e = env().await;
    let id = e.lobby("custom", 3, 3).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.solve("h2", 1500);
    e.advance(MATCH_TTL_SECONDS);
    let views = e.svc.expire_due().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(deltas(&views[0]), d(&[(2, 74), (1, -37), (3, -37)]));
}

#[tokio::test]
async fn lobby_expiry_cancels() {
    let e = env().await;
    e.ok(1, "duel", 2).await;
    e.advance(LOBBY_TTL_SECONDS);
    let views = e.svc.expire_due().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].r#match.status, "cancelled");
    assert!(views[0].note.as_ref().unwrap().to_lowercase().contains("expired"));
    assert!(e.svc.current(GUILD, 1).await.unwrap().is_none());
}

fn ok_or_closed<T>(r: &anyhow::Result<T>) -> bool {
    match r {
        Ok(_) => true,
        Err(e) => e.downcast_ref::<BotError>().is_some_and(closed),
    }
}

// Python also records every `store.finish_match` result ([False, True]); the Store is a concrete type, so
// this checks the outcome instead: both calls are fine, and the ratings moved once.
#[tokio::test]
async fn concurrent_checks_score_once() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.solve("h1", 1100);
    e.cf.solve("h2", 1200);
    let (a, b) = tokio::join!(e.svc.check(id, 1), e.svc.check(id, 2));
    assert!(ok_or_closed(&a) && ok_or_closed(&b));
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 849);
    assert_eq!(e.store.rating(GUILD, 2).await.unwrap(), 751);
}

#[tokio::test]
async fn expiry_racing_check_scores_once() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.solve("h1", 1100);
    e.cf.solve("h2", 1200);
    e.advance(MATCH_TTL_SECONDS);
    let (a, b) = tokio::join!(e.svc.check(id, 1), e.svc.expire_due());
    assert!(ok_or_closed(&a) && ok_or_closed(&b));
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 849);
    assert_eq!(e.store.rating(GUILD, 2).await.unwrap(), 751);
}

#[tokio::test]
async fn concurrent_starts_start_once() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    let (a, b) = tokio::join!(e.svc.start(id, 1), e.svc.start(id, 1));
    let (views, errors): (Vec<_>, Vec<_>) = [a, b].into_iter().partition(|r| r.is_ok());
    assert_eq!((views.len(), errors.len()), (1, 1));
    let view = views.into_iter().next().unwrap().unwrap();
    assert_eq!(view.r#match.status, "active");
    assert!(closed(&err(errors.into_iter().next().unwrap())));
    let m = e.store.get_match(id).await.unwrap().unwrap();
    assert_eq!(m.status, "active");
    assert_eq!(m.started_at, view.r#match.started_at);
}

#[tokio::test]
async fn leave_during_start_keeps_lobby() {
    let e = env().await;
    let id = e.lobby("custom", 3, 3).await;
    let (svc, left) = (e.svc.clone(), Arc::new(AtomicBool::new(false)));
    *e.cf.before_status.lock().unwrap() = Some(hook(move |h| {
        let (svc, left) = (svc.clone(), left.clone());
        async move {
            if h == "h1" && !left.swap(true, SeqCst) {
                svc.leave(id, 2).await.unwrap();
            }
        }
    }));
    let msg = err(e.svc.start(id, 1).await);
    assert!(plain(&msg) && msg.to_string().contains("changed"));
    assert_eq!(e.store.get_match(id).await.unwrap().unwrap().status, "lobby");
    let ids: Vec<_> = e.store.get_players(id).await.unwrap().iter().map(|p| p.user_id).collect();
    assert_eq!(ids, [1, 3]);
}

#[tokio::test]
async fn leave_during_duel_start_does_not_activate_one_player() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    let (svc, left) = (e.svc.clone(), Arc::new(AtomicBool::new(false)));
    *e.cf.before_status.lock().unwrap() = Some(hook(move |h| {
        let (svc, left) = (svc.clone(), left.clone());
        async move {
            if h == "h1" && !left.swap(true, SeqCst) {
                svc.leave(id, 2).await.unwrap();
            }
        }
    }));
    assert!(plain(&err(e.svc.start(id, 1).await)));
    assert_eq!(e.store.get_match(id).await.unwrap().unwrap().status, "lobby");
    let ids: Vec<_> = e.store.get_players(id).await.unwrap().iter().map(|p| p.user_id).collect();
    assert_eq!(ids, [1]);
}

#[tokio::test]
async fn join_during_start_keeps_lobby() {
    let e = env().await;
    let id = e.lobby("custom", 4, 2).await;
    let (svc, joined) = (e.svc.clone(), Arc::new(AtomicBool::new(false)));
    *e.cf.before_status.lock().unwrap() = Some(hook(move |h| {
        let (svc, joined) = (svc.clone(), joined.clone());
        async move {
            if h == "h1" && !joined.swap(true, SeqCst) {
                svc.join(id, 3).await.unwrap();
            }
        }
    }));
    let msg = err(e.svc.start(id, 1).await);
    assert!(plain(&msg) && msg.to_string().contains("changed"));
    assert_eq!(e.store.get_match(id).await.unwrap().unwrap().status, "lobby");
    let ids: Vec<_> = e.store.get_players(id).await.unwrap().iter().map(|p| p.user_id).collect();
    assert_eq!(ids, [1, 2, 3]);
}

#[tokio::test]
async fn expiry_survives_a_failing_handle_lookup() {
    let e = env().await;
    let id = e.lobby("custom", 3, 3).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.solve("h2", 1500);
    e.cf.errors.lock().unwrap().insert("h3".into(), BotError::handle_not_found()); // e.g. a renamed handle
    e.advance(MATCH_TTL_SECONDS);
    let views = e.svc.expire_due().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].r#match.status, "finished");
    assert_eq!(deltas(&views[0]), d(&[(2, 74), (1, -37), (3, -37)]));
    for uid in 1..=3 {
        assert!(e.svc.current(GUILD, uid).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn check_with_failing_own_lookup_still_raises() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.errors.lock().unwrap().insert("h2".into(), BotError::handle_not_found());
    assert!(matches!(err(e.svc.check(id, 2).await), BotError::HandleNotFound(_)));
    assert_eq!(e.store.get_match(id).await.unwrap().unwrap().status, "active");
}

#[tokio::test]
async fn set_message_is_reflected() {
    let e = env().await;
    let id = e.ok(1, "duel", 2).await.r#match.id;
    e.svc.set_message(id, 77, 88).await.unwrap();
    let current = e.svc.current(GUILD, 1).await.unwrap().unwrap();
    assert_eq!((current.r#match.channel_id, current.r#match.message_id), (77, Some(88)));
}

#[tokio::test]
async fn check_tolerates_another_players_failing_lookup() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.cf.errors.lock().unwrap().insert("h1".into(), BotError::handle_not_found()); // the opponent renamed their handle
    e.cf.solve("h2", 1500);
    let view = e.svc.check(id, 2).await.unwrap();
    assert_eq!(view.r#match.status, "finished");
    assert_eq!(deltas(&view), d(&[(1, -49), (2, 49)]));
}

async fn finished_duel(e: &Env, rating_arg: Option<i64>, tags: Option<&str>, exclude: Option<&str>) -> MatchView {
    let lobby = e
        .svc
        .create(CreateMatch {
            rating_arg,
            tags: tags.map(Into::into),
            exclude_tags: exclude.map(Into::into),
            ..cm(1, "duel", 2)
        })
        .await
        .unwrap();
    let id = lobby.r#match.id;
    e.svc.join(id, 2).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    e.svc.give_up(id, 1).await.unwrap() // player 2 wins by forfeit
}

#[tokio::test]
async fn rematch_opens_a_duel_lobby_hosted_by_the_presser() {
    let e = env().await;
    let done = finished_duel(&e, Some(800), None, None).await;
    let new = e.svc.rematch(done.r#match.id, 1, 77).await.unwrap(); // the loser asks
    assert_eq!((new.r#match.status.as_str(), new.r#match.mode.as_str()), ("lobby", "duel"));
    assert_eq!((new.r#match.host_id, new.r#match.invited_id, new.r#match.channel_id), (1, Some(2), 77));
    assert_eq!(new.r#match.rating_arg, Some(800));
    assert_ne!(new.r#match.id, done.r#match.id);
    assert!(plain(&err(e.svc.join(new.r#match.id, 3).await))); // reserved for the old opponent
    assert_eq!(e.svc.join(new.r#match.id, 2).await.unwrap().r#match.status, "lobby");
}

#[tokio::test]
async fn rematch_rules() {
    let e = env().await;
    let done = finished_duel(&e, None, None, None).await;
    assert!(plain(&err(e.svc.rematch(done.r#match.id, 3, 5).await))); // not a player
    e.svc.rematch(done.r#match.id, 1, 5).await.unwrap();
    assert_eq!(err(e.svc.rematch(done.r#match.id, 1, 5).await), BotError::AlreadyInMatch); // still in the new lobby
    let lobby = e.ok(3, "duel", 2).await;
    assert!(closed(&err(e.svc.rematch(lobby.r#match.id, 3, 5).await))); // not finished
    let solo = e.ok(4, "solo", 1).await;
    let solo = e.svc.give_up(solo.r#match.id, 4).await.unwrap();
    assert!(closed(&err(e.svc.rematch(solo.r#match.id, 4, 5).await))); // only duels
}

#[tokio::test]
async fn match_problem_follows_the_tag_filters() {
    let dp = problem(1, "A", "DP", 800, &["dp", "math"]);
    let greedy = problem(2, "A", "Greedy", 800, &["greedy"]);
    let e = env_with(vec![dp, greedy], 3, 1).await;
    let only_dp = e.svc.create(CreateMatch { tags: Some("DP, math".into()), ..cm(1, "solo", 1) }).await.unwrap();
    assert_eq!(only_dp.r#match.problem_key.as_deref(), Some("1A"));
    assert_eq!(only_dp.r#match.tags, "dp,math");
    let not_dp = e.svc.create(CreateMatch { exclude_tags: Some("dp".into()), ..cm(2, "solo", 1) }).await.unwrap();
    assert_eq!(not_dp.r#match.problem_key.as_deref(), Some("2A"));
    assert_eq!(not_dp.r#match.exclude_tags, "dp");
    let msg = err(e.svc.create(CreateMatch { tags: Some("graphs".into()), ..cm(3, "solo", 1) }).await);
    assert!(msg.to_string().contains("with those tags"));
    assert!(e.store.active_match_for(GUILD, 3).await.unwrap().is_none()); // the failed solo lobby is cancelled
}

#[tokio::test]
async fn rematch_keeps_the_tag_filters() {
    let e = env().await;
    e.problems.set(vec![problem(1700, "A", "Foo", 800, &["dp"])]);
    let done = finished_duel(&e, None, Some("dp"), Some("math")).await;
    let new = e.svc.rematch(done.r#match.id, 1, 5).await.unwrap();
    assert_eq!((new.r#match.tags.as_str(), new.r#match.exclude_tags.as_str()), ("dp", "math"));
}

#[tokio::test]
async fn expiry_of_a_match_finished_meanwhile_adds_no_time_is_up_note() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    e.svc.start(id, 1).await.unwrap();
    e.advance(MATCH_TTL_SECONDS);
    let stale = e.store.matches_due(e.now() - LOBBY_TTL_SECONDS, e.now()).await.unwrap();
    let done = e.svc.give_up(id, 2).await.unwrap(); // finishes it before the expiry loop gets there
    assert_eq!(done.r#match.status, "finished");
    // Python monkeypatches store.matches_due to return `stale`; expire_rows takes the list directly.
    let views = e.svc.expire_rows(stale).await;
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].r#match.status, "finished");
    assert!(views[0].note.is_none());
    assert_eq!(deltas(&views[0]), deltas(&done)); // scored once
}

#[tokio::test]
async fn check_reads_reach_back_to_the_start_of_the_match() {
    let e = env().await;
    let id = e.lobby("duel", 2, 2).await;
    let started = e.svc.start(id, 1).await.unwrap();
    e.svc.check(id, 1).await.unwrap();
    assert_eq!(*e.cf.since.lock().unwrap(), started.r#match.started_at);
}

// ---- seeded picks (PARITY: injected rng; picks are valid, not equal to Python's) ----

#[tokio::test]
async fn solo_picks_stay_in_the_pool_and_never_repeat() {
    // five problems at 800 (one tagged dp), one at 900; the player has solved 800A already
    let mut pool: Vec<Problem> = (1..=5).map(|i| problem(i, "A", "p", 800, if i == 5 { &["dp"] } else { &[] })).collect();
    pool.push(problem(9, "A", "hard", 900, &[]));
    for seed in 0..5 {
        let e = env_with(pool.clone(), 1, seed).await;
        e.cf.solve_key("h1", 1, "1A", "OK");
        let mut seen = vec!["1A".to_string()];
        for n in 0..3 {
            let v = e.svc.create(CreateMatch { rating_arg: Some(800), exclude_tags: Some("dp".into()), ..cm(1, "solo", 1) }).await;
            let v = v.unwrap();
            let key = v.r#match.problem_key.clone().unwrap();
            assert!(!seen.contains(&key), "seed {seed} pick {n}: {key} repeated");
            assert!(["2A", "3A", "4A"].contains(&key.as_str()), "{key} outside the filtered pool");
            seen.push(key.clone());
            e.cf.solve_key("h1", e.now() + 1, &key, "OK");
            e.svc.check(v.r#match.id, 1).await.unwrap(); // finishes: frees the player, marks it solved
        }
        // 2A, 3A, 4A used up: no problem is left
        let msg = err(e.svc.create(CreateMatch { rating_arg: Some(800), exclude_tags: Some("dp".into()), ..cm(1, "solo", 1) }).await);
        assert!(msg.to_string().contains("No problem rated 800 with those tags is unsolved by every player."));
    }
}

#[tokio::test]
async fn same_seed_gives_the_same_pick() {
    let pool: Vec<Problem> = (1..=20).map(|i| problem(i, "A", "p", 800, &[])).collect();
    let mut keys = Vec::new();
    for _ in 0..2 {
        let e = env_with(pool.clone(), 1, 42).await;
        keys.push(e.ok(1, "solo", 1).await.r#match.problem_key.unwrap());
    }
    assert_eq!(keys[0], keys[1]);
}
