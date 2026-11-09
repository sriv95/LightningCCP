//! `lightningccp --bench [--items N] [--idle]`: offline benchmark, the Python original's bench.py was deleted at the cut-over.
//!
//! No token, no network: a fake Codeforces `Http` serves bench/fixtures (raw JSON, parsed per call like a
//! response body), the Discord side drops what it is given. The real client, caches, store, services and cog
//! bodies run. Env: DB_PATH (must not exist yet), BENCH_FIXTURES.
//!
//! Workload (the order the Python original used):
//! 1. startup: store connect + migrate, clients, problem and contest caches (load, then refresh from the fake CF).
//! 2. setup, not timed: 60 linked members, 20 guilds with a reminder (exclude regexes), 8 running solo matches
//!    (users 1-8), one running team mashup (40 players, users 11-50).
//! 3. per item i (clock = BASE + i * 300 s), each step timed: gimme (user i), contest list (upcoming, running,
//!    recent in turn), contest .ics, reminder tick over all guilds, match check, mashup tick, ranklist.
//! Seed 1 in both; the RNG streams differ, so the picks differ, the work does not.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use poise::serenity_prelude::CreateEmbed;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::{Value, json};

use crate::cache::contests::ContestCache;
use crate::cache::problems::ProblemCache;
use crate::cf::client::{BASE_URL, BoxFuture, CodeforcesClient, Http, Params};
use crate::cf::models::{Contest, Standing};
use crate::cogs::contests::{
    Channels, ContestData, LoopState, PagesOutput, Post, ReplyOutput, calendar, list, send_due_reminders, show_ranklist,
};
use crate::cogs::problems::{GimmeArgs, ProblemsOutput, gimme};
use crate::matches::{Clock, CreateMatch, MatchService};
use crate::mashup::service::{CreateMashup, MashupService};
use crate::store::base::{Reminder, Store};
use crate::ui::embeds::a::EmbedData;

const BASE: i64 = 1_800_000_000; // the bench clock starts here (the fixtures' contest times are relative to it)
const SEED: u64 = 1;
const GUILD: i64 = 1;
const USERS: i64 = 60;
const MATCH_USERS: i64 = 8; // users 1..8 each hold a running solo match; users 11..50 play the mashup
const REMINDER_GUILDS: i64 = 20;
const TICK_SECONDS: i64 = 300; // the clock moves this much per item
const EXCLUDES: [&str; 4] = ["Div\\. 3|Div\\. 4|Educational", "Kotlin|Hello|Good Bye", "", "(?i)global|codeton"];
const STEPS: [&str; 7] =
    ["gimme", "contest_pages", "contest_ics", "reminder_tick", "match_check", "mashup_tick", "ranklist"];
const DEFAULT_ITEMS: usize = 50;
const FILES: [(&str, &str); 5] = [
    ("problemset.problems", "problemset.json"),
    ("contest.list", "contest_list.json"),
    ("user.info", "user_info.json"),
    ("user.status", "user_status.json"),
    ("contest.standings", "contest_standings.json"),
];

/// Stands in for reqwest: the fixture file bytes, parsed on every call.
struct FakeHttp {
    files: HashMap<&'static str, Vec<u8>>,
    bodies: Mutex<HashMap<String, Arc<Vec<u8>>>>, // one response body per request, built on first use
}

impl FakeHttp {
    fn new(dir: &std::path::Path) -> anyhow::Result<Self> {
        let mut files = HashMap::new();
        for (method, file) in FILES {
            files.insert(method, std::fs::read(dir.join(file)).with_context(|| format!("fixture {file} in {}", dir.display()))?);
        }
        Ok(Self { files, bodies: Mutex::default() })
    }

    fn body(&self, method: &'static str, params: &Params) -> Arc<Vec<u8>> {
        let mut sorted: Vec<_> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        sorted.sort();
        let key = format!("{method}?{}", sorted.join("&"));
        if let Some(body) = self.bodies.lock().unwrap().get(&key) {
            return body.clone();
        }
        let param = |name: &str| params.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str());
        let raw = &self.files[method];
        let mut body = if method == "user.info" || method == "user.status" {
            let mut payload: Value = serde_json::from_slice(raw).expect("fixture is JSON");
            let Value::Array(rows) = payload["result"].take() else { panic!("result is a list") };
            payload["result"] = if method == "user.info" {
                let wanted: Vec<String> = param("handles").unwrap_or("").split(';').map(str::to_lowercase).collect();
                rows.into_iter().filter(|u| wanted.contains(&u["handle"].as_str().unwrap_or("").to_lowercase())).collect()
            } else {
                let from: usize = param("from").and_then(|f| f.parse().ok()).unwrap_or(1);
                let rows = rows.into_iter().skip(from - 1);
                match param("count").and_then(|c| c.parse::<usize>().ok()) {
                    Some(n) => rows.take(n).collect(),
                    None => rows.collect(),
                }
            };
            serde_json::to_vec(&payload).expect("serializes")
        } else {
            raw.clone()
        };
        body.shrink_to_fit(); // the serializer's doubling slack would stay resident in the body cache
        let body = Arc::new(body);
        self.bodies.lock().unwrap().insert(key, body.clone());
        body
    }
}

impl Http for FakeHttp {
    fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Vec<u8>, ()>> {
        Box::pin(async move {
            let method = url.strip_prefix(BASE_URL).and_then(|m| FILES.iter().find(|(k, _)| *k == m)).ok_or(())?.0;
            Ok(self.body(method, params).to_vec()) // a fresh copy per call, like a response body
        })
    }
}

/// What the cogs reach: the real caches, store and client.
struct Data {
    db: Arc<Store>,
    cf: Arc<CodeforcesClient>,
    contests: Arc<ContestCache>,
}

impl ContestData for Data {
    fn contests(&self) -> Arc<Vec<Contest>> {
        self.contests.contests()
    }
    async fn reminder(&self, guild: i64) -> anyhow::Result<Option<Reminder>> {
        self.db.get_reminder(guild).await
    }
    async fn add_contest_remind(&self, guild: i64, contest: i64, user: i64) -> anyhow::Result<bool> {
        self.db.add_contest_remind(guild, contest, user).await
    }
    async fn linked_handles(&self, guild: i64) -> anyhow::Result<Vec<(i64, String)>> {
        self.db.linked_handles(guild).await
    }
    async fn standings(&self, contest: i64, handles: &[String]) -> anyhow::Result<Vec<Standing>> {
        self.cf.standings(contest, handles).await
    }
}

/// The Discord side: drops everything.
struct Drop;

impl ProblemsOutput for Drop {
    async fn followup_embed(&self, _: EmbedData) -> anyhow::Result<()> {
        Ok(())
    }
    async fn followup_pages(&self, _: Vec<EmbedData>) -> anyhow::Result<()> {
        Ok(())
    }
}

impl PagesOutput for Drop {
    async fn defer(&self) -> anyhow::Result<()> {
        Ok(())
    }
    async fn send_pages(&self, _: Vec<CreateEmbed>) -> anyhow::Result<()> {
        Ok(())
    }
}

impl ReplyOutput for Drop {
    async fn reply(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn reply_file(&self, _: &str, _: &str, _: Vec<u8>) -> anyhow::Result<()> {
        Ok(())
    }
}

impl Channels for Drop {
    fn exists(&self, _: i64) -> bool {
        true
    }
    async fn send(&self, _: i64, _: Post) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Microseconds: nearest-rank percentile of a sorted list.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((p * sorted.len() as f64).ceil() as usize).max(1) - 1]
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Milliseconds since this process started (Linux, 10 ms resolution; assumes 100 ticks per second).
fn proc_start_age_ms() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let ticks: f64 = stat.rsplit_once(") ")?.1.split_whitespace().nth(19)?.parse().ok()?;
    let uptime: f64 = std::fs::read_to_string("/proc/uptime").ok()?.split_whitespace().next()?.parse().ok()?;
    Some(((uptime - ticks / 100.0) * 1000.0).max(0.0))
}

fn peak_rss_kb() -> Option<i64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| l.strip_prefix("VmHWM:")?.split_whitespace().next()?.parse().ok())
}

fn fixtures_dir() -> PathBuf {
    std::env::var_os("BENCH_FIXTURES")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/bench/fixtures")), PathBuf::from)
}

pub async fn run(args: &[String]) -> anyhow::Result<()> {
    let t0 = Instant::now();
    let items = match args.iter().position(|a| a == "--items") {
        Some(i) => args.get(i + 1).context("--items needs a number")?.parse()?,
        None => DEFAULT_ITEMS,
    };
    let idle = args.iter().any(|a| a == "--idle");
    let (db_path, tmp) = match std::env::var("DB_PATH").ok().filter(|p| !p.is_empty()) {
        Some(p) => {
            anyhow::ensure!(!std::path::Path::new(&p).exists(), "DB_PATH {p} exists; the bench needs a fresh database");
            (PathBuf::from(p), None)
        }
        None => {
            let dir = std::env::temp_dir().join(format!("lightningccp-bench-{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            (dir.join("bench.db"), Some(dir))
        }
    };

    // 1. startup: store (opens and migrates), clients, caches from the fake CF
    let store = Arc::new(Store::connect(&db_path).await?);
    let cf = Arc::new(CodeforcesClient::with_timing(Arc::new(FakeHttp::new(&fixtures_dir())?), Duration::ZERO, 3, Duration::ZERO));
    let problems = Arc::new(ProblemCache::new(cf.clone(), store.clone()));
    let contests = Arc::new(ContestCache::new(cf.clone(), store.clone()));
    problems.load().await?;
    problems.refresh().await?;
    contests.load().await?;
    contests.refresh().await?;
    let clock: Clock = Arc::new(|| BASE as f64);
    let matches = MatchService::with(store.clone(), cf.clone(), problems.clone(), clock.clone(), StdRng::seed_from_u64(SEED));
    let mashups = MashupService::with(store.clone(), cf.clone(), problems.clone(), clock, StdRng::seed_from_u64(SEED));
    let mut startup_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let mut basis = "main_entry";
    if let Some(age) = proc_start_age_ms() {
        (startup_ms, basis) = (age, "process_start");
    }

    // setup (not timed): linked members, reminders, running matches and one mashup
    let handles: Vec<String> = (1..=USERS).map(|i| format!("bench_user_{i:03}")).collect();
    for (user_id, user) in (1..).zip(cf.users_info(&handles, 300).await?) {
        store.link_handle(GUILD, user_id, &user.handle, user.rating).await?;
    }
    for g in 1..=REMINDER_GUILDS {
        let role = (g % 2 == 1).then_some(200 + g);
        store.set_reminder(g, Some(100 + g), role, &[60, 10], EXCLUDES[g as usize % EXCLUDES.len()]).await?;
    }
    let mut match_ids = vec![];
    for u in 1..=MATCH_USERS {
        let c = CreateMatch {
            guild_id: GUILD,
            channel_id: 5,
            host_id: u,
            mode: "solo".into(),
            max_players: 1,
            invited_id: None,
            rating_arg: None,
            tags: None,
            exclude_tags: None,
        };
        match_ids.push(matches.create(c).await?.r#match.id);
    }
    let host = 11;
    let mashup = mashups
        .create(CreateMashup {
            guild_id: GUILD,
            channel_id: 5,
            host_id: host,
            min_rating: 800,
            max_rating: 1600,
            count: 5,
            duration_seconds: 7200,
            teams: 10,
            team_size: 5,
            ..Default::default()
        })
        .await?
        .mashup
        .id;
    for (n, u) in (11..=50).enumerate() {
        if u != host {
            mashups.join(mashup, u, Some(n as i64 % 10 + 1)).await?;
        }
    }
    mashups.start(mashup, host).await?;

    let data = Data { db: store.clone(), cf: cf.clone(), contests: contests.clone() };
    let all = data.contests();
    let ids = |phase: &str| -> Vec<i64> {
        let mut v: Vec<i64> = all.iter().filter(|c| c.phase == phase).map(|c| c.id).collect();
        v.sort();
        v
    };
    let (upcoming, finished) = (ids("BEFORE"), ids("FINISHED"));
    let kinds = ["upcoming", "running", "recent"];
    let mut rng = StdRng::seed_from_u64(SEED);
    let mut state = LoopState::default();
    let mut times: Vec<Vec<f64>> = vec![Vec::with_capacity(items); STEPS.len()];
    for i in 0..items {
        let now = BASE + i as i64 * TICK_SECONDS;
        let user = (i as i64 % USERS) + 1;
        let m = i as i64 % MATCH_USERS;
        for (s, step) in STEPS.iter().enumerate() {
            let t = Instant::now();
            match *step {
                "gimme" => gimme(&store, &*cf, &*problems, &mut rng, &Drop, GUILD, user as u64, GimmeArgs::default()).await?,
                "contest_pages" => list(&data, &Drop, kinds[i % 3]).await?,
                "contest_ics" => calendar(&data, &Drop, upcoming[i % upcoming.len()], now).await?,
                "reminder_tick" => send_due_reminders(&store, &data, &Drop, &mut state, now).await?,
                "match_check" => drop(matches.check(match_ids[m as usize], m + 1).await?),
                "mashup_tick" => drop(mashups.tick().await?),
                _ => show_ranklist(&data, &Drop, GUILD, finished[i % finished.len()]).await?,
            }
            times[s].push(t.elapsed().as_secs_f64() * 1e6);
        }
    }

    let mut summary = serde_json::Map::new();
    for (step, us) in STEPS.iter().zip(&times) {
        let mut sorted = us.clone();
        sorted.sort_by(f64::total_cmp);
        let mean = us.iter().sum::<f64>() / us.len() as f64;
        summary.insert(
            step.to_string(),
            json!({"mean_us": round1(mean), "p50_us": round1(percentile(&sorted, 0.5)), "p95_us": round1(percentile(&sorted, 0.95))}),
        );
    }
    println!(
        "{}",
        json!({"impl": "rust", "startup_ms": round1(startup_ms), "startup_basis": basis, "items": items, "steps": summary, "peak_rss_kb": peak_rss_kb()})
    );
    if idle {
        std::future::pending::<()>().await; // sleeps forever, no busy loop
    }
    store.conn.clone().close().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(dir) = tmp {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(())
}
