//! Port of the problems parts of `tests/test_cogs.py` (commands registered, contest autocomplete) plus the logic of
//! `src/cogs/problems.py`: `/gimme` filters, footer and rng, tag autocomplete, `/daily` embed, `/stalk`.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use lightningccp::cf::models::{Contest, Problem, Submission, User};
use lightningccp::cogs::plot::Person;
use lightningccp::cogs::problems::{self, GimmeArgs, ProblemsOutput};
use lightningccp::daily::{CfApi, DailyService, ProblemSource};
use lightningccp::errors::{BotError, error_message};
use lightningccp::store::base::Store;
use lightningccp::ui::embeds::a::EmbedData;
use rand::SeedableRng;
use rand::rngs::StdRng;

const G: i64 = 1;

#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Embed(EmbedData),
    Pages(Vec<EmbedData>),
}

#[derive(Default)]
struct Fake {
    users: Vec<User>,
    subs: Mutex<Vec<Submission>>,
    problems: Mutex<Vec<Problem>>,
    sent: Mutex<Vec<Sent>>,
}

impl Fake {
    fn sent(&self) -> Vec<Sent> {
        std::mem::take(&mut *self.sent.lock().unwrap())
    }
}

impl CfApi for Fake {
    async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        self.users.iter().find(|u| u.handle.eq_ignore_ascii_case(handle)).cloned().ok_or_else(|| BotError::handle_not_found().into())
    }
    async fn users_info(&self, _: &[String]) -> anyhow::Result<Vec<User>> {
        unreachable!()
    }
    async fn user_status(&self, _: &str, _: Option<i64>) -> anyhow::Result<Vec<Submission>> {
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

impl ProblemsOutput for Fake {
    async fn followup_embed(&self, embed: EmbedData) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(Sent::Embed(embed));
        Ok(())
    }
    async fn followup_pages(&self, pages: Vec<EmbedData>) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push(Sent::Pages(pages));
        Ok(())
    }
}

struct Env {
    dir: PathBuf,
    db: Arc<Store>,
    cf: Arc<Fake>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn env(cf: Fake) -> Env {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("lccp-problems-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::create_dir_all(&dir).unwrap();
    Env { db: Arc::new(Store::connect(dir.join("t.db")).await.unwrap()), dir, cf: Arc::new(cf) }
}

fn problem(contest: i64, name: &str, rating: i64, tags: &[&str]) -> Problem {
    Problem { contest_id: contest, index: "A".into(), name: name.into(), rating: Some(rating), tags: tags.iter().map(|t| t.to_string()).collect() }
}

fn sub(created: i64, key: &str, verdict: &str) -> Submission {
    Submission { id: created, created, problem_key: key.into(), verdict: Some(verdict.into()) }
}

fn msg(err: anyhow::Error) -> String {
    error_message(&err)
}

async fn gimme(e: &Env, seed: u64, user: u64, args: GimmeArgs) -> anyhow::Result<EmbedData> {
    problems::gimme(&e.db, e.cf.as_ref(), e.cf.as_ref(), &mut StdRng::seed_from_u64(seed), e.cf.as_ref(), G, user, args).await?;
    let [Sent::Embed(embed)] = &e.cf.sent()[..] else { panic!("one embed") };
    Ok(embed.clone())
}

fn field<'a>(e: &'a EmbedData, name: &str) -> &'a str {
    &e.fields.iter().find(|f| f.name == name).unwrap().value
}

#[test]
fn problems_commands_registered_with_python_options() {
    let names: Vec<String> = [problems::gimme_command(), problems::daily_command(), problems::stalk_command()].iter().map(|c| c.name.to_string()).collect();
    assert_eq!(names, ["gimme", "daily", "stalk"]);
    let gimme = problems::gimme_command();
    let opts: Vec<(String, String)> = gimme.parameters.iter().map(|p| (p.name.to_string(), p.description.clone().unwrap().to_string())).collect();
    assert_eq!(
        opts,
        [
            ("rating", "Exact rating"),
            ("min_rating", "Minimum rating"),
            ("max_rating", "Maximum rating"),
            ("tags", "Comma-separated tags; the problem must have all of them"),
            ("exclude_tags", "Comma-separated tags to avoid"),
            ("contest_id", "Only problems from this Codeforces contest (type a name or id)"),
        ]
        .map(|(a, b)| (a.to_string(), b.to_string()))
    );
    assert_eq!(gimme.description.as_deref(), Some("Get a random Codeforces problem"));
    assert!(gimme.guild_only && problems::daily_command().guild_only && problems::stalk_command().guild_only);
    assert_eq!(problems::daily_command().description.as_deref(), Some("Your daily problem: solve it for double rating (resets 00:00 UTC)"));
    assert_eq!(problems::stalk_command().description.as_deref(), Some("List problems someone tried but never solved"));
    let stalk: Vec<String> = problems::stalk_command().parameters.iter().map(|p| p.description.clone().unwrap().to_string()).collect();
    assert_eq!(stalk, ["A Codeforces handle", "A server member with a linked handle (default: you)"]);
    let daily = problems::daily_command();
    assert_eq!(daily.parameters[0].description.as_deref(), Some("Problem rating (default: your match rating, floored to 100). A different rating than today's…")); // discord.py `_shorten`s it to 100 characters; Discord rejects more
    for c in [&gimme, &daily] {
        let rating = &c.parameters[0];
        assert_eq!((rating.type_setter.is_some(), rating.autocomplete_callback.is_none()), (true, true));
    }
    assert!(gimme.parameters[3].autocomplete_callback.is_some() && gimme.parameters[4].autocomplete_callback.is_some() && gimme.parameters[5].autocomplete_callback.is_some());
}

#[test]
fn contest_autocomplete_matches_name_and_id() {
    let contest = |id, name: &str, kind: &str, start| Contest { id, name: name.into(), kind: kind.into(), phase: "FINISHED".into(), start_time: Some(start), duration: 7200 };
    let cs = [contest(1700, "Round 800 (Div. 2)", "CF", 1000), contest(1701, "Educational 5", "ICPC", 2000), contest(1702, "Unknown problems", "CF", 3000)];
    let ps = [problem(1700, "x", 800, &[]), problem(1701, "y", 900, &[])];
    let both = problems::contest_choices(&cs, &ps, "");
    // newest first; the contest with no known problems is left out
    assert_eq!(both, [("1701 - Educational 5".to_string(), 1701), ("1700 - Round 800 (Div. 2)".to_string(), 1700)]);
    assert_eq!(problems::contest_choices(&cs, &ps, "round").iter().map(|c| c.1).collect::<Vec<_>>(), [1700]);
    assert_eq!(problems::contest_choices(&cs, &ps, "1701").iter().map(|c| c.1).collect::<Vec<_>>(), [1701]);
    // unrated problems do not make a contest known (Python `if p.rating`)
    assert!(problems::contest_choices(&cs, &[Problem { rating: None, ..problem(1700, "x", 0, &[]) }], "").is_empty());
}

#[test]
fn tag_autocomplete_filters_sorts_and_caps_at_25() {
    let tags: HashSet<String> = ["dp", "graphs", "greedy", "math", "implementation"].map(String::from).into();
    assert_eq!(problems::tag_choices("g", &tags), ["graphs", "greedy"]);
    assert_eq!(problems::tag_choices("math, g", &tags), ["math,graphs", "math,greedy"]);
    assert_eq!(problems::tag_choices("dp,", &tags), ["dp,graphs", "dp,greedy", "dp,implementation", "dp,math"]);
    let many: HashSet<String> = (0..40).map(|i| format!("t{i:02}")).collect();
    let all = problems::tag_choices("", &many);
    assert_eq!((all.len(), all[0].as_str(), all[24].as_str()), (25, "t00", "t24"));
    assert!(problems::tag_choices("zzz", &tags).is_empty());
}

#[tokio::test]
async fn gimme_filters_and_footers() {
    let cf = Fake { users: vec![], ..Default::default() };
    *cf.problems.lock().unwrap() = vec![
        problem(1, "Dp one", 800, &["dp", "math"]),
        problem(2, "Graph", 900, &["graphs"]),
        problem(3, "Unrated", 0, &[]),
        problem(4, "Hard", 1700, &["dp"]),
    ];
    let e = env(cf).await;
    // not linked, no range: the match rating (800 for a new player), and the link hint
    let embed = gimme(&e, 1, 7, GimmeArgs::default()).await.unwrap();
    assert_eq!((embed.title.as_deref(), embed.footer.as_deref()), (Some("1A. Dp one"), Some("Link your handle with /handle link to hide solved problems.")));
    assert_eq!(field(&embed, "Tags"), "||dp, math||");
    let by = |args| gimme(&e, 1, 7, args);
    assert_eq!(by(GimmeArgs { rating: Some(900), ..Default::default() }).await.unwrap().title.as_deref(), Some("2A. Graph"));
    assert_eq!(by(GimmeArgs { min_rating: Some(1000), ..Default::default() }).await.unwrap().title.as_deref(), Some("4A. Hard"));
    assert_eq!(by(GimmeArgs { max_rating: Some(850), tags: Some("DP, math".into()), ..Default::default() }).await.unwrap().title.as_deref(), Some("1A. Dp one"));
    assert_eq!(by(GimmeArgs { contest_id: Some(2), ..Default::default() }).await.unwrap().title.as_deref(), Some("2A. Graph"));
    assert_eq!(by(GimmeArgs { tags: Some("dp".into()), exclude_tags: Some("math".into()), min_rating: Some(800), ..Default::default() }).await.unwrap().title.as_deref(), Some("4A. Hard"));
    // a given range or contest keeps the link hint and skips the rating footer
    assert_eq!(by(GimmeArgs { max_rating: Some(900), ..Default::default() }).await.unwrap().footer.as_deref(), Some("Link your handle with /handle link to hide solved problems."));
    let none = by(GimmeArgs { rating: Some(3000), ..Default::default() }).await.unwrap_err();
    assert_eq!(msg(none), "No problems match those filters.");
}

#[tokio::test]
async fn gimme_linked_hides_solved_and_uses_the_match_rating() {
    let cf = Fake::default();
    *cf.problems.lock().unwrap() = vec![problem(1, "Solved", 800, &[]), problem(2, "Fresh", 800, &[])];
    *cf.subs.lock().unwrap() = vec![sub(1, "1A", "OK"), sub(2, "2A", "WRONG_ANSWER")];
    let e = env(cf).await;
    e.db.link_handle(G, 7, "h", Some(1000)).await.unwrap();
    for seed in 0..5 {
        let embed = gimme(&e, seed, 7, GimmeArgs::default()).await.unwrap();
        assert_eq!((embed.title.as_deref(), embed.footer.as_deref()), (Some("2A. Fresh"), Some("Rating 800 (from your match rating).")));
    }
    // an explicit range drops the rating footer; linked users get no footer at all
    let embed = gimme(&e, 1, 7, GimmeArgs { rating: Some(800), ..Default::default() }).await.unwrap();
    assert_eq!(embed.footer, None);
    e.cf.problems.lock().unwrap().clear();
    assert_eq!(msg(gimme(&e, 1, 7, GimmeArgs::default()).await.unwrap_err()), "Codeforces data is still loading. Try again in a minute.");
}

/// PARITY: the pick comes from the injected rng, is always in the pool and follows the seed.
#[tokio::test]
async fn gimme_pick_is_valid_and_follows_the_seed() {
    let cf = Fake::default();
    *cf.problems.lock().unwrap() = (0..30).map(|i| problem(100 + i, "P", 800, &[])).collect();
    let e = env(cf).await;
    let mut seen = HashSet::new();
    for seed in 0..10 {
        let title = gimme(&e, seed, 7, GimmeArgs::default()).await.unwrap().title.unwrap();
        assert!(title.starts_with("1") && title.ends_with("A. P"), "{title}");
        assert_eq!(gimme(&e, seed, 7, GimmeArgs::default()).await.unwrap().title.unwrap(), title);
        seen.insert(title);
    }
    assert!(seen.len() > 1);
}

#[tokio::test]
async fn daily_sends_the_claim_embed() {
    let cf = Fake::default();
    *cf.problems.lock().unwrap() = vec![problem(1700, "Foo", 800, &["dp"])];
    let e = env(cf).await;
    let svc = DailyService::with(e.db.clone(), e.cf.clone(), e.cf.clone(), Box::new(|| 1000.0), StdRng::seed_from_u64(1));
    assert_eq!(msg(problems::daily(&svc, e.cf.as_ref(), G, 5, 7, None).await.unwrap_err()), "Link your Codeforces handle first with /handle link.");
    e.db.link_handle(G, 7, "h", Some(1000)).await.unwrap();
    problems::daily(&svc, e.cf.as_ref(), G, 5, 7, None).await.unwrap();
    let [Sent::Embed(embed)] = &e.cf.sent()[..] else { panic!() };
    assert_eq!(embed.title.as_deref(), Some("Daily: 1700A. Foo"));
    assert_eq!(embed.description.as_deref(), Some("New daily problem <t:86400:R>."));
}

#[tokio::test]
async fn stalk_lists_unsolved_problems_and_checks_its_arguments() {
    let cf = Fake { users: vec![User { handle: "Tourist".into(), rating: None, max_rating: None, rank: None }], ..Default::default() };
    *cf.problems.lock().unwrap() = vec![problem(1700, "Fo_o", 1400, &[])];
    *cf.subs.lock().unwrap() = vec![sub(3, "1700A", "WRONG_ANSWER"), sub(2, "9999Z", "RUNTIME_ERROR"), sub(1, "1A", "OK"), sub(0, "1700A", "WRONG_ANSWER")];
    let e = env(cf).await;
    let go = |m: Option<Person>, h: Option<&'static str>| problems::stalk(&e.db, e.cf.as_ref(), e.cf.as_ref(), e.cf.as_ref(), G, Person { id: 9, name: "Me".into() }, m, h);
    assert_eq!(problems::check_stalk_args(true, true).unwrap_err().to_string(), "Give a handle or a member, not both.");
    assert!(problems::check_stalk_args(true, false).is_ok() && problems::check_stalk_args(false, true).is_ok());
    go(None, Some("tourist")).await.unwrap();
    let [Sent::Pages(pages)] = &e.cf.sent()[..] else { panic!() };
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].title.as_deref(), Some("Unsolved by Tourist")); // the casing Codeforces returns
    assert_eq!(pages[0].description.as_deref(), Some("[1700A. Fo\\_o](https://codeforces.com/problemset/problem/1700/A) (1400)\n9999Z"));
    assert_eq!(pages[0].footer.as_deref(), Some("Page 1/1 - 2 problems"));
    assert_eq!(msg(go(None, None).await.unwrap_err()), "Me has no linked handle.");
    e.db.link_handle(G, 5, "linked_h", None).await.unwrap();
    go(Some(Person { id: 5, name: "Al".into() }), None).await.unwrap();
    let [Sent::Pages(p)] = &e.cf.sent()[..] else { panic!() };
    assert_eq!(p[0].title.as_deref(), Some("Unsolved by linked_h")); // a linked handle is used as stored
    assert_eq!(msg(go(None, Some("ghost")).await.unwrap_err()), "No Codeforces user matches that handle.");
    *e.cf.subs.lock().unwrap() = vec![sub(1, "1A", "OK")];
    assert_eq!(msg(go(None, Some("tourist")).await.unwrap_err()), "Tourist has no attempted-but-unsolved problems.");
}

#[tokio::test]
async fn stalk_pages_every_15_problems() {
    let cf = Fake { users: vec![User { handle: "x".into(), rating: None, max_rating: None, rank: None }], ..Default::default() };
    *cf.subs.lock().unwrap() = (0..16).map(|i| sub(i, &format!("1A{i}"), "WRONG_ANSWER")).collect();
    let e = env(cf).await;
    problems::stalk(&e.db, e.cf.as_ref(), e.cf.as_ref(), e.cf.as_ref(), G, Person { id: 9, name: "Me".into() }, None, Some("x")).await.unwrap();
    let [Sent::Pages(pages)] = &e.cf.sent()[..] else { panic!() };
    let footers: Vec<_> = pages.iter().map(|p| p.footer.clone().unwrap()).collect();
    assert_eq!(footers, ["Page 1/2 - 16 problems", "Page 2/2 - 16 problems"]);
    assert_eq!(pages[0].description.as_deref().unwrap().lines().count(), 15);
}
