//! Port of the contests parts of `tests/test_cogs.py` (`test_contests_groups`, ranklist registration) and
//! `tests/test_contests_reminders.py::test_list_raises_cache_not_ready_when_empty`, plus the command logic of
//! src/cogs/contests.py lines 1-260 over fake data and fake outputs. Expected values come from reading the
//! Python and from running it with `.venv/bin/python -I` (the .ics text).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Contest, Standing};
use lightningccp::cogs::contests::{
    self, Action, ContestData, PagesOutput, ReplyOutput, check_regex, contest_buttons, parse_custom_id,
};
use lightningccp::errors::BotError;
use lightningccp::store::base::Reminder;
use poise::serenity_prelude::CreateEmbed;

fn contest(id: i64, name: &str, phase: &str, start: Option<i64>) -> Contest {
    Contest { id, name: name.into(), kind: "CF".into(), phase: phase.into(), start_time: start, duration: 7200 }
}

type Reply = (String, Option<(String, Vec<u8>)>);

#[derive(Default)]
struct Fake {
    contests: Vec<Contest>,
    reminder: Option<Reminder>,
    reminds: Mutex<Vec<(i64, i64, i64)>>,
    linked: Vec<(i64, String)>,
    rows: Vec<Standing>,
    events: Mutex<Vec<String>>,
    pages: Mutex<Vec<serde_json::Value>>,
    replies: Mutex<Vec<Reply>>,
}

impl ContestData for Fake {
    fn contests(&self) -> Arc<Vec<Contest>> {
        Arc::new(self.contests.clone())
    }
    async fn reminder(&self, _g: i64) -> anyhow::Result<Option<Reminder>> {
        Ok(self.reminder.clone())
    }
    async fn add_contest_remind(&self, g: i64, c: i64, u: i64) -> anyhow::Result<bool> {
        let mut r = self.reminds.lock().unwrap();
        if r.contains(&(g, c, u)) {
            return Ok(false);
        }
        r.push((g, c, u));
        Ok(true)
    }
    async fn linked_handles(&self, _g: i64) -> anyhow::Result<Vec<(i64, String)>> {
        Ok(self.linked.clone())
    }
    async fn standings(&self, _c: i64, _h: &[String]) -> anyhow::Result<Vec<Standing>> {
        Ok(self.rows.clone())
    }
}

impl ReplyOutput for Fake {
    async fn reply(&self, text: &str) -> anyhow::Result<()> {
        self.replies.lock().unwrap().push((text.into(), None));
        Ok(())
    }
    async fn reply_file(&self, text: &str, filename: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
        self.replies.lock().unwrap().push((text.into(), Some((filename.into(), bytes))));
        Ok(())
    }
}

impl PagesOutput for Fake {
    async fn defer(&self) -> anyhow::Result<()> {
        self.events.lock().unwrap().push("defer".into());
        Ok(())
    }
    async fn send_pages(&self, pages: Vec<CreateEmbed>) -> anyhow::Result<()> {
        self.events.lock().unwrap().push("pages".into());
        self.pages.lock().unwrap().extend(pages.iter().map(|p| serde_json::to_value(p).unwrap()));
        Ok(())
    }
}

fn msg(e: anyhow::Error) -> String {
    e.downcast_ref::<BotError>().expect("a BotError").to_string()
}

fn reminder(channel: Option<i64>) -> Reminder {
    Reminder { guild_id: 1, channel_id: channel, role_id: None, before_mins: vec![60, 10], exclude: String::new() }
}

// ---- registration ----

#[test]
fn contests_group_and_ranklist() {
    let c = contests::contests();
    assert_eq!((&*c.name, c.description.as_deref(), c.guild_only), ("contests", Some("List Codeforces contests"), true));
    let subs: Vec<_> = c.subcommands.iter().map(|s| (s.name.to_string(), s.description.clone().unwrap().into_owned())).collect();
    assert_eq!(
        subs,
        [
            ("upcoming".to_string(), "List future contests".to_string()),
            ("running".to_string(), "List contests running now".to_string()),
            ("recent".to_string(), "List recently finished contests".to_string()),
        ]
    );
    let r = contests::ranklist();
    assert_eq!((&*r.name, r.description.as_deref(), r.guild_only), ("ranklist", Some("Standings of linked members in a contest"), true));
    assert_eq!(r.parameters.len(), 1);
    let p = &r.parameters[0];
    assert_eq!((&*p.name, p.description.as_deref(), p.required), ("contest_id", Some("Codeforces contest id"), true));
    assert!(p.autocomplete_callback.is_none()); // Python /ranklist has no autocomplete
}

// ---- _check_regex ----

#[test]
fn check_regex_trims_limits_and_validates() {
    assert_eq!(check_regex("  div\\. 2 \u{1c}\n").unwrap(), "div\\. 2");
    assert_eq!(check_regex("").unwrap(), "");
    assert_eq!(check_regex("(?<!x)y").unwrap(), "(?<!x)y"); // lookaround accepted, like re
    assert!(check_regex(&"a".repeat(100)).is_ok());
    assert_eq!(msg(check_regex(&"a".repeat(101)).unwrap_err()), "The regex can be at most 100 characters.");
    // 100 code points of 3 bytes each: Python counts characters
    assert!(check_regex(&"\u{e01}".repeat(100)).is_ok());
    assert!(msg(check_regex("(").unwrap_err()).starts_with("Invalid regex: "));
    assert!(msg(check_regex("*a").unwrap_err()).starts_with("Invalid regex: "));
}

// ---- persistent buttons ----

#[test]
fn custom_id_parses_like_the_dynamic_item_template() {
    assert_eq!(parse_custom_id("cal:1700"), Some((Action::Cal, 1700)));
    assert_eq!(parse_custom_id("notify:7"), Some((Action::Notify, 7)));
    for bad in ["cal:", "cal", "cal:12a", "cal:-1", "cal: 1", "cal:1\n", "Cal:1", "notify:1:2", "match:cal:1", "page:next", ":1", "cal:\u{661}"] {
        assert_eq!(parse_custom_id(bad), None, "{bad:?}");
    }
    // too big for i64: saturates, no contest has that id
    assert_eq!(parse_custom_id("cal:99999999999999999999999"), Some((Action::Cal, i64::MAX)));
}

#[test]
fn buttons_match_python_labels_emoji_styles_and_ids() {
    let v = serde_json::to_value(contest_buttons(1700, true)).unwrap();
    let c = &v["components"];
    assert_eq!(c.as_array().unwrap().len(), 2);
    let get = |i: usize| (c[i]["custom_id"].as_str().unwrap(), c[i]["label"].as_str().unwrap(), c[i]["style"].as_u64().unwrap(), c[i]["emoji"]["name"].as_str().unwrap());
    assert_eq!(get(0), ("cal:1700", "Add to calendar", 1, "\u{1F5D3}"));
    assert_eq!(get(1), ("notify:1700", "Notify me", 2, "\u{1F514}"));
    let only = serde_json::to_value(contest_buttons(5, false)).unwrap();
    assert_eq!(only["components"].as_array().unwrap().len(), 1);
    assert_eq!(only["components"][0]["custom_id"], "cal:5");
}

const ICS_1700: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//LightningCCP//Contests//EN\r\nBEGIN:VEVENT\r\nUID:cf-contest-1700@lightningccp\r\nDTSTAMP:20230722T042640Z\r\nDTSTART:20231114T221320Z\r\nDTEND:20231115T001320Z\r\nSUMMARY:Round 800\\, (Div. 2)\\; a\\\\b\r\nURL:https://codeforces.com/contest/1700\r\nDESCRIPTION:https://codeforces.com/contest/1700\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Round 800\\, (Div. 2)\\; a\\\\b starts in 10 minutes\r\nTRIGGER:-PT10M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

#[tokio::test]
async fn calendar_sends_the_ics_file_privately() {
    let f = Fake { contests: vec![contest(1700, "Round 800, (Div. 2); a\\b", "BEFORE", Some(1_700_000_000))], ..Default::default() };
    contests::calendar(&f, &f, 1700, 1_690_000_000).await.unwrap();
    let r = f.replies.lock().unwrap();
    let (text, file) = &r[0];
    let (name, bytes) = file.as_ref().unwrap();
    assert_eq!(text, "Open the file to add the contest to your calendar.");
    assert_eq!(name, "contest-1700.ics");
    assert_eq!(std::str::from_utf8(bytes).unwrap(), ICS_1700);
}

#[tokio::test]
async fn calendar_and_notify_need_an_upcoming_contest() {
    let gone = "That contest is not upcoming any more.";
    let f = Fake {
        contests: vec![contest(1, "A", "FINISHED", Some(10)), contest(2, "B", "BEFORE", None), contest(3, "C", "CODING", Some(10))],
        reminder: Some(reminder(Some(5))),
        ..Default::default()
    };
    for id in [1, 2, 3, 99] {
        assert_eq!(msg(contests::calendar(&f, &f, id, 0).await.unwrap_err()), gone);
        assert_eq!(msg(contests::notify(&f, &f, id, Some(1), 9).await.unwrap_err()), gone);
    }
    assert!(f.replies.lock().unwrap().is_empty() && f.reminds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn notify_needs_a_reminder_channel_then_adds_once() {
    let no_channel = "This server has no reminder channel. An admin can set one with /channel reminder.";
    let up = vec![contest(7, "Round", "BEFORE", Some(1000))];
    for reminder in [None, Some(reminder(None))] {
        let f = Fake { contests: up.clone(), reminder, ..Default::default() };
        assert_eq!(msg(contests::notify(&f, &f, 7, Some(1), 9).await.unwrap_err()), no_channel);
        assert!(f.reminds.lock().unwrap().is_empty());
    }
    let f = Fake { contests: up.clone(), reminder: Some(reminder(Some(55))), ..Default::default() };
    assert_eq!(msg(contests::notify(&f, &f, 7, None, 9).await.unwrap_err()), no_channel); // no guild
    contests::notify(&f, &f, 7, Some(1), 9).await.unwrap();
    contests::notify(&f, &f, 7, Some(1), 9).await.unwrap();
    let r = f.replies.lock().unwrap();
    assert_eq!(r[0].0, "You will be pinged in <#55> when the reminder is posted.");
    assert_eq!(r[1].0, "You are already on the list.");
    assert!(r.iter().all(|x| x.1.is_none()));
    assert_eq!(*f.reminds.lock().unwrap(), [(1, 7, 9)]);
}

// ---- /contests ----

#[tokio::test]
async fn list_raises_cache_not_ready_when_empty() {
    let f = Fake::default();
    let e = contests::list(&f, &f, "upcoming").await.unwrap_err();
    assert_eq!(e.downcast_ref::<BotError>(), Some(&BotError::CacheNotReady));
    assert!(f.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn list_errors_before_the_defer_and_pages_after_it() {
    let f = Fake { contests: vec![contest(1, "A", "FINISHED", Some(10))], ..Default::default() };
    for kind in ["upcoming", "running"] {
        assert_eq!(msg(contests::list(&f, &f, kind).await.unwrap_err()), format!("No {kind} contests."));
    }
    assert!(f.events.lock().unwrap().is_empty()); // no defer: the error is the first response
    contests::list(&f, &f, "recent").await.unwrap();
    assert_eq!(*f.events.lock().unwrap(), ["defer", "pages"]);
    let p = f.pages.lock().unwrap();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0]["title"], "Recently finished contests");
    assert_eq!(p[0]["footer"]["text"], "Page 1/1");
}

#[tokio::test]
async fn list_titles_selection_and_paging() {
    let cs: Vec<_> = (0..23).map(|i| contest(i, &format!("C{i}"), "BEFORE", Some(1000 + 23 - i))).collect();
    let mut all = cs.clone();
    all.push(contest(100, "Live", "SYSTEM_TEST", Some(5)));
    let f = Fake { contests: all, ..Default::default() };
    contests::list(&f, &f, "upcoming").await.unwrap();
    {
        let p = f.pages.lock().unwrap();
        assert_eq!(p.iter().map(|x| x["footer"]["text"].as_str().unwrap()).collect::<Vec<_>>(), ["Page 1/3", "Page 2/3", "Page 3/3"]);
        assert_eq!(p[0]["title"], "Upcoming contests");
        // soonest first: C22 starts at 1001
        assert!(p[0]["description"].as_str().unwrap().starts_with("[C22](https://codeforces.com/contest/22)"));
    }
    f.pages.lock().unwrap().clear();
    contests::list(&f, &f, "running").await.unwrap();
    let p = f.pages.lock().unwrap();
    assert_eq!((p.len(), p[0]["title"].as_str()), (1, Some("Running contests")));
}

// ---- /ranklist ----

fn row(rank: i64, handles: &[&str], points: f64, penalty: i64) -> Standing {
    Standing { rank, handles: handles.iter().map(|h| h.to_string()).collect(), points, penalty }
}

fn ranked() -> Fake {
    Fake {
        contests: vec![contest(5, "Round *5*", "FINISHED", Some(10)), contest(6, "Soon", "BEFORE", Some(99))],
        linked: vec![(1, "Alice".into()), (2, "bob_x".into())],
        // chunked calls arrive out of rank order; a team row counts for its first linked member
        rows: vec![row(3, &["Zed", "BOB_X"], 80.0, 45), row(1, &["alice"], 100.0, 0)],
        ..Default::default()
    }
}

#[tokio::test]
async fn ranklist_sorts_entries_and_titles_with_the_contest_name() {
    let f = ranked();
    contests::show_ranklist(&f, &f, 1, 5).await.unwrap();
    assert_eq!(*f.events.lock().unwrap(), ["defer", "pages"]);
    {
        let p = f.pages.lock().unwrap();
        assert_eq!(p[0]["title"], "Ranklist - Round *5*"); // the title is not escaped, like Python
        assert_eq!(p[0]["description"], "1. <@1> (alice) 100\n3. <@2> (BOB\\_X) 80, penalty 45");
    }
    let e = contests::entries(&f, 1, 5).await.unwrap();
    assert_eq!(e, [(1, 1, "alice".to_string(), 100.0, 0), (3, 2, "BOB_X".to_string(), 80.0, 45)]);
}

#[tokio::test]
async fn ranklist_unknown_contest_title_and_errors_after_the_defer() {
    let f = ranked();
    contests::show_ranklist(&f, &f, 1, 77).await.unwrap();
    assert_eq!(f.pages.lock().unwrap()[0]["title"], "Ranklist - contest 77");

    let f = ranked();
    let e = contests::show_ranklist(&f, &f, 1, 6).await.unwrap_err();
    assert_eq!(msg(e), "That contest has not started yet.");
    assert_eq!(*f.events.lock().unwrap(), ["defer"]); // Python defers first

    let f = Fake { rows: vec![row(1, &["stranger"], 1.0, 0)], ..ranked() };
    assert_eq!(msg(contests::show_ranklist(&f, &f, 1, 5).await.unwrap_err()), "No linked member took part in that contest.");
    let f = Fake { linked: vec![], ..ranked() };
    assert_eq!(msg(contests::show_ranklist(&f, &f, 1, 5).await.unwrap_err()), "No linked member took part in that contest.");
}

#[tokio::test]
async fn ranklist_pages_like_python_test() {
    let mut f = Fake { contests: vec![contest(5, "R", "FINISHED", Some(1))], ..Default::default() };
    let owners: HashMap<i64, String> = (1..=12).map(|i| (i, format!("h{i}"))).collect();
    f.linked = (1..=12).map(|i| (i, owners[&i].clone())).collect();
    f.rows = (1..=12).rev().map(|i| row(i, &[owners[&i].as_str()], 100.0 - i as f64, if i == 2 { 45 } else { 0 })).collect();
    f.rows[10].points = 90.0; // rank 2
    contests::show_ranklist(&f, &f, 1, 5).await.unwrap();
    let p = f.pages.lock().unwrap();
    assert_eq!(p.iter().map(|x| x["footer"]["text"].as_str().unwrap()).collect::<Vec<_>>(), ["Page 1/2", "Page 2/2"]);
    let lines: Vec<_> = p[0]["description"].as_str().unwrap().lines().collect();
    assert_eq!((lines[0], lines[1]), ("1. <@1> (h1) 99", "2. <@2> (h2) 90, penalty 45"));
}
