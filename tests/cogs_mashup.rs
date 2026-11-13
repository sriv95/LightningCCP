//! Port of the mashup parts of `tests/test_cogs.py` and `tests/test_buttons.py` and the cog-level tests of
//! `tests/test_mashup.py` and `tests/test_bingo.py`, over fake outputs, a fake Codeforces client and a real store.
//! Expected values come from the Python tests; the `/host` command shape is compared with
//! `MashupCog.host.to_dict(tree)` of the Python bot.
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow};
use lightningccp::cf::models::{Contest, Problem, Submission, User};
use lightningccp::cogs::mashup::*;
use lightningccp::cogs::matches::{ButtonOutput, Editor, Member, MatchEnv, PostOutput, Sent, SlashOutput, forfeit};
use lightningccp::errors::{BotError, Interaction, SendFailed};
use lightningccp::logic::base::MASHUP_LOBBY_TTL_SECONDS;
use lightningccp::matches::{CfApi, Clock, MatchService, ProblemSource};
use lightningccp::mashup::service::{CreateMashup, MashupService, MashupView};
use lightningccp::store::base::Store;
use lightningccp::store::mashup::{MashupCell, MashupPlayer, MashupRow};
use lightningccp::ui::embeds::a::MashupRecord;
use lightningccp::ui::embeds::b::{final_message, give_up_message, mashup_embed, start_message};
use poise::serenity_prelude::{CreateActionRow, CreateEmbed};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::{Value, json};

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
        let dir = std::env::temp_dir().join(format!("lccp-cogmashup-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
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

// ---- fake outputs ----------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Edited { channel: i64, message: i64, embed: Value, row: Option<Value> },
    Sent { thread: i64, text: String, embed: Option<Value>, ping: bool },
    Cleared { thread: i64, message: i64 },
    Created { channel: i64, message: i64, name: String },
    Defer,
    Post { embed: Value, row: Option<Value> },
    Original { embed: Value, row: Option<Value> },
    Text(String),
    Modal { id: i64, teams: i64 },
}

struct FakeOut {
    user: AtomicI64,
    log: Mutex<Vec<Ev>>,
    next: AtomicI64,
    thread_fails: AtomicBool,
}

impl FakeOut {
    fn new() -> Self {
        Self { user: AtomicI64::new(1), log: Mutex::default(), next: AtomicI64::new(100), thread_fails: AtomicBool::new(false) }
    }
    fn as_user(&self, user: i64) -> &Self {
        self.user.store(user, SeqCst);
        self
    }
    fn push(&self, ev: Ev) {
        self.log.lock().unwrap().push(ev);
    }
    fn events(&self) -> Vec<Ev> {
        self.log.lock().unwrap().clone()
    }
    fn sent(&self) -> Vec<(String, Option<Value>, bool)> {
        self.events().into_iter().filter_map(|e| if let Ev::Sent { text, embed, ping, .. } = e { Some((text, embed, ping)) } else { None }).collect()
    }
}

fn row_json(row: Option<CreateActionRow>) -> Option<Value> {
    row.map(|r| serde_json::to_value(r).unwrap())
}

/// (label, style, custom id) of each button of a row.
fn buttons(row: &Option<Value>) -> Vec<(String, u64, String)> {
    row.as_ref().map_or(vec![], |r| {
        r["components"].as_array().unwrap().iter().map(|b| (b["label"].as_str().unwrap().into(), b["style"].as_u64().unwrap(), b["custom_id"].as_str().unwrap().into())).collect()
    })
}

impl Editor for FakeOut {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> Result<()> {
        self.push(Ev::Edited { channel, message, embed: serde_json::to_value(embed)?, row: row_json(row) });
        Ok(())
    }
    async fn retire_message(&self, _channel: i64, _message: i64, _content: &str) -> Result<()> {
        unimplemented!()
    }
}

impl Threads for FakeOut {
    async fn create_thread(&self, channel: i64, message: i64, name: &str) -> Result<i64> {
        self.push(Ev::Created { channel, message, name: name.into() });
        if self.thread_fails.load(SeqCst) { Err(anyhow!("403")) } else { Ok(777) }
    }
    async fn send(&self, thread: i64, text: &str, embed: Option<CreateEmbed>, ping: bool) -> Result<i64> {
        let embed = embed.map(|e| serde_json::to_value(e).unwrap());
        self.push(Ev::Sent { thread, text: text.into(), embed, ping });
        Ok(self.next.fetch_add(1, SeqCst) + 1)
    }
    async fn clear_board(&self, thread: i64, message: i64) -> Result<()> {
        self.push(Ev::Cleared { thread, message });
        Ok(())
    }
}

impl PostOutput for FakeOut {
    fn user_id(&self) -> i64 {
        self.user.load(SeqCst)
    }
    fn channel_id(&self) -> i64 {
        6
    }
    async fn defer(&self, _ephemeral: bool) -> Result<()> {
        self.push(Ev::Defer);
        Ok(())
    }
    async fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> Result<Sent> {
        self.push(Ev::Post { embed: serde_json::to_value(embed)?, row: row_json(row) });
        Ok(Sent { id: 222, url: "https://discord.com/channels/1/6/222".into() })
    }
    async fn followup_text(&self, text: &str) -> Result<()> {
        self.push(Ev::Text(text.into()));
        Ok(())
    }
}

impl SlashOutput for FakeOut {
    async fn followup_embed(&self, _embed: CreateEmbed) -> Result<()> {
        unimplemented!()
    }
    async fn send_pages(&self, _pages: Vec<CreateEmbed>) -> Result<()> {
        unimplemented!()
    }
    async fn fetch_member(&self, _user: i64) -> Result<Option<Member>> {
        unimplemented!()
    }
}

impl ButtonOutput for FakeOut {
    async fn edit_original(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> Result<()> {
        self.push(Ev::Original { embed: serde_json::to_value(embed)?, row: row_json(row) });
        Ok(())
    }
}

impl Interaction for FakeOut {
    fn is_done(&self) -> bool {
        self.events().contains(&Ev::Defer)
    }
    fn is_application_command(&self) -> bool {
        false
    }
    async fn send_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.push(Ev::Text(message.into()));
        Ok(())
    }
    async fn followup_ephemeral(&self, message: &str) -> Result<(), SendFailed> {
        self.push(Ev::Text(message.into()));
        Ok(())
    }
    async fn delete_original(&self) -> Result<(), SendFailed> {
        Ok(())
    }
}

impl PressOutput for FakeOut {
    fn message_id(&self) -> Option<i64> {
        Some(77)
    }
    async fn send_modal(&self, mashup_id: i64, teams: i64) -> Result<()> {
        self.push(Ev::Modal { id: mashup_id, teams });
        Ok(())
    }
}

struct FakeEnv {
    tags: Vec<String>,
    contests: Vec<Contest>,
}

impl MashupEnv for FakeEnv {
    fn tags(&self) -> Vec<String> {
        self.tags.clone()
    }
    fn contests(&self) -> Arc<Vec<Contest>> {
        Arc::new(self.contests.clone())
    }
}

type Svc = MashupService<FakeCf, FakeProblems>;

/// `MatchEnv` whose contest half is the cog's `forfeit_contest`, as `bot` wires it.
struct FfEnv<'a> {
    svc: &'a Svc,
    out: &'a FakeOut,
}

impl MatchEnv for FfEnv<'_> {
    fn problems(&self) -> Arc<Vec<Problem>> {
        unimplemented!()
    }
    fn tags(&self) -> Vec<String> {
        unimplemented!()
    }
    async fn user_info(&self, _handle: &str) -> Result<User> {
        unimplemented!()
    }
    async fn contest_history(&self, _guild: i64, _user: i64) -> Result<Vec<MashupRecord>> {
        unimplemented!()
    }
    async fn contest_wins(&self, _guild: i64, _user: i64) -> Result<i64> {
        unimplemented!()
    }
    async fn forfeit_contest(&self, guild: i64, user: i64) -> Result<Option<bool>> {
        forfeit_contest(self.svc, self.out, guild, user).await
    }
}

fn berr<T>(r: Result<T>) -> BotError {
    match r {
        Err(e) => e.downcast_ref::<BotError>().expect("a BotError").clone(),
        Ok(_) => panic!("expected an error"),
    }
}

fn desc(e: &Value) -> &str {
    e["description"].as_str().unwrap_or("")
}

fn title(e: &Value) -> &str {
    e["title"].as_str().unwrap_or("")
}

fn team_cm(host: i64, teams: i64) -> CreateMashup {
    CreateMashup { teams, team_size: 5, ..cm(host) }
}

fn plain_bingo(mode: &str, size: i64) -> CreateMashup {
    CreateMashup { min_rating: 800, max_rating: 1700, count: 0, mode: mode.into(), size, win: "line".into(), ..cm(1) }
}

// ---- tests/test_cogs.py: the host group and the button template --------------------------------------

/// `MashupCog.host.to_dict(tree)` of the Python bot (option fields that matter).
const PY_HOST: &str = r#"{"name":"host","description":"Host a timed multi-problem contest","dm":false,"options":[{"name":"contest","description":"Open a timed contest lobby with a live scoreboard","type":1,"options":[{"name":"min_rating","description":"Easiest problem rating (default 800)","type":4,"required":false,"min_value":800,"max_value":3500},{"name":"max_rating","description":"Hardest problem rating (default 1500)","type":4,"required":false,"min_value":800,"max_value":3500},{"name":"count","description":"Number of problems (default 5)","type":4,"required":false,"min_value":1,"max_value":10},{"name":"hour","description":"Hours the contest lasts (default 2)","type":4,"required":false,"min_value":0,"max_value":24},{"name":"minute","description":"Extra minutes (default 0)","type":4,"required":false,"min_value":0,"max_value":59},{"name":"penalty","description":"Minutes added per wrong attempt before a solve (default 0)","type":4,"required":false,"min_value":0,"max_value":120},{"name":"teams","description":"Number of teams (default 10)","type":4,"required":false,"min_value":2,"max_value":10},{"name":"players","description":"Players in each team, 1-5 (default 1: every player is a team of their own)","type":4,"required":false,"min_value":1,"max_value":5},{"name":"tags","description":"Comma-separated tags; the problem must have all of them","type":3,"required":false,"autocomplete":true},{"name":"exclude_tags","description":"Comma-separated tags to avoid","type":3,"required":false,"autocomplete":true},{"name":"rated","description":"True (default) changes match ratings; False leaves them alone (unrated)","type":5,"required":false}]},{"name":"virtual","description":"Open a lobby that replays a finished Codeforces contest","type":1,"options":[{"name":"contest_id","description":"Codeforces contest id to replay (it must have finished)","type":4,"required":true,"autocomplete":true},{"name":"penalty","description":"Minutes added per wrong attempt before a solve (default 0)","type":4,"required":false,"min_value":0,"max_value":120},{"name":"teams","description":"Number of teams (default 10)","type":4,"required":false,"min_value":2,"max_value":10},{"name":"players","description":"Players in each team, 1-5 (default 1: every player is a team of their own)","type":4,"required":false,"min_value":1,"max_value":5},{"name":"rated","description":"True (default) changes match ratings; False leaves them alone (unrated)","type":5,"required":false}]},{"name":"bingo","description":"Open a bingo lobby: teams race for the squares","type":1,"options":[{"name":"size","description":"Squares on each side of the board, 3-7 (default 5)","type":4,"required":false,"min_value":3,"max_value":7},{"name":"mode","description":"classic: a claimed square stays; replace: a claimed square gets a +100 problem to steal","type":3,"required":false,"choices":[{"name":"classic","value":"classic"},{"name":"replace","value":"replace"}]},{"name":"win","description":"line (default): the first full line wins; continuous: play to the end, rank by lines","type":3,"required":false,"choices":[{"name":"line","value":"line"},{"name":"continuous","value":"continuous"}]},{"name":"min_rating","description":"Easiest problem rating (default 800)","type":4,"required":false,"min_value":800,"max_value":3500},{"name":"max_rating","description":"Hardest problem rating (default 1500)","type":4,"required":false,"min_value":800,"max_value":3500},{"name":"hour","description":"Hours the contest lasts (default 2)","type":4,"required":false,"min_value":0,"max_value":24},{"name":"minute","description":"Extra minutes (default 0)","type":4,"required":false,"min_value":0,"max_value":59},{"name":"teams","description":"Number of teams (default 10)","type":4,"required":false,"min_value":2,"max_value":10},{"name":"players","description":"Players in each team, 1-5 (default 1: every player is a team of their own)","type":4,"required":false,"min_value":1,"max_value":5},{"name":"tags","description":"Comma-separated tags; the problem must have all of them","type":3,"required":false,"autocomplete":true},{"name":"exclude_tags","description":"Comma-separated tags to avoid","type":3,"required":false,"autocomplete":true},{"name":"rated","description":"True (default) changes match ratings; False leaves them alone (unrated)","type":5,"required":false}]}]}"#;

fn norm(o: &Value) -> Value {
    let mut m = json!({"name": o["name"], "description": o["description"], "type": o["type"]});
    for k in ["required", "autocomplete"] {
        m[k] = json!(o[k].as_bool().unwrap_or(false));
    }
    for k in ["min_value", "max_value", "choices"] {
        if !o[k].is_null() && o[k] != json!([]) {
            m[k] = if o[k].is_number() { json!(o[k].as_f64().unwrap()) } else { o[k].clone() };
        }
    }
    // poise bounds an i64 option to Discord's own range; discord.py sends none (same limit in effect)
    if o["name"] == "contest_id" {
        m.as_object_mut().unwrap().remove("min_value");
        m.as_object_mut().unwrap().remove("max_value");
    }
    if let Some(options) = o["options"].as_array().filter(|a| !a.is_empty()) {
        m["options"] = Value::Array(options.iter().map(norm).collect());
    }
    m
}

#[test]
fn host_group_commands_options_and_button_template() {
    let rust = serde_json::to_value(host_group().create_as_slash_command().unwrap()).unwrap();
    let py: Value = serde_json::from_str(PY_HOST.trim()).unwrap();
    assert_eq!(norm(&rust), norm(&py));
    // `guild_only`
    assert_eq!(rust["contexts"], json!([0]), "{rust}");
    // the button template: every action, any non-ASCII digit like `\d`, nothing else
    for action in ["join", "start", "leave", "check", "giveup", "rematch"] {
        assert_eq!(parse_custom_id(&format!("mashup:{action}:7")).map(|(a, i)| (a.id(), i)), Some((action, 7)));
    }
    for bad in ["mashup:other:7", "mashup:join:", "mashup:join:7x", "mashup:join:-7", "match:join:7", "xmashup:join:7", "mashup:join:7:"] {
        assert_eq!(parse_custom_id(bad), None, "{bad}");
    }
    assert_eq!(parse_custom_id("mashup:join:\u{667}\u{663}").map(|x| x.1), Some(73)); // Arabic-Indic digits
    assert_eq!(parse_custom_id("mashup:join:99999999999999999999999").map(|x| x.1), Some(i64::MAX));
    assert_eq!(parse_team_modal_id(&team_modal_id(5)), Some(5));
}

#[tokio::test]
async fn components_by_status_and_button_labels() {
    // test_embed_lobby_active_cancelled (the buttons part)
    let e = env().await;
    let view = e.lobby(cm(1)).await;
    let id = view.mashup.id;
    let row = row_json(mashup_components(&view));
    assert_eq!(
        buttons(&row),
        [("Join".into(), 3, format!("mashup:join:{id}")), ("Start".into(), 1, format!("mashup:start:{id}")), ("Leave".into(), 2, format!("mashup:leave:{id}"))]
    );
    let view = e.svc.start(id, 1).await.unwrap();
    assert_eq!(buttons(&row_json(mashup_components(&view))), [("Check".into(), 1, format!("mashup:check:{id}")), ("Give Up".into(), 4, format!("mashup:giveup:{id}"))]);
    let done = e.finish_contest(&view, &[]).await;
    assert_eq!(buttons(&row_json(mashup_components(&done))), [("Rematch".into(), 1, format!("mashup:rematch:{id}"))]);
    let gone = e.svc.leave(e.lobby(cm(3)).await.mashup.id, 3).await.unwrap();
    assert!(mashup_components(&gone).is_none());
}

// ---- tests/test_buttons.py: the team modal ----------------------------------------------------------

#[tokio::test]
async fn team_modal_rejects_a_non_number_and_joins_with_the_number() {
    let e = env().await;
    let id = e.lobby(team_cm(1, 3)).await.mashup.id;
    let modal = serde_json::to_value(team_modal(id, 3)).unwrap();
    assert_eq!(modal["title"], "Join a team");
    let input = &modal["components"][0]["components"][0];
    assert_eq!(
        (input["label"].as_str(), input["placeholder"].as_str(), input["min_length"].as_u64(), input["max_length"].as_u64()),
        (Some("Team number (1-3)"), Some("1"), Some(1), Some(2))
    );
    let out = FakeOut::new();
    out.as_user(2);
    submit_modal(&e.svc, &out, id, "abc").await;
    assert_eq!(out.events(), [Ev::Text("The team must be a number.".into())]); // no defer, no join
    assert_eq!(e.svc.view(id, None).await.unwrap().players.len(), 1);
    let out = FakeOut::new();
    out.as_user(2);
    submit_modal(&e.svc, &out, id, " \u{662} ").await; // int() reads non-ASCII digits; strip() the blanks
    let players = e.svc.view(id, None).await.unwrap().players;
    assert_eq!(players.iter().map(|p| (p.user_id, p.team)).collect::<Vec<_>>(), [(1, Some(1)), (2, Some(2))]);
    let events = out.events();
    assert_eq!(events[0], Ev::Defer);
    assert!(matches!(&events[1], Ev::Original { embed, .. } if title(embed).contains("lobby")));
    // an error after the defer is reported as a private followup
    let out = FakeOut::new();
    out.as_user(2);
    submit_modal(&e.svc, &out, id, "9").await;
    assert_eq!(out.events(), [Ev::Defer, Ev::Text("Pick a team number from 1 to 3.".into())]);
}

// ---- tests/test_mashup.py -----------------------------------------------------------------------------

#[tokio::test]
async fn embed_lobby_active_cancelled() {
    let e = env().await;
    let by_key: HashMap<String, Problem> = three().into_iter().map(|p| (p.key(), p)).collect();
    let view = e.lobby(CreateMashup { penalty_minutes: 20, ..cm(1) }).await;
    let lobby = serde_json::to_value(CreateEmbed::from(mashup_embed(&view, &by_key).unwrap())).unwrap();
    assert!(desc(&lobby).contains("3 problems rated 800-1200") && desc(&lobby).contains("20 min per wrong attempt"));
    assert!(title(&lobby) == "Mashup lobby" && desc(&lobby).contains("<@1> ([h1](https://codeforces.com/profile/h1))"));
    e.svc.start(view.mashup.id, 1).await.unwrap();
    e.sub("h1", "1A", 1160);
    e.set_time(1200);
    let view = e.svc.check(view.mashup.id, 1).await.unwrap();
    let text = serde_json::to_value(CreateEmbed::from(mashup_embed(&view, &by_key).unwrap())).unwrap();
    assert!(desc(&text).contains("A. [1A Name1](") && desc(&text).contains("C. [3A Name3](") && desc(&text).contains("(1200)"));
    e.set_time(8200);
    let finals = e.svc.tick().await.unwrap();
    let embed = serde_json::to_value(CreateEmbed::from(mashup_embed(&finals[0], &by_key).unwrap())).unwrap();
    assert_eq!(title(&embed), "Mashup contest - final standings");
    let gone = e.svc.leave(e.lobby(cm(3)).await.mashup.id, 3).await.unwrap();
    assert_eq!(mashup_embed(&gone, &by_key).unwrap().title.as_deref(), Some("Mashup cancelled"));
    // /host contest posts the lobby with its buttons and remembers the message
    let out = FakeOut::new();
    out.as_user(4);
    open(&e.svc, &out, GUILD, cm(4)).await.unwrap();
    let ev = out.events();
    assert_eq!(ev[0], Ev::Defer);
    let Ev::Post { embed, row } = &ev[1] else { panic!("{ev:?}") };
    assert_eq!(title(embed), "Mashup lobby");
    assert_eq!(buttons(row).len(), 3);
    let m = e.store.get_mashup(3).await.unwrap().unwrap();
    assert_eq!((m.channel_id, m.message_id, m.host_id), (6, Some(222), 4));
}

#[tokio::test]
async fn team_contest_embeds_and_refresh_skips_finished_teams() {
    let e = env().await;
    let id = e.lobby(CreateMashup { count: 1, min_rating: 800, max_rating: 800, ..team_cm(1, 2) }).await.mashup.id;
    e.svc.join(id, 2, Some(2)).await.unwrap();
    let text = serde_json::to_value(CreateEmbed::from(mashup_embed(&e.svc.view(id, None).await.unwrap(), &HashMap::new()).unwrap())).unwrap();
    assert!(desc(&text).contains("Team 1: <@1> ([h1](https://codeforces.com/profile/h1))") && desc(&text).contains("Team 2: <@2> ([h2](https://codeforces.com/profile/h2))"));
    assert!(desc(&text).contains("Join asks for a team number (1-2)") && desc(&text).contains("2 teams"));
    e.svc.join(id, 3, Some(1)).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    e.sub("h1", "1A", 1100);
    e.svc.check(id, 1).await.unwrap();
    e.clear_calls();
    e.set_time(1200);
    let out = FakeOut::new();
    assert!(tick_round(&e.svc, &out).await.is_some()); // the loop body
    assert_eq!(e.calls(), ["h2"]); // team 1 solved everything: neither h1 nor teammate h3 is read
    let embed = serde_json::to_value(CreateEmbed::from(mashup_embed(&e.svc.view(id, None).await.unwrap(), &HashMap::new()).unwrap())).unwrap();
    assert_eq!(title(&embed), "Team mashup");
    assert!(desc(&embed).contains("1\\. Team 1 (<@1>, <@3>) - 1 solved"));
}

#[tokio::test]
async fn ff_leaves_a_lobby_gives_up_a_running_contest_or_match() {
    let e = env().await;
    let clock: Clock = {
        let t = e.t.clone();
        Arc::new(move || t.load(SeqCst) as f64)
    };
    let matches = MatchService::with(e.store.clone(), e.cf.clone(), e.problems.clone(), clock, StdRng::seed_from_u64(1));
    let out = FakeOut::new();
    let env_ = FfEnv { svc: &e.svc, out: &out };
    let ff = |uid: i64| {
        out.as_user(uid);
        forfeit(&matches, &env_, &out, GUILD)
    };
    assert_eq!(berr(ff(1).await).to_string(), "You are not in an unfinished match or contest.");
    let view = e.lobby(cm(1)).await;
    let id = view.mashup.id;
    e.svc.set_message(id, 6, 222).await.unwrap();
    e.svc.join(id, 2, None).await.unwrap();
    ff(2).await.unwrap(); // leaves the contest lobby
    let edited = |out: &FakeOut| {
        out.events().into_iter().filter_map(|ev| if let Ev::Edited { embed, .. } = ev { Some(title(&embed).to_string()) } else { None }).collect::<Vec<_>>()
    };
    assert_eq!(out.events().last(), Some(&Ev::Text("You left the contest lobby.".into())));
    assert_eq!(edited(&out), ["Mashup lobby"]);
    assert_eq!(e.svc.view(id, None).await.unwrap().players.iter().map(|p| p.user_id).collect::<Vec<_>>(), [1]);
    e.svc.join(id, 3, None).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    e.svc.set_thread(id, 9).await.unwrap();
    ff(3).await.unwrap(); // gives up the running contest
    assert_eq!(out.events().last(), Some(&Ev::Text("You gave up the contest.".into())));
    assert_eq!(edited(&out).len(), 2);
    assert!(out.sent().iter().any(|(t, _, ping)| t == "<@3> gave up." && !ping)); // `log_gave_up`
    assert!(e.store.active_mashup_for(GUILD, 3).await.unwrap().is_none());
    ff(1).await.unwrap();
    e.svc.tick().await.unwrap();
    assert_eq!(e.svc.view(id, None).await.unwrap().mashup.status, "finished"); // everyone is out
    let duel = matches
        .create(lightningccp::matches::CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 4, mode: "duel".into(), max_players: 2, ..Default::default() })
        .await
        .unwrap();
    ff(4).await.unwrap(); // the host of a match lobby cancels it
    assert_eq!(out.events().last(), Some(&Ev::Text("You left the match lobby.".into())));
    assert_eq!(e.store.get_match(duel.r#match.id).await.unwrap().unwrap().status, "cancelled");
}

#[tokio::test]
async fn final_message_names_the_winner_a_tie_or_nobody() {
    let e = env().await;
    let view = e.started(1, &[2]).await;
    let done = e.finish_contest(&view, &[]).await;
    assert_eq!(final_message(&done).unwrap(), "The contest is over. Nobody solved anything.");
    let view = e.started(4, &[3]).await;
    let at = view.mashup.started_at.unwrap() + 100;
    let done = e.finish_contest(&view, &[("h4", &[(0, at)]), ("h3", &[(0, at)])]).await;
    assert!(final_message(&done).unwrap().starts_with("The contest is over. Tied for first: <@4>, <@3>"));
    let view = e.started(1, &[2]).await;
    let done = e.finish_contest(&view, &[("h1", &[(0, view.mashup.started_at.unwrap() + 100)])]).await;
    assert_eq!(final_message(&done).unwrap(), "The contest is over. Winner: <@1>. Ratings changed.");
    let lobby = e.lobby(cm(4)).await;
    e.svc.leave(lobby.mashup.id, 4).await.unwrap();
    assert_eq!(final_message(&e.svc.view(lobby.mashup.id, None).await.unwrap()).unwrap(), "This contest was cancelled.");
}

#[tokio::test]
async fn give_up_is_logged_to_the_thread() {
    let e = env().await;
    let view = e.started(1, &[2]).await;
    e.svc.set_thread(view.mashup.id, 9).await.unwrap();
    let view = e.svc.give_up(view.mashup.id, 2).await.unwrap();
    assert_eq!(give_up_message(&view, 2).unwrap(), "<@2> gave up.");
    let out = FakeOut::new();
    log_gave_up(&out, &view, 2).await;
    assert_eq!(out.events(), [Ev::Sent { thread: 9, text: "<@2> gave up.".into(), embed: None, ping: false }]);
    let team = e.lobby(team_cm(3, 2)).await; // teams show the team too
    e.svc.join(team.mashup.id, 4, Some(2)).await.unwrap();
    let team = e.svc.view(team.mashup.id, None).await.unwrap();
    assert_eq!(give_up_message(&team, 4).unwrap(), "Team 2 (<@4>) gave up.");
    log_gave_up(&out, &team, 4).await; // no thread: nothing happens
    assert_eq!(out.events().len(), 1);
}

#[tokio::test]
async fn start_pings_every_player_in_the_thread() {
    let e = env().await;
    let view = e.started(1, &[2, 3]).await;
    let text = start_message(&view);
    assert!(text.starts_with("<@1> <@2> <@3>\nThe contest has started and ends "));
    assert!(text.contains(&format!("<t:{}:R>", view.mashup.ends_at.unwrap())));
    let out = FakeOut::new();
    log_started(&out, &view).await; // no thread yet
    assert!(out.events().is_empty());
    e.svc.set_thread(view.mashup.id, 9).await.unwrap();
    log_started(&out, &e.svc.view(view.mashup.id, None).await.unwrap()).await;
    assert_eq!(out.events(), [Ev::Sent { thread: 9, text, embed: None, ping: true }]);
}

#[tokio::test]
async fn start_button_opens_the_thread_edits_the_message_and_pings() {
    let e = env().await;
    let id = e.lobby(cm(1)).await.mashup.id;
    let out = FakeOut::new();
    on_button(&e.svc, &out, Action::Start, id).await.unwrap();
    let ev = out.events();
    assert_eq!(ev[0], Ev::Defer);
    assert!(matches!(&ev[1], Ev::Original { embed, row } if title(embed) == "Mashup contest" && buttons(row).len() == 2));
    assert_eq!(ev[2], Ev::Created { channel: 6, message: 77, name: format!("Contest {id} solve log") });
    assert_eq!(e.svc.view(id, None).await.unwrap().mashup.thread_id, Some(777));
    assert!(matches!(&ev[3], Ev::Sent { thread: 777, text, ping: true, .. } if text.starts_with("<@1>\nThe contest has started")));
    // a refused thread: the presser is told, and nothing is logged
    let id = e.lobby(cm(2)).await.mashup.id;
    let out = FakeOut::new();
    out.thread_fails.store(true, SeqCst);
    out.as_user(2);
    on_button(&e.svc, &out, Action::Start, id).await.unwrap();
    assert!(out.events().contains(&Ev::Text(
        "I could not open the solve log thread here. Give me the Create Public Threads and Send Messages in Threads permissions.".into()
    )));
    assert!(out.sent().is_empty());
    // Join in a team contest opens the modal before any defer; errors are reported privately
    let team = e.lobby(team_cm(3, 4)).await.mashup.id;
    let out = FakeOut::new();
    out.as_user(4);
    on_button(&e.svc, &out, Action::Join, team).await.unwrap();
    assert_eq!(out.events(), [Ev::Modal { id: team, teams: 4 }]);
    let out = FakeOut::new();
    press(&e.svc, &out, Action::Join, 999).await;
    assert_eq!(out.events(), [Ev::Defer, Ev::Text("That contest no longer exists.".into())]);
}

#[tokio::test]
async fn rematch_posts_a_new_lobby_and_leaves_the_result() {
    let e = env().await;
    let view = e.started(1, &[2]).await;
    let done = e.finish_contest(&view, &[]).await;
    let out = FakeOut::new();
    out.as_user(2);
    on_button(&e.svc, &out, Action::Rematch, done.mashup.id).await.unwrap();
    let ev = out.events();
    assert_eq!(ev[0], Ev::Defer);
    assert!(matches!(&ev[1], Ev::Post { embed, .. } if title(embed) == "Mashup lobby"));
    assert_eq!(ev.len(), 2); // the finished message is not edited
    assert_eq!(e.store.get_mashup(done.mashup.id + 1).await.unwrap().unwrap().message_id, Some(222));
}

#[tokio::test]
async fn tick_round_edits_changed_messages_and_sets_the_interval() {
    let e = env().await;
    let id = e.lobby(cm(1)).await.mashup.id;
    e.svc.set_message(id, 6, 222).await.unwrap();
    e.set_time(1000 + MASHUP_LOBBY_TTL_SECONDS);
    let out = FakeOut::new();
    let next = tick_round(&e.svc, &out).await.unwrap();
    assert!((10..=60).contains(&next.as_secs()));
    let ev = out.events();
    assert!(matches!(&ev[..], [Ev::Edited { channel: 6, message: 222, embed, row: None }] if title(embed) == "Mashup cancelled"));
    assert_eq!(TICK_INTERVAL.as_secs(), 60);
}

#[test]
fn virtual_and_option_specs() {
    let contests = vec![
        Contest { id: 7, name: "Round [Div. 2]".into(), kind: "CF".into(), phase: "FINISHED".into(), start_time: Some(5), duration: 9000 },
        Contest { id: 8, name: "Live".into(), kind: "CF".into(), phase: "CODING".into(), start_time: Some(9), duration: 7200 },
    ];
    let spec = virtual_spec(&contests, 7, 10, 3, 2, false).unwrap();
    assert_eq!((spec.min_rating, spec.max_rating, spec.count, spec.duration_seconds, spec.contest_id), (800, 3500, 1, 9000, Some(7)));
    assert_eq!((spec.penalty_minutes, spec.teams, spec.team_size, spec.rated), (10, 3, 2, false));
    assert_eq!(berr(virtual_spec(&contests, 9, 0, 10, 1, true)).to_string(), "I do not know that contest id. See /contests recent.");
    assert_eq!(berr(virtual_spec(&contests, 8, 0, 10, 1, true)).to_string(), "That contest has not finished yet.");
    let b = bingo_spec(4, "replace", "continuous", 800, 1500, 1, 30, 10, 1, true, None, None);
    assert_eq!((b.count, b.duration_seconds, b.penalty_minutes, b.mode.as_str(), b.size, b.win.as_str()), (16, 5400, 0, "replace", 4, "continuous"));
    let c = contest_spec(800, 1500, 5, 2, 0, 0, 10, 1, true, Some("dp".into()), None);
    assert_eq!((c.duration_seconds, c.mode.as_str(), c.tags.as_deref()), (7200, "contest", Some("dp")));
}

#[tokio::test]
async fn autocomplete_lists_known_finished_contests_and_tags() {
    let e = env().await;
    let c = |id: i64, name: &str, start: i64| Contest { id, name: name.into(), kind: "CF".into(), phase: "FINISHED".into(), start_time: Some(start), duration: 7200 };
    let env_ = FakeEnv { tags: vec!["dp".into(), "math".into()], contests: vec![c(1, "One", 5), c(2, "Two", 9), c(99, "Unknown", 10)] };
    assert_eq!(contest_autocomplete(&e.svc, &env_, ""), [("2 - Two".to_string(), 2), ("1 - One".to_string(), 1)]);
    assert_eq!(contest_autocomplete(&e.svc, &env_, "one"), [("1 - One".to_string(), 1)]);
    assert_eq!(tag_autocomplete(&env_, "m"), ["math"]);
}

// ---- tests/test_bingo.py ----------------------------------------------------------------------------------

#[tokio::test]
async fn bingo_embeds_and_rematch() {
    let e = bingo_env().await;
    let by_key: HashMap<String, Problem> = fifty().into_iter().map(|p| (p.key(), p)).collect();
    let lobby = e.lobby(CreateMashup { duration_seconds: 3600, ..plain_bingo("classic", 3) }).await;
    let em = mashup_embed(&lobby, &HashMap::new()).unwrap();
    assert!(em.title.as_deref() == Some("Bingo lobby") && em.description.unwrap().contains("3x3 classic bingo, the first full line wins"));
    e.svc.join(lobby.mashup.id, 2, None).await.unwrap();
    let view = e.svc.start(lobby.mashup.id, 1).await.unwrap();
    let em = mashup_embed(&view, &by_key).unwrap();
    assert_eq!(em.title.as_deref(), Some("Bingo"));
    assert_eq!(em.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["Row 1", "Row 2", "Row 3"]);
    assert!(em.fields[0].value.contains("codeforces.com"));
    e.set_time(view.mashup.ends_at.unwrap());
    e.svc.tick().await.unwrap();
    let done = e.svc.view(view.mashup.id, None).await.unwrap();
    assert_eq!(mashup_embed(&done, &by_key).unwrap().title.as_deref(), Some("Bingo - final standings"));
    let again = e.svc.rematch(view.mashup.id, 2, 5).await.unwrap();
    let m = &again.mashup;
    assert_eq!((m.mode.as_str(), m.size, m.win.as_str(), m.count), ("classic", 3, "line", 9));
    let replace = e.lobby(CreateMashup { host_id: 3, duration_seconds: 3600, ..plain_bingo("replace", 4) }).await;
    assert_eq!(mashup_embed(&replace, &HashMap::new()).unwrap().description.unwrap().matches("replace bingo").count(), 1);
}

#[test]
fn a_full_size_bingo_embed_fits_discords_limit() {
    let m = MashupRow {
        id: 1, guild_id: 1, channel_id: 5, message_id: Some(9), host_id: 1000, min_rating: 800, max_rating: 1500, count: 49, duration_seconds: 7200,
        penalty_minutes: 0, status: "active".into(), created_at: 0, started_at: Some(0), ends_at: Some(7200), teams: 10, max_players: 50, rated: 1, team_size: 5,
        tags: "implementation,math".into(), exclude_tags: "geometry".into(), contest_id: None, mode: "replace".into(), size: 7, win: "line".into(),
        thread_id: None, log_board_id: None,
    };
    let cells: Vec<MashupCell> = (0..49).map(|i| MashupCell { idx: i, key: format!("{}B2", 1000 + i), rating: 3500, cell: Some(i), since: 0 }).collect();
    let players: Vec<MashupPlayer> =
        (0..50).map(|i| MashupPlayer { user_id: 1000 + i, handle: format!("handle{i}_long_name"), joined_at: i, team: Some(i % 10 + 1), delta: None, active: true }).collect();
    let solves = (0..49).map(|i| (1000 + i, vec![(cells[i as usize].key.clone(), (100 + i, 0))])).collect();
    let view = MashupView { mashup: m, problems: vec![], players, solves, note: None, cells: cells.clone() };
    let by_key: HashMap<String, Problem> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| (c.key.clone(), Problem { contest_id: 1000 + i as i64, index: "B2".into(), name: "x".repeat(40), rating: Some(3500), tags: vec![] }))
        .collect();
    let em = mashup_embed(&view, &by_key).unwrap();
    let n = |s: &Option<String>| s.as_ref().map_or(0, |s| s.chars().count());
    assert!(n(&em.title) + n(&em.description) + n(&em.footer) + em.fields.iter().map(|f| f.name.chars().count() + f.value.chars().count()).sum::<usize>() <= 6000);
    assert!(n(&em.description) <= 4096);
}

#[tokio::test]
async fn claims_are_logged_to_the_thread_once_and_only_the_newest_shows_the_board() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], plain_bingo("replace", 3)).await;
    let mid = view.mashup.id;
    assert!(e.svc.claims_to_log(&e.svc.view(mid, None).await.unwrap()).await.unwrap().is_empty()); // no thread yet
    e.svc.set_thread(mid, 777).await.unwrap();
    let cells: Vec<&MashupCell> = view.cells.iter().filter(|c| c.rating < 1700).take(2).collect();
    let (first, second) = (cells[0].clone(), cells[1].clone());
    e.sub("h2", &first.key, 1010);
    e.set_time(1020);
    let after = e.svc.check(mid, 2).await.unwrap();
    assert_eq!(after.mashup.thread_id, Some(777));
    let out = FakeOut::new();
    log_board(&e.svc, &out, &after).await;
    log_board(&e.svc, &out, &e.svc.view(mid, None).await.unwrap()).await; // nothing new the second time
    let sent = out.sent();
    assert_eq!(sent.len(), 1);
    assert!(out.events().iter().all(|ev| !matches!(ev, Ev::Cleared { .. })));
    let (text, embed, ping) = &sent[0];
    assert!(text.starts_with("<t:1010:T> (+0m) <@2> claimed [") && !ping);
    assert!(text.contains(&format!("({})", first.rating)) && text.contains("New problem there:") && text.contains(&format!("({})", first.rating + 100)));
    assert!(desc(embed.as_ref().unwrap()).contains("[2]")); // the board shows team 2 holding the square
    assert_eq!(e.store.get_mashup(mid).await.unwrap().unwrap().log_board_id, Some(101));
    e.sub("h1", &second.key, 1030);
    e.set_time(1040);
    e.svc.check(mid, 1).await.unwrap();
    log_board(&e.svc, &out, &e.svc.view(mid, None).await.unwrap()).await;
    let sent = out.sent();
    assert_eq!(sent.len(), 2);
    assert!(desc(sent[1].1.as_ref().unwrap()).starts_with("```"));
    assert_eq!(out.events().iter().filter(|ev| matches!(ev, Ev::Cleared { thread: 777, message: 101 })).count(), 1); // the older message loses its board
    assert_eq!(e.store.get_mashup(mid).await.unwrap().unwrap().log_board_id, Some(102));
}

#[tokio::test]
async fn a_finished_bingo_posts_its_closing_message_once() {
    let e = bingo_env().await;
    let view = e.bingo(&[2], plain_bingo("classic", 3)).await;
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
    let out = FakeOut::new();
    log_board(&e.svc, &out, &done).await;
    log_board(&e.svc, &out, &e.svc.view(mid, None).await.unwrap()).await; // nothing twice
    let sent = out.sent();
    assert_eq!(sent.len(), 4); // three claims, then the closing message
    assert!(sent[2].1.is_none()); // the claims before it carry no board
    assert_eq!(sent[3].0, "The bingo is over. Winner: <@1>. Ratings changed.");
    let board = desc(sent[3].1.as_ref().unwrap());
    assert!(board.contains("1\\. [1]") && board.contains("rating +"));
    assert!(out.events().iter().all(|ev| !matches!(ev, Ev::Cleared { .. })));
    assert_eq!(e.store.get_mashup(mid).await.unwrap().unwrap().log_board_id, Some(104));
}

#[test]
fn custom_id_takes_unicode_digits_like_python() {
    use lightningccp::cogs::mashup::parse_custom_id;
    assert_eq!(parse_custom_id("mashup:join:٣٤").map(|(_, id)| id), Some(34));
    assert!(parse_custom_id("mashup:join:+4").is_none());
    assert!(parse_custom_id("mashup:join:").is_none());
}
