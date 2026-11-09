//! Parity harness runner (Rust side). Reads JSONL cases `{"id","fn","args"}` from the files given as
//! arguments (stdin when none) and writes JSONL `{"id","out"}` or `{"id","err":{"kind":"bot","text"}|{"kind":"other"}}`.
//! Python twin: parity/run_py.py. A panic is reported as `{"kind":"panic"}`, which never equals Python's result.
//! Add a function: one arm in `call` here, one entry in FNS of run_py.py, cases in gen_corpus.py.
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use lightningccp::cf::client::{BoxFuture, CodeforcesClient, Http, Params};
use lightningccp::cf::models::{Contest, Submission};
use lightningccp::cogs::contests::{check_regex, not_excluded};
use lightningccp::errors::BotError;
use lightningccp::logic::base as lb;
use lightningccp::cf::models::{Problem, User};
use lightningccp::store::base::{Store, is_constraint_violation};
use lightningccp::logic::base::WeeklyRow;
use lightningccp::store::matches::DailyRow;
use lightningccp::ui::embeds::a::{self as ea, EmbedData, esc, fmt_g};
use serde_json::{Value, json};

fn i(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| panic!("harness: not an i64: {v}"))
}
fn oi(v: &Value) -> Option<i64> {
    (!v.is_null()).then(|| i(v))
}
fn s(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("harness: not a string: {v}"))
}
fn os(v: &Value) -> Option<&str> {
    (!v.is_null()).then(|| s(v))
}
fn arr(v: &Value) -> &Vec<Value> {
    v.as_array().unwrap_or_else(|| panic!("harness: not an array: {v}"))
}
fn ints(v: &Value) -> Vec<i64> {
    arr(v).iter().map(i).collect()
}
fn idset(v: &Value) -> HashSet<i64> {
    ints(v).into_iter().collect()
}
fn pairs(v: &Value) -> Vec<(i64, i64)> {
    arr(v).iter().map(|p| (i(&p[0]), i(&p[1]))).collect()
}
fn contest(v: &Value) -> Contest {
    Contest { id: i(&v["id"]), name: s(&v["name"]).into(), kind: s(&v["type"]).into(), phase: s(&v["phase"]).into(), start_time: oi(&v["start_time"]), duration: i(&v["duration"]) }
}
fn contests(v: &Value) -> Vec<Contest> {
    arr(v).iter().map(contest).collect()
}
fn contest_json(c: &Contest) -> Value {
    json!({"id": c.id, "name": c.name, "type": c.kind, "phase": c.phase, "start_time": c.start_time, "duration": c.duration})
}
fn subs(v: &Value) -> Vec<Submission> {
    arr(v).iter().map(|x| Submission { id: i(&x["id"]), created: i(&x["created"]), problem_key: s(&x["problem_key"]).into(), verdict: os(&x["verdict"]).map(Into::into) }).collect()
}
fn problem(v: &Value) -> Problem {
    Problem { contest_id: i(&v["contest_id"]), index: s(&v["index"]).into(), name: s(&v["name"]).into(), rating: oi(&v["rating"]), tags: arr(&v["tags"]).iter().map(|t| s(t).into()).collect() }
}
fn oproblem(v: &Value) -> Option<Problem> {
    (!v.is_null()).then(|| problem(v))
}
fn embed(e: &EmbedData) -> Value {
    let fields: Vec<Value> = e.fields.iter().map(|f| json!({"name": f.name, "value": f.value, "inline": f.inline})).collect();
    // Empty title, description and url are not sent (From<EmbedData> for CreateEmbed; Python to_dict drops them too).
    let sent = |v: &Option<String>| v.clone().filter(|t| !t.is_empty());
    json!({"title": sent(&e.title), "description": sent(&e.description), "url": sent(&e.url), "colour": e.colour, "fields": fields, "footer": e.footer,
           "author": e.author, "timestamp": e.timestamp.as_ref().map(|t| t.to_string()), "thumbnail": e.thumbnail})
}
fn embeds(r: anyhow::Result<Vec<EmbedData>>) -> Result<Value> {
    Ok(Value::Array(r?.iter().map(embed).collect()))
}
fn rank_entries(v: &Value) -> Vec<ea::RankEntry> {
    arr(v).iter().map(|e| (i(&e[0]), i(&e[1]), s(&e[2]).into(), float(&e[3]), i(&e[4]))).collect()
}
fn sorted<I: IntoIterator<Item = String>>(it: I) -> Value {
    let mut v: Vec<String> = it.into_iter().collect();
    v.sort();
    json!(v)
}
/// "nan", "inf", "-inf" travel as strings: JSON has no such numbers.
fn float(v: &Value) -> f64 {
    match v {
        Value::String(t) => t.parse().unwrap_or_else(|_| panic!("harness: float {t}")),
        _ => v.as_f64().unwrap_or_else(|| panic!("harness: not a float: {v}")),
    }
}

/// Fake Codeforces: every GET answers the case's payload.
struct Fixed(Value);
impl Http for Fixed {
    fn get<'a>(&'a self, _: &'a str, _: &'a Params) -> BoxFuture<'a, Result<Value, ()>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

fn cf(payload: &Value, f: impl AsyncFnOnce(CodeforcesClient) -> Result<Value>) -> Result<Value> {
    let client = CodeforcesClient::with_timing(Arc::new(Fixed(payload.clone())), Duration::ZERO, 3, Duration::ZERO);
    tokio::runtime::Builder::new_current_thread().enable_time().build()?.block_on(f(client))
}

fn sorted_ids(set: HashSet<i64>) -> Value {
    let mut v: Vec<i64> = set.into_iter().collect();
    v.sort();
    json!(v)
}
fn reminder_json(r: &lightningccp::store::base::Reminder) -> Value {
    json!({"guild_id": r.guild_id, "channel_id": r.channel_id, "role_id": r.role_id, "before_mins": r.before_mins, "exclude": r.exclude})
}

/// One store method by name; Python twin: STORE_OPS in run_py.py. Results are plain JSON (sets sorted).
async fn store_op(st: &Store, op: &str, a: &[Value]) -> Result<Value> {
    let a = |n: usize| &a[n];
    Ok(match op {
        "link_handle" => json!(st.link_handle(i(a(0)), i(a(1)), s(a(2)), oi(a(3))).await?),
        "get_handle" => json!(st.get_handle(i(a(0)), i(a(1))).await?.map(|h| json!({"handle": h.handle, "rating": h.rating}))),
        "linked_handles" => json!(st.linked_handles(i(a(0))).await?),
        "user_for_handle" => json!(st.user_for_handle(i(a(0)), s(a(1))).await?),
        "unlink" => json!(st.unlink(i(a(0)), i(a(1))).await?),
        "save_pending" => json!(st.save_pending(i(a(0)), i(a(1)), s(a(2)), s(a(3)), i(a(4))).await?),
        "get_pending" => json!(st.get_pending(i(a(0)), i(a(1))).await?.map(|p| json!({"handle": p.handle, "problem_key": p.problem_key, "issued_at": p.issued_at}))),
        "delete_pending" => json!(st.delete_pending(i(a(0)), i(a(1))).await?),
        "set_reminder" => json!(st.set_reminder(i(a(0)), oi(a(1)), oi(a(2)), &ints(a(3)), s(a(4))).await?),
        "get_reminder" => json!(st.get_reminder(i(a(0))).await?.as_ref().map(reminder_json)),
        "all_reminders" => json!(st.all_reminders().await?.iter().map(reminder_json).collect::<Vec<_>>()),
        "clear_reminder" => json!(st.clear_reminder(i(a(0))).await?),
        "set_recap" => json!(st.set_recap(i(a(0)), i(a(1))).await?),
        "get_recap" => json!(st.get_recap(i(a(0))).await?),
        "all_recaps" => json!(st.all_recaps().await?),
        "clear_recap" => json!(st.clear_recap(i(a(0))).await?),
        "set_weekly" => json!(st.set_weekly(i(a(0)), i(a(1)), i(a(2))).await?),
        "get_weekly" => json!(st.get_weekly(i(a(0))).await?),
        "all_weeklies" => json!(st.all_weeklies().await?),
        "clear_weekly" => json!(st.clear_weekly(i(a(0))).await?),
        "mark_weekly" => json!(st.mark_weekly(i(a(0)), i(a(1))).await?),
        "set_newcontest" => json!(st.set_newcontest(i(a(0)), i(a(1)), &ints(a(2))).await?),
        "get_newcontest" => json!(st.get_newcontest(i(a(0))).await?),
        "all_newcontests" => json!(st.all_newcontests().await?),
        "clear_newcontest" => json!(st.clear_newcontest(i(a(0))).await?),
        "mark_posted" => json!(st.mark_posted(i(a(0)), i(a(1))).await?),
        "posted" => sorted_ids(st.posted(i(a(0))).await?),
        "mark_soon" => json!(st.mark_soon(i(a(0)), i(a(1))).await?),
        "soon_posted" => sorted_ids(st.soon_posted(i(a(0))).await?),
        "add_contest_remind" => json!(st.add_contest_remind(i(a(0)), i(a(1)), i(a(2))).await?),
        "contest_reminders" => json!(st.contest_reminders(i(a(0))).await?),
        "mark_sent" => json!(st.mark_sent(i(a(0)), i(a(1)), i(a(2))).await?),
        "sent_for_guild" => {
            let mut v: Vec<(i64, i64)> = st.sent_for_guild(i(a(0))).await?.into_iter().collect();
            v.sort();
            json!(v)
        }
        "mark_recapped" => json!(st.mark_recapped(i(a(0)), i(a(1))).await?),
        "recapped" => sorted_ids(st.recapped(i(a(0))).await?),
        other => bail!("harness: unknown store op {other}"),
    })
}

/// `SELECT * FROM <table> ORDER BY rowid` of every table, tables sorted by name.
fn dump(path: &std::path::Path) -> Result<Value> {
    let db = rusqlite::Connection::open(path)?;
    let tables: Vec<String> = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    let mut out = serde_json::Map::new();
    for t in tables {
        let mut stmt = db.prepare(&format!("SELECT * FROM \"{t}\" ORDER BY rowid"))?;
        let n = stmt.column_count();
        let rows: Vec<Value> = stmt
            .query_map([], |r| {
                Ok(Value::Array((0..n).map(|k| match r.get_ref_unwrap(k) {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => json!(v),
                    rusqlite::types::ValueRef::Real(v) => json!(v),
                    rusqlite::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    rusqlite::types::ValueRef::Blob(v) => json!(format!("blob:{}", v.iter().map(|b| format!("{b:02x}")).collect::<String>())),
                }).collect()))
            })?
            .collect::<Result<_, _>>()?;
        out.insert(t, Value::Array(rows));
    }
    Ok(Value::Object(out))
}

/// A scenario: `[[method, args...], ...]` on a fresh temp DB. Result: one entry per op (value, or {"err": ...}) and the table dump.
fn store_run(ops: &Value) -> Result<Value> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!("parity-rs-{}-{}.db", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let _ = std::fs::remove_file(&path);
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build()?;
    let results = rt.block_on(async {
        let st = Store::connect(&path).await?;
        let mut results = Vec::new();
        for op in arr(ops) {
            let r = store_op(&st, s(&op[0]), &arr(op)[1..]).await;
            results.push(match r {
                Ok(v) => json!({"ok": v}),
                Err(e) if is_constraint_violation(&e) => json!({"err": "constraint"}),
                Err(e) => match e.downcast_ref::<BotError>() {
                    Some(b) => json!({"err": "bot", "text": b.user_message()}),
                    None => json!({"err": "other"}),
                },
            });
        }
        st.close().await?;
        anyhow::Ok(results)
    })?;
    let d = dump(&path)?;
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
    Ok(json!({"results": results, "dump": d}))
}

fn call(name: &str, a: &Value) -> Result<Value> {
    Ok(match name {
        "esc" => json!(esc(s(&a[0]))),
        "fmt_g" => json!(fmt_g(float(&a[0]))),
        "casefold" => json!(lb::casefold(s(&a[0]))),
        "py_int" => json!(lb::py_int(s(&a[0]))),
        "parse_digits" => json!(lb::parse_digits(s(&a[0]))),
        "parse_tags" => sorted(lb::parse_tags(os(&a[0]))),
        "normalize_tags" => json!(lb::normalize_tags(os(&a[0]))),
        "tag_suggestions" => {
            let tags: Vec<String> = arr(&a[1]).iter().map(|t| s(t).to_string()).collect();
            json!(lb::tag_suggestions(s(&a[0]), &tags, i(&a[2]) as usize))
        }
        "format_hm" => json!(lb::format_hm(i(&a[0]))),
        "ordinal" => json!(lb::ordinal(i(&a[0]))),
        "placement" => json!(lb::placement(&ints(&a[0]), i(&a[1]))),
        "rating_series" => json!(lb::rating_series(&pairs(&a[0]))),
        "round_rating" => json!(lb::round_rating(oi(&a[0]))),
        "floor_rating" => json!(lb::floor_rating(i(&a[0]))),
        "utc_day" => json!(lb::utc_day(i(&a[0]))),
        "week_start" => json!(lb::week_start(i(&a[0]))),
        "daily_streak" => json!(lb::daily_streak(&ints(&a[0]), i(&a[1]))),
        "match_rating" => json!(lb::match_rating(oi(&a[0]), &ints(&a[1]))?),
        "rank_groups" => {
            let entries: Vec<(i64, Option<i64>)> = arr(&a[0]).iter().map(|p| (i(&p[0]), oi(&p[1]))).collect();
            json!(lb::rank_groups(&entries))
        }
        "solo_delta" => json!(lb::solo_delta(i(&a[0]), i(&a[1]), a[2].as_bool().expect("harness: bool"))),
        "duel_gain" => json!(lb::duel_gain(i(&a[0]))),
        "match_deltas" => {
            let groups: Vec<Vec<i64>> = arr(&a[0]).iter().map(ints).collect();
            let ratings: HashMap<i64, i64> = pairs(&a[1]).into_iter().collect();
            json!(lb::match_deltas(&groups, &ratings, float(&a[2]))?)
        }
        "has_compile_error" => json!(lb::has_compile_error(&subs(&a[0]), s(&a[1]), i(&a[2]), i(&a[3]))),
        "solved_keys" => sorted(lb::solved_keys(&subs(&a[0]))),
        "unsolved_keys" => json!(lb::unsolved_keys(&subs(&a[0]))),
        "weekly_board" => {
            let events: Vec<(i64, String, i64, i64)> = arr(&a[0]).iter().map(|e| (i(&e[0]), s(&e[1]).into(), i(&e[2]), i(&e[3]))).collect();
            let rows = lb::weekly_board(&events, i(&a[1]));
            Value::Array(rows.iter().map(|r| json!([r.rank, r.user_id, r.handle, r.rating, r.gain, r.moved])).collect())
        }
        "select_contests" => Value::Array(lb::select_contests(&contests(&a[0]), s(&a[1]), i(&a[2]) as usize)?.iter().map(contest_json).collect()),
        "refresh_minutes" => json!(lb::refresh_minutes(&contests(&a[0]))),
        "parse_minutes" => json!(lb::parse_minutes(s(&a[0])).map_err(anyhow::Error::from)?),
        "reminders_due" => {
            let sent: HashSet<(i64, i64)> = pairs(&a[2]).into_iter().collect();
            Value::Array(lb::reminders_due(&contests(&a[0]), &ints(&a[1]), &sent, i(&a[3])).iter().map(|(c, b)| json!([contest_json(c), b])).collect())
        }
        "new_contests" => Value::Array(lb::new_contests(&contests(&a[0]), &idset(&a[1]), i(&a[2])).iter().map(contest_json).collect()),
        "soon_contests" => Value::Array(lb::soon_contests(&contests(&a[0]), &idset(&a[1]), i(&a[2]), i(&a[3])).iter().map(contest_json).collect()),
        "recaps_due" => Value::Array(lb::recaps_due(&contests(&a[0]), &idset(&a[1]), i(&a[2])).iter().map(contest_json).collect()),
        "contest_ics" => json!(lb::contest_ics(&contest(&a[0]), i(&a[1]), i(&a[2]))?),
        "check_regex" => json!(check_regex(s(&a[0]))?),
        "not_excluded" => {
            let keep = idset(&a[2]);
            Value::Array(not_excluded(&contests(&a[0]), s(&a[1]), &keep)?.iter().map(|c| json!(c.id)).collect())
        }
        // Codeforces payload parsing: the client over a fake that answers `payload` to every request.
        "cf.user_info" => cf(&a[1], async |c| {
            let u = c.user_info(s(&a[0])).await?;
            Ok(json!({"handle": u.handle, "rating": u.rating, "max_rating": u.max_rating, "rank": u.rank}))
        })?,
        "cf.users_info" => cf(&a[1], async |c| {
            let hs: Vec<String> = arr(&a[0]).iter().map(|h| s(h).into()).collect();
            let us = c.users_info(&hs, 300).await?;
            Ok(Value::Array(us.iter().map(|u| json!({"handle": u.handle, "rating": u.rating, "max_rating": u.max_rating, "rank": u.rank})).collect()))
        })?,
        "cf.problemset" => cf(&a[0], async |c| {
            let ps = c.problemset().await?;
            Ok(Value::Array(ps.iter().map(|p| json!({"contest_id": p.contest_id, "index": p.index, "name": p.name, "rating": p.rating, "tags": p.tags})).collect()))
        })?,
        "cf.contests" => cf(&a[0], async |c| Ok(Value::Array(c.contests().await?.iter().map(contest_json).collect())))?,
        "cf.user_status" => cf(&a[3], async |c| {
            let rows = c.user_status(s(&a[0]), oi(&a[1]), oi(&a[2])).await?;
            Ok(Value::Array(rows.iter().map(|r| json!({"id": r.id, "created": r.created, "problem_key": r.problem_key, "verdict": r.verdict})).collect()))
        })?,
        "cf.standings" => cf(&a[2], async |c| {
            let hs: Vec<String> = arr(&a[1]).iter().map(|h| s(h).into()).collect();
            let rows = c.standings(i(&a[0]), &hs).await?;
            Ok(Value::Array(rows.iter().map(|r| json!({"rank": r.rank, "handles": r.handles, "points": r.points, "penalty": r.penalty})).collect()))
        })?,
        "cf.rating_history" => cf(&a[1], async |c| {
            let rows = c.rating_history(s(&a[0])).await?;
            Ok(Value::Array(rows.iter().map(|r| json!({"time": r.time, "rating": r.rating})).collect()))
        })?,
        // Embeds: the full embed (title, description, url, colour, fields, footer, author, timestamp, thumbnail) as JSON.
        "embed.problem" => embed(&ea::problem_embed(&problem(&a[0]), os(&a[1]))),
        "embed.daily" => {
            let r = &a[0];
            let row = DailyRow { guild_id: 1, user_id: 1, day: i(&r["day"]), problem_key: os(&r["problem_key"]).map(Into::into), rating: oi(&r["rating"]), issued_at: oi(&r["issued_at"]), solved_at: oi(&r["solved_at"]), match_id: None };
            embed(&ea::daily_embed(&ea::DailyView { row, problem: oproblem(&a[1]), delta: oi(&a[2]), streak: i(&a[3]) }))
        }
        "embed.profile" => {
            let u = &a[0];
            embed(&ea::profile_embed(&User { handle: s(&u["handle"]).into(), rating: oi(&u["rating"]), max_rating: oi(&u["max_rating"]), rank: os(&u["rank"]).map(Into::into) }, s(&a[1])))
        }
        "embed.member_profile" => embed(&ea::member_profile_embed(&ea::MemberProfile {
            display_name: s(&a[0]), rating: i(&a[1]), rank: oi(&a[2]), ranked: i(&a[3]), matches: i(&a[4]), contests: i(&a[5]), changes: &ints(&a[6]),
            peak: oi(&a[7]), streak: i(&a[8]), codeforces: s(&a[9]), contest_wins: i(&a[10]),
        })),
        "embed.contest_pages" => embeds(ea::contest_pages(&contests(&a[0]), s(&a[1]), i(&a[2]) as usize))?,
        "embed.new_contest" => embed(&ea::new_contest_embed(&contest(&a[0]), s(&a[1]))),
        "embed.weekly" => {
            let rows: Vec<WeeklyRow> = arr(&a[0]).iter().map(|r| WeeklyRow { rank: i(&r[0]), user_id: i(&r[1]), handle: s(&r[2]).into(), rating: i(&r[3]), gain: i(&r[4]), moved: oi(&r[5]) }).collect();
            embed(&ea::weekly_embed(&rows))
        }
        "embed.stats_pages" => {
            let rows: Vec<(i64, String, i64, i64)> = arr(&a[0]).iter().map(|r| (i(&r[0]), s(&r[1]).into(), i(&r[2]), i(&r[3]))).collect();
            let streaks: HashMap<i64, i64> = pairs(&a[2]).into_iter().collect();
            Value::Array(ea::stats_pages(&rows, i(&a[1]), &streaks, a[3].as_bool().expect("harness: bool"), s(&a[4])).iter().map(embed).collect())
        }
        "embed.ranklist" => embeds(ea::ranklist_pages(&rank_entries(&a[0]), s(&a[1]), i(&a[2]) as usize))?,
        "embed.handle_list" => {
            let es: Vec<(i64, String, Option<i64>, Option<String>)> = arr(&a[0]).iter().map(|e| (i(&e[0]), s(&e[1]).into(), oi(&e[2]), os(&e[3]).map(Into::into))).collect();
            embeds(ea::handle_list_pages(&es, i(&a[1]) as usize))?
        }
        "embed.recap" => embed(&ea::recap_embed(&contest(&a[0]), &rank_entries(&a[1]))?),
        "embed.stalk" => {
            let items: Vec<(String, Option<Problem>)> = arr(&a[1]).iter().map(|e| (s(&e[0]).into(), oproblem(&e[1]))).collect();
            embeds(ea::stalk_pages(s(&a[0]), &items, i(&a[2]) as usize))?
        }
        "store.run" => store_run(&a[0])?,
        other => bail!("harness: unknown fn {other}"),
    })
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut inputs: Vec<Box<dyn BufRead>> = Vec::new();
    for path in std::env::args().skip(1) {
        inputs.push(Box::new(std::io::BufReader::new(std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path}: {e}")))));
    }
    if inputs.is_empty() {
        inputs.push(Box::new(std::io::stdin().lock()));
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for input in inputs {
        for line in input.lines() {
            let line = line.expect("read");
            if line.trim().is_empty() {
                continue;
            }
            let case: Value = serde_json::from_str(&line).expect("case json");
            let result = catch_unwind(AssertUnwindSafe(|| call(s(&case["fn"]), &case["args"])));
            let mut row = json!({"id": case["id"]});
            match result {
                Ok(Ok(v)) => row["out"] = v,
                Ok(Err(e)) => {
                    row["err"] = match e.downcast_ref::<BotError>() {
                        Some(b) => json!({"kind": "bot", "text": b.user_message()}),
                        None => json!({"kind": "other"}),
                    }
                }
                Err(_) => row["err"] = json!({"kind": "panic"}),
            }
            writeln!(out, "{row}").expect("write");
        }
    }
}
