//! Port of the match parts of `tests/test_cogs.py` and `tests/test_buttons.py`, plus the command flows
//! (`/match`, `/ff`, `/leaderboard`, `/profile`, the buttons, the expiry loop) over the real store, the real
//! `MatchService` and a fake output.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::SeqCst};
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Problem, Submission, User};
use lightningccp::cogs::matches::*;
use lightningccp::errors::{BotError, Interaction, SendFailed};
use lightningccp::matches::{CfApi, Clock, CreateMatch, MatchService, MatchView, ProblemSource};
use lightningccp::store::base::Store;
use lightningccp::store::matches::{MatchPlayer, MatchRow};
use lightningccp::ui::embeds::a::{MashupRecord, match_embed};
use poise::serenity_prelude::{CreateActionRow, CreateEmbed};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde_json::{Value, json};

const GUILD: i64 = 1;

// ---- fakes ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeCf {
    subs: Mutex<HashMap<String, Vec<Submission>>>,
    fail: AtomicBool,
}

impl CfApi for FakeCf {
    async fn user_info(&self, _: &str) -> anyhow::Result<lightningccp::cf::models::User> {
        unreachable!()
    }
    async fn users_info(&self, _: &[String]) -> anyhow::Result<Vec<lightningccp::cf::models::User>> {
        unreachable!()
    }
    async fn user_status(&self, handle: &str, _count: Option<i64>, _since: Option<i64>) -> anyhow::Result<Vec<Submission>> {
        anyhow::ensure!(!self.fail.load(SeqCst), "boom");
        Ok(self.subs.lock().unwrap().get(handle).cloned().unwrap_or_default())
    }
}

struct FakeProblems(Vec<Problem>);

impl ProblemSource for FakeProblems {
    fn tags(&self) -> Arc<std::collections::HashSet<String>> {
        Arc::default()
    }
    fn problems(&self) -> Arc<Vec<Problem>> {
        Arc::new(self.0.clone())
    }
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        Ok(self.problems())
    }
}

struct FakeEnv {
    user: Mutex<Option<anyhow::Result<User>>>,
    forfeit: Option<bool>,
    wins: i64,
}

impl Default for FakeEnv {
    fn default() -> Self {
        Self { user: Mutex::new(None), forfeit: None, wins: 0 }
    }
}

impl MatchEnv for FakeEnv {
    fn problems(&self) -> Arc<Vec<Problem>> {
        Arc::new(vec![problem()])
    }
    fn tags(&self) -> Vec<String> {
        ["dp", "graphs", "greedy"].map(String::from).to_vec()
    }
    async fn user_info(&self, _handle: &str) -> anyhow::Result<User> {
        self.user.lock().unwrap().take().expect("user_info not expected")
    }
    async fn contest_history(&self, _guild: i64, _user: i64) -> anyhow::Result<Vec<MashupRecord>> {
        Ok(vec![])
    }
    async fn contest_wins(&self, _guild: i64, _user: i64) -> anyhow::Result<i64> {
        Ok(self.wins)
    }
    async fn forfeit_contest(&self, _guild: i64, _user: i64) -> anyhow::Result<Option<bool>> {
        Ok(self.forfeit)
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Defer(bool),
    Post(Value, Value),
    Retire(i64, i64, String),
    Edit(i64, i64, Value, Value),
    EditOriginal(Value, Value),
    Text(String),
    Embed(Value),
    Pages(Vec<Value>),
    // the `Interaction` calls of `report_error`
    Send(String),
    Followup(String),
    Delete,
}

fn row_json(row: Option<CreateActionRow>) -> Value {
    row.map_or(Value::Null, |r| serde_json::to_value(r).unwrap())
}

fn embed_json(e: CreateEmbed) -> Value {
    serde_json::to_value(e).unwrap()
}

struct FakeOut {
    user: i64,
    channel: i64,
    next_message: i64,
    members: HashMap<i64, Member>,
    done: AtomicBool,
    calls: Mutex<Vec<Call>>,
}

impl FakeOut {
    fn new(user: i64) -> Self {
        Self { user, channel: 6, next_message: 222, members: HashMap::new(), done: AtomicBool::new(false), calls: Mutex::new(vec![]) }
    }
    fn push(&self, c: Call) {
        self.calls.lock().unwrap().push(c);
    }
    fn calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.calls.lock().unwrap())
    }
}

impl Editor for FakeOut {
    async fn edit_message(&self, channel: i64, message: i64, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        self.push(Call::Edit(channel, message, embed_json(embed), row_json(row)));
        Ok(())
    }
    async fn retire_message(&self, channel: i64, message: i64, content: &str) -> anyhow::Result<()> {
        self.push(Call::Retire(channel, message, content.into()));
        Ok(())
    }
}

impl PostOutput for FakeOut {
    fn user_id(&self) -> i64 {
        self.user
    }
    fn channel_id(&self) -> i64 {
        self.channel
    }
    async fn defer(&self, ephemeral: bool) -> anyhow::Result<()> {
        self.done.store(true, SeqCst);
        self.push(Call::Defer(ephemeral));
        Ok(())
    }
    async fn post(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<Sent> {
        self.push(Call::Post(embed_json(embed), row_json(row)));
        Ok(Sent { id: self.next_message, url: format!("https://discord.com/channels/1/6/{}", self.next_message) })
    }
    async fn followup_text(&self, text: &str) -> anyhow::Result<()> {
        self.push(Call::Text(text.into()));
        Ok(())
    }
}

impl SlashOutput for FakeOut {
    async fn followup_embed(&self, embed: CreateEmbed) -> anyhow::Result<()> {
        self.push(Call::Embed(embed_json(embed)));
        Ok(())
    }
    async fn send_pages(&self, pages: Vec<CreateEmbed>) -> anyhow::Result<()> {
        self.push(Call::Pages(pages.into_iter().map(embed_json).collect()));
        Ok(())
    }
    async fn fetch_member(&self, user: i64) -> anyhow::Result<Option<Member>> {
        Ok(self.members.get(&user).cloned())
    }
}

impl ButtonOutput for FakeOut {
    async fn edit_original(&self, embed: CreateEmbed, row: Option<CreateActionRow>) -> anyhow::Result<()> {
        self.push(Call::EditOriginal(embed_json(embed), row_json(row)));
        Ok(())
    }
}

impl Interaction for FakeOut {
    fn is_done(&self) -> bool {
        self.done.load(SeqCst)
    }
    fn is_application_command(&self) -> bool {
        false
    }
    async fn send_ephemeral(&self, m: &str) -> Result<(), SendFailed> {
        self.push(Call::Send(m.into()));
        Ok(())
    }
    async fn followup_ephemeral(&self, m: &str) -> Result<(), SendFailed> {
        self.push(Call::Followup(m.into()));
        Ok(())
    }
    async fn delete_original(&self) -> Result<(), SendFailed> {
        self.push(Call::Delete);
        Ok(())
    }
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type Svc = MatchService<FakeCf, FakeProblems>;

struct Env {
    svc: Svc,
    store: Arc<Store>,
    cf: Arc<FakeCf>,
    t: Arc<AtomicI64>,
    _dir: TempDir,
}

fn problem() -> Problem {
    Problem { contest_id: 1700, index: "A".into(), name: "Foo".into(), rating: Some(800), tags: vec![] }
}

async fn env() -> Env {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("lccp-cog-{}-{}", std::process::id(), N.fetch_add(1, SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::connect(dir.join("t.db")).await.unwrap());
    let cf = Arc::new(FakeCf::default());
    let t = Arc::new(AtomicI64::new(1000));
    let t2 = t.clone();
    let clock: Clock = Arc::new(move || t2.load(SeqCst) as f64);
    let svc = MatchService::with(store.clone(), cf.clone(), Arc::new(FakeProblems(vec![problem()])), clock, StdRng::seed_from_u64(1));
    for uid in 1..=4 {
        store.link_handle(GUILD, uid, &format!("h{uid}"), Some(1000)).await.unwrap();
    }
    Env { svc, store, cf, t, _dir: TempDir(dir) }
}

fn me(id: i64) -> Member {
    Member { id, name: format!("User{id}"), bot: false }
}

fn bot_error(e: &anyhow::Error) -> String {
    e.downcast_ref::<BotError>().expect("a BotError").to_string()
}

fn row(status: &str, mode: &str) -> MatchRow {
    MatchRow {
        id: 1,
        guild_id: 1,
        channel_id: 5,
        message_id: None,
        host_id: 10,
        mode: mode.into(),
        max_players: 2,
        invited_id: None,
        rating_arg: None,
        rating: Some(1000),
        problem_key: Some("1700A".into()),
        status: status.into(),
        created_at: 1000,
        started_at: None,
        expires_at: None,
        finished_at: None,
        tags: String::new(),
        exclude_tags: String::new(),
    }
}

fn player(uid: i64, solved_at: Option<i64>) -> MatchPlayer {
    MatchPlayer {
        match_id: 1,
        guild_id: 1,
        user_id: uid,
        handle: format!("h{uid}"),
        joined_at: 1000,
        solved_at,
        gave_up: false,
        delta: None,
        active: true,
    }
}

fn view(status: &str, mode: &str, players: Vec<MatchPlayer>) -> MatchView {
    MatchView { r#match: row(status, mode), players, problem: None, ratings: None, note: None }
}

fn duel_spec() -> CreateMatch {
    CreateMatch { mode: "duel".into(), max_players: 2, ..Default::default() }
}

// ---- test_cogs.py ----------------------------------------------------------------------------------------

#[test]
fn match_group_commands() {
    let group = match_group();
    assert_eq!(group.name, "match");
    let names: std::collections::BTreeSet<_> = group.subcommands.iter().map(|c| &*c.name).collect();
    assert_eq!(names, ["custom", "duel", "history", "solo"].into_iter().collect());
    assert!(group.guild_only);
    let top = [ff(), leaderboard_command(), profile_command()];
    assert_eq!(top.iter().map(|c| &*c.name).collect::<Vec<_>>(), ["ff", "leaderboard", "profile"]);
    assert!(top.iter().all(|c| c.guild_only));
}

#[test]
fn match_button_template_matches_all_actions() {
    for (id, action) in [
        ("join", Action::Join),
        ("start", Action::Start),
        ("leave", Action::Leave),
        ("check", Action::Check),
        ("giveup", Action::GiveUp),
        ("rematch", Action::Rematch),
    ] {
        assert_eq!(parse_custom_id(&format!("match:{id}:42")), Some((action, 42)));
    }
    for bad in ["match:other:42", "match:join:", "match:join:4x", "match:join:42 ", "xmatch:join:42", "match:join", "mashup:join:42"] {
        assert_eq!(parse_custom_id(bad), None, "{bad}");
    }
}

#[test]
fn buttons_keep_python_labels_styles_and_custom_ids() {
    let v = view("lobby", "duel", vec![player(10, None)]);
    let got = serde_json::to_value(match_components(&v).unwrap()).unwrap();
    let want = json!({"type": 1, "components": [
        {"type": 2, "style": 3, "disabled": false, "label": "Join", "custom_id": "match:join:1"},
        {"type": 2, "style": 1, "disabled": false, "label": "Start", "custom_id": "match:start:1"},
        {"type": 2, "style": 2, "disabled": false, "label": "Leave", "custom_id": "match:leave:1"},
    ]});
    assert_eq!(got, want);
    let v = view("active", "duel", vec![player(10, None)]);
    let got = serde_json::to_value(match_components(&v).unwrap()).unwrap();
    assert_eq!(got["components"][0], json!({"type": 2, "style": 3, "disabled": false, "label": "Check", "custom_id": "match:check:1"}));
    assert_eq!(got["components"][1], json!({"type": 2, "style": 4, "disabled": false, "label": "Give Up", "custom_id": "match:giveup:1"}));
}

// ---- test_buttons.py -------------------------------------------------------------------------------------

#[test]
fn rematch_button_only_on_a_finished_duel() {
    let actions = |status, mode| match_actions(&view(status, mode, vec![player(10, None), player(11, None)]));
    assert_eq!(actions("finished", "duel"), Some(&[Action::Rematch][..]));
    assert_eq!(actions("finished", "solo"), None);
    assert_eq!(actions("finished", "custom"), None);
    assert_eq!(actions("cancelled", "duel"), None);
}

#[tokio::test]
async fn button_callback_reports_errors_privately() {
    let e = env().await;
    // a BotError: the match is not open
    let out = FakeOut::new(1);
    press(&e.svc, &out, Action::Start, 99).await;
    assert_eq!(out.calls(), [Call::Defer(false), Call::Followup("That match no longer exists.".into())]);
    // anything else: the generic message (Codeforces failing with a non-BotError)
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.join(id, 2).await.unwrap();
    e.cf.fail.store(true, SeqCst);
    let out = FakeOut::new(1);
    press(&e.svc, &out, Action::Start, id).await;
    assert_eq!(out.calls(), [Call::Defer(false), Call::Followup("Something went wrong. Try again later.".into())]);
}

#[tokio::test]
async fn repost_retires_the_old_message() {
    let e = env().await;
    let first = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap();
    let id = first.r#match.id;
    e.svc.set_message(id, 5, 111).await.unwrap();
    let out = FakeOut::new(1);
    create(&e.svc, &out, GUILD, duel_spec()).await.unwrap();
    let again = e.svc.current(GUILD, 1).await.unwrap().unwrap();
    assert_eq!((again.r#match.id, again.r#match.channel_id, again.r#match.message_id), (id, 6, Some(222)));
    let calls = out.calls();
    assert_eq!(calls[0], Call::Defer(false));
    assert!(matches!(&calls[1], Call::Post(..)));
    assert_eq!(calls[2], Call::Retire(5, 111, "This match moved to https://discord.com/channels/1/6/222".into()));
    assert_eq!(calls.len(), 3);
}

#[tokio::test]
async fn new_match_is_posted_once_with_buttons_and_stored() {
    let e = env().await;
    let out = FakeOut::new(1);
    solo(&e.svc, &out, GUILD, None, None, None).await.unwrap();
    let v = e.svc.current(GUILD, 1).await.unwrap().unwrap();
    assert_eq!((v.r#match.channel_id, v.r#match.message_id, v.r#match.mode.as_str()), (6, Some(222), "solo"));
    let calls = out.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], Call::Defer(false));
    // the posted embed is the stored match's embed, with the Check and Give Up buttons
    assert_eq!(calls[1], Call::Post(embed_json(match_embed(&v).unwrap().into()), row_json(match_components(&v))));
    assert_eq!(row_json(match_components(&v))["components"][0]["custom_id"], format!("match:check:{}", v.r#match.id));
    // a tag filter nothing satisfies is the service's error, after the defer
    let out = FakeOut::new(2);
    let err = solo(&e.svc, &out, GUILD, None, Some("dp".into()), None).await.unwrap_err();
    assert_eq!(bot_error(&err), "No problem rated 800 with those tags is unsolved by every player.");
    assert_eq!(out.calls(), [Call::Defer(false)]);
}

#[tokio::test]
async fn duel_against_a_bot_is_refused_before_the_defer() {
    let e = env().await;
    let out = FakeOut::new(1);
    let bot = Member { id: 9, name: "Bot".into(), bot: true };
    let err = duel(&e.svc, &out, GUILD, Some(&bot), None, None, None).await.unwrap_err();
    assert_eq!(bot_error(&err), "You cannot duel a bot.");
    assert_eq!(out.calls(), []);
    duel(&e.svc, &out, GUILD, Some(&me(2)), None, None, None).await.unwrap();
    assert_eq!(e.svc.current(GUILD, 1).await.unwrap().unwrap().r#match.invited_id, Some(2));
}

#[tokio::test]
async fn check_without_finish_gives_presser_feedback() {
    let solved = view("active", "custom", vec![player(10, Some(2100)), player(11, None)]);
    assert_eq!(check_note(&solved, 10), "You solved it. Waiting for the other players.");
    assert!(check_note(&solved, 11).starts_with("No accepted submission found yet."));
    assert_eq!(
        check_note(&solved, 11),
        "No accepted submission found yet. Solve the problem on Codeforces, then press Check again."
    );
    // through the service: nobody has solved yet, so the match stays active and the presser gets the note
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.join(id, 2).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    let out = FakeOut::new(2);
    on_button(&e.svc, &out, Action::Check, id).await.unwrap();
    let calls = out.calls();
    assert_eq!(calls[0], Call::Defer(false));
    assert!(matches!(&calls[1], Call::EditOriginal(..)));
    assert_eq!(calls[2], Call::Text(check_note(&view("active", "duel", vec![player(2, None)]), 2).into()));
    assert_eq!(calls.len(), 3);
}

#[tokio::test]
async fn rematch_button_posts_a_new_lobby() {
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.join(id, 2).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    e.svc.give_up(id, 1).await.unwrap(); // finished: player 2 wins
    let out = FakeOut::new(1);
    on_button(&e.svc, &out, Action::Rematch, id).await.unwrap();
    let calls = out.calls();
    assert_eq!(calls[0], Call::Defer(false));
    assert!(matches!(&calls[1], Call::Post(..)));
    assert_eq!(calls.len(), 2); // no edit of the finished result, no retire
    let new = e.svc.current(GUILD, 1).await.unwrap().unwrap();
    assert_ne!(new.r#match.id, id);
    assert_eq!((new.r#match.status.as_str(), new.r#match.channel_id, new.r#match.message_id), ("lobby", 6, Some(222)));
}

#[tokio::test]
async fn button_edits_the_original_with_the_new_buttons() {
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    let out = FakeOut::new(2);
    on_button(&e.svc, &out, Action::Join, id).await.unwrap();
    let joined = e.svc.current(GUILD, 2).await.unwrap().unwrap();
    let want = Call::EditOriginal(embed_json(match_embed(&joined).unwrap().into()), row_json(match_components(&joined)));
    assert_eq!(out.calls(), [Call::Defer(false), want]);
}

// ---- the expiry loop -------------------------------------------------------------------------------------

#[test]
fn expiry_loop_runs_every_60_seconds() {
    assert_eq!(EXPIRE_INTERVAL, std::time::Duration::from_secs(60));
}

#[tokio::test]
async fn expiry_tick_edits_the_stored_message_of_a_cancelled_lobby() {
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.set_message(id, 5, 111).await.unwrap();
    let out = FakeOut::new(1);
    expire_tick(&e.svc, &out).await;
    assert_eq!(out.calls(), []); // not due yet
    e.t.fetch_add(10 * 60 + 1, SeqCst);
    expire_tick(&e.svc, &out).await;
    let cancelled = e.svc.current(GUILD, 1).await.unwrap();
    assert!(cancelled.is_none());
    let calls = out.calls();
    assert_eq!(calls.len(), 1);
    let Call::Edit(channel, message, embed, row) = &calls[0] else { panic!("one edit") };
    assert_eq!((*channel, *message, row), (5, 111, &Value::Null)); // cancelled: no buttons
    assert!(embed.to_string().contains("The lobby expired."));
    expire_tick(&e.svc, &out).await;
    assert_eq!(out.calls(), []); // once only
}

#[tokio::test]
async fn expiry_tick_skips_a_message_without_id() {
    let e = env().await;
    e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap();
    e.t.fetch_add(10 * 60 + 1, SeqCst);
    let out = FakeOut::new(1);
    expire_tick(&e.svc, &out).await; // no stored message: nothing to edit
    assert_eq!(out.calls(), []);
}

// ---- /ff -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn ff_leaves_a_lobby_gives_up_a_match_and_edits_the_stored_message() {
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.set_message(id, 5, 111).await.unwrap();
    e.svc.join(id, 2).await.unwrap();
    let out = FakeOut::new(2);
    forfeit(&e.svc, &FakeEnv::default(), &out, GUILD).await.unwrap();
    let calls = out.calls();
    assert_eq!(calls[0], Call::Defer(true));
    assert!(matches!(&calls[1], Call::Edit(5, 111, ..)));
    assert_eq!(calls[2], Call::Text("You left the match lobby.".into()));
    e.svc.join(id, 2).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    forfeit(&e.svc, &FakeEnv::default(), &out, GUILD).await.unwrap();
    assert_eq!(out.calls()[2], Call::Text("You gave up the match.".into()));
}

#[tokio::test]
async fn ff_falls_back_to_the_contest_then_to_an_error() {
    let e = env().await;
    let out = FakeOut::new(3);
    let contest = FakeEnv { forfeit: Some(false), ..Default::default() };
    forfeit(&e.svc, &contest, &out, GUILD).await.unwrap();
    assert_eq!(out.calls(), [Call::Defer(true), Call::Text("You gave up the contest.".into())]);
    let lobby = FakeEnv { forfeit: Some(true), ..Default::default() };
    forfeit(&e.svc, &lobby, &out, GUILD).await.unwrap();
    assert_eq!(out.calls()[1], Call::Text("You left the contest lobby.".into()));
    let err = forfeit(&e.svc, &FakeEnv::default(), &out, GUILD).await.unwrap_err();
    assert_eq!(bot_error(&err), "You are not in an unfinished match or contest.");
}

// ---- /leaderboard, /profile, /match history --------------------------------------------------------------

#[tokio::test]
async fn leaderboard_pages_and_empty_text() {
    let e = env().await;
    let out = FakeOut::new(1);
    leaderboard(&e.store, &out, GUILD, 1000, Sort::Rating, Kind::All).await.unwrap();
    let calls = out.calls();
    assert_eq!(calls[0], Call::Defer(true));
    let Call::Pages(p) = &calls[1] else { panic!("pages") };
    assert_eq!(p.len(), 1);
    assert_eq!((p[0]["title"].as_str(), p[0]["description"].as_str()), (Some("Match stats"), Some("No finished games yet.")));
    leaderboard(&e.store, &out, GUILD, 1000, Sort::Streak, Kind::Contests).await.unwrap();
    let Call::Pages(p) = &out.calls()[1] else { panic!("pages") };
    assert_eq!((p[0]["title"].as_str(), p[0]["description"].as_str()), (Some("Daily streaks"), Some("No daily streaks yet.")));
}

#[tokio::test]
async fn profile_flows() {
    let e = env().await;
    let env_ = FakeEnv { wins: 2, ..Default::default() };
    let out = FakeOut::new(1);
    // both options: refused before the defer
    let err = profile(&e.store, &env_, &out, GUILD, 1000, &me(1), Some(me(2)), Some("h2")).await.unwrap_err();
    assert_eq!(bot_error(&err), "Give a member or a handle, not both.");
    assert_eq!(out.calls(), []);
    // unknown handle and a member who left
    let err = profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, Some("a_b")).await.unwrap_err();
    assert_eq!(bot_error(&err), "No member is linked to a\\_b.");
    let err = profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, Some("h2")).await.unwrap_err();
    assert_eq!(bot_error(&err), "h2 is linked to a member who left this server.");
    out.calls();
    // Codeforces answers
    *env_.user.lock().unwrap() = Some(Ok(User { handle: "h1".into(), rating: Some(1500), max_rating: None, rank: Some("specialist".into()) }));
    profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, None).await.unwrap();
    let Call::Embed(embed) = &out.calls()[1] else { panic!("embed") };
    let text = embed["description"].as_str().unwrap();
    assert!(text.contains("Codeforces: [h1](https://codeforces.com/profile/h1) 1500 (specialist)"), "{text}");
    assert!(text.contains("Contest wins: 2") && text.contains("unranked"), "{text}");
    // Codeforces does not answer, then no linked handle
    *env_.user.lock().unwrap() = Some(Err(BotError::Codeforces("down".into()).into()));
    profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, None).await.unwrap();
    let Call::Embed(embed) = &out.calls()[1] else { panic!("embed") };
    assert!(embed["description"].as_str().unwrap().contains("Codeforces: h1 (Codeforces did not answer)"));
    profile(&e.store, &env_, &out, GUILD, 1000, &me(9), None, None).await.unwrap();
    let Call::Embed(embed) = &out.calls()[1] else { panic!("embed") };
    assert!(embed["description"].as_str().unwrap().contains("Codeforces: no linked handle"));
    // an unknown handle error propagates
    *env_.user.lock().unwrap() = Some(Err(BotError::handle_not_found().into()));
    let err = profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, None).await.unwrap_err();
    assert_eq!(bot_error(&err), "No Codeforces user matches that handle.");
}

#[tokio::test]
async fn profile_by_handle_fetches_the_member() {
    let e = env().await;
    let mut out = FakeOut::new(1);
    out.members.insert(2, me(2));
    *FakeEnv::default().user.lock().unwrap() = None;
    let env_ = FakeEnv::default();
    *env_.user.lock().unwrap() = Some(Ok(User { handle: "h2".into(), rating: None, max_rating: None, rank: None }));
    profile(&e.store, &env_, &out, GUILD, 1000, &me(1), None, Some("h2")).await.unwrap();
    let Call::Embed(embed) = &out.calls()[1] else { panic!("embed") };
    assert_eq!(embed["title"], "User2");
    assert!(embed["description"].as_str().unwrap().contains("[h2](https://codeforces.com/profile/h2) unrated"));
}

#[tokio::test]
async fn history_empty_and_other_member_texts() {
    let e = env().await;
    let env_ = FakeEnv::default();
    let out = FakeOut::new(1);
    for (kind, none) in [(Kind::All, "matches or mashups"), (Kind::Matches, "matches"), (Kind::Contests, "contests")] {
        let err = history(&e.store, &env_, &out, GUILD, &me(1), None, kind).await.unwrap_err();
        assert_eq!(bot_error(&err), format!("You have no finished {none} yet."));
        let err = history(&e.store, &env_, &out, GUILD, &me(1), Some(&me(1)), kind).await.unwrap_err();
        assert_eq!(bot_error(&err), format!("You have no finished {none} yet."));
    }
    out.calls();
    history(&e.store, &env_, &out, GUILD, &me(1), Some(&me(2)), Kind::All).await.unwrap();
    let Call::Pages(p) = &out.calls()[1] else { panic!("pages") };
    assert_eq!(p.len(), 2);
    assert_eq!(p[0]["title"], "You vs User2");
    assert_eq!(p[0]["description"], "No matches against each other yet.");
    assert_eq!((p[1]["title"].as_str(), p[1]["description"].as_str()), (Some("Match history - User2"), Some("No finished matches or mashups yet.")));
    assert_eq!((p[0]["footer"]["text"].as_str(), p[1]["footer"]["text"].as_str()), (Some("Page 1/2"), Some("Page 2/2")));
    history(&e.store, &env_, &out, GUILD, &me(1), Some(&me(2)), Kind::Contests).await.unwrap();
    let Call::Pages(p) = &out.calls()[1] else { panic!("pages") };
    assert_eq!(p.len(), 1); // no head to head for contests only
}

#[tokio::test]
async fn history_lists_a_finished_match() {
    let e = env().await;
    let id = e.svc.create(CreateMatch { guild_id: GUILD, channel_id: 5, host_id: 1, ..duel_spec() }).await.unwrap().r#match.id;
    e.svc.join(id, 2).await.unwrap();
    e.svc.start(id, 1).await.unwrap();
    e.svc.give_up(id, 1).await.unwrap();
    let out = FakeOut::new(1);
    history(&e.store, &FakeEnv::default(), &out, GUILD, &me(1), None, Kind::All).await.unwrap();
    let Call::Pages(p) = &out.calls()[1] else { panic!("pages") };
    let text = p[0]["description"].as_str().unwrap();
    assert!(text.contains("Duel [1700A](https://codeforces.com/problemset/problem/1700/A) (800)"), "{text}");
    assert_eq!(p[0]["footer"]["text"], "Page 1/1");
    // head to head: the other player's history uses their own id
    let out = FakeOut::new(1);
    history(&e.store, &FakeEnv::default(), &out, GUILD, &me(1), Some(&me(2)), Kind::Matches).await.unwrap();
    let Call::Pages(p) = &out.calls()[1] else { panic!("pages") };
    assert!(p[0]["description"].as_str().unwrap().starts_with("**Wins 0, losses 1, ties 0** (1 matches)"));
}

#[test]
fn tag_autocomplete_uses_the_problem_tags() {
    assert_eq!(tag_autocomplete(&FakeEnv::default(), "g"), ["graphs", "greedy"]);
    assert_eq!(tag_autocomplete(&FakeEnv::default(), "dp, g"), ["dp,graphs", "dp,greedy"]);
}

#[test]
fn custom_id_takes_unicode_digits_like_python() {
    use lightningccp::cogs::matches::parse_custom_id;
    assert_eq!(parse_custom_id("match:join:٣٤").map(|(_, id)| id), Some(34));
    assert_eq!(parse_custom_id("match:check:１２").map(|(_, id)| id), Some(12));
    assert!(parse_custom_id("match:join:1a").is_none());
    assert!(parse_custom_id("match:join:").is_none());
}
