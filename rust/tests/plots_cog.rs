//! Port of the PlotCog parts of `tests/test_cogs.py`, plus the command logic of `src/cogs/plot.py`:
//! messages, notes, titles, ordering and the attachment name, over fake data and a fake output.
use std::collections::HashMap;
use std::sync::Mutex;

use lightningccp::cogs::plot::{self, Person, PlotData, PlotOutput, Request, esc};
use lightningccp::errors::BotError;
use lightningccp::ui::plots::CF_BANDS;

#[derive(Default)]
struct Fake {
    handles: HashMap<u64, &'static str>,
    users: HashMap<&'static str, i64>,
    changes: HashMap<i64, Vec<(i64, i64)>>,
    cf: HashMap<&'static str, Vec<(i64, i64)>>, // lowercase key; the stored handle's casing is `cf_cased`
    sent: Mutex<Vec<(Option<String>, String, Vec<u8>)>>,
}

impl PlotData for Fake {
    async fn linked_handle(&self, _g: i64, user: u64) -> anyhow::Result<Option<String>> {
        Ok(self.handles.get(&user).map(|h| h.to_string()))
    }
    async fn user_for_handle(&self, _g: i64, handle: &str) -> anyhow::Result<Option<i64>> {
        Ok(self.users.get(handle).copied())
    }
    async fn rating_changes(&self, _g: i64, user: i64) -> anyhow::Result<Vec<(i64, i64)>> {
        Ok(self.changes.get(&user).cloned().unwrap_or_default())
    }
    async fn cf_handle(&self, name: &str) -> anyhow::Result<String> {
        match self.cf.keys().find(|k| k.eq_ignore_ascii_case(name)) {
            Some(k) => Ok(k.replace("tourist", "Tourist")), // Codeforces returns the canonical casing
            None => Err(BotError::handle_not_found().into()),
        }
    }
    async fn cf_history(&self, name: &str) -> anyhow::Result<Vec<(i64, i64)>> {
        Ok(self.cf[name.to_lowercase().as_str()].clone())
    }
}

impl PlotOutput for Fake {
    async fn send_plot(&self, note: Option<String>, filename: &str, png: Vec<u8>) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push((note, filename.into(), png));
        Ok(())
    }
}

fn p(id: u64, name: &str) -> Person {
    Person { id, name: name.into() }
}

fn err(r: anyhow::Result<Request>) -> String {
    r.unwrap_err().downcast_ref::<BotError>().expect("a BotError").to_string()
}

#[test]
fn plot_group_commands() {
    let c = plot::plot();
    assert_eq!((&*c.name, c.description.as_deref()), ("plot", Some("Rating graphs")));
    let names: Vec<_> = c.subcommands.iter().map(|s| s.name.to_string()).collect();
    assert_eq!(names, ["rating", "codeforces"]);
    let desc = |i: usize| c.subcommands[i].description.clone().unwrap();
    assert_eq!(desc(0), "Graph a member's match rating in this server");
    assert_eq!(desc(1), "Graph Codeforces rating histories");
    let params = |i: usize| c.subcommands[i].parameters.iter().map(|x| x.name.to_string()).collect::<Vec<_>>();
    assert_eq!(params(0), ["member", "handle", "member2", "member3", "member4", "member5"]);
    assert_eq!(params(1), ["member", "handle", "handle2", "handle3", "handle4", "handle5", "member2", "member3", "member4", "member5"]);
    assert!(c.guild_only && c.subcommands.iter().all(|s| s.parameters.iter().all(|x| !x.required)));
}

#[test]
fn argument_checks_run_before_the_defer() {
    assert_eq!(plot::check_args(true, true).unwrap_err().to_string(), "Give a member or a handle, not both.");
    assert!(plot::check_args(true, false).is_ok() && plot::check_args(false, true).is_ok());
    assert_eq!(plot::check_rating_args(false, true, 1).unwrap_err().to_string(), "Compare members, not a handle.");
    assert_eq!(plot::check_rating_args(true, true, 1).unwrap_err().to_string(), "Give a member or a handle, not both.");
    assert!(plot::check_rating_args(false, true, 0).is_ok());
}

#[tokio::test]
async fn rating_single_member_title_and_errors() {
    let mut f = Fake::default();
    f.changes.insert(7, vec![(100, 25), (200, -10)]);
    f.users.insert("alice_cf", 7);
    let r = plot::rating(&f, 1, p(7, "Alice"), None, None, vec![]).await.unwrap();
    assert_eq!(r, Request { title: "Match rating - Alice".into(), lines: vec![("Alice".into(), vec![(100, 825), (200, 815)])], bands: &[], note: None });
    let r = plot::rating(&f, 1, p(9, "Me"), None, Some("alice_cf"), vec![]).await.unwrap();
    assert_eq!(r.title, "Match rating - alice_cf");
    assert_eq!(err(plot::rating(&f, 1, p(9, "Me"), None, None, vec![]).await), "Me has no finished matches yet.");
    assert_eq!(err(plot::rating(&f, 1, p(9, "Me"), None, Some("a_b"), vec![]).await), "No member is linked to a\\_b.");
}

#[tokio::test]
async fn rating_compare_notes_missing_and_dedupes() {
    let mut f = Fake::default();
    f.changes.insert(1, vec![(10, 5)]);
    f.changes.insert(2, vec![(20, 5)]);
    let r = plot::rating(&f, 1, p(1, "A"), None, None, vec![p(2, "B"), p(3, "C"), p(1, "A"), p(4, "D")]).await.unwrap();
    assert_eq!(r.title, "Match rating");
    assert_eq!(r.lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["A", "B"]);
    assert_eq!(r.note.as_deref(), Some("No finished matches yet: C, D."));
    // the explicit member replaces the invoker
    let r = plot::rating(&f, 1, p(3, "C"), Some(p(2, "B")), None, vec![p(1, "A")]).await.unwrap();
    assert_eq!(r.lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["B", "A"]);
    assert_eq!(err(plot::rating(&f, 1, p(3, "C"), None, None, vec![p(4, "D")]).await), "None of them has finished matches yet.");
}

fn cf_fake() -> Fake {
    let mut f = Fake::default();
    f.cf.insert("tourist", vec![(1, 1500), (2, 1600)]);
    f.cf.insert("petr", vec![(3, 1400)]);
    f.cf.insert("newbie", vec![]);
    f.handles.insert(1, "TOURIST");
    f.handles.insert(2, "petr");
    f
}

fn handles(h: &[&str]) -> Vec<Option<String>> {
    h.iter().map(|s| Some(s.to_string())).chain(std::iter::repeat_n(None, 4 - h.len())).collect()
}

#[tokio::test]
async fn codeforces_single_handle_uses_bands_and_fixes_casing() {
    let f = cf_fake();
    let r = plot::codeforces(&f, 1, p(9, "Me"), None, Some("tOuRiSt".into()), handles(&[]), vec![]).await.unwrap();
    assert_eq!(r.title, "Codeforces rating - Tourist");
    assert_eq!((r.lines.clone(), r.bands, r.note), (vec![("Tourist".into(), vec![(1, 1500), (2, 1600)])], &CF_BANDS[..], None));
    // no handle: the member's (or the invoker's) linked handle
    let r = plot::codeforces(&f, 1, p(1, "Me"), None, None, handles(&[]), vec![]).await.unwrap();
    assert_eq!(r.title, "Codeforces rating - Tourist");
}

#[tokio::test]
async fn codeforces_compare_order_dedupe_and_notes() {
    let f = cf_fake();
    let r = plot::codeforces(&f, 1, p(9, "Me"), None, Some("petr".into()), handles(&["TOURIST", "newbie", "PETR"]), vec![p(1, "T")]).await.unwrap();
    assert_eq!(r.title, "Codeforces rating");
    assert_eq!(r.lines.iter().map(|l| l.0.as_str()).collect::<Vec<_>>(), ["petr", "Tourist"]);
    assert_eq!(r.note.as_deref(), Some("No rated contest yet: newbie."));
}

#[tokio::test]
async fn codeforces_errors_match_python() {
    let f = cf_fake();
    let go = |h: Option<&str>, others: Vec<Option<String>>, members: Vec<Person>| {
        plot::codeforces(&f, 1, p(9, "Me"), None, h.map(String::from), others, members)
    };
    assert_eq!(err(go(None, handles(&[]), vec![]).await), "Me has no linked handle.");
    assert_eq!(err(go(Some("tourist"), handles(&[]), vec![p(5, "Zed")]).await), "Zed has no linked handle.");
    assert_eq!(err(go(Some("a"), handles(&["b", "c", "d", "e"]), vec![p(1, "T")]).await), "Compare at most 5 at once.");
    assert_eq!(err(go(Some("newbie"), handles(&[]), vec![]).await), "newbie has not taken part in a rated contest.");
    assert_eq!(err(go(Some("newbie"), handles(&["newbie"]), vec![]).await), "newbie has not taken part in a rated contest.");
    let mut f2 = cf_fake();
    f2.cf.insert("other", vec![]);
    let r = plot::codeforces(&f2, 1, p(9, "Me"), None, Some("newbie".into()), handles(&["other"]), vec![]).await;
    assert_eq!(err(r), "None of them has taken part in a rated contest.");
    // empty handle options are skipped; an unknown handle is Codeforces' own error
    assert!(go(Some("tourist"), vec![Some(String::new()), None, None, None], vec![]).await.is_ok());
    assert_eq!(err(go(Some("nobody"), handles(&[]), vec![]).await), "No Codeforces user matches that handle.");
}

#[tokio::test]
async fn reply_sends_a_png_named_rating_png_with_the_note() {
    let f = cf_fake();
    let req = plot::codeforces(&f, 1, p(9, "Me"), None, Some("tourist".into()), handles(&["newbie"]), vec![]).await.unwrap();
    plot::reply(&f, req).await.unwrap();
    let sent = f.sent.lock().unwrap();
    assert_eq!((sent[0].0.as_deref(), sent[0].1.as_str()), (Some("No rated contest yet: newbie."), "rating.png"));
    assert_eq!(&sent[0].2[..8], b"\x89PNG\r\n\x1a\n");
}

/// Expected values from Python `ui.embeds.esc` (discord.py `escape_markdown` with links kept, then brackets).
#[test]
fn esc_matches_python() {
    let cases = [
        ("plain", "plain"),
        ("a_b_c", "a\\_b\\_c"),
        ("*bold*", "\\*bold\\*"),
        ("x[1]", "x\\[1\\]"),
        ("[a](http://x_y.com)", "\\\\[a\\](http://x_y.com)"),
        ("a|b|c", "a\\|b\\|c"),
        ("~~t~~", "\\~\\~t\\~\\~"),
        ("`c`", "\\`c\\`"),
        ("_x", "\\_x"),
        ("# h", "\\# h"),
        ("> q", "\\> q"),
        ("- *a*", "\\- \\*a\\*"),
        ("a\\\\b", "a\\\\\\\\b"),
    ];
    for (input, want) in cases {
        assert_eq!(esc(input), want, "{input}");
    }
}
