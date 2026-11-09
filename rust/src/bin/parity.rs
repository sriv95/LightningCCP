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
use lightningccp::ui::embeds::a::{esc, fmt_g};
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
