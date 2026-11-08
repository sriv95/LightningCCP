//! Port of the service-level tests of `tests/test_mashup.py` and `tests/test_bingo.py` (lobby, start, check,
//! give up, rematch, history, wins, claims to log, bingo reads) over a fake Codeforces client and a real
//! store on a temp file. Tests that need `tick`, embeds or the cog belong to s17 and later. Expected values
//! come from the Python tests; random picks are checked for validity, not identity.
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use lightningccp::cf::models::{Problem, Submission};
use lightningccp::errors::BotError;
use lightningccp::logic::base::{MASHUP_MAX_PLAYERS, MASHUP_MAX_TEAMS};
use lightningccp::matches::{CfApi, Clock, CreateMatch, MatchService, ProblemSource};
use lightningccp::mashup::service::{CreateMashup, LogEntry, MashupService, MashupView};
use lightningccp::store::base::Store;
use rand::SeedableRng;
use rand::rngs::StdRng;

const GUILD: i64 = 1;

type Hook = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

fn problem(contest: i64, index: &str, name: &str, rating: Option<i64>) -> Problem {
    Problem { contest_id: contest, index: index.into(), name: name.into(), rating, tags: vec![] }
}

fn three() -> Vec<Problem> {
    [800, 1000, 1200].iter().enumerate().map(|(i, r)| problem(i as i64 + 1, "A", &format!("Name{}", i + 1), Some(*r))).collect()
}

/// Five problems at each rating 800-1700.
fn fifty() -> Vec<Problem> {
    (1..=50).map(|i| problem(i, "A", &format!("Name{i}"), Some(800 + 100 * (i % 10)))).collect()
}

#[derive(Default)]
struct FakeCf {
    subs: Mutex<HashMap<String, Vec<Submission>>>,
    before_status: Mutex<Option<Hook>>,
    since: Mutex<Option<i64>>,
}

impl CfApi for FakeCf {
    fn user_status(&self, handle: &str, _count: Option<i64>, since: Option<i64>) -> impl Future<Output = Result<Vec<Submission>>> + Send {
        let handle = handle.to_string();
        async move {
            tokio::task::yield_now().await;
            *self.since.lock().unwrap() = since;
            let hook = self.before_status.lock().unwrap().clone();
            if let Some(h) = hook {
                h(handle.clone()).await;
            }
            Ok(self.subs.lock().unwrap().get(&handle).cloned().unwrap_or_default())
        }
    }
}

struct FakeProblems(Mutex<Arc<Vec<Problem>>>);

impl ProblemSource for FakeProblems {
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
        let dir = std::env::temp_dir().join(format!("lccp-mashup-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Svc = MashupService<FakeCf, FakeProblems>;

struct Env {
    svc: Arc<Svc>,
    matches: MatchService<FakeCf, FakeProblems>,
    store: Arc<Store>,
    cf: Arc<FakeCf>,
    problems: Arc<FakeProblems>,
    t: Arc<AtomicI64>,
    _dir: TempDir,
}

async fn env_with(problems: Vec<Problem>, seed: u64) -> Env {
    let dir = TempDir::new();
    let store = Arc::new(Store::connect(dir.0.join("t.db")).await.unwrap());
    let cf = Arc::new(FakeCf::default());
    let problems = Arc::new(FakeProblems(Mutex::new(Arc::new(problems))));
    let t = Arc::new(AtomicI64::new(1000));
    let t2 = t.clone();
    let clock: Clock = Arc::new(move || t2.load(SeqCst) as f64);
    let svc = Arc::new(MashupService::with(store.clone(), cf.clone(), problems.clone(), clock.clone(), StdRng::seed_from_u64(seed)));
    let matches = MatchService::with(store.clone(), cf.clone(), problems.clone(), clock, StdRng::seed_from_u64(seed));
    for uid in 1..=4 {
        store.link_handle(GUILD, uid, &format!("h{uid}"), None).await.unwrap();
    }
    Env { svc, matches, store, cf, problems, t, _dir: dir }
}

async fn env() -> Env {
    env_with(three(), 1).await
}

async fn bingo_env() -> Env {
    env_with(fifty(), 1).await
}

fn cm(host: i64) -> CreateMashup {
    CreateMashup {
        guild_id: GUILD,
        channel_id: 5,
        host_id: host,
        min_rating: 800,
        max_rating: 1200,
        count: 3,
        duration_seconds: 7200,
        ..Default::default()
    }
}

fn team_cm(host: i64) -> CreateMashup {
    CreateMashup { teams: 2, team_size: 5, ..cm(host) }
}

fn bingo_cm(mode: &str, win: &str, size: i64) -> CreateMashup {
    CreateMashup { min_rating: 800, max_rating: 1700, count: 0, mode: mode.into(), size, win: win.into(), ..cm(1) }
}

fn berr<T>(r: Result<T>) -> BotError {
    match r {
        Err(e) => e.downcast_ref::<BotError>().expect("a BotError").clone(),
        Ok(_) => panic!("expected an error"),
    }
}

fn plain_with(e: &BotError, text: &str) -> bool {
    matches!(e, BotError::Plain(m) if m.contains(text))
}

fn closed(e: &BotError) -> bool {
    matches!(e, BotError::MatchClosed(_))
}

fn users(v: &MashupView) -> Vec<i64> {
    v.players.iter().map(|p| p.user_id).collect()
}

fn teams(v: &MashupView) -> Vec<(i64, Option<i64>)> {
    v.players.iter().map(|p| (p.user_id, p.team)).collect()
}

fn keys(v: &MashupView) -> Vec<String> {
    v.problems.iter().map(|(k, _)| k.clone()).collect()
}

impl Env {
    fn set_time(&self, t: i64) {
        self.t.store(t, SeqCst);
    }
    fn sub(&self, handle: &str, key: &str, at: i64) {
        self.sub_v(handle, key, at, "OK");
    }
    fn sub_v(&self, handle: &str, key: &str, at: i64, verdict: &str) {
        let s = Submission { id: at, created: at, problem_key: key.into(), verdict: Some(verdict.into()) };
        self.cf.subs.lock().unwrap().entry(handle.into()).or_default().push(s);
    }
    async fn lobby(&self, c: CreateMashup) -> MashupView {
        self.svc.create(c).await.unwrap()
    }
    /// A default contest hosted by `host` with the joiners in, started.
    async fn started(&self, host: i64, joiners: &[i64]) -> MashupView {
        let id = self.lobby(cm(host)).await.mashup.id;
        for &u in joiners {
            self.svc.join(id, u, None).await.unwrap();
        }
        self.svc.start(id, host).await.unwrap()
    }
    async fn bingo(&self, joiners: &[i64], c: CreateMashup) -> MashupView {
        let id = self.lobby(c).await.mashup.id;
        for &u in joiners {
            self.svc.join(id, u, None).await.unwrap();
        }
        self.svc.start(id, 1).await.unwrap()
    }
    /// Runs past the end and finishes the contest (the tick is s17; the store call is the same).
    async fn finish(&self, id: i64) {
        let ends = self.store.get_mashup(id).await.unwrap().unwrap().ends_at.unwrap();
        self.set_time(self.t.load(SeqCst).max(ends));
        assert!(self.store.finish_mashup(id, &[], None).await.unwrap());
    }
}

// ---- tests/test_mashup.py ----

#[tokio::test]
async fn create_opens_a_lobby_without_problems_or_codeforces_calls() {
    let e = env().await;
    let v = e.lobby(CreateMashup { penalty_minutes: 5, ..cm(1) }).await;
    assert_eq!((v.mashup.status.as_str(), v.mashup.penalty_minutes), ("lobby", 5));
    assert!(v.problems.is_empty() && v.players.iter().map(|p| p.handle.as_str()).eq(["h1"]));
    assert!(e.cf.since.lock().unwrap().is_none() && e.cf.subs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn create_rules() {
    let e = env().await;
    assert!(matches!(berr(e.svc.create(CreateMashup { min_rating: 1500, max_rating: 800, ..cm(1) }).await), BotError::Plain(m) if m == "min_rating cannot be higher than max_rating."));
    assert!(matches!(berr(e.svc.create(CreateMashup { duration_seconds: 0, ..cm(1) }).await), BotError::Plain(m) if m == "The contest must last at least one minute."));
    assert!(matches!(berr(e.svc.create(cm(9)).await), BotError::NotLinked(_)));
}

#[tokio::test]
async fn join_and_leave_in_the_lobby() {
    let e = env().await;
    let mid = e.lobby(cm(1)).await.mashup.id;
    assert_eq!(users(&e.svc.join(mid, 2, None).await.unwrap()), [1, 2]);
    assert!(matches!(berr(e.svc.join(mid, 2, None).await), BotError::AlreadyInMatch));
    assert!(matches!(berr(e.svc.join(mid, 9, None).await), BotError::NotLinked(_)));
    assert_eq!(users(&e.svc.leave(mid, 2).await.unwrap()), [1]);
    assert!(plain_with(&berr(e.svc.leave(mid, 2).await), "not in"));
    let cancelled = e.svc.leave(mid, 1).await.unwrap(); // the host leaving cancels the contest
    assert_eq!(cancelled.mashup.status, "cancelled");
    assert!(cancelled.note.unwrap().contains("host left"));
    assert!(closed(&berr(e.svc.join(mid, 3, None).await)));
    assert_eq!(e.lobby(cm(2)).await.mashup.status, "lobby"); // everyone is free again
}

#[tokio::test]
async fn lobby_stops_at_the_player_cap() {
    let e = env().await;
    let mid = e.lobby(cm(1)).await.mashup.id;
    for uid in 100..100 + MASHUP_MAX_TEAMS as i64 - 1 {
        e.store.link_handle(GUILD, uid, &format!("h{uid}"), None).await.unwrap();
        e.svc.join(mid, uid, None).await.unwrap();
    }
    assert_eq!(berr(e.svc.join(mid, 2, None).await), BotError::match_full());
}

#[tokio::test]
async fn start_rules_and_late_joiners_are_rejected() {
    let e = env().await;
    let mid = e.lobby(cm(1)).await.mashup.id;
    e.svc.join(mid, 2, None).await.unwrap();
    assert!(matches!(berr(e.svc.start(mid, 2).await), BotError::Plain(m) if m == "Only the host can start the contest."));
    let v = e.svc.start(mid, 1).await.unwrap();
    assert_eq!(v.mashup.status, "active");
    assert_eq!((v.mashup.started_at, v.mashup.ends_at), (Some(1000), Some(8200)));
    assert_eq!(keys(&v), ["1A", "2A", "3A"]);
    assert!(matches!(berr(e.svc.join(mid, 3, None).await), BotError::MatchClosed(m) if m.contains("already started")));
    assert_eq!(berr(e.svc.start(mid, 1).await), BotError::match_closed());
    assert!(closed(&berr(e.svc.leave(mid, 2).await)));
}

#[tokio::test]
async fn start_excludes_problems_anyone_in_the_lobby_solved() {
    let e = env().await;
    e.sub("h2", "1A", 5); // a joiner (not the host) solved the easiest one
    let mid = e.lobby(CreateMashup { count: 2, ..cm(1) }).await.mashup.id;
    e.svc.join(mid, 2, None).await.unwrap();
    assert_eq!(keys(&e.svc.start(mid, 1).await.unwrap()), ["2A", "3A"]);
    let low = e.lobby(cm(3)).await.mashup.id;
    e.svc.join(low, 4, None).await.unwrap();
    e.sub("h4", "2A", 5);
    let msg = match berr(e.svc.start(low, 3).await) {
        BotError::Plain(m) => m,
        other => panic!("{other:?}"),
    };
    // Python's f-string leaves two spaces when there are no tags
    assert_eq!(msg, "Not enough problems rated 800-1200  that nobody here has solved, for 3 problems.");
    assert_eq!(e.store.get_mashup(low).await.unwrap().unwrap().status, "lobby"); // stays open to retry
}

#[tokio::test]
async fn start_fails_when_the_players_changed_underneath() {
    let e = env().await;
    let mid = e.lobby(cm(1)).await.mashup.id;
    let done = Arc::new(AtomicBool::new(false));
    let svc = e.svc.clone();
    let hook: Hook = Arc::new(move |handle| {
        let (svc, done) = (svc.clone(), done.clone());
        Box::pin(async move {
            if handle == "h1" && !done.swap(true, SeqCst) {
                svc.join(mid, 2, None).await.unwrap();
            }
        })
    });
    *e.cf.before_status.lock().unwrap() = Some(hook);
    assert!(matches!(berr(e.svc.start(mid, 1).await), BotError::Plain(m) if m == "The players changed while starting. Press Start again."));
    assert_eq!(users(&e.svc.view(mid, None).await.unwrap()), [1, 2]);
    assert_eq!(e.svc.start(mid, 1).await.unwrap().mashup.status, "active");
}

#[tokio::test]
async fn check_scores_only_solves_inside_the_window_with_wrong_attempts() {
    let e = env().await;
    let mid = e.lobby(CreateMashup { penalty_minutes: 20, ..cm(1) }).await.mashup.id;
    e.svc.start(mid, 1).await.unwrap();
    e.sub("h1", "1A", 900); // before the start
    e.sub_v("h1", "2A", 1200, "WRONG_ANSWER");
    e.sub("h1", "2A", 1300);
    e.sub("h1", "3A", 9000); // after the end
    e.sub("h1", "9A", 1300); // not a contest problem
    let v = e.svc.check(mid, 1).await.unwrap();
    assert_eq!(v.solves, vec![(1, vec![("2A".to_string(), (1300, 1))])]);
    let ranks = v.standings().unwrap();
    assert_eq!(ranks.iter().map(|r| (r.solved, r.total_seconds)).collect::<Vec<_>>(), [(1, 300 + 20 * 60)]);
    assert!(plain_with(&berr(e.svc.check(mid, 4).await), "not in"));
}

#[tokio::test]
async fn check_after_the_end_is_rejected() {
    let e = env().await;
    let mid = e.started(1, &[]).await.mashup.id;
    e.set_time(8200);
    assert!(matches!(berr(e.svc.check(mid, 1).await), BotError::MatchClosed(m) if m == "This contest is not running."));
}

#[tokio::test]
async fn one_contest_or_match_at_a_time() {
    let e = env().await;
    let duel = |host| CreateMatch { guild_id: GUILD, channel_id: 5, host_id: host, mode: "duel".into(), max_players: 2, ..Default::default() };
    let mid = e.lobby(cm(1)).await.mashup.id; // h1 in a lobby
    assert!(matches!(berr(e.svc.create(cm(1)).await), BotError::AlreadyInMatch)); // another contest
    assert!(matches!(berr(e.matches.create(duel(1)).await), BotError::AlreadyInMatch));
    let m = e.matches.create(duel(2)).await.unwrap();
    assert!(matches!(berr(e.svc.join(mid, 2, None).await), BotError::AlreadyInMatch)); // busy in a match
    e.matches.join(m.r#match.id, 3).await.unwrap();
    let other = e.lobby(cm(4)).await.mashup.id;
    assert!(matches!(berr(e.svc.join(other, 3, None).await), BotError::AlreadyInMatch)); // busy in the match lobby
    assert_eq!(e.svc.start(mid, 1).await.unwrap().mashup.status, "active");
    let solo = CreateMatch { mode: "solo".into(), max_players: 1, ..duel(1) };
    assert!(matches!(berr(e.matches.create(solo).await), BotError::AlreadyInMatch)); // busy while the contest runs
    e.svc.leave(other, 4).await.unwrap();
}

#[tokio::test]
async fn match_join_blocked_by_a_contest() {
    let e = env().await;
    e.lobby(cm(3)).await;
    let m = e.matches.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, mode: "duel".into(), max_players: 2, ..Default::default() }).await.unwrap();
    assert!(matches!(berr(e.matches.join(m.r#match.id, 3).await), BotError::AlreadyInMatch));
}

#[tokio::test]
async fn team_contest_join_needs_a_valid_team() {
    let e = env().await;
    let v = e.lobby(CreateMashup { teams: 3, ..team_cm(1) }).await;
    assert_eq!(teams(&v), [(1, Some(1))]); // the host is in team 1
    assert_eq!(v.mashup.teams, 3);
    let mid = v.mashup.id;
    for bad in [None, Some(0), Some(4)] {
        assert!(matches!(berr(e.svc.join(mid, 2, bad).await), BotError::Plain(m) if m == "Pick a team number from 1 to 3."));
    }
    assert_eq!(teams(&e.svc.join(mid, 2, Some(3)).await.unwrap()), [(1, Some(1)), (2, Some(3))]);
    let solo = e.lobby(cm(4)).await; // teams of one ignore the number and take the first free team
    assert_eq!(e.svc.join(solo.mashup.id, 3, Some(7)).await.unwrap().players[1].team, Some(2));
}

#[tokio::test]
async fn team_standings_count_a_problem_once_from_the_first_solver() {
    let e = env().await;
    let mid = e.lobby(CreateMashup { penalty_minutes: 10, ..team_cm(1) }).await.mashup.id;
    e.svc.join(mid, 2, Some(1)).await.unwrap();
    e.svc.join(mid, 3, Some(2)).await.unwrap();
    let k = keys(&e.svc.start(mid, 1).await.unwrap());
    e.sub("h1", &k[0], 1100); // team 1: A at 100 s by h1, also A at 150 s by h2
    e.sub("h2", &k[0], 1150);
    e.sub("h2", &k[1], 1200);
    e.sub("h3", &k[0], 1050); // team 2: A at 50 s
    e.sub_v("h3", &k[1], 1020, "WRONG_ANSWER");
    e.sub("h3", &k[1], 1300);
    for uid in [1, 2, 3] {
        e.svc.check(mid, uid).await.unwrap();
    }
    let ranks = e.svc.view(mid, None).await.unwrap().standings().unwrap();
    let got: Vec<_> = ranks.iter().map(|r| (r.rank, r.user_id, r.handle.as_str(), r.solved)).collect();
    assert_eq!(got, [(1, 1, "Team 1", 2), (2, 2, "Team 2", 2)]);
    assert_eq!(ranks[0].total_seconds, 100 + 200); // not 150: the earliest solve of A counts
    assert_eq!(ranks[1].total_seconds, 50 + 300 + 10 * 60); // one wrong attempt before B
}

#[tokio::test]
async fn player_cap_is_chosen_by_the_host() {
    let e = env().await;
    let v = e.lobby(CreateMashup { teams: 2, ..cm(1) }).await;
    assert_eq!(v.mashup.max_players, 2);
    e.svc.join(v.mashup.id, 2, None).await.unwrap();
    assert_eq!(berr(e.svc.join(v.mashup.id, 3, None).await), BotError::match_full());
    for bad in [0, MASHUP_MAX_TEAMS as i64 + 1] {
        assert!(matches!(berr(e.svc.create(CreateMashup { teams: bad, ..cm(4) }).await), BotError::Plain(m) if m == "The number of teams must be from 1 to 10."));
    }
    for bad in [0, 6] {
        assert!(matches!(berr(e.svc.create(CreateMashup { team_size: bad, ..cm(4) }).await), BotError::Plain(m) if m == "Players per team must be from 1 to 5."));
    }
    assert_eq!(e.lobby(cm(3)).await.mashup.max_players, 10); // default: 10 teams of one
    let big = e.lobby(CreateMashup { teams: 10, team_size: 5, ..cm(4) }).await;
    assert_eq!(big.mashup.max_players, MASHUP_MAX_PLAYERS as i64); // the 50 player limit still holds
}

#[tokio::test]
async fn history_lists_finished_contests_with_the_users_place() {
    let e = env().await;
    let v = e.started(1, &[2, 3]).await;
    let k = keys(&v);
    e.sub("h1", &k[0], 1100);
    e.sub("h2", &k[0], 1100);
    e.sub("h2", &k[1], 1200);
    for uid in [1, 2] {
        e.svc.check(v.mashup.id, uid).await.unwrap();
    }
    e.finish(v.mashup.id).await;
    let rec: Vec<_> = e.svc.history(GUILD, 1, 50).await.unwrap().iter().map(|r| (r.rank, r.entrants, r.solved, r.team)).collect();
    assert_eq!(rec, [(2, 3, 1, Some(1))]);
    assert_eq!(e.svc.history(GUILD, 3, 50).await.unwrap().iter().map(|r| r.rank).collect::<Vec<_>>(), [3]); // nothing solved ranks last
    assert!(e.svc.history(GUILD, 4, 50).await.unwrap().is_empty());
}

#[tokio::test]
async fn history_of_a_team_contest_shows_the_team_place() {
    let e = env().await;
    let mid = e.lobby(team_cm(1)).await.mashup.id;
    e.svc.join(mid, 2, Some(2)).await.unwrap();
    e.svc.join(mid, 3, Some(2)).await.unwrap();
    let v = e.svc.start(mid, 1).await.unwrap();
    e.sub("h3", &keys(&v)[0], 1100);
    e.svc.check(mid, 3).await.unwrap();
    e.finish(mid).await;
    let rec = e.svc.history(GUILD, 2, 50).await.unwrap();
    assert_eq!(rec.len(), 1);
    assert_eq!((rec[0].rank, rec[0].entrants, rec[0].solved, rec[0].team), (1, 2, 1, Some(2)));
}

#[tokio::test]
async fn team_size_caps_each_team() {
    let e = env().await;
    let mid = e.lobby(CreateMashup { team_size: 2, ..team_cm(1) }).await.mashup.id;
    e.svc.join(mid, 2, Some(1)).await.unwrap(); // team 1 now has the host and player 2
    assert!(matches!(berr(e.svc.join(mid, 3, Some(1)).await), BotError::MatchFull(m) if m == "That team is full."));
    let t: Vec<_> = e.svc.join(mid, 3, Some(2)).await.unwrap().players.iter().map(|p| p.team).collect();
    assert_eq!(t, [Some(1), Some(1), Some(2)]);
    e.svc.create(CreateMashup { teams: 0, team_size: 2, ..team_cm(4) }).await.unwrap_err(); // needs a team contest
    e.svc.create(CreateMashup { team_size: MASHUP_MAX_PLAYERS as i64 + 1, ..team_cm(4) }).await.unwrap_err();
}

#[tokio::test]
async fn join_again_changes_team() {
    let e = env().await;
    let mid = e.lobby(CreateMashup { team_size: 2, ..team_cm(1) }).await.mashup.id;
    e.svc.join(mid, 2, Some(1)).await.unwrap();
    e.svc.join(mid, 3, Some(2)).await.unwrap();
    assert!(matches!(berr(e.svc.join(mid, 3, Some(1)).await), BotError::MatchFull(_))); // team 1 is full; 3 stays in team 2
    let v = e.svc.join(mid, 2, Some(2)).await.unwrap();
    let by: HashMap<i64, Option<i64>> = teams(&v).into_iter().collect();
    assert_eq!(by, HashMap::from([(1, Some(1)), (2, Some(2)), (3, Some(2))]));
    assert_eq!(e.svc.join(mid, 2, Some(2)).await.unwrap().players[1].team, Some(2)); // same team again changes nothing
    assert!(matches!(berr(e.svc.join(mid, 1, Some(2)).await), BotError::Plain(m) if m == "The host stays in team 1."));
    e.svc.join(mid, 2, Some(9)).await.unwrap_err(); // an unknown team
}

#[tokio::test]
async fn tags_filter_the_problems() {
    let e = env().await;
    let v = e.lobby(CreateMashup { tags: Some("DP, math".into()), exclude_tags: Some("graphs".into()), ..cm(1) }).await;
    assert_eq!((v.mashup.tags.as_str(), v.mashup.exclude_tags.as_str()), ("dp,math", "graphs"));
    // no test problem has those tags
    assert!(matches!(berr(e.svc.start(v.mashup.id, 1).await), BotError::Plain(m) if m.contains("  with those tags that nobody")));
    e.svc.leave(v.mashup.id, 1).await.unwrap();
    let plain = e.lobby(CreateMashup { exclude_tags: Some("dp".into()), ..cm(1) }).await;
    assert_eq!(e.svc.start(plain.mashup.id, 1).await.unwrap().mashup.status, "active");
}

#[tokio::test]
async fn rematch_copies_the_settings_for_a_player() {
    let e = env().await;
    let id = e.lobby(CreateMashup { tags: Some(String::new()), exclude_tags: Some("graphs".into()), penalty_minutes: 5, rated: false, ..cm(1) }).await.mashup.id;
    e.svc.join(id, 2, None).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    assert!(matches!(berr(e.svc.rematch(id, 2, 77).await), BotError::MatchClosed(m) if m == "Only a finished contest can be rematched.")); // still running
    e.finish(id).await;
    assert!(plain_with(&berr(e.svc.rematch(id, 9, 77).await), "players"));
    let new = e.svc.rematch(id, 2, 77).await.unwrap();
    let old = e.svc.view(id, None).await.unwrap().mashup;
    let m = &new.mashup;
    assert_eq!((m.status.as_str(), m.host_id, m.channel_id), ("lobby", 2, 77));
    assert_eq!(users(&new), [2]);
    assert_eq!((m.min_rating, m.max_rating, m.count, m.duration_seconds), (old.min_rating, old.max_rating, old.count, old.duration_seconds));
    assert_eq!((m.penalty_minutes, m.exclude_tags.as_str(), m.rated), (5, "graphs", 0));
}

fn with_contest_500(e: &Env, extra: Vec<Problem>) {
    let mut all = three();
    all.extend(extra);
    *e.problems.0.lock().unwrap() = Arc::new(all);
}

#[tokio::test]
async fn virtual_contest_replays_a_contests_rated_problems() {
    let e = env().await;
    with_contest_500(&e, vec![problem(500, "B", "Second", Some(1400)), problem(500, "A", "First", Some(900)), problem(500, "C", "Unrated", None)]);
    e.sub("h1", "500A", 100); // solved long ago: still part of the contest
    let v = e.lobby(CreateMashup { contest_id: Some(500), tags: Some("dp".into()), count: 9, min_rating: 2000, ..cm(1) }).await;
    let m = &v.mashup;
    assert_eq!((m.contest_id, m.count, m.min_rating, m.max_rating, m.tags.as_str()), (Some(500), 2, 900, 1400, ""));
    let started = e.svc.start(m.id, 1).await.unwrap();
    assert_eq!(started.problems, [("500A".to_string(), 900), ("500B".to_string(), 1400)]);
    let none = berr(e.svc.create(CreateMashup { contest_id: Some(12345), ..cm(2) }).await);
    assert!(plain_with(&none, "no rated problems"));
    assert_eq!(none, BotError::Plain("I know no rated problems of contest 12345 yet.".into()));
}

#[tokio::test]
async fn team_virtual_contest_and_rematch() {
    let e = env().await;
    with_contest_500(&e, vec![problem(500, "A", "First", Some(900)), problem(500, "B", "Two", Some(1400))]);
    let v = e.lobby(CreateMashup { contest_id: Some(500), teams: 2, team_size: 2, ..cm(1) }).await;
    e.svc.join(v.mashup.id, 2, Some(2)).await.unwrap();
    let s = e.svc.start(v.mashup.id, 1).await.unwrap();
    assert_eq!(keys(&s), ["500A", "500B"]);
    assert_eq!(s.mashup.teams, 2);
    e.sub("h2", "500A", 1100);
    e.svc.check(s.mashup.id, 2).await.unwrap();
    e.finish(s.mashup.id).await; // the deltas [-49, 49] come with the tick (s17)
    let again = e.svc.rematch(s.mashup.id, 2, 5).await.unwrap();
    assert_eq!((again.mashup.contest_id, again.mashup.teams, again.mashup.team_size), (Some(500), 2, 2));
    assert_eq!(teams(&again), [(2, Some(1))]);
}

#[tokio::test]
async fn give_up_keeps_the_place_but_frees_the_player() {
    let e = env().await;
    let v = e.started(1, &[2, 3]).await;
    let mid = v.mashup.id;
    e.sub("h2", &keys(&v)[0], 1100);
    e.svc.check(mid, 2).await.unwrap();
    assert!(matches!(berr(e.svc.give_up(mid, 4).await), BotError::Plain(m) if m == "You are not playing in this contest."));
    let gone = e.svc.give_up(mid, 2).await.unwrap();
    assert_eq!(gone.players.iter().map(|p| p.active).collect::<Vec<_>>(), [true, false, true]);
    let ranks = gone.standings().unwrap();
    assert_eq!((ranks[0].handle.as_str(), ranks[0].solved), ("h2", 1));
    assert!(plain_with(&berr(e.svc.give_up(mid, 2).await), "not playing"));
    assert!(matches!(berr(e.svc.check(mid, 2).await), BotError::Plain(m) if m == "You gave up this contest."));
    assert!(closed(&berr(e.svc.leave(mid, 3).await))); // an active contest is left with Give Up
    assert_eq!(e.lobby(cm(2)).await.mashup.status, "lobby"); // free for another contest
}

#[tokio::test]
async fn wins_count_finished_contests_won_against_someone() {
    let e = env().await;
    let v = e.started(1, &[2]).await;
    e.sub("h1", &keys(&v)[0], v.mashup.started_at.unwrap() + 60);
    e.svc.check(v.mashup.id, 1).await.unwrap();
    e.finish(v.mashup.id).await;
    assert_eq!((e.svc.wins(GUILD, 1, 200).await.unwrap(), e.svc.wins(GUILD, 2, 200).await.unwrap()), (1, 0));
    let lone = e.started(3, &[]).await;
    e.sub("h3", &keys(&lone)[0], lone.mashup.started_at.unwrap() + 60);
    e.svc.check(lone.mashup.id, 3).await.unwrap();
    e.finish(lone.mashup.id).await;
    assert_eq!(e.svc.wins(GUILD, 3, 200).await.unwrap(), 0); // nobody to beat
    let tie = e.started(4, &[2]).await;
    e.finish(tie.mashup.id).await;
    assert_eq!(e.svc.wins(GUILD, 4, 200).await.unwrap(), 0); // nothing solved
}

#[tokio::test]
async fn reads_reach_back_to_the_start_of_the_contest() {
    let e = env().await;
    let v = e.started(1, &[2]).await;
    e.svc.check(v.mashup.id, 1).await.unwrap();
    assert_eq!(*e.cf.since.lock().unwrap(), v.mashup.started_at);
}

#[tokio::test]
async fn contest_log_has_every_teams_solve_once_with_the_solver() {
    let e = env().await;
    let v = e.started(1, &[2, 3]).await;
    let (mid, k) = (v.mashup.id, keys(&v));
    e.sub("h1", &k[0], 1100);
    e.sub_v("h2", &k[0], 1200, "WRONG_ANSWER");
    e.sub("h2", &k[0], 1300);
    e.sub("h2", &k[1], 1400);
    e.set_time(1500);
    let mut view = v;
    for uid in [1, 2, 3] {
        view = e.svc.check(mid, uid).await.unwrap();
    }
    assert!(e.svc.claims_to_log(&view).await.unwrap().is_empty()); // no thread yet: nothing is marked
    e.svc.set_thread(mid, 5).await.unwrap();
    let view = e.svc.view(mid, None).await.unwrap();
    let entries = e.svc.claims_to_log(&view).await.unwrap();
    let got: Vec<_> = entries.iter().map(|x| (x.at, x.team, x.key.clone(), x.user, x.wrong)).collect();
    // team 2 solves the same problem: logged for its own team
    assert_eq!(got, [(1100, 1, k[0].clone(), 1, 0), (1300, 2, k[0].clone(), 2, 1), (1400, 2, k[1].clone(), 2, 0)]);
    assert!(e.svc.claims_to_log(&e.svc.view(mid, None).await.unwrap()).await.unwrap().is_empty()); // once each
    assert!(!e.svc.final_due(&view).await.unwrap()); // still active
    e.finish(mid).await;
    let done = e.svc.view(mid, None).await.unwrap();
    assert!(e.svc.final_due(&done).await.unwrap());
    assert!(!e.svc.final_due(&done).await.unwrap()); // once
}

// ---- seeded picks and Python errors ----

#[tokio::test]
async fn start_picks_are_valid_for_any_seed() {
    for seed in 0..4 {
        let e = env_with(fifty(), seed).await;
        let id = e.lobby(CreateMashup { min_rating: 800, max_rating: 1700, count: 5, ..cm(1) }).await.mashup.id;
        let v = e.svc.start(id, 1).await.unwrap();
        let distinct: HashSet<_> = keys(&v).into_iter().collect();
        assert_eq!(distinct.len(), 5);
        let ratings: Vec<i64> = v.problems.iter().map(|(_, r)| *r).collect();
        assert!(ratings.windows(2).all(|w| w[0] <= w[1]) && ratings.iter().all(|r| (800..=1700).contains(r)));
    }
}

#[tokio::test]
async fn a_replace_bingo_of_a_virtual_contest_fails_to_start_like_python() {
    // Python reads `solved` (bound only on the random branch) and raises UnboundLocalError
    let e = env().await;
    with_contest_500(&e, vec![problem(500, "A", "First", Some(900)), problem(500, "B", "Two", Some(1400))]);
    let id = e.lobby(CreateMashup { contest_id: Some(500), mode: "replace".into(), size: 3, ..cm(1) }).await.mashup.id;
    let err = e.svc.start(id, 1).await.unwrap_err();
    assert!(err.downcast_ref::<BotError>().is_none());
    assert_eq!(e.store.get_mashup(id).await.unwrap().unwrap().status, "lobby");
}

// ---- tests/test_bingo.py ----

#[tokio::test]
async fn bingo_create_validates_size_and_sets_the_board_count() {
    let e = bingo_env().await;
    let bad = e.svc.create(CreateMashup { min_rating: 800, max_rating: 900, count: 0, duration_seconds: 60, mode: "classic".into(), size: 8, ..cm(1) }).await;
    assert_eq!(berr(bad), BotError::Plain("The board size must be from 3 to 7.".into()));
    let v = e.bingo(&[2], CreateMashup { ..bingo_cm("classic", "line", 4) }).await;
    assert_eq!((v.mashup.count, v.cells.len()), (16, 16));
    let mut cells: Vec<_> = v.cells.iter().map(|c| c.cell.unwrap()).collect();
    cells.sort();
    assert_eq!(cells, (0..16).collect::<Vec<_>>());
    let distinct: HashSet<_> = v.cells.iter().map(|c| c.key.clone()).collect();
    assert_eq!(distinct.len(), 16);
}

#[tokio::test]
async fn bingo_board_shuffle_is_seeded() {
    let order = |seed| async move {
        let e = env_with(fifty(), seed).await;
        e.bingo(&[], bingo_cm("classic", "line", 3)).await.cells.iter().map(|c| c.key.clone()).collect::<Vec<_>>()
    };
    assert_eq!(order(7).await, order(7).await);
}

fn square(v: &MashupView, cell: Option<i64>) -> (String, i64, Option<i64>) {
    v.board().unwrap().squares[cell.unwrap() as usize].clone()
}

#[tokio::test]
async fn replace_gives_a_claimed_square_a_problem_100_higher_to_steal() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("replace", "line", 3)).await;
    let mid = view.mashup.id;
    let first = view.cells.iter().find(|c| c.rating == 800).unwrap().clone(); // shuffled; not the top rating
    e.sub("h1", &first.key, 1010);
    e.set_time(1020);
    let after = e.svc.check(mid, 1).await.unwrap();
    let on = |v: &MashupView| v.cells.iter().filter(|c| c.cell == first.cell).cloned().collect::<Vec<_>>();
    let newest = on(&after).pop().unwrap();
    assert!(newest.idx > first.idx && newest.key != first.key);
    assert_eq!((newest.rating, newest.since), (first.rating + 100, 1010));
    assert_eq!(square(&after, first.cell), (newest.key.clone(), newest.rating, Some(1))); // still held by team 1
    // Checking again does not stack a second replacement on the same cell.
    assert_eq!(on(&e.svc.check(mid, 1).await.unwrap()).len(), 2);
    // A solve before the claim does not count for the new problem; a later one takes the square.
    e.sub("h2", &newest.key, 1005);
    e.set_time(1100);
    assert_eq!(square(&e.svc.check(mid, 2).await.unwrap(), first.cell).2, Some(1));
    e.sub("h2", &newest.key, 1060);
    let third = e.svc.check(mid, 2).await.unwrap();
    assert_eq!(square(&third, first.cell).2, Some(2));
    assert_eq!(on(&third).len(), 3);
    let ranks = third.standings().unwrap();
    assert_eq!(ranks.iter().map(|r| (r.handle.as_str(), r.solved)).collect::<Vec<_>>(), [("h2", 1), ("h1", 0)]);
}

#[tokio::test]
async fn replace_never_picks_a_problem_someone_in_the_lobby_solved() {
    let e = bingo_env().await;
    let solved: HashSet<String> = fifty().iter().filter(|p| p.rating == Some(1100)).map(|p| p.key()).collect();
    for k in &solved {
        e.sub("h1", k, 10); // h1 solved every problem rated 1100
    }
    let view = e.bingo(&[2], CreateMashup { max_rating: 1200, ..bingo_cm("replace", "line", 3) }).await;
    let mid = view.mashup.id;
    assert!(e.store.mashup_excluded(mid).await.unwrap().is_superset(&solved));
    assert!(!view.cells.iter().any(|c| c.rating == 1100));
    let top = view.cells.iter().find(|c| c.rating == 1000).unwrap().clone();
    e.sub("h2", &top.key, 1010);
    e.set_time(1020);
    let after = e.svc.check(mid, 2).await.unwrap();
    let newest = after.cells.iter().filter(|c| c.cell == top.cell).next_back().unwrap();
    assert_eq!(newest.rating, 1200); // 1100 is all solved, so the next rating up
}

#[tokio::test]
async fn replace_never_goes_above_max_rating() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], CreateMashup { max_rating: 1300, ..bingo_cm("replace", "line", 3) }).await;
    let top = view.cells.iter().find(|c| c.rating == 1300).unwrap().clone();
    e.sub("h1", &top.key, 1010);
    e.set_time(1020);
    let after = e.svc.check(view.mashup.id, 1).await.unwrap();
    let newest = after.cells.iter().filter(|c| c.cell == top.cell).next_back().unwrap();
    assert!(newest.rating == 1300 && newest.key != top.key); // the cap: the same rating again
}

#[tokio::test]
async fn solve_log_names_the_team_and_the_player() {
    let e = bingo_env().await;
    let id = e.lobby(CreateMashup { teams: 2, team_size: 2, ..bingo_cm("classic", "line", 3) }).await.mashup.id;
    e.svc.join(id, 2, Some(1)).await.unwrap();
    e.svc.join(id, 3, Some(2)).await.unwrap();
    let view = e.svc.start(id, 1).await.unwrap();
    let by_cell: HashMap<i64, _> = view.cells.iter().map(|c| (c.cell.unwrap(), c.clone())).collect();
    e.sub("h2", &by_cell[&0].key, 1010); // team 1, the second member
    e.sub("h3", &by_cell[&1].key, 1020); // team 2
    e.sub("h1", &by_cell[&1].key, 1030); // team 1, too late for the square
    e.set_time(1040);
    let mut view = view;
    for uid in [1, 2, 3] {
        view = e.svc.check(id, uid).await.unwrap();
    }
    let entry = |at, team, c: &lightningccp::store::mashup::MashupCell, user| LogEntry { at, team, key: c.key.clone(), rating: c.rating, user, wrong: 0 };
    assert_eq!(view.log().unwrap(), [entry(1010, 1, &by_cell[&0], 2), entry(1020, 2, &by_cell[&1], 3)]);
}

#[tokio::test]
async fn claims_are_logged_once_per_bingo_problem() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("replace", "line", 3)).await;
    let mid = view.mashup.id;
    assert!(e.svc.claims_to_log(&e.svc.view(mid, None).await.unwrap()).await.unwrap().is_empty()); // no thread yet
    e.svc.set_thread(mid, 777).await.unwrap();
    let mut open = view.cells.iter().filter(|c| c.rating < 1700);
    let (first, second) = (open.next().unwrap().clone(), open.next().unwrap().clone());
    e.sub("h2", &first.key, 1010);
    e.set_time(1020);
    let after = e.svc.check(mid, 2).await.unwrap();
    assert_eq!(after.mashup.thread_id, Some(777));
    let got = e.svc.claims_to_log(&after).await.unwrap();
    assert_eq!(got.iter().map(|x| (x.at, x.team, x.key.clone(), x.user)).collect::<Vec<_>>(), [(1010, 2, first.key.clone(), 2)]);
    assert!(e.svc.claims_to_log(&e.svc.view(mid, None).await.unwrap()).await.unwrap().is_empty()); // nothing twice
    e.sub("h1", &second.key, 1030);
    e.set_time(1040);
    e.svc.check(mid, 1).await.unwrap();
    let more = e.svc.claims_to_log(&e.svc.view(mid, None).await.unwrap()).await.unwrap();
    assert_eq!(more.iter().map(|x| x.key.clone()).collect::<Vec<_>>(), [second.key.clone()]);
}
