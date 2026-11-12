use std::borrow::Cow;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_json::value::RawValue;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

use super::models::{Contest, ContestResult, Problem, RatingChange, Standing, Submission, User};
use crate::errors::BotError;

pub const BASE_URL: &str = "https://codeforces.com/api/";
const STATUS_PAGES: usize = 5; // pages of submissions a `since` read may take
const UNREACHABLE: &str = "Codeforces is unreachable or down for maintenance.";

pub type Params = Vec<(&'static str, String)>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// GET `url` with `params`; the raw body whatever the HTTP status. A network failure or timeout is an
/// `Err`; a body that is not JSON is the client's to find, and it reports "unreachable" too.
pub trait Http: Send + Sync {
    fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Vec<u8>, ()>>;
}

pub struct ReqwestHttp(reqwest::Client);

impl ReqwestHttp {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self(reqwest::Client::builder().user_agent("LightningCCP").build()?))
    }
}

impl Http for ReqwestHttp {
    fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Vec<u8>, ()>> {
        Box::pin(async move {
            let resp = self.0.get(url).query(params).timeout(Duration::from_secs(15)).send().await.map_err(|_| ())?;
            let body = resp.bytes().await.map_err(|_| ())?; // content type ignored, like aiohttp content_type=None
            Ok(body.into())
        })
    }
}

pub struct CodeforcesClient {
    http: Arc<dyn Http>,
    interval: Duration,
    retries: usize,
    retry_delay: Duration,
    /// Held across each call so the 1 req/s spacing holds; the time of the last request.
    last_call: Mutex<Option<Instant>>,
}

fn unreachable_err() -> anyhow::Error {
    BotError::Codeforces(UNREACHABLE.into()).into()
}

impl CodeforcesClient {
    pub fn new(http: Arc<dyn Http>) -> Self {
        Self::with_timing(http, Duration::from_secs(1), 3, Duration::from_secs(2))
    }

    pub fn with_timing(http: Arc<dyn Http>, interval: Duration, retries: usize, retry_delay: Duration) -> Self {
        Self { http, interval, retries, retry_delay, last_call: Mutex::new(None) }
    }

    async fn call<T: DeserializeOwned>(&self, method: &str, params: Params) -> anyhow::Result<T> {
        self.call_with(method, params, |result| Ok(serde_json::from_str(result)?)).await
    }

    /// Sends the request and hands `parse` the raw `result` text of an OK reply, borrowed from the body: no
    /// `Value` tree for the big endpoints. Only the envelope is read here; `result` stays unparsed.
    async fn call_with<T>(&self, method: &str, params: Params, parse: impl Fn(&str) -> anyhow::Result<T>) -> anyhow::Result<T> {
        /// Python reads `status` and `comment` with `.get`; `comment` must be a str if read, absent is "".
        #[derive(Deserialize)]
        struct Envelope<'a> {
            #[serde(default)]
            status: Value,
            #[serde(default, deserialize_with = "some")]
            comment: Option<Value>, // `Some(Null)` (present, null) differs from `None` (absent)
            #[serde(default, borrow)]
            result: Option<&'a RawValue>, // null and absent are both "null"
        }
        fn some<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
            Value::deserialize(d).map(Some)
        }
        let url = format!("{BASE_URL}{method}");
        for attempt in 0..self.retries {
            let body = {
                let mut last = self.last_call.lock().await;
                if let Some(t) = *last {
                    let wait = self.interval.saturating_sub(t.elapsed());
                    if !wait.is_zero() {
                        sleep(wait).await;
                    }
                }
                *last = Some(Instant::now());
                self.http.get(&url, &params).await.map_err(|()| unreachable_err())?
            };
            // Python `payload.get`/`comment.lower()` raise AttributeError on a non-dict payload or a
            // non-str comment: not a BotError, so the cogs reply "Something went wrong". Not JSON at
            // all is "unreachable". A struct would also read a JSON array as a sequence: check for `{`.
            let envelope = if body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
                serde_json::from_slice::<Envelope>(&body).map_err(|_| unreachable_err())?
            } else if serde_json::from_slice::<serde::de::IgnoredAny>(&body).is_ok() {
                anyhow::bail!("Codeforces payload is not a JSON object")
            } else {
                return Err(unreachable_err());
            };
            if envelope.status.as_str() == Some("OK") {
                return parse(envelope.result.map_or("null", |r| r.get()));
            }
            let comment = match &envelope.comment {
                None => "",
                Some(Value::String(c)) => c.as_str(),
                Some(_) => anyhow::bail!("Codeforces comment is not a string"),
            };
            let lowered = comment.to_lowercase();
            if lowered.contains("limit exceeded") {
                if attempt + 1 < self.retries {
                    sleep(self.retry_delay).await;
                }
                continue;
            }
            if lowered.contains("not found") && lowered.starts_with("handle") {
                return Err(BotError::handle_not_found().into());
            }
            let comment = if comment.is_empty() { "unknown" } else { comment };
            return Err(BotError::Codeforces(format!("Codeforces error: {comment}")).into());
        }
        Err(BotError::Codeforces("Codeforces rate limit hit. Try again shortly.".into()).into())
    }

    pub async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        let result: Value = self.call("user.info", vec![("handles", handle.into())]).await?;
        // Python `len(result)` on list, dict or str (a null or number raises TypeError: not a BotError)
        let len = match &result {
            Value::Array(a) => a.len(),
            Value::Object(o) => o.len(),
            Value::String(s) => s.chars().count(),
            _ => anyhow::bail!("user.info result has no length"),
        };
        if len != 1 {
            // ";" in the handle makes user.info treat it as several handles
            return Err(BotError::handle_not_found().into());
        }
        let [user]: [User; 1] = serde_json::from_value(result)?;
        Ok(user)
    }

    /// Profiles of the given handles; handles Codeforces does not know are skipped.
    pub async fn users_info(&self, handles: &[String], chunk: usize) -> anyhow::Result<Vec<User>> {
        let mut out = Vec::new();
        for part in handles.chunks(chunk) {
            out.extend(self.users_info_part(part).await?);
        }
        Ok(out)
    }

    fn users_info_part<'a>(&'a self, handles: &'a [String]) -> BoxFuture<'a, anyhow::Result<Vec<User>>> {
        Box::pin(async move {
            match self.call("user.info", vec![("handles", handles.join(";"))]).await {
                Err(e) if matches!(e.downcast_ref::<BotError>(), Some(BotError::HandleNotFound(_))) => {
                    // One renamed or deleted handle fails the whole call: split to find and drop it.
                    if handles.len() == 1 {
                        return Ok(Vec::new());
                    }
                    let (a, b) = handles.split_at(handles.len() / 2);
                    let mut out = self.users_info_part(a).await?;
                    out.extend(self.users_info_part(b).await?);
                    Ok(out)
                }
                other => other,
            }
        })
    }

    /// `call` for a result that must be a JSON object: serde would also build a struct from an array, Python raises.
    async fn call_object<T>(&self, method: &str, params: Params, parse: impl Fn(&str) -> anyhow::Result<T>) -> anyhow::Result<T> {
        self.call_with(method, params, |result| {
            anyhow::ensure!(result.trim_start().starts_with('{'), "Codeforces result is not a JSON object");
            parse(result)
        })
        .await
    }

    pub async fn problemset(&self) -> anyhow::Result<Vec<Problem>> {
        #[derive(Deserialize)]
        struct Data {
            problems: Vec<Problem>,
        }
        let data: Data = self.call_object("problemset.problems", vec![], |r| Ok(serde_json::from_str(r)?)).await?;
        Ok(data.problems)
    }

    pub async fn contests(&self) -> anyhow::Result<Vec<Contest>> {
        self.call("contest.list", vec![]).await
    }

    /// The newest `count` submissions (all when None). With `since`, keeps reading older pages of
    /// `count` until one reaches back before that time (at most STATUS_PAGES pages), so a busy
    /// player's early submissions of a contest are not cut off.
    pub async fn user_status(&self, handle: &str, count: Option<i64>, since: Option<i64>) -> anyhow::Result<Vec<Submission>> {
        #[derive(Deserialize)]
        struct Row {
            id: i64,
            #[serde(rename = "creationTimeSeconds")]
            created: i64,
            problem: serde_json::Value, // raw: a missing contestId and a null one differ in Python
            verdict: Option<String>,
        }
        let n = count.unwrap_or(0); // Python `count or 0`: None and 0 are the same here
        let pages = if since.is_some() && n != 0 { STATUS_PAGES } else { 1 };
        let mut out = Vec::new();
        for page in 0..pages {
            let mut params: Params = vec![("handle", handle.into()), ("from", (1 + page as i64 * n).to_string())];
            if let Some(c) = count {
                params.push(("count", c.to_string()));
            }
            let rows: Vec<Row> = self.call("user.status", params).await?;
            let got = rows.len() as i64;
            let reached_back = rows.iter().any(|r| r.created < since.unwrap_or(0));
            for r in rows {
                // Python f"{p.get('contestId', '')}{p['index']}": missing is "", null is "None"
                let contest = match r.problem.get("contestId") {
                    None => String::new(),
                    Some(serde_json::Value::Null) => "None".into(),
                    Some(serde_json::Value::String(s)) => s.clone(),
                    Some(v) => v.to_string(),
                };
                let index = r.problem.get("index").and_then(|i| i.as_str()).ok_or_else(|| anyhow::anyhow!("missing problem index"))?;
                out.push(Submission { id: r.id, created: r.created, problem_key: format!("{contest}{index}"), verdict: r.verdict });
            }
            if got < n || reached_back {
                break;
            }
        }
        Ok(out)
    }

    /// Official standing rows with any of the given handles, in rank order. Codeforces only serves
    /// non-admin users a bare `contest.standings?contestId=<id>`, so this fetches every row and
    /// filters here; unknown handles simply match nothing.
    pub async fn standings(&self, contest_id: i64, handles: &[String]) -> anyhow::Result<Vec<Standing>> {
        #[derive(Deserialize)]
        struct Member<'a> {
            #[serde(borrow)]
            handle: Cow<'a, str>,
        }
        #[derive(Deserialize)]
        struct Party<'a> {
            #[serde(borrow)]
            members: Vec<Member<'a>>,
        }
        #[derive(Deserialize)]
        struct Row<'a> {
            #[serde(borrow)]
            party: Party<'a>,
            // Read only on matched rows, like Python: an unmatched row may lack them or hold anything,
            // so they stay raw text until a row matches.
            #[serde(default, borrow)]
            rank: Option<&'a RawValue>,
            #[serde(default, borrow)]
            points: Option<&'a RawValue>,
            #[serde(default, borrow)]
            penalty: Option<&'a RawValue>,
        }
        #[derive(Deserialize)]
        struct Data<'a> {
            #[serde(borrow)]
            rows: Vec<Row<'a>>,
        }
        fn field(raw: Option<&RawValue>) -> anyhow::Result<Value> {
            raw.map_or(Ok(Value::Null), |r| Ok(serde_json::from_str(r.get())?))
        }
        let wanted: Vec<String> = handles.iter().map(|h| h.to_lowercase()).collect();
        let parse = |result: &str| {
            let data: Data = serde_json::from_str(result)?;
            data.rows
                .into_iter()
                .filter(|r| r.party.members.iter().any(|m| wanted.contains(&m.handle.to_lowercase())))
                .map(|r| {
                    let bad = || anyhow::anyhow!("malformed standings row");
                    Ok(Standing {
                        rank: field(r.rank)?.as_i64().ok_or_else(bad)?,
                        handles: r.party.members.into_iter().map(|m| m.handle.into_owned()).collect(),
                        points: field(r.points)?.as_f64().ok_or_else(bad)?,
                        penalty: field(r.penalty)?.as_i64().ok_or_else(bad)?,
                    })
                })
                .collect()
        };
        self.call_object("contest.standings", vec![("contestId", contest_id.to_string())], parse).await
    }

    /// A member missing from the capped standings: (real rank from `user.rating`, ICPC (solved, penalty) from
    /// `contest.status` when `icpc`). `None` when the member has no rating entry for the contest.
    pub async fn member_result(&self, contest_id: i64, handle: &str, icpc: bool) -> anyhow::Result<Option<(i64, Option<(i64, i64)>)>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Entry {
            contest_id: i64,
            rank: i64,
        }
        let history: Vec<Entry> = self.call("user.rating", vec![("handle", handle.into())]).await?;
        let Some(rank) = history.iter().find(|e| e.contest_id == contest_id).map(|e| e.rank) else { return Ok(None) };
        if !icpc {
            return Ok(Some((rank, None)));
        }
        let params = vec![("contestId", contest_id.to_string()), ("handle", handle.into())];
        let subs: Vec<IcpcSub> = self.call("contest.status", params).await?;
        Ok(Some((rank, Some(icpc_score(subs)))))
    }

    /// Results of the given handles from `contest.standings` (bare call, filtered here; team rows give one per member).
    pub async fn standings_results(&self, contest_id: i64, handles: &[String]) -> anyhow::Result<Vec<ContestResult>> {
        let wanted: Vec<String> = handles.iter().map(|h| h.to_lowercase()).collect();
        let parse = |result: &str| {
            #[derive(Deserialize)]
            struct Data<'a> {
                #[serde(borrow)]
                rows: Vec<&'a RawValue>,
            }
            let data: Data = serde_json::from_str(result)?;
            let mut out = Vec::new();
            for raw in data.rows {
                // Cheap pre-filter: only matched rows are parsed in full, like `standings`.
                #[derive(Deserialize)]
                struct Probe<'a> {
                    #[serde(borrow)]
                    party: PartyProbe<'a>,
                }
                #[derive(Deserialize)]
                struct PartyProbe<'a> {
                    #[serde(borrow)]
                    members: Vec<MemberProbe<'a>>,
                }
                #[derive(Deserialize)]
                struct MemberProbe<'a> {
                    #[serde(borrow)]
                    handle: Cow<'a, str>,
                }
                let probe: Probe = serde_json::from_str(raw.get())?;
                if probe.party.members.iter().any(|m| wanted.contains(&m.handle.to_lowercase())) {
                    let row: StandingsRow = serde_json::from_str(raw.get())?;
                    out.extend(standing_results(row).into_iter().filter(|r| wanted.contains(&r.handle.to_lowercase())));
                }
            }
            Ok(out)
        };
        self.call_object("contest.standings", vec![("contestId", contest_id.to_string())], parse).await
    }

    /// One handle's result from `contest.status`; `None` when it has no CONTESTANT/OUT_OF_COMPETITION submission.
    pub async fn status_result(&self, contest_id: i64, handle: &str) -> anyhow::Result<Option<ContestResult>> {
        let params = vec![("contestId", contest_id.to_string()), ("handle", handle.into())];
        let subs: Vec<IcpcSub> = self.call("contest.status", params).await?;
        Ok(submissions_result(handle, subs))
    }

    /// (contest_id, real rank) for each entry of `user.rating`.
    pub async fn contest_ranks(&self, handle: &str) -> anyhow::Result<Vec<(i64, i64)>> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Entry {
            contest_id: i64,
            rank: i64,
        }
        let history: Vec<Entry> = self.call("user.rating", vec![("handle", handle.into())]).await?;
        Ok(history.into_iter().map(|e| (e.contest_id, e.rank)).collect())
    }

    pub async fn rating_history(&self, handle: &str) -> anyhow::Result<Vec<RatingChange>> {
        self.call("user.rating", vec![("handle", handle.into())]).await
    }
}

/// One `contest.status` submission, as far as the ICPC score needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IcpcSub {
    creation_time_seconds: i64,
    relative_time_seconds: i64,
    verdict: Option<String>,
    #[serde(default)]
    passed_test_count: i64,
    problem: IcpcProblem,
    author: IcpcAuthor,
}
#[derive(Deserialize)]
struct IcpcProblem {
    index: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IcpcAuthor {
    participant_type: String,
}

/// (solved, penalty minutes) like the official ICPC standings: only CONTESTANT submissions; per problem the
/// first OK solves it, costing its minute plus 10 per earlier rejected attempt (not compile errors, not
/// attempts that passed 0 tests); later attempts are ignored.
pub fn icpc_score(subs: Vec<IcpcSub>) -> (i64, i64) {
    let (solved, penalty, _) = score(subs, &["CONTESTANT"]);
    (solved, penalty)
}

/// (solved, penalty, contest-relative seconds of the last accepted) over submissions by the `types` participants.
fn score(mut subs: Vec<IcpcSub>, types: &[&str]) -> (i64, i64, Option<i64>) {
    subs.retain(|s| types.contains(&s.author.participant_type.as_str()));
    subs.sort_by_key(|s| s.creation_time_seconds);
    let mut rejected: std::collections::HashMap<String, i64> = Default::default();
    let mut solved: std::collections::HashSet<String> = Default::default();
    let mut penalty = 0;
    let mut last_ac: Option<i64> = None;
    for s in &subs {
        if solved.contains(&s.problem.index) {
            continue;
        }
        match s.verdict.as_deref() {
            Some("OK") => {
                penalty += s.relative_time_seconds / 60 + 10 * rejected.get(&s.problem.index).copied().unwrap_or(0);
                solved.insert(s.problem.index.clone());
                last_ac = last_ac.max(Some(s.relative_time_seconds));
            }
            Some("COMPILATION_ERROR") | None => {}
            Some(_) if s.passed_test_count == 0 => {}
            Some(_) => *rejected.entry(s.problem.index.clone()).or_default() += 1,
        }
    }
    (solved.len() as i64, penalty, last_ac)
}

/// A handle's result from its `contest.status` submissions (CONTESTANT and OUT_OF_COMPETITION only, the ICPC
/// penalty rule of [`icpc_score`]). `points` is the solved count: the status feed has no CF points.
/// `None` when no such submission exists. Type is CONTESTANT if any submission is one, else OUT_OF_COMPETITION.
pub fn submissions_result(handle: &str, subs: Vec<IcpcSub>) -> Option<ContestResult> {
    let any = |t: &str| subs.iter().any(|s| s.author.participant_type == t);
    let participant_type = if any("CONTESTANT") {
        "CONTESTANT"
    } else if any("OUT_OF_COMPETITION") {
        "OUT_OF_COMPETITION"
    } else {
        return None;
    };
    let (solved, penalty, last_ac) = score(subs, &["CONTESTANT", "OUT_OF_COMPETITION"]);
    Some(ContestResult { handle: handle.into(), points: solved as f64, penalty, solved, last_ac, participant_type: participant_type.into() })
}

/// One `contest.standings` row, as far as [`standing_results`] needs.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandingsRow {
    party: RowParty,
    points: f64,
    penalty: i64,
    #[serde(default)]
    problem_results: Vec<ProblemResult>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RowParty {
    members: Vec<RowMember>,
    participant_type: String,
}
#[derive(Deserialize)]
struct RowMember {
    handle: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProblemResult {
    points: f64,
    best_submission_time_seconds: Option<i64>,
}

/// One result per party member: solved = problems with points > 0, last AC = latest best submission time of those.
pub fn standing_results(row: StandingsRow) -> Vec<ContestResult> {
    let solved_rows = || row.problem_results.iter().filter(|p| p.points > 0.0);
    let solved = solved_rows().count() as i64;
    let last_ac = solved_rows().filter_map(|p| p.best_submission_time_seconds).max();
    row.party
        .members
        .iter()
        .map(|m| ContestResult {
            handle: m.handle.clone(),
            points: row.points,
            penalty: row.penalty,
            solved,
            last_ac,
            participant_type: row.party.participant_type.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;

    type Calls = Arc<StdMutex<Vec<(String, Params)>>>;

    /// Fake HTTP layer: queued responses in order (`Err` = network failure or non-JSON body).
    struct FakeHttp {
        responses: StdMutex<VecDeque<Result<Vec<u8>, ()>>>,
        calls: Calls,
    }

    impl Http for FakeHttp {
        fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Vec<u8>, ()>> {
            self.calls.lock().unwrap().push((url.to_string(), params.clone()));
            let r = self.responses.lock().unwrap().pop_front().expect("no queued response");
            Box::pin(async move { r })
        }
    }

    fn raw(v: Value) -> Result<Vec<u8>, ()> {
        Ok(serde_json::to_vec(&v).unwrap())
    }

    fn ok(result: Value) -> Result<Vec<u8>, ()> {
        raw(json!({"status": "OK", "result": result}))
    }

    fn failed(comment: &str) -> Result<Vec<u8>, ()> {
        raw(json!({"status": "FAILED", "comment": comment}))
    }

    fn make_client(responses: Vec<Result<Vec<u8>, ()>>) -> (CodeforcesClient, Calls) {
        let calls = Calls::default();
        let http = FakeHttp { responses: StdMutex::new(responses.into()), calls: calls.clone() };
        let client = CodeforcesClient::with_timing(Arc::new(http), Duration::ZERO, 3, Duration::ZERO);
        (client, calls)
    }

    fn params_of(calls: &Calls, i: usize) -> Vec<(String, String)> {
        calls.lock().unwrap()[i].1.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    fn p(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn is_handle_not_found(e: &anyhow::Error) -> bool {
        matches!(e.downcast_ref::<BotError>(), Some(BotError::HandleNotFound(_)))
    }

    fn is_cf_error(e: &anyhow::Error) -> bool {
        matches!(e.downcast_ref::<BotError>(), Some(BotError::Codeforces(_)))
    }

    fn sub(t: i64, rel: i64, v: &str, passed: i64, idx: &str, who: &str) -> IcpcSub {
        serde_json::from_value(json!({"creationTimeSeconds": t, "relativeTimeSeconds": rel, "verdict": v,
            "passedTestCount": passed, "problem": {"index": idx}, "author": {"participantType": who}}))
        .unwrap()
    }

    #[test]
    fn icpc_score_rules() {
        let subs = vec![
            sub(5, 3000, "OK", 9, "A", "CONTESTANT"),             // 50 min, 2 rejected before: +20
            sub(1, 600, "WRONG_ANSWER", 3, "A", "CONTESTANT"),    // counts
            sub(2, 900, "COMPILATION_ERROR", 0, "A", "CONTESTANT"), // not penalized
            sub(3, 1200, "WRONG_ANSWER", 0, "A", "CONTESTANT"),   // passed 0: not penalized
            sub(4, 1500, "TIME_LIMIT_EXCEEDED", 7, "A", "CONTESTANT"), // counts
            sub(6, 3600, "WRONG_ANSWER", 1, "A", "CONTESTANT"),   // after AC: ignored
            sub(7, 100, "OK", 9, "B", "PRACTICE"),                // not a contestant
            sub(8, 120, "OK", 9, "C", "VIRTUAL"),
        ];
        assert_eq!(icpc_score(subs), (1, 50 + 20));
    }

    #[test]
    fn submissions_result_solved_and_last_ac() {
        let subs = vec![
            sub(1, 600, "WRONG_ANSWER", 3, "A", "CONTESTANT"),
            sub(5, 3000, "OK", 9, "A", "CONTESTANT"), // A: 50 + 10
            sub(2, 900, "COMPILATION_ERROR", 0, "B", "CONTESTANT"),
            sub(3, 1200, "WRONG_ANSWER", 0, "B", "CONTESTANT"), // not penalized
            sub(4, 1500, "OK", 9, "B", "CONTESTANT"),           // B: 25
            sub(6, 4000, "WRONG_ANSWER", 2, "A", "CONTESTANT"), // after AC
            sub(7, 100, "OK", 9, "C", "PRACTICE"),
        ];
        let r = submissions_result("Bob", subs).unwrap();
        assert_eq!((r.solved, r.penalty, r.last_ac, r.participant_type.as_str()), (2, 85, Some(3000), "CONTESTANT"));
        assert_eq!(r.points, 2.0);
        let ooc = submissions_result("Bob", vec![sub(1, 60, "OK", 9, "A", "OUT_OF_COMPETITION")]).unwrap();
        assert_eq!((ooc.solved, ooc.penalty, ooc.participant_type.as_str()), (1, 1, "OUT_OF_COMPETITION"));
        assert!(submissions_result("Bob", vec![sub(1, 60, "OK", 9, "A", "VIRTUAL")]).is_none());
        let none = submissions_result("Bob", vec![sub(1, 60, "WRONG_ANSWER", 4, "A", "CONTESTANT")]).unwrap();
        assert_eq!((none.solved, none.last_ac), (0, None));
    }

    #[tokio::test]
    async fn standings_results_parse_rows() {
        let row = |h: &[&str], pts: f64, pen: i64, kind: &str, probs: Value| {
            json!({"party": {"members": h.iter().map(|x| json!({"handle": x})).collect::<Vec<_>>(), "participantType": kind},
                   "rank": 1, "points": pts, "penalty": pen, "problemResults": probs})
        };
        let rows = json!({"rows": [
            row(&["Ann"], 1500.0, 0, "CONTESTANT", json!([
                {"points": 500.0, "bestSubmissionTimeSeconds": 600},
                {"points": 0.0, "rejectedAttemptCount": 2},
                {"points": 1000.0, "bestSubmissionTimeSeconds": 4000}])),
            row(&["bob"], 0.0, 0, "OUT_OF_COMPETITION", json!([{"points": 0.0}])),
            row(&["Zed"], 9.0, 0, "CONTESTANT", json!([])),
        ]});
        let (client, _) = make_client(vec![ok(rows)]);
        let got = client.standings_results(1, &["ann".into(), "BOB".into()]).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].handle.as_str(), got[0].solved, got[0].last_ac, got[0].points), ("Ann", 2, Some(4000), 1500.0));
        assert_eq!((got[1].solved, got[1].last_ac, got[1].participant_type.as_str()), (0, None, "OUT_OF_COMPETITION"));
    }

    #[tokio::test]
    async fn contest_ranks_reads_user_rating() {
        let (client, _) = make_client(vec![ok(json!([{"contestId": 5, "rank": 12, "newRating": 1500, "ratingUpdateTimeSeconds": 1}]))]);
        assert_eq!(client.contest_ranks("x").await.unwrap(), vec![(5, 12)]);
    }

    #[tokio::test]
    async fn user_info_parses() {
        let (c, calls) = make_client(vec![ok(json!([{"handle": "Tourist", "rating": 3800, "maxRating": 3979, "rank": "legendary grandmaster"}]))]);
        let u = c.user_info("tourist").await.unwrap();
        assert_eq!((u.handle.as_str(), u.rating, u.max_rating, u.rank.as_deref()), ("Tourist", Some(3800), Some(3979), Some("legendary grandmaster")));
        assert!(calls.lock().unwrap()[0].0.ends_with("user.info"));
        assert_eq!(params_of(&calls, 0), p(&[("handles", "tourist")]));
    }

    /// Python `payload.get` raises AttributeError on a non-dict payload: not a BotError, so the cog's
    /// generic "Something went wrong" reply. Same for a non-str `comment` (`.lower()`).
    #[tokio::test]
    async fn malformed_payload_is_not_a_bot_error() {
        for bad in [json!([1]), json!("x"), json!(7), json!(null), json!({"status": "FAILED", "comment": null}), json!({"status": "FAILED", "comment": 5}), json!(["OK", null, []])] {
            let (c, _) = make_client(vec![raw(bad.clone())]);
            let e = c.user_info("a").await.unwrap_err();
            assert!(e.downcast_ref::<BotError>().is_none(), "{bad}: {e}");
        }
        // missing comment is still "unknown"; an OK payload with no result (KeyError) is not a BotError
        let (c, _) = make_client(vec![raw(json!({"status": "FAILED"}))]);
        assert_eq!(c.user_info("a").await.unwrap_err().to_string(), "Codeforces error: unknown");
        let (c, _) = make_client(vec![raw(json!({"status": "OK"}))]);
        assert!(c.user_info("a").await.unwrap_err().downcast_ref::<BotError>().is_none());
    }

    /// serde builds a struct from an array (`[[]]` read as `{problems: []}`); Python indexes a list by name and raises.
    #[tokio::test]
    async fn array_where_an_object_belongs_is_rejected() {
        let (c, _) = make_client(vec![ok(json!([[]]))]);
        assert!(c.problemset().await.unwrap_err().downcast_ref::<BotError>().is_none());
        let (c, _) = make_client(vec![ok(json!([[]]))]);
        assert!(c.standings(1, &["a".to_string()]).await.unwrap_err().downcast_ref::<BotError>().is_none());
    }

    /// Python checks `len(result) != 1` before reading any field.
    #[tokio::test]
    async fn user_info_wrong_length_is_handle_not_found_even_if_malformed() {
        let (c, _) = make_client(vec![ok(json!([1, 2])), ok(json!({}))]);
        assert!(is_handle_not_found(&c.user_info("a;b").await.unwrap_err()));
        assert!(is_handle_not_found(&c.user_info("a;b").await.unwrap_err()));
    }

    #[tokio::test]
    async fn user_info_unrated_user() {
        let (c, _) = make_client(vec![ok(json!([{"handle": "newbie"}]))]);
        let u = c.user_info("newbie").await.unwrap();
        assert!(u.rating.is_none() && u.rank.is_none());
    }

    #[tokio::test]
    async fn handle_not_found_maps_to_error() {
        let (c, _) = make_client(vec![failed("handles: User with handle nobody not found")]);
        let e = c.user_info("nobody").await.unwrap_err();
        assert!(is_handle_not_found(&e));
        assert_eq!(e.to_string(), "No Codeforces user matches that handle.");
    }

    #[tokio::test]
    async fn retry_on_rate_limit_then_success() {
        let (c, calls) = make_client(vec![failed("Call limit exceeded"), ok(json!([{"handle": "a"}]))]);
        assert_eq!(c.user_info("a").await.unwrap().handle, "a");
        assert_eq!(calls.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_backs_off_2s_and_requests_are_spaced_1s() {
        let calls = Calls::default();
        let responses = vec![failed("Call limit exceeded"), ok(json!([{"handle": "a"}])), ok(json!([{"handle": "a"}]))];
        let http = FakeHttp { responses: StdMutex::new(responses.into()), calls: calls.clone() };
        let c = CodeforcesClient::new(Arc::new(http));
        let t = Instant::now();
        c.user_info("a").await.unwrap();
        // first call is immediate, then 2 s retry delay (which also covers the 1 s spacing)
        assert_eq!(t.elapsed(), Duration::from_secs(2));
        c.user_info("a").await.unwrap();
        assert_eq!(t.elapsed(), Duration::from_secs(3)); // next request waits out the 1 s spacing
    }

    #[tokio::test]
    async fn rate_limit_exhausted_raises() {
        let (c, calls) = make_client((0..3).map(|_| failed("Call limit exceeded")).collect());
        let e = c.user_info("a").await.unwrap_err();
        assert!(is_cf_error(&e));
        assert_eq!(e.to_string(), "Codeforces rate limit hit. Try again shortly.");
        assert_eq!(calls.lock().unwrap().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn calls_are_spaced_one_second_and_no_sleep_after_last_attempt() {
        let responses: Vec<_> = (0..3).map(|_| failed("Call limit exceeded")).collect();
        let http = FakeHttp { responses: StdMutex::new(responses.into()), calls: Calls::default() };
        let c = CodeforcesClient::new(Arc::new(http));
        let t = Instant::now();
        c.contests().await.unwrap_err();
        // attempts at 0 s, 2 s, 4 s; no retry sleep after the third
        assert_eq!(t.elapsed(), Duration::from_secs(4));
    }

    #[tokio::test]
    async fn other_failure_raises_codeforces_error() {
        let (c, _) = make_client(vec![failed("Something odd")]);
        let e = c.contests().await.unwrap_err();
        assert!(is_cf_error(&e));
        assert_eq!(e.to_string(), "Codeforces error: Something odd");
        let (c, _) = make_client(vec![raw(json!({"status": "FAILED"}))]);
        assert_eq!(c.contests().await.unwrap_err().to_string(), "Codeforces error: unknown");
    }

    #[tokio::test]
    async fn non_json_body_maps_to_codeforces_error() {
        // maintenance pages are HTML; a network failure is the HTTP layer's Err; truncated JSON too
        for body in [Err(()), Ok(b"<html>down</html>".to_vec()), Ok(br#"{"status": "OK", "result": [1,"#.to_vec()), Ok(b"".to_vec())] {
            let (c, _) = make_client(vec![body]);
            let e = c.problemset().await.unwrap_err();
            assert!(is_cf_error(&e));
            assert_eq!(e.to_string(), "Codeforces is unreachable or down for maintenance.");
        }
    }

    #[tokio::test]
    async fn problemset_parses_and_keeps_unrated() {
        let (c, _) = make_client(vec![ok(json!({"problems": [
            {"contestId": 1700, "index": "A", "name": "Foo", "rating": 800, "tags": ["math"]},
            {"contestId": 1700, "index": "B", "name": "Bar", "tags": []},
        ], "problemStatistics": []}))]);
        let ps = c.problemset().await.unwrap();
        assert_eq!(ps.iter().map(|p| p.key()).collect::<Vec<_>>(), ["1700A", "1700B"]);
        assert!(ps[0].rating == Some(800) && ps[0].tags == ["math"]);
        assert!(ps[1].rating.is_none());
        assert_eq!(ps[0].url(), "https://codeforces.com/problemset/problem/1700/A");
    }

    #[tokio::test]
    async fn contests_parses_missing_start_time() {
        let (c, _) = make_client(vec![ok(json!([
            {"id": 1, "name": "R1", "type": "CF", "phase": "BEFORE", "durationSeconds": 7200, "startTimeSeconds": 1000},
            {"id": 2, "name": "R2", "type": "CF", "phase": "BEFORE", "durationSeconds": 60},
        ]))]);
        let cs = c.contests().await.unwrap();
        assert!(cs[0].start_time == Some(1000) && cs[0].duration == 7200);
        assert!(cs[1].start_time.is_none());
        assert_eq!(cs[0].url(), "https://codeforces.com/contest/1");
    }

    #[tokio::test]
    async fn user_status_params_and_parse() {
        let (c, calls) = make_client(vec![
            ok(json!([
                {"id": 9, "creationTimeSeconds": 500, "problem": {"contestId": 1700, "index": "A", "name": "x"}, "verdict": "OK"},
                {"id": 8, "creationTimeSeconds": 400, "problem": {"contestId": 1700, "index": "B", "name": "y"}},
            ])),
            ok(json!([])),
        ]);
        let subs = c.user_status("a", Some(10), None).await.unwrap();
        assert_eq!(params_of(&calls, 0), p(&[("handle", "a"), ("from", "1"), ("count", "10")]));
        assert!(subs[0].problem_key == "1700A" && subs[0].verdict.as_deref() == Some("OK"));
        assert!(subs[1].verdict.is_none());
        c.user_status("a", None, None).await.unwrap();
        assert_eq!(params_of(&calls, 1), p(&[("handle", "a"), ("from", "1")]));
    }

    #[tokio::test]
    async fn user_status_without_contest_id_has_bare_index() {
        let (c, _) = make_client(vec![ok(json!([{"id": 1, "creationTimeSeconds": 1, "problem": {"index": "Z"}}]))]);
        assert_eq!(c.user_status("a", None, None).await.unwrap()[0].problem_key, "Z");
    }

    #[tokio::test]
    async fn user_status_null_contest_id_is_none_string() {
        // Python (.venv/bin/python -I): f"{ {'contestId': None, 'index': 'Z'}.get('contestId', '') }Z" == "NoneZ"
        let (c, _) = make_client(vec![ok(json!([{"id": 1, "creationTimeSeconds": 1, "problem": {"contestId": null, "index": "Z"}}]))]);
        assert_eq!(c.user_status("a", None, None).await.unwrap()[0].problem_key, "NoneZ");
    }

    #[tokio::test]
    async fn user_info_semicolon_handle_is_not_found() {
        let (c, _) = make_client(vec![ok(json!([{"handle": "a"}, {"handle": "b"}]))]);
        assert!(is_handle_not_found(&c.user_info("a;b").await.unwrap_err()));
    }

    fn standing_row(rank: i64, handles: &[&str], points: f64, penalty: i64) -> Value {
        let members: Vec<Value> = handles.iter().map(|h| json!({"handle": h})).collect();
        json!({"party": {"members": members}, "rank": rank, "points": points, "penalty": penalty})
    }

    #[tokio::test]
    async fn standings_sends_only_contest_id_and_filters_handles() {
        let rows = json!({"rows": [
            standing_row(1, &["x"], 1500.0, 0),
            standing_row(3, &["A", "b"], 7.0, 0),
            standing_row(4, &["c"], 1500.0, 0),
        ]});
        let (c, calls) = make_client(vec![ok(rows)]);
        let handles = ["a", "c", "gone"].map(String::from);
        let got = c.standings(1700, &handles).await.unwrap();
        let got: Vec<_> = got.iter().map(|r| (r.rank, r.handles.clone(), r.points, r.penalty)).collect();
        assert_eq!(got, [(3, vec!["A".to_string(), "b".into()], 7.0, 0), (4, vec!["c".to_string()], 1500.0, 0)]);
        assert_eq!(params_of(&calls, 0), p(&[("contestId", "1700")]));
    }

    /// Python filters before reading rank, points or penalty, so an unmatched row may lack them.
    #[tokio::test]
    async fn standings_unmatched_row_may_lack_fields() {
        let rows = json!({"rows": [
            {"party": {"members": [{"handle": "x"}]}},
            {"party": {"members": [{"handle": "x"}]}, "rank": "1", "points": null},
            standing_row(2, &["a"], 3.0, 4),
        ]});
        let (c, _) = make_client(vec![ok(rows.clone()), ok(rows)]);
        let got = c.standings(1, &["a".to_string()]).await.unwrap();
        assert_eq!((got.len(), got[0].rank, got[0].points, got[0].penalty), (1, 2, 3.0, 4));
        // A matched row still must have them.
        let bad = json!({"rows": [{"party": {"members": [{"handle": "a"}]}}]});
        let (c, _) = make_client(vec![ok(bad)]);
        assert!(c.standings(1, &["a".to_string()]).await.is_err());
    }

    #[tokio::test]
    async fn unknown_contest_is_not_a_handle_error() {
        let (c, _) = make_client(vec![failed("contestId: Contest with id 5 not found")]);
        let e = c.standings(5, &["a".to_string()]).await.unwrap_err();
        assert!(is_cf_error(&e) && !is_handle_not_found(&e));
    }

    #[tokio::test]
    async fn rating_history_parses() {
        let (c, calls) = make_client(vec![ok(json!([
            {"ratingUpdateTimeSeconds": 100, "newRating": 1500, "oldRating": 1400},
            {"ratingUpdateTimeSeconds": 200, "newRating": 1450, "oldRating": 1500},
        ]))]);
        let ch = c.rating_history("a").await.unwrap();
        assert_eq!(ch.iter().map(|c| (c.time, c.rating)).collect::<Vec<_>>(), [(100, 1500), (200, 1450)]);
        assert!(calls.lock().unwrap()[0].0.ends_with("user.rating"));
        assert_eq!(params_of(&calls, 0), p(&[("handle", "a")]));
    }

    fn status_row(i: i64, at: i64) -> Value {
        json!({"id": i, "creationTimeSeconds": at, "problem": {"contestId": 1, "index": "A"}, "verdict": "OK"})
    }

    #[tokio::test]
    async fn user_status_since_reads_older_pages_until_one_reaches_back() {
        let newest: Vec<_> = (0..3).map(|i| status_row(i, 5000 - i)).collect();
        let middle: Vec<_> = (0..3).map(|i| status_row(10 + i, 4000 - i)).collect();
        let oldest: Vec<_> = (0..2).map(|i| status_row(20 + i, 900 - i)).collect();
        let (c, calls) = make_client(vec![ok(json!(newest)), ok(json!(middle)), ok(json!(oldest))]);
        let subs = c.user_status("a", Some(3), Some(1000)).await.unwrap();
        assert_eq!(subs.len(), 8);
        let froms: Vec<_> = (0..3).map(|i| params_of(&calls, i)[1].1.clone()).collect();
        assert_eq!(froms, ["1", "4", "7"]);
        assert!((0..3).all(|i| params_of(&calls, i)[2].1 == "3"));
    }

    #[tokio::test]
    async fn user_status_since_stops_after_one_page_when_it_reaches_back() {
        let (c, calls) = make_client(vec![ok(json!([status_row(1, 5000), status_row(2, 100)]))]);
        assert_eq!(c.user_status("a", Some(2), Some(1000)).await.unwrap().len(), 2);
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn user_status_since_stops_on_a_short_page_and_caps_the_pages() {
        let (c, calls) = make_client(vec![ok(json!([status_row(1, 5000)]))]); // fewer than count: no more
        c.user_status("a", Some(3), Some(1000)).await.unwrap();
        assert_eq!(calls.lock().unwrap().len(), 1);
        let full = (0..9).map(|_| ok(json!((0..2).map(|i| status_row(i, 5000 - i)).collect::<Vec<_>>()))).collect();
        let (c, _) = make_client(full);
        assert_eq!(c.user_status("a", Some(2), Some(1000)).await.unwrap().len(), 10); // 5 pages at most
    }

    #[tokio::test]
    async fn users_info_chunks_and_splits_to_drop_unknown_handle() {
        let (c, calls) = make_client(vec![ok(json!([{"handle": "A", "rating": 1500, "rank": "specialist"}, {"handle": "c"}]))]);
        let users = c.users_info(&["a".to_string(), "c".to_string()], 2).await.unwrap();
        assert_eq!(users.iter().map(|u| (u.handle.as_str(), u.rating)).collect::<Vec<_>>(), [("A", Some(1500)), ("c", None)]);
        assert_eq!(params_of(&calls, 0), p(&[("handles", "a;c")]));

        let nf = "handles: User with handle b not found";
        let (c, calls) = make_client(vec![failed(nf), ok(json!([{"handle": "a"}])), failed(nf)]);
        let users = c.users_info(&["a".to_string(), "b".to_string()], 300).await.unwrap();
        assert_eq!(users.iter().map(|u| u.handle.as_str()).collect::<Vec<_>>(), ["a"]);
        let sent: Vec<_> = (0..3).map(|i| params_of(&calls, i)[0].1.clone()).collect();
        assert_eq!(sent, ["a;b", "a", "b"]);
    }
}
