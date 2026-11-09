//! Port of the handles parts of `tests/test_cogs.py` and the VerifyButton parts of `tests/test_buttons.py`, plus the
//! command logic of `src/cogs/handles.py` over a fake Codeforces, the real store and a fake output.
//! The persistent button's gateway side (`on_component`) needs a live serenity context; its id parsing, its owner check
//! and the `verify` body are tested here, and its error reporting by `errors.rs` (component branch).
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Problem, Submission, User};
use lightningccp::cogs::handles::{self, HandlesOutput, Reply, parse_verify_id, verify_custom_id};
use lightningccp::cogs::plot::Person;
use lightningccp::daily::{CfApi, ProblemSource};
use lightningccp::errors::{BotError, error_message};
use lightningccp::store::base::Store;
use lightningccp::ui::embeds::a::EmbedData;
use rand::SeedableRng;
use rand::rngs::StdRng;

const G: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Text(String),
    Verify(String, u64),
    Embed(EmbedData),
    Pages(Vec<EmbedData>),
    Respond(String),
}

#[derive(Default)]
struct Fake {
    users: Mutex<Vec<User>>,
    subs: Mutex<Vec<Submission>>,
    counts: Mutex<Vec<Option<i64>>>,
    members: Vec<Person>,
    sent: Mutex<Vec<Sent>>,
    problems: Mutex<Vec<Problem>>,
}

impl Fake {
    fn sent(&self) -> Vec<Sent> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }
    fn push(&self, s: Sent) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(s);
        Ok(())
    }
}

impl CfApi for Fake {
    async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        let users = self.users.lock().unwrap();
        users.iter().find(|u| u.handle.eq_ignore_ascii_case(handle)).cloned().ok_or_else(|| BotError::handle_not_found().into())
    }
    async fn users_info(&self, handles: &[String]) -> anyhow::Result<Vec<User>> {
        let users = self.users.lock().unwrap();
        Ok(users.iter().filter(|u| handles.iter().any(|h| h.eq_ignore_ascii_case(&u.handle))).cloned().collect())
    }
    async fn user_status(&self, _: &str, count: Option<i64>, _since: Option<i64>) -> anyhow::Result<Vec<Submission>> {
        self.counts.lock().unwrap().push(count);
        Ok(self.subs.lock().unwrap().clone())
    }
}

impl ProblemSource for Fake {
    fn problems(&self) -> Arc<Vec<Problem>> {
        Arc::new(self.problems.lock().unwrap().clone())
    }
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        match self.problems() {
            p if p.is_empty() => Err(BotError::CacheNotReady),
            p => Ok(p),
        }
    }
    fn tags(&self) -> Arc<HashSet<String>> {
        Arc::default()
    }
}

impl Reply for Fake {
    async fn followup_text(&self, content: &str) -> anyhow::Result<()> {
        self.push(Sent::Text(content.into()))
    }
}

impl HandlesOutput for Fake {
    async fn followup_verify(&self, content: &str, user_id: u64) -> anyhow::Result<()> {
        self.push(Sent::Verify(content.into(), user_id))
    }
    async fn followup_embed(&self, embed: EmbedData) -> anyhow::Result<()> {
        self.push(Sent::Embed(embed))
    }
    async fn followup_pages(&self, pages: Vec<EmbedData>) -> anyhow::Result<()> {
        self.push(Sent::Pages(pages))
    }
    async fn respond_text(&self, content: &str) -> anyhow::Result<()> {
        self.push(Sent::Respond(content.into()))
    }
    async fn fetch_member(&self, id: u64) -> anyhow::Result<Option<Person>> {
        Ok(self.members.iter().find(|m| m.id == id).cloned())
    }
}

struct Env {
    dir: PathBuf,
    db: Store,
    cf: Fake,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn env(cf: Fake) -> Env {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("lccp-handles-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    Env { db: Store::connect(dir.join("t.db")).await.unwrap(), dir, cf }
}

fn user(handle: &str, rating: Option<i64>, rank: Option<&str>) -> User {
    User { handle: handle.into(), rating, max_rating: rating, rank: rank.map(Into::into) }
}

fn problem(contest: i64, name: &str, rating: i64) -> Problem {
    Problem { contest_id: contest, index: "A".into(), name: name.into(), rating: Some(rating), tags: vec![] }
}

fn sub(created: i64, key: &str, verdict: &str) -> Submission {
    Submission { id: created, created, problem_key: key.into(), verdict: Some(verdict.into()) }
}

fn person(id: u64, name: &str) -> Person {
    Person { id, name: name.into() }
}

fn msg(err: anyhow::Error) -> String {
    error_message(&err)
}

async fn run_link(e: &Env, seed: u64, user_id: u64, handle: &str) -> anyhow::Result<()> {
    handles::link(&e.db, &e.cf, &e.cf, &mut StdRng::seed_from_u64(seed), &e.cf, 5000, G, user_id, handle).await
}

#[tokio::test]
async fn handles_group_commands() {
    let group = handles::handle();
    let mut names: Vec<_> = group.subcommands.iter().map(|c| c.name.to_string()).collect();
    names.sort();
    assert_eq!(names, ["link", "list", "show", "unlink"]);
    assert!(group.guild_only);
    let descriptions: Vec<_> = group.subcommands.iter().map(|c| (c.name.to_string(), c.description.clone().unwrap().to_string())).collect();
    for (name, want) in [
        ("link", "Link your Codeforces handle (proved by a compile error)"),
        ("show", "Show a member's Codeforces profile"),
        ("list", "Linked members ranked by Codeforces rating"),
        ("unlink", "Remove your linked handle"),
    ] {
        assert!(descriptions.contains(&(name.into(), want.into())), "{name}");
    }
    assert_eq!(group.description.as_deref(), Some("Link Codeforces handles"));
    let show = group.subcommands.iter().find(|c| c.name == "show").unwrap();
    let opts: Vec<_> = show.parameters.iter().map(|p| (p.name.to_string(), p.description.clone().unwrap().to_string())).collect();
    assert_eq!(
        opts,
        [
            ("member".to_string(), "Defaults to you".to_string()),
            ("handle".to_string(), "Find the member who linked this Codeforces handle".to_string())
        ]
    );
}

/// Python `VerifyButton.__discord_ui_compiled_template__.fullmatch`.
#[test]
fn verify_button_template_matches_custom_id() {
    assert_eq!(parse_verify_id("verify:12345"), Some(12345));
    assert_eq!(verify_custom_id(12345), "verify:12345");
    assert_eq!(parse_verify_id(&verify_custom_id(i64::MAX as u64)), Some(i64::MAX as u64));
    assert_eq!(parse_verify_id("verify:99999999999999999999"), Some(i64::MAX as u64)); // saturates: no user has it
    for bad in ["verify:", "verify:12a", "xverify:1", "verify:1 ", "verify:-1", "match:start:1", "page:next", ""] {
        assert_eq!(parse_verify_id(bad), None, "{bad:?}");
    }
}

/// `VerifyButton.callback` reports through `report_error`: a BotError's text, anything else the generic text.
#[test]
fn button_errors_are_reported_privately_as_python_does() {
    assert_eq!(error_message(&BotError::Plain("boom".into()).into()), "boom");
    assert_eq!(error_message(&anyhow::anyhow!("x")), "Something went wrong. Try again later.");
}

#[tokio::test]
async fn link_picks_an_unsolved_easy_problem_and_saves_pending() {
    let cf = Fake { users: vec![user("Tourist", Some(3000), Some("legendary grandmaster"))].into(), ..Default::default() };
    *cf.problems.lock().unwrap() = vec![problem(1, "Easy_one", 800), problem(2, "Hard", 1300), problem(3, "Solved", 800)];
    *cf.subs.lock().unwrap() = vec![sub(1, "3A", "OK")];
    let e = env(cf).await;
    run_link(&e, 1, 7, "tourist").await.unwrap();
    assert_eq!(
        e.cf.sent(),
        [Sent::Verify(
            "To prove you own **Tourist**, submit a **compile error** to [1A - Easy\\_one](https://codeforces.com/problemset/problem/1/A) within 5 minutes, then press **Verify**."
                .into(),
            7
        )]
    );
    let pending = e.db.get_pending(G, 7).await.unwrap().unwrap();
    assert_eq!((pending.handle.as_str(), pending.problem_key.as_str(), pending.issued_at), ("Tourist", "1A", 5000));
    assert_eq!(*e.cf.counts.lock().unwrap(), [None]); // every submission, not the recent ones
}

/// PARITY: the pick comes from the injected rng, is always in the pool and follows the seed.
#[tokio::test]
async fn link_pick_is_valid_and_follows_the_seed() {
    let cf = Fake { users: vec![user("a", None, None)].into(), ..Default::default() };
    *cf.problems.lock().unwrap() = (0..30).map(|i| problem(10 + i, "P", if i < 25 { 800 + i * 10 } else { 1500 })).collect();
    let e = env(cf).await;
    let mut seen = HashSet::new();
    for seed in 0..10 {
        run_link(&e, seed, 7, "a").await.unwrap();
        let key = e.db.get_pending(G, 7).await.unwrap().unwrap().problem_key;
        let p = e.cf.problems().iter().find(|p| p.key() == key).cloned().unwrap();
        assert!(p.rating.unwrap() <= 1200, "{key}");
        run_link(&e, seed, 7, "a").await.unwrap();
        assert_eq!(e.db.get_pending(G, 7).await.unwrap().unwrap().problem_key, key);
        seen.insert(key);
    }
    assert!(seen.len() > 1);
}

#[tokio::test]
async fn link_falls_back_to_solved_problems_and_fails_without_any() {
    let cf = Fake { users: vec![user("a", None, None)].into(), ..Default::default() };
    *cf.problems.lock().unwrap() = vec![problem(1, "Only", 800)];
    *cf.subs.lock().unwrap() = vec![sub(1, "1A", "OK")];
    let e = env(cf).await;
    run_link(&e, 1, 7, "a").await.unwrap();
    assert_eq!(e.db.get_pending(G, 7).await.unwrap().unwrap().problem_key, "1A"); // all solved: any easy one
    *e.cf.problems.lock().unwrap() = vec![problem(2, "Hard", 2000)];
    assert_eq!(msg(run_link(&e, 1, 7, "a").await.unwrap_err()), "No suitable problem found. Try again later.");
    e.cf.problems.lock().unwrap().clear();
    assert_eq!(msg(run_link(&e, 1, 7, "a").await.unwrap_err()), "Codeforces data is still loading. Try again in a minute.");
    assert_eq!(msg(run_link(&e, 1, 7, "nobody").await.unwrap_err()), "No Codeforces user matches that handle.");
}

#[tokio::test]
async fn link_refuses_a_handle_linked_to_someone_else_but_not_to_you() {
    let cf = Fake { users: vec![user("a_b", None, None)].into(), ..Default::default() };
    *cf.problems.lock().unwrap() = vec![problem(1, "x", 800)];
    let e = env(cf).await;
    e.db.link_handle(G, 99, "A_B", None).await.unwrap();
    assert_eq!(msg(run_link(&e, 1, 7, "a_b").await.unwrap_err()), "a\\_b is already linked to <@99>.");
    run_link(&e, 1, 99, "a_b").await.unwrap(); // relinking your own handle is fine
}

async fn pending_env(issued: i64) -> Env {
    let cf = Fake { users: vec![user("Tourist", Some(2999), None)].into(), ..Default::default() };
    let e = env(cf).await;
    e.db.save_pending(G, 7, "tourist", "1A", issued).await.unwrap();
    e
}

async fn run_verify(e: &Env, now: i64) -> anyhow::Result<()> {
    handles::verify(&e.db, &e.cf, &e.cf, now, G, 7).await
}

#[tokio::test]
async fn verify_links_after_a_compile_error_inside_the_window() {
    let e = pending_env(1000).await;
    assert_eq!(msg(run_verify(&e, 1000).await.unwrap_err()), "No compile error found on 1A yet. Submit one on Codeforces, then press Verify again.");
    // wrong key, wrong verdict, before issue and after the window do not count
    *e.cf.subs.lock().unwrap() = vec![sub(1100, "2A", "COMPILATION_ERROR"), sub(1100, "1A", "OK"), sub(999, "1A", "COMPILATION_ERROR"), sub(1301, "1A", "COMPILATION_ERROR")];
    assert!(run_verify(&e, 1200).await.is_err());
    *e.cf.subs.lock().unwrap() = vec![sub(1300, "1A", "COMPILATION_ERROR")]; // 1000..=1300 counts
    run_verify(&e, 1300).await.unwrap();
    assert_eq!(e.cf.sent(), [Sent::Text("Linked you to **Tourist**.".into())]);
    let linked = e.db.get_handle(G, 7).await.unwrap().unwrap();
    assert_eq!((linked.handle.as_str(), linked.rating), ("Tourist", Some(2999)));
    assert!(e.db.get_pending(G, 7).await.unwrap().is_none());
    assert_eq!(*e.cf.counts.lock().unwrap(), [Some(20), Some(20), Some(20)]);
}

#[tokio::test]
async fn verify_without_or_after_the_window() {
    let e = pending_env(1000).await;
    assert_eq!(msg(handles::verify(&e.db, &e.cf, &e.cf, 1000, G, 8).await.unwrap_err()), "No link in progress. Run /handle link again.");
    run_verify(&e, 1300).await.unwrap_err(); // exactly at the end of the window is still open
    assert!(e.db.get_pending(G, 7).await.unwrap().is_some());
    assert_eq!(msg(run_verify(&e, 1301).await.unwrap_err()), "That link attempt expired. Run /handle link again.");
    assert!(e.db.get_pending(G, 7).await.unwrap().is_none());
}

#[tokio::test]
async fn show_resolves_member_or_handle_and_sends_the_profile() {
    let mut cf = Fake { users: vec![user("tourist", Some(3000), Some("legendary grandmaster"))].into(), ..Default::default() };
    cf.members = vec![person(5, "Alice")];
    let e = env(cf).await;
    e.db.link_handle(G, 5, "tourist", Some(3000)).await.unwrap();
    assert_eq!(handles::check_show_args(true, true).unwrap_err().to_string(), "Give a member or a handle, not both.");
    assert!(handles::check_show_args(true, false).is_ok() && handles::check_show_args(false, true).is_ok());
    let go = |m: Option<Person>, h: Option<&'static str>| handles::show(&e.db, &e.cf, &e.cf, G, person(9, "Me"), m, h);
    go(Some(person(5, "Alice")), None).await.unwrap();
    go(None, Some("tourist")).await.unwrap();
    let sent = e.cf.sent();
    assert_eq!(sent[0], sent[1]);
    let Sent::Embed(embed) = &sent[0] else { panic!() };
    assert_eq!(embed.title.as_deref(), Some("tourist - Alice"));
    assert_eq!(msg(go(None, None).await.unwrap_err()), "Me has no linked handle.");
    assert_eq!(msg(go(None, Some("ghost")).await.unwrap_err()), "No member is linked to ghost.");
    e.db.link_handle(G, 6, "gone_h", None).await.unwrap();
    assert_eq!(msg(go(None, Some("gone_h")).await.unwrap_err()), "gone\\_h is linked to a member who left this server.");
}

#[tokio::test]
async fn list_sorts_by_rating_unrated_last_then_name() {
    let cf = Fake {
        users: vec![user("b", Some(1500), Some("specialist")), user("A", Some(1500), Some("specialist")), user("z", None, None), user("top", Some(2500), Some("master"))].into(),
        ..Default::default()
    };
    let e = env(cf).await;
    assert_eq!(msg(handles::list(&e.db, &e.cf, &e.cf, G).await.unwrap_err()), "No member has linked a handle yet.");
    for (uid, h) in [(1, "B"), (2, "a"), (3, "Z"), (4, "TOP"), (5, "gone")] {
        e.db.link_handle(G, uid, h, None).await.unwrap();
    }
    handles::list(&e.db, &e.cf, &e.cf, G).await.unwrap();
    let [Sent::Pages(pages)] = &e.cf.sent()[..] else { panic!() };
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0].description.as_deref(),
        Some("1. <@4> ([top](https://codeforces.com/profile/top)) 2500 (master)\n2. <@2> ([A](https://codeforces.com/profile/A)) 1500 (specialist)\n3. <@1> ([b](https://codeforces.com/profile/b)) 1500 (specialist)\n4. <@3> ([z](https://codeforces.com/profile/z)) unrated")
    );
    assert_eq!(pages[0].footer.as_deref(), Some("Page 1/1 - 4 linked members")); // the handle Codeforces dropped is skipped
}

#[tokio::test]
async fn list_when_every_handle_is_gone_is_a_generic_error() {
    let e = env(Fake::default()).await;
    e.db.link_handle(G, 1, "gone", None).await.unwrap();
    assert_eq!(msg(handles::list(&e.db, &e.cf, &e.cf, G).await.unwrap_err()), "Something went wrong. Try again later.");
}

#[tokio::test]
async fn unlink_removes_or_says_there_is_nothing() {
    let e = env(Fake::default()).await;
    assert_eq!(msg(handles::unlink(&e.db, &e.cf, G, 7).await.unwrap_err()), "You have no linked handle.");
    e.db.link_handle(G, 7, "h", None).await.unwrap();
    handles::unlink(&e.db, &e.cf, G, 7).await.unwrap();
    assert_eq!(e.cf.sent(), [Sent::Respond("Unlinked.".into())]);
    assert!(e.db.get_handle(G, 7).await.unwrap().is_none());
}

#[test]
fn verify_id_takes_unicode_digits_like_python() {
    assert_eq!(parse_verify_id("verify:٣٤"), Some(34));
    assert_eq!(parse_verify_id("verify:१२३"), Some(123));
    assert_eq!(parse_verify_id("verify:12a"), None);
    assert_eq!(parse_verify_id("verify:"), None);
    assert_eq!(parse_verify_id("verify:+5"), None);
}
