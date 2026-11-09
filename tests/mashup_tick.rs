//! Port of the `tick` tests of `tests/test_mashup.py` and `tests/test_bingo.py` (reads, budget, interval,
//! ending, expired lobbies, match ratings, bingo end conditions) over a fake Codeforces client that records
//! its calls, and a real store on a temp file. Expected values come from the Python tests. The cog side of
//! `a_finished_bingo_posts_its_closing_message_once` (sending to the thread) is in `cogs_mashup.rs`; here the service parts
//! (claims once, the final message once) are checked.
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use lightningccp::cf::models::{Problem, Submission};
use lightningccp::errors::BotError;
use lightningccp::logic::base::MASHUP_LOBBY_TTL_SECONDS;
use lightningccp::matches::{CfApi, Clock, ProblemSource};
use lightningccp::mashup::service::{CreateMashup, MashupService, MashupView};
use lightningccp::store::base::Store;
use lightningccp::ui::embeds::b::{final_message, log_board_embed, mashup_embed};
use rand::SeedableRng;
use rand::rngs::StdRng;

const GUILD: i64 = 1;

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
    errors: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<String>>,
}

impl CfApi for FakeCf {
    async fn user_info(&self, _: &str) -> anyhow::Result<lightningccp::cf::models::User> {
        unreachable!()
    }
    async fn users_info(&self, _: &[String]) -> anyhow::Result<Vec<lightningccp::cf::models::User>> {
        unreachable!()
    }
    fn user_status(&self, handle: &str, _count: Option<i64>, _since: Option<i64>) -> impl Future<Output = Result<Vec<Submission>>> + Send {
        let handle = handle.to_string();
        async move {
            tokio::task::yield_now().await;
            self.calls.lock().unwrap().push(handle.clone());
            if let Some(e) = self.errors.lock().unwrap().get(&handle) {
                return Err(anyhow!("{e}"));
            }
            Ok(self.subs.lock().unwrap().get(&handle).cloned().unwrap_or_default())
        }
    }
}

struct FakeProblems(Mutex<Arc<Vec<Problem>>>);

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
        let dir = std::env::temp_dir().join(format!("lccp-tick-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Env {
    svc: MashupService<FakeCf, FakeProblems>,
    store: Arc<Store>,
    cf: Arc<FakeCf>,
    problems: Arc<FakeProblems>,
    t: Arc<AtomicI64>,
    _dir: TempDir,
}

async fn env_with(problems: Vec<Problem>, reads_per_second: Option<f64>) -> Env {
    let dir = TempDir::new();
    let store = Arc::new(Store::connect(dir.0.join("t.db")).await.unwrap());
    let cf = Arc::new(FakeCf::default());
    let problems = Arc::new(FakeProblems(Mutex::new(Arc::new(problems))));
    let t = Arc::new(AtomicI64::new(1000));
    let t2 = t.clone();
    let clock: Clock = Arc::new(move || t2.load(SeqCst) as f64);
    let mut svc = MashupService::with(store.clone(), cf.clone(), problems.clone(), clock, StdRng::seed_from_u64(1));
    if let Some(r) = reads_per_second {
        svc = svc.with_reads_per_second(r);
    }
    for uid in 1..=5 {
        store.link_handle(GUILD, uid, &format!("h{uid}"), None).await.unwrap();
    }
    Env { svc, store, cf, problems, t, _dir: dir }
}

async fn env() -> Env {
    env_with(three(), None).await
}

async fn bingo_env() -> Env {
    env_with(fifty(), None).await
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

fn bingo_cm(mode: &str, win: &str) -> CreateMashup {
    CreateMashup { min_rating: 800, max_rating: 1700, count: 0, mode: mode.into(), size: 3, win: win.into(), ..cm(1) }
}

impl Env {
    fn set_time(&self, t: i64) {
        self.t.store(t, SeqCst);
    }
    fn sub(&self, handle: &str, key: &str, at: i64) {
        let s = Submission { id: at, created: at, problem_key: key.into(), verdict: Some("OK".into()) };
        self.cf.subs.lock().unwrap().entry(handle.into()).or_default().push(s);
    }
    fn calls(&self) -> Vec<String> {
        self.cf.calls.lock().unwrap().clone()
    }
    fn clear_calls(&self) {
        self.cf.calls.lock().unwrap().clear();
    }
    async fn lobby(&self, c: CreateMashup) -> MashupView {
        self.svc.create(c).await.unwrap()
    }
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
    /// Python `finish_contest`: `solves` is handle -> (problem index, time); then runs past the end and ticks.
    async fn finish_contest(&self, view: &MashupView, solves: &[(&str, &[(usize, i64)])]) -> MashupView {
        for (handle, found) in solves {
            for (index, at) in *found {
                self.sub(handle, &view.problems[*index].0, *at);
            }
        }
        self.set_time(view.mashup.ends_at.unwrap());
        self.svc.tick().await.unwrap();
        self.svc.view(view.mashup.id, None).await.unwrap()
    }
    /// The bingo problem keys ordered by board cell.
    fn board_keys(view: &MashupView) -> Vec<String> {
        let mut cells = view.cells.clone();
        cells.sort_by_key(|c| c.cell);
        cells.into_iter().map(|c| c.key).collect()
    }
}

fn deltas(v: &MashupView) -> Vec<(i64, Option<i64>)> {
    v.players.iter().map(|p| (p.user_id, p.delta)).collect()
}

fn delta_of(v: &MashupView, user: i64) -> Option<i64> {
    v.players.iter().find(|p| p.user_id == user).unwrap().delta
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---- tests/test_mashup.py ----

#[tokio::test]
async fn tick_reports_changes_once_and_skips_players_who_solved_everything() {
    let e = env().await;
    let id = e.started(1, &[2]).await.mashup.id;
    e.clear_calls();
    assert!(e.svc.tick().await.unwrap().is_empty()); // nothing solved yet
    assert_eq!(sorted(e.calls()), ["h1", "h2"]);
    for key in ["1A", "2A", "3A"] {
        e.sub("h2", key, 1100);
    }
    let views = e.svc.tick().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].mashup.id, id);
    assert_eq!(views[0].solves.iter().find(|(u, _)| *u == 2).unwrap().1.len(), 3);
    e.clear_calls();
    assert!(e.svc.tick().await.unwrap().is_empty());
    assert_eq!(e.calls(), ["h1"]); // h2 solved everything, so no call for them
}

#[tokio::test]
async fn tick_ends_the_contest_frees_players_and_ignores_a_failing_handle() {
    let e = env().await;
    e.started(1, &[2]).await;
    e.cf.errors.lock().unwrap().insert("h2".into(), "renamed".into());
    e.sub("h1", "1A", 1100);
    e.set_time(8200);
    let views = e.svc.tick().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].mashup.status, "finished");
    assert_eq!(views[0].solves, vec![(1, vec![("1A".to_string(), (1100, 0))])]);
    assert!(e.svc.tick().await.unwrap().is_empty());
    assert_eq!(e.lobby(cm(2)).await.mashup.status, "lobby"); // players are free again
}

#[tokio::test]
async fn tick_cancels_an_expired_lobby() {
    let e = env().await;
    e.lobby(cm(1)).await;
    e.set_time(1000 + MASHUP_LOBBY_TTL_SECONDS - 1);
    assert!(e.svc.tick().await.unwrap().is_empty());
    e.set_time(1000 + MASHUP_LOBBY_TTL_SECONDS);
    let views = e.svc.tick().await.unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].mashup.status, "cancelled");
    assert_eq!(views[0].note.as_deref(), Some("The lobby expired."));
    assert!(e.svc.tick().await.unwrap().is_empty());
    assert_eq!(e.lobby(cm(1)).await.mashup.status, "lobby");
}

#[tokio::test]
async fn tick_reads_the_stalest_players_within_the_budget() {
    let e = env_with(three(), Some(2.0 / 60.0)).await; // 2 reads in a 60 s interval
    let view = e.started(1, &[2, 3, 4]).await;
    e.clear_calls();
    e.set_time(1060);
    e.svc.tick().await.unwrap();
    assert_eq!(e.calls(), ["h1", "h2"]); // nobody read yet: joining order
    e.set_time(1120);
    e.svc.tick().await.unwrap();
    assert_eq!(e.calls(), ["h1", "h2", "h3", "h4"]); // the two read longest ago come next
    e.set_time(1180);
    e.svc.tick().await.unwrap();
    assert_eq!(e.calls()[4..], ["h1", "h2"]);
    e.clear_calls();
    e.set_time(view.mashup.ends_at.unwrap()); // out of time: everyone is read, whatever the budget
    e.svc.tick().await.unwrap();
    assert_eq!(sorted(e.calls()), ["h1", "h2", "h3", "h4"]);
    assert_eq!(e.svc.view(view.mashup.id, None).await.unwrap().mashup.status, "finished");
}

#[tokio::test]
async fn interval_shrinks_with_fewer_players_waiting() {
    let e = env().await;
    assert_eq!(e.svc.interval.load(SeqCst), 60); // nothing running yet
    e.started(1, &[2]).await; // 2 players: 4 s of reads at 0.5 per second, so the minimum
    e.clear_calls();
    e.set_time(1060);
    e.svc.tick().await.unwrap();
    assert_eq!((e.svc.interval.load(SeqCst), e.calls()), (10, strs(&["h1", "h2"])));
    e.svc.tick().await.unwrap();
    assert_eq!(e.calls(), ["h1", "h2", "h1", "h2"]); // 10 s pay for 5 reads, so both are read again
}

#[tokio::test]
async fn contest_ends_when_everyone_still_in_solved_everything() {
    let e = env().await;
    let view = e.started(1, &[2, 3]).await;
    let (mid, ends_at) = (view.mashup.id, view.mashup.ends_at.unwrap());
    for handle in ["h1", "h2"] {
        for (key, _) in &view.problems {
            e.sub(handle, key, 1100);
        }
    }
    e.svc.give_up(mid, 3).await.unwrap();
    e.set_time(1060);
    e.svc.tick().await.unwrap();
    let done = e.svc.view(mid, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    assert!(done.mashup.ends_at == Some(1060) && 1060 < ends_at);
    assert!(delta_of(&done, 3).unwrap() < 0); // the one who left is ranked last
}

#[tokio::test]
async fn contest_ends_when_everyone_gave_up() {
    let e = env().await;
    let view = e.started(1, &[2]).await;
    e.svc.give_up(view.mashup.id, 1).await.unwrap();
    e.svc.give_up(view.mashup.id, 2).await.unwrap();
    e.set_time(1060);
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    assert_eq!(deltas(&done), [(1, Some(0)), (2, Some(0))]); // nobody solved: a tie
}

#[tokio::test]
async fn finished_mashup_moves_match_ratings() {
    let e = env().await;
    let view = e.started(1, &[2, 3]).await;
    let done = e.finish_contest(&view, &[("h1", &[(0, 1100)]), ("h2", &[(0, 1100), (1, 1200)])]).await;
    assert_eq!(deltas(&done), [(1, Some(0)), (2, Some(49)), (3, Some(-49))]);
    let mut ratings = Vec::new();
    for u in 1..=3 {
        ratings.push(e.store.rating(GUILD, u).await.unwrap());
    }
    assert_eq!(ratings, [800, 849, 751]);
    let mut played: Vec<(i64, i64)> = e.store.stats(GUILD, "all").await.unwrap().into_iter().map(|(u, _, _, n)| (u, n)).collect();
    played.sort();
    assert_eq!(played, [(1, 1), (2, 1), (3, 1)]);
    let changes: Vec<i64> = e.store.rating_changes(GUILD, 2).await.unwrap().into_iter().map(|(_, d)| d).collect();
    assert_eq!(changes, [49]);
    assert!(e.store.stats(GUILD, "matches").await.unwrap().is_empty());
    let mut contests: Vec<(i64, i64)> = e.store.stats(GUILD, "contests").await.unwrap().into_iter().map(|(u, _, r, _)| (u, r)).collect();
    contests.sort();
    assert_eq!(contests, [(1, 800), (2, 849), (3, 751)]);
    let text = mashup_embed(&done, &HashMap::new()).unwrap().description.unwrap();
    assert!(text.contains("1. [h2](https://codeforces.com/profile/h2) - 2 solved") && text.matches("rating +49").count() == 1);
    e.set_time(e.t.load(SeqCst) + 5000);
    e.svc.tick().await.unwrap(); // a second tick must not apply the change again
    assert_eq!(e.store.rating(GUILD, 2).await.unwrap(), 849);
}

#[tokio::test]
async fn team_contest_rates_every_member_by_team_average() {
    let e = env().await;
    let mid = e.lobby(CreateMashup { teams: 2, team_size: 5, ..cm(1) }).await.mashup.id;
    e.svc.join(mid, 2, Some(1)).await.unwrap();
    e.svc.join(mid, 3, Some(2)).await.unwrap();
    e.svc.join(mid, 5, Some(2)).await.unwrap();
    let view = e.svc.start(mid, 1).await.unwrap();
    let done = e.finish_contest(&view, &[("h5", &[(0, 1100)])]).await;
    let mut got = deltas(&done);
    got.sort();
    assert_eq!(got, [(1, Some(-49)), (2, Some(-49)), (3, Some(49)), (5, Some(49))]);
    assert!(mashup_embed(&done, &HashMap::new()).unwrap().description.unwrap().contains("rating +49"));
}

#[tokio::test]
async fn nothing_solved_or_one_entrant_changes_no_rating() {
    let e = env().await;
    let v = e.started(1, &[2]).await;
    let done = e.finish_contest(&v, &[]).await;
    assert_eq!(deltas(&done), [(1, Some(0)), (2, Some(0))]); // a tie moves nothing
    let v = e.started(3, &[]).await;
    let alone = e.finish_contest(&v, &[("h3", &[(0, 9999)])]).await;
    assert_eq!(deltas(&alone), [(3, None)]); // one entrant: not rated
    assert_eq!(e.store.rating(GUILD, 3).await.unwrap(), 800);
    let mut users: Vec<i64> = e.store.stats(GUILD, "all").await.unwrap().into_iter().map(|(u, ..)| u).collect();
    users.sort();
    assert_eq!(users, [1, 2]); // player 3 played nothing rated
}

#[tokio::test]
async fn unrated_contest_changes_no_rating() {
    let e = env().await;
    let id = e.lobby(CreateMashup { rated: false, ..cm(1) }).await.mashup.id;
    e.svc.join(id, 2, None).await.unwrap();
    let view = e.svc.start(id, 1).await.unwrap();
    assert!(mashup_embed(&view, &HashMap::new()).unwrap().description.unwrap().contains("unrated"));
    let done = e.finish_contest(&view, &[("h2", &[(0, 1100)])]).await;
    assert_eq!(deltas(&done), [(1, None), (2, None)]);
    assert_eq!(e.store.rating(GUILD, 1).await.unwrap(), 800);
    assert_eq!(e.store.rating(GUILD, 2).await.unwrap(), 800);
    let text = mashup_embed(&done, &HashMap::new()).unwrap().description.unwrap();
    assert!(!text.split("**Standings**").nth(1).unwrap().contains("rating"));
}

#[tokio::test]
async fn team_virtual_contest_deltas() {
    let e = env().await;
    let mut all = three();
    all.extend([problem(500, "A", "First", Some(900)), problem(500, "B", "Two", Some(1400))]);
    *e.problems.0.lock().unwrap() = Arc::new(all);
    let v = e.lobby(CreateMashup { contest_id: Some(500), teams: 2, team_size: 2, ..cm(1) }).await;
    e.svc.join(v.mashup.id, 2, Some(2)).await.unwrap();
    let started = e.svc.start(v.mashup.id, 1).await.unwrap();
    let done = e.finish_contest(&started, &[("h2", &[(0, 1100)])]).await;
    assert_eq!(deltas(&done), [(1, Some(-49)), (2, Some(49))]);
}

#[tokio::test]
async fn give_up_deltas_and_no_read_after_giving_up() {
    let e = env().await;
    let view = e.started(1, &[2, 3]).await;
    let mid = view.mashup.id;
    e.sub("h2", &view.problems[0].0, 1100);
    e.svc.check(mid, 2).await.unwrap();
    e.svc.give_up(mid, 2).await.unwrap();
    assert_eq!(e.lobby(cm(2)).await.mashup.status, "lobby"); // free for another contest
    e.clear_calls();
    e.sub("h2", &view.problems[1].0, 1150); // solved after giving up: not read, not counted
    e.set_time(1060);
    e.svc.tick().await.unwrap();
    assert!(!e.calls().contains(&"h2".to_string()));
    let done = e.finish_contest(&e.svc.view(mid, None).await.unwrap(), &[]).await;
    assert_eq!(done.mashup.status, "finished");
    let (d1, d2, d3) = (delta_of(&done, 1).unwrap(), delta_of(&done, 2).unwrap(), delta_of(&done, 3).unwrap());
    assert!(d2 > 0 && d1 < 0 && d3 < 0); // h2 still wins with 1 solve
    assert_eq!(e.store.rating(GUILD, 2).await.unwrap(), 800 + d2);
    assert!(mashup_embed(&done, &HashMap::new()).unwrap().description.unwrap().contains("1. [h2](https://codeforces.com/profile/h2) - 1 solved"));
}

// ---- tests/test_bingo.py ----

#[tokio::test]
async fn classic_line_win_ends_the_bingo_and_rates_it() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("classic", "line")).await;
    let keys = Env::board_keys(&view);
    for (i, at) in [(0, 1010), (1, 1020), (2, 1030)] {
        e.sub("h1", &keys[i], at);
    }
    e.sub("h2", &keys[3], 1015);
    e.sub("h2", &keys[4], 1025);
    e.set_time(1040);
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert!(done.mashup.status == "finished" && done.mashup.ends_at == Some(1040));
    let ranks: Vec<_> = done.standings().unwrap().into_iter().map(|r| (r.handle, r.rank, r.solved, r.lines)).collect();
    assert_eq!(ranks, [("h1".to_string(), 1, 3, 1), ("h2".to_string(), 2, 2, 0)]);
    let by_handle = |h: &str| done.players.iter().find(|p| p.handle == h).unwrap().delta.unwrap();
    assert!(by_handle("h1") > 0 && 0 > by_handle("h2"));
    assert!(mashup_embed(&done, &HashMap::new()).unwrap().description.unwrap().contains("[1]")); // the board shows the owner
}

#[tokio::test]
async fn a_line_waits_for_an_earlier_claim_of_a_player_not_read_yet() {
    let e = env_with(fifty(), Some(0.0001)).await; // one read per tick
    let view = e.bingo(&[2], bingo_cm("classic", "line")).await;
    let keys = Env::board_keys(&view);
    for (i, at) in [(0, 1010), (1, 1020), (2, 1030)] {
        e.sub("h1", &keys[i], at);
    }
    for (i, at) in [(3, 1005), (4, 1006), (5, 1007)] {
        e.sub("h2", &keys[i], at); // h2's line is earlier but not read yet
    }
    e.set_time(1040);
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    let ranks: Vec<_> = done.standings().unwrap().into_iter().map(|r| (r.handle, r.rank)).collect();
    assert_eq!(ranks, [("h2".to_string(), 1), ("h1".to_string(), 2)]);
}

#[tokio::test]
async fn continuous_bingo_keeps_going_after_a_line() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("classic", "continuous")).await;
    let keys = Env::board_keys(&view);
    for (i, at) in [(0, 1010), (1, 1020), (2, 1030)] {
        e.sub("h1", &keys[i], at);
    }
    e.sub("h2", &keys[3], 1015);
    e.set_time(1040);
    e.svc.tick().await.unwrap();
    assert_eq!(e.svc.view(view.mashup.id, None).await.unwrap().mashup.status, "active");
    e.set_time(view.mashup.ends_at.unwrap());
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    let ranks: Vec<_> = done.standings().unwrap().into_iter().map(|r| (r.handle, r.lines)).collect();
    assert_eq!(ranks, [("h1".to_string(), 1), ("h2".to_string(), 0)]);
}

#[tokio::test]
async fn a_full_classic_board_ends_the_bingo() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("classic", "continuous")).await;
    for (i, key) in Env::board_keys(&view).iter().enumerate() {
        e.sub(if i % 2 == 1 { "h1" } else { "h2" }, key, 1010 + i as i64);
    }
    e.set_time(1100);
    for _ in 0..3 {
        e.svc.tick().await.unwrap(); // reads share a budget, so a few ticks
    }
    assert_eq!(e.svc.view(view.mashup.id, None).await.unwrap().mashup.status, "finished");
}

#[tokio::test]
async fn replace_ends_at_the_first_line_and_continuous_runs_on() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("replace", "line")).await;
    let by_cell: HashMap<Option<i64>, String> = view.cells.iter().map(|c| (c.cell, c.key.clone())).collect();
    for (i, at) in [(0, 1010), (1, 1020), (2, 1030)] {
        e.sub("h1", &by_cell[&Some(i)], at);
    }
    e.set_time(1040);
    e.svc.check(view.mashup.id, 1).await.unwrap(); // stores the solves and adds the replacements
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    let ranks: Vec<_> = done.standings().unwrap().into_iter().map(|r| (r.handle, r.rank, r.solved, r.lines)).collect();
    assert_eq!(ranks, [("h1".to_string(), 1, 3, 1), ("h2".to_string(), 2, 0, 0)]);
    // Python's second half ("continuous runs on") is the continuous test above: replace ends at the line
}

#[tokio::test]
async fn a_finished_bingo_posts_its_closing_message_once() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], bingo_cm("classic", "line")).await;
    let mid = view.mashup.id;
    e.svc.set_thread(mid, 777).await.unwrap();
    let keys = Env::board_keys(&view);
    for (i, at) in [(0, 1010), (1, 1020), (2, 1030)] {
        e.sub("h1", &keys[i], at);
    }
    e.set_time(1040);
    e.svc.tick().await.unwrap();
    let done = e.svc.view(mid, None).await.unwrap();
    assert_eq!(done.mashup.status, "finished");
    // the cog's `_log`: claims once, then the final message once (sending is the cog's)
    assert_eq!(e.svc.claims_to_log(&done).await.unwrap().len(), 3);
    assert!(e.svc.final_due(&done).await.unwrap());
    assert!(e.svc.claims_to_log(&done).await.unwrap().is_empty()); // nothing twice
    assert!(!e.svc.final_due(&done).await.unwrap());
    assert_eq!(final_message(&done).unwrap(), "The bingo is over. Winner: <@1>. Ratings changed.");
    let text = log_board_embed(&done).unwrap().description.unwrap();
    assert!(text.contains("1. [1]") && text.contains("rating +"));
}
