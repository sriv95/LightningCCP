//! Port of `tests/test_contests_reminders.py` (17 tests; `test_list_raises_cache_not_ready_when_empty` is in
//! `cogs_contests_cmds.rs`, the other 16 are here) and of the channel-config parts of `tests/test_cogs.py`
//! (`test_contests_groups`), over a real temp-file `Store`, a fake Codeforces/contest source and a fake channel
//! sink, with the clock passed in. Extra tests pin the mark-before-send order, the loop intervals and the
//! `/channel` command bodies of src/cogs/contests.py lines 260-564.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightningccp::cf::models::{Contest, Standing};
use lightningccp::cogs::contests::{
    self, ChannelArg, Channels, ContestData, LoopState, Post, ReminderArgs, ReplyOutput, remind_tick,
};
use lightningccp::errors::BotError;
use lightningccp::store::base::{Reminder, Store};
use lightningccp::store::matches::NewMatch;

const NOW: i64 = 10_000;
const START: i64 = NOW + 3600;
/// NOW's Sunday 00:00 UTC (Python `WEEK`); the epoch day of NOW is 0, a Thursday.
const WEEK: i64 = -4 * 86400;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("lccp-loops-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn contest(id: i64, name: &str, phase: &str, start: i64) -> Contest {
    Contest { id, name: name.into(), kind: "CF".into(), phase: phase.into(), start_time: Some(start), duration: 7200 }
}

fn round(start: i64) -> Contest {
    contest(7, "Round", "BEFORE", start)
}

fn finished(id: i64, ended_ago: i64) -> Contest {
    contest(id, "Round", "FINISHED", NOW - ended_ago - 7200)
}

/// The contest list, the linked members and the standings fetch.
#[derive(Default)]
struct Data {
    contests: Vec<Contest>,
    linked: Vec<(i64, String)>,
    rows: Vec<Standing>,
    fail: bool,
    fetches: AtomicUsize,
}

impl ContestData for Data {
    fn contests(&self) -> Arc<Vec<Contest>> {
        Arc::new(self.contests.clone())
    }
    async fn reminder(&self, _g: i64) -> anyhow::Result<Option<Reminder>> {
        Ok(None)
    }
    async fn add_contest_remind(&self, _g: i64, _c: i64, _u: i64) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn linked_handles(&self, _g: i64) -> anyhow::Result<Vec<(i64, String)>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Ok(self.linked.clone())
    }
    async fn standings(&self, _c: i64, _h: &[String]) -> anyhow::Result<Vec<Standing>> {
        anyhow::ensure!(!self.fail, "cf down");
        Ok(self.rows.clone())
    }
}

/// What the store held at the moment of a send (to check marks come first).
#[derive(Debug, Clone)]
struct Seen {
    sent: HashSet<(i64, i64)>,
    recapped: HashSet<i64>,
    last_sent: Option<i64>,
    posted: HashSet<i64>,
}

struct Chans {
    known: Vec<i64>,
    db: Arc<Store>,
    fail_sends: bool,
    sent: Mutex<Vec<(i64, Post, Seen)>>,
}

impl Channels for Chans {
    fn exists(&self, channel: i64) -> bool {
        self.known.contains(&channel)
    }
    async fn send(&self, channel: i64, post: Post) -> anyhow::Result<()> {
        let seen = Seen {
            sent: self.db.sent_for_guild(1).await?,
            recapped: self.db.recapped(1).await?,
            last_sent: self.db.all_weeklies().await?.first().map(|w| w.2),
            posted: self.db.posted(1).await?,
        };
        self.sent.lock().unwrap().push((channel, post, seen));
        anyhow::ensure!(!self.fail_sends, "403 Forbidden");
        Ok(())
    }
}

impl Chans {
    fn posts(&self) -> Vec<Post> {
        self.sent.lock().unwrap().iter().map(|s| s.1.clone()).collect()
    }
    fn titles(&self) -> Vec<String> {
        self.posts().iter().map(|p| p.embed.as_ref().unwrap().title.clone().unwrap()).collect()
    }
}

struct World {
    _dir: TempDir,
    db: Arc<Store>,
    data: Data,
    chans: Chans,
    state: LoopState,
}

async fn world(contests: Vec<Contest>) -> World {
    let dir = TempDir::new();
    let db = Arc::new(Store::connect(dir.0.join("t.db")).await.unwrap());
    let chans = Chans { known: vec![5], db: db.clone(), fail_sends: false, sent: Mutex::new(vec![]) };
    World { _dir: dir, db, data: Data { contests, ..Default::default() }, chans, state: LoopState::default() }
}

impl World {
    async fn remind(&self, before: &[i64], exclude: &str) {
        self.db.set_reminder(1, Some(5), None, before, exclude).await.unwrap();
    }
    async fn reminders(&mut self, now: i64) {
        contests::send_due_reminders(&self.db, &self.data, &self.chans, &mut self.state, now).await.unwrap();
    }
    async fn recaps(&mut self, now: i64) {
        contests::send_recaps(&self.db, &self.data, &self.chans, &mut self.state, now).await.unwrap();
    }
    async fn marks(&self) -> HashSet<(i64, i64)> {
        self.db.sent_for_guild(1).await.unwrap()
    }
}

fn marks(items: &[(i64, i64)]) -> HashSet<(i64, i64)> {
    items.iter().copied().collect()
}

// ---- reminders (test_contests_reminders.py) ----

#[tokio::test]
async fn catch_up_sends_one_message_and_marks_all_offsets() {
    let mut w = world(vec![round(NOW + 300)]).await; // 5 min out: both 60 and 10 are due
    w.remind(&[60, 10], "").await;
    w.reminders(NOW).await;
    let posts = w.chans.posts();
    assert_eq!(posts.len(), 1);
    assert_eq!(w.marks().await, marks(&[(7, 60), (7, 10)]));
    // the exact message, view and mentions of Python `_send_due_reminders`
    assert_eq!(posts[0].content.as_deref(), Some("**[Round](https://codeforces.com/contest/7)** starts <t:10300:R>."));
    assert_eq!((posts[0].buttons, posts[0].pings, posts[0].embed.is_none()), (Some((7, false)), true, true));
}

#[tokio::test]
async fn exclude_regex_skips_matching_contest_names() {
    let mut w = world(vec![round(NOW + 300)]).await;
    w.remind(&[60, 10], "^round|educational").await; // IGNORECASE: matches "Round"
    w.reminders(NOW).await;
    assert!(w.chans.posts().is_empty() && w.marks().await.is_empty());
    w.remind(&[60, 10], "educational").await;
    w.reminders(NOW).await;
    assert_eq!(w.chans.posts().len(), 1);
}

#[tokio::test]
async fn members_who_asked_are_pinged_even_if_excluded() {
    let mut w = world(vec![round(NOW + 300)]).await;
    w.remind(&[60, 10], "round").await;
    for user in [11, 12] {
        w.db.add_contest_remind(1, 7, user).await.unwrap();
    }
    w.reminders(NOW).await;
    let posts = w.chans.posts();
    assert_eq!(posts.len(), 1);
    assert!(posts[0].content.as_deref().unwrap().contains("<@11> <@12>"));
}

#[tokio::test]
async fn at_most_once() {
    let mut w = world(vec![round(NOW + 300)]).await;
    w.remind(&[60, 10], "").await;
    w.reminders(NOW).await;
    w.reminders(NOW).await;
    assert_eq!(w.chans.posts().len(), 1);
}

#[tokio::test]
async fn nothing_sent_at_or_after_start() {
    let mut w = world(vec![round(NOW)]).await;
    w.remind(&[60, 10], "").await;
    w.reminders(NOW).await;
    assert!(w.chans.posts().is_empty() && w.marks().await.is_empty());
}

#[tokio::test]
async fn not_yet_due_offset_not_marked() {
    let mut w = world(vec![round(NOW + 1800)]).await;
    w.remind(&[120, 60, 10], "").await;
    w.reminders(NOW).await;
    assert_eq!(w.chans.posts().len(), 1);
    assert_eq!(w.marks().await, marks(&[(7, 120), (7, 60)])); // 10 not due yet
}

#[tokio::test]
async fn missing_channel_does_not_raise_or_mark_and_warns_once() {
    let mut w = world(vec![round(START)]).await;
    w.chans.known.clear();
    w.remind(&[60, 10], "").await;
    w.reminders(NOW).await;
    w.reminders(NOW).await;
    assert!(w.marks().await.is_empty() && w.chans.posts().is_empty());
    assert_eq!(w.state.warned_channels, HashSet::from([(1, 5)])); // one warning per (guild, channel)
}

// ---- mark order and send failures (PARITY: at most once) ----

#[tokio::test]
async fn reminder_marks_are_stored_before_the_send_and_survive_a_failed_send() {
    let mut w = world(vec![round(NOW + 300)]).await;
    w.chans.fail_sends = true; // Python: discord.HTTPException is logged, the marks stay
    w.remind(&[60, 10], "").await;
    w.reminders(NOW).await;
    let seen = w.chans.sent.lock().unwrap()[0].2.clone();
    assert_eq!(seen.sent, marks(&[(7, 60), (7, 10)])); // both offsets marked when the message goes out
    w.reminders(NOW).await;
    assert_eq!(w.chans.posts().len(), 1); // not retried
}

#[tokio::test]
async fn an_offset_marked_elsewhere_is_not_due_again() {
    let mut w = world(vec![round(NOW + 300)]).await;
    w.remind(&[60, 10], "").await;
    w.db.mark_sent(1, 7, 10).await.unwrap(); // another process marked the smallest
    w.reminders(NOW).await;
    // reminders_due leaves only offset 60; the smallest *due* offset is 60 and it posts
    assert_eq!(w.chans.posts().len(), 1);
    assert_eq!(w.marks().await, marks(&[(7, 60), (7, 10)]));
}

#[tokio::test]
async fn reminder_text_pings_role_then_at_most_fifty_members_and_escapes_the_name() {
    let mut w = world(vec![contest(7, "A_b [x]", "BEFORE", NOW + 300)]).await;
    w.db.set_reminder(1, Some(5), Some(9), &[60, 10], "").await.unwrap();
    for user in 100..160 {
        w.db.add_contest_remind(1, 7, user).await.unwrap();
    }
    w.reminders(NOW).await;
    let text = w.chans.posts()[0].content.clone().unwrap();
    let members: String = (100..150).map(|u| format!("<@{u}> ")).collect();
    assert_eq!(text, format!("<@&9> {members}**[A\\_b \\[x\\]](https://codeforces.com/contest/7)** starts <t:10300:R>."));
}

#[tokio::test]
async fn an_invalid_stored_pattern_fails_the_reminder_pass_but_not_the_other_passes() {
    let mut w = world(vec![round(NOW + 300), finished(8, 600)]).await;
    w.remind(&[60, 10], "(").await; // not valid: Python re.compile raises inside the loop body
    w.db.set_recap(1, 5).await.unwrap();
    w.data.linked = vec![(10, "a".into())];
    w.data.rows = vec![Standing { rank: 1, handles: vec!["a".into()], points: 3.0, penalty: 0 }];
    assert!(contests::send_due_reminders(&w.db, &w.data, &w.chans, &mut w.state, NOW).await.is_err());
    remind_tick(&w.db, &w.data, &w.chans, &mut w.state, NOW).await; // logs the failure, then recaps
    assert_eq!(w.chans.titles(), ["Contest recap - Round"]);
}

// ---- recaps ----

fn recap_world_data(w: &mut World) {
    w.data.linked = vec![(10, "a".into()), (11, "b".into())];
    let row = |rank, h: &str, points, penalty| Standing { rank, handles: vec![h.into()], points, penalty };
    w.data.rows = vec![row(1, "a", 3.0, 0), row(5, "b", 1.0, 40)];
}

async fn recap_world(contests: Vec<Contest>) -> World {
    let w = world(contests).await;
    w.db.set_recap(1, 5).await.unwrap();
    w
}

#[tokio::test]
async fn recap_posts_linked_members_once() {
    let mut w = recap_world(vec![finished(7, 600)]).await;
    recap_world_data(&mut w);
    w.recaps(NOW).await;
    w.recaps(NOW).await;
    assert_eq!(w.chans.posts().len(), 1);
    assert_eq!(w.db.recapped(1).await.unwrap(), HashSet::from([7]));
    let (_, post, seen) = w.chans.sent.lock().unwrap()[0].clone();
    let e = post.embed.unwrap();
    assert_eq!(e.title.as_deref(), Some("Contest recap - Round"));
    assert_eq!(e.footer.as_deref(), Some("2 linked members took part"));
    assert_eq!(e.description.unwrap().lines().next(), Some("1. <@10> (a) 3"));
    assert!(post.content.is_none() && post.buttons.is_none() && !post.pings);
    assert_eq!(seen.recapped, HashSet::from([7])); // marked before the post
}

#[tokio::test]
async fn recap_skips_empty_and_marks_it() {
    let mut w = recap_world(vec![finished(7, 600)]).await; // nobody linked
    w.recaps(NOW).await;
    w.recaps(NOW).await;
    assert!(w.chans.posts().is_empty());
    assert_eq!(w.db.recapped(1).await.unwrap(), HashSet::from([7]));
    assert_eq!(w.data.fetches.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn recap_failure_is_retried_with_a_growing_back_off_not_marked() {
    let mut w = recap_world(vec![finished(7, 600)]).await;
    recap_world_data(&mut w);
    w.data.fail = true;
    let fetches = |w: &World| w.data.fetches.load(Ordering::SeqCst);
    w.recaps(NOW).await;
    w.recaps(NOW).await; // the same minute: still backing off
    assert!(w.db.recapped(1).await.unwrap().is_empty());
    assert_eq!(fetches(&w), 1);
    assert_eq!(w.state.recap_retry[&(1, 7)], (NOW + 60, 60)); // 1 min
    w.recaps(NOW + 60).await;
    assert_eq!(fetches(&w), 2);
    assert_eq!(w.state.recap_retry[&(1, 7)], (NOW + 60 + 120, 120)); // 2 min
    w.recaps(NOW + 120).await; // the delay is 2 minutes now
    assert_eq!(fetches(&w), 2);
    w.recaps(NOW + 180).await;
    assert_eq!(fetches(&w), 3);
    let mut clock = NOW + 180;
    for _ in 0..10 {
        // many failures: the wait stops growing at 30 minutes
        clock += 1800;
        w.recaps(clock).await;
    }
    assert_eq!(w.state.recap_retry[&(1, 7)].1, 1800);
    assert!(w.chans.posts().is_empty());
}

#[tokio::test]
async fn recap_back_off_ends_when_it_works() {
    let mut w = recap_world(vec![finished(7, 600)]).await;
    recap_world_data(&mut w);
    w.state.recap_retry.insert((1, 7), (NOW - 1, 120));
    w.recaps(NOW).await;
    assert_eq!(w.chans.posts().len(), 1);
    assert!(w.state.recap_retry.is_empty());
}

#[tokio::test]
async fn recap_ignores_old_running_and_upcoming_contests() {
    let running = contest(9, "R", "CODING", NOW - 100);
    let mut w = recap_world(vec![finished(8, 2 * 86400), running, round(NOW + 100)]).await;
    recap_world_data(&mut w);
    w.recaps(NOW).await;
    assert_eq!(w.data.fetches.load(Ordering::SeqCst), 0);
    assert!(w.chans.posts().is_empty());
}

// ---- weekly leaderboard ----

async fn seed_match(db: &Store, user: i64, handle: &str, at: i64, delta: i64) {
    let new = NewMatch { guild_id: 1, channel_id: 1, host_id: user, host_handle: handle.into(), mode: "solo".into(), max_players: 1, now: 1, ..Default::default() };
    let id = db.create_match(new).await.unwrap();
    db.start_match(id, 800, "1700A", 1, at + 99, &[user]).await.unwrap();
    assert!(db.finish_match(id, at, &[(user, delta)], None).await.unwrap());
}

#[tokio::test]
async fn weekly_posts_once_per_week() {
    let w = world(vec![]).await;
    w.db.set_weekly(1, 5, WEEK - 604800).await.unwrap();
    seed_match(&w.db, 10, "a", WEEK - 100, 5).await;
    contests::send_weekly(&w.db, &w.chans, NOW).await.unwrap();
    contests::send_weekly(&w.db, &w.chans, NOW).await.unwrap();
    assert_eq!(w.chans.posts().len(), 1);
    assert_eq!(w.db.all_weeklies().await.unwrap(), [(1, 5, WEEK)]);
    let seen = w.chans.sent.lock().unwrap()[0].2.clone();
    assert_eq!(seen.last_sent, Some(WEEK)); // marked before the post
    let e = w.chans.posts()[0].embed.clone().unwrap();
    assert_eq!(e.title.as_deref(), Some("Weekly leaderboard"));
    assert!(e.description.unwrap().contains("<@10> (a) 805 (+5) new"));
}

#[tokio::test]
async fn weekly_skips_a_week_already_sent_and_an_empty_board() {
    let w = world(vec![]).await;
    w.db.set_weekly(1, 5, WEEK).await.unwrap();
    seed_match(&w.db, 10, "a", WEEK - 100, 5).await;
    contests::send_weekly(&w.db, &w.chans, NOW).await.unwrap();
    assert!(w.chans.posts().is_empty());
    let w = world(vec![]).await;
    w.db.set_weekly(1, 5, WEEK - 604800).await.unwrap();
    contests::send_weekly(&w.db, &w.chans, NOW).await.unwrap();
    assert!(w.chans.posts().is_empty());
    assert_eq!(w.db.all_weeklies().await.unwrap(), [(1, 5, WEEK)]); // marked without a post
}

// ---- new contests ----

async fn post_world(start: i64) -> World {
    let w = world(vec![round(start)]).await;
    w.db.set_newcontest(1, 5, &[]).await.unwrap();
    w
}

#[tokio::test]
async fn new_contest_posts_when_seen_and_again_within_48_hours() {
    let w = post_world(NOW + 5 * 86400).await;
    for _ in 0..2 {
        contests::send_new_contests(&w.db, &w.data, &w.chans, NOW).await.unwrap();
    }
    assert_eq!(w.chans.titles(), ["New contest"]);
    for _ in 0..2 {
        contests::send_new_contests(&w.db, &w.data, &w.chans, NOW + 3 * 86400 + 60).await.unwrap();
    }
    assert_eq!(w.chans.titles(), ["New contest", "Starts within 48 hours"]);
    assert!(w.chans.posts().iter().all(|p| p.buttons == Some((7, true)) && p.content.is_none() && !p.pings));
    // the first post is marked before it goes out
    assert_eq!(w.chans.sent.lock().unwrap()[0].2.posted, HashSet::from([7]));
}

#[tokio::test]
async fn contest_first_seen_within_48_hours_is_posted_once() {
    let w = post_world(NOW + 3600).await;
    for _ in 0..2 {
        contests::send_new_contests(&w.db, &w.data, &w.chans, NOW).await.unwrap();
    }
    assert_eq!(w.chans.titles(), ["New contest"]);
}

#[tokio::test]
async fn contests_known_when_the_channel_was_set_are_not_new() {
    let w = world(vec![round(NOW + 5 * 86400), contest(8, "Later", "BEFORE", NOW + 6 * 86400)]).await;
    let out = Out::default();
    let ok = ChannelArg { id: 5, can_send: true };
    contests::new_contest_config(&w.db, &w.data, &out, 1, Some(ok), false).await.unwrap();
    assert_eq!(out.last(), "New contests are posted in <#5>, checked every 30 minutes.");
    contests::send_new_contests(&w.db, &w.data, &w.chans, NOW).await.unwrap();
    assert!(w.chans.posts().is_empty());
}

// ---- intervals ----

#[test]
fn loop_constants_match_python() {
    assert_eq!(contests::NEW_CONTEST_INTERVAL, Duration::from_secs(30 * 60)); // tasks.loop(minutes=30)
    assert_eq!(contests::REMIND_INTERVAL, Duration::from_secs(60)); // tasks.loop(seconds=60)
    assert_eq!((contests::RECAP_MAX_BACKOFF, contests::MAX_PINGED_MEMBERS), (1800, 50));
}

// ---- registration (test_cogs.py::test_contests_groups) ----

#[test]
fn channel_group_has_the_four_admin_commands() {
    let c = contests::channel();
    assert_eq!(
        (&*c.name, c.description.as_deref(), c.guild_only),
        ("channel", Some("Channels for contest reminders, recaps and the weekly leaderboard"), true)
    );
    let subs: Vec<_> = c.subcommands.iter().map(|s| (s.name.to_string(), s.description.clone().unwrap().into_owned())).collect();
    assert_eq!(
        subs,
        [
            ("reminder", "Where to remind about upcoming contests; no options shows it"),
            ("recap", "Where to post how linked members placed after a contest; no options shows it"),
            ("leaderboard", "Where to post the leaderboard every Sunday 00:00 UTC; no options shows it"),
            ("new-contest", "Where to announce new contests with an Add to calendar button; no options shows it"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    for s in &c.subcommands {
        assert_eq!(s.required_permissions, poise::serenity_prelude::Permissions::MANAGE_GUILD, "{}", s.name);
        assert!(s.guild_only && s.parameters.iter().all(|p| !p.required), "{}", s.name);
    }
    let params = |i: usize| {
        c.subcommands[i].parameters.iter().map(|p| (p.name.to_string(), p.description.clone().unwrap().into_owned())).collect::<Vec<_>>()
    };
    let want = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
    assert_eq!(
        params(0),
        want(&[
            ("set", "Channel to post reminders in"),
            ("role", "Role to ping"),
            ("before", "Minutes before the start, comma-separated (default 10,60)"),
            ("exclude", "Regex on the contest name: matching contests get no reminder; clear removes it"),
            ("clear", "Stop reminders (forgets the channel only)"),
            ("clear_role", "Stop pinging the role"),
        ])
    );
    assert_eq!(params(1), want(&[("set", "Channel to post recaps in"), ("clear", "Stop recaps")]));
    assert_eq!(params(2), want(&[("set", "Channel to post the weekly leaderboard in"), ("clear", "Stop it")]));
    assert_eq!(params(3), want(&[("set", "Channel to announce new contests in"), ("clear", "Stop announcements")]));
}

// ---- /channel command bodies ----

#[derive(Default)]
struct Out {
    texts: Mutex<Vec<String>>,
}

impl Out {
    fn last(&self) -> String {
        self.texts.lock().unwrap().last().cloned().expect("a reply")
    }
}

impl ReplyOutput for Out {
    async fn reply(&self, text: &str) -> anyhow::Result<()> {
        self.texts.lock().unwrap().push(text.into());
        Ok(())
    }
    async fn reply_file(&self, text: &str, _f: &str, _b: Vec<u8>) -> anyhow::Result<()> {
        self.reply(text).await
    }
}

fn msg(e: anyhow::Error) -> String {
    e.downcast_ref::<BotError>().expect("a BotError").to_string()
}

fn chan(id: i64) -> Option<ChannelArg> {
    Some(ChannelArg { id, can_send: true })
}

#[tokio::test]
async fn reminder_config_shows_sets_merges_and_clears() {
    let w = world(vec![]).await;
    let out = Out::default();
    let go = |a: ReminderArgs| contests::reminder_config(&w.db, &out, 1, a);
    go(ReminderArgs::default()).await.unwrap();
    assert_eq!(out.last(), "Reminders: no channel (use set), 60, 10 minutes before start.");
    assert!(w.db.get_reminder(1).await.unwrap().is_none()); // showing stores nothing
    go(ReminderArgs { set: chan(55), role: Some(9), before: Some("5,30".into()), exclude: Some(" div\\. 2 ".into()), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(out.last(), "Reminders: <#55>, pinging <@&9>, 30, 5 minutes before start.\nExcluding names matching `div\\. 2`.");
    // only the given fields change
    go(ReminderArgs { before: Some("15".into()), ..Default::default() }).await.unwrap();
    assert_eq!(out.last(), "Reminders: <#55>, pinging <@&9>, 15 minutes before start.\nExcluding names matching `div\\. 2`.");
    go(ReminderArgs { exclude: Some("  CLEAR ".into()), clear_role: true, ..Default::default() }).await.unwrap();
    assert_eq!(out.last(), "Reminders: <#55>, 15 minutes before start.");
    let stored = w.db.get_reminder(1).await.unwrap().unwrap();
    assert_eq!((stored.channel_id, stored.role_id, stored.before_mins, stored.exclude), (Some(55), None, vec![15], String::new()));
    go(ReminderArgs { clear: true, ..Default::default() }).await.unwrap();
    assert_eq!(out.last(), "Reminders stopped.");
    assert_eq!(msg(go(ReminderArgs { clear: true, ..Default::default() }).await.unwrap_err()), "No reminder channel set.");
}

#[tokio::test]
async fn reminder_config_errors() {
    let w = world(vec![]).await;
    let out = Out::default();
    let go = |a: ReminderArgs| contests::reminder_config(&w.db, &out, 1, a);
    for a in [
        ReminderArgs { clear: true, set: chan(1), ..Default::default() },
        ReminderArgs { clear: true, role: Some(1), ..Default::default() },
        ReminderArgs { clear: true, before: Some("5".into()), ..Default::default() },
        ReminderArgs { clear: true, exclude: Some("x".into()), ..Default::default() },
        ReminderArgs { clear: true, clear_role: true, ..Default::default() },
    ] {
        assert_eq!(msg(go(a).await.unwrap_err()), "Use clear on its own.");
    }
    // Python truthiness: an empty `before` or `exclude` does not count as "another option" next to clear
    assert_eq!(
        msg(go(ReminderArgs { clear: true, before: Some(String::new()), exclude: Some(String::new()), ..Default::default() }).await.unwrap_err()),
        "No reminder channel set."
    );
    let blocked = Some(ChannelArg { id: 7, can_send: false });
    assert_eq!(msg(go(ReminderArgs { set: blocked, ..Default::default() }).await.unwrap_err()), "I cannot send messages in <#7>.");
    assert_eq!(
        msg(go(ReminderArgs { role: Some(2), clear_role: true, ..Default::default() }).await.unwrap_err()),
        "Give a role or clear_role, not both."
    );
    let too_long = ReminderArgs { exclude: Some("a".repeat(101)), ..Default::default() };
    assert_eq!(msg(go(too_long).await.unwrap_err()), "The regex can be at most 100 characters.");
    assert!(msg(go(ReminderArgs { exclude: Some("a{2,1}".into()), ..Default::default() }).await.unwrap_err()).starts_with("Invalid regex: "));
    assert!(go(ReminderArgs { before: Some("x".into()), ..Default::default() }).await.is_err());
    assert!(w.db.get_reminder(1).await.unwrap().is_none()); // no failed command wrote anything
    // an empty `exclude` with nothing else is given ("is None" in Python), so it saves the defaults
    go(ReminderArgs { exclude: Some(String::new()), ..Default::default() }).await.unwrap();
    assert_eq!(out.last(), "Reminders: no channel (use set), 60, 10 minutes before start.");
    assert!(w.db.get_reminder(1).await.unwrap().is_some());
}

#[tokio::test]
async fn recap_config_shows_sets_and_clears() {
    let w = world(vec![]).await;
    let out = Out::default();
    contests::recap_config(&w.db, &out, 1, None, false).await.unwrap();
    assert_eq!(out.last(), "No recap channel set.");
    assert_eq!(msg(contests::recap_config(&w.db, &out, 1, None, true).await.unwrap_err()), "No recap channel set.");
    assert_eq!(msg(contests::recap_config(&w.db, &out, 1, chan(3), true).await.unwrap_err()), "Use clear on its own.");
    let blocked = Some(ChannelArg { id: 3, can_send: false });
    assert_eq!(msg(contests::recap_config(&w.db, &out, 1, blocked, false).await.unwrap_err()), "I cannot send messages in <#3>.");
    contests::recap_config(&w.db, &out, 1, chan(3), false).await.unwrap();
    assert_eq!(out.last(), "Recaps go to <#3>.");
    contests::recap_config(&w.db, &out, 1, None, false).await.unwrap();
    assert_eq!(out.last(), "Recaps go to <#3>.");
    contests::recap_config(&w.db, &out, 1, None, true).await.unwrap();
    assert_eq!(out.last(), "Recaps stopped.");
    assert_eq!(w.db.get_recap(1).await.unwrap(), None);
}

#[tokio::test]
async fn leaderboard_config_shows_sets_the_week_and_clears() {
    let w = world(vec![]).await;
    let out = Out::default();
    contests::leaderboard_config(&w.db, &out, 1, None, false, NOW).await.unwrap();
    assert_eq!(out.last(), "No weekly leaderboard channel set.");
    assert_eq!(msg(contests::leaderboard_config(&w.db, &out, 1, None, true, NOW).await.unwrap_err()), "No weekly leaderboard channel set.");
    assert_eq!(msg(contests::leaderboard_config(&w.db, &out, 1, chan(3), true, NOW).await.unwrap_err()), "Use clear on its own.");
    let blocked = Some(ChannelArg { id: 3, can_send: false });
    assert_eq!(msg(contests::leaderboard_config(&w.db, &out, 1, blocked, false, NOW).await.unwrap_err()), "I cannot send messages in <#3>.");
    contests::leaderboard_config(&w.db, &out, 1, chan(3), false, NOW).await.unwrap();
    assert_eq!(out.last(), "The leaderboard goes to <#3> every Sunday 00:00 UTC, starting next Sunday.");
    assert_eq!(w.db.all_weeklies().await.unwrap(), [(1, 3, WEEK)]); // this week counts as sent
    contests::leaderboard_config(&w.db, &out, 1, None, false, NOW).await.unwrap();
    assert_eq!(out.last(), "The leaderboard goes to <#3> every Sunday 00:00 UTC.");
    contests::leaderboard_config(&w.db, &out, 1, None, true, NOW).await.unwrap();
    assert_eq!(out.last(), "Weekly leaderboard stopped.");
}

#[tokio::test]
async fn new_contest_config_shows_sets_and_clears() {
    let w = world(vec![round(NOW + 100)]).await;
    let out = Out::default();
    let go = |set, clear| contests::new_contest_config(&w.db, &w.data, &out, 1, set, clear);
    go(None, false).await.unwrap();
    assert_eq!(out.last(), "No new-contest channel set.");
    assert_eq!(msg(go(None, true).await.unwrap_err()), "No new-contest channel set.");
    assert_eq!(msg(go(chan(3), true).await.unwrap_err()), "Use clear on its own.");
    assert_eq!(msg(go(Some(ChannelArg { id: 3, can_send: false }), false).await.unwrap_err()), "I cannot send messages in <#3>.");
    go(chan(3), false).await.unwrap();
    assert_eq!(out.last(), "New contests are posted in <#3>, checked every 30 minutes.");
    assert_eq!(w.db.posted(1).await.unwrap(), HashSet::from([7])); // known contests are not new
    go(None, false).await.unwrap();
    assert_eq!(out.last(), "New contests are posted in <#3>.");
    go(None, true).await.unwrap();
    assert_eq!(out.last(), "New-contest posts stopped.");
}
