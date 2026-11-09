use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::{Instant, sleep};

use super::models::{Contest, Problem, RatingChange, Standing, Submission, User};
use crate::errors::BotError;

pub const BASE_URL: &str = "https://codeforces.com/api/";
const STATUS_PAGES: usize = 5; // pages of submissions a `since` read may take
const UNREACHABLE: &str = "Codeforces is unreachable or down for maintenance.";

pub type Params = Vec<(&'static str, String)>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// GET `url` with `params`; the parsed JSON body whatever the HTTP status. Any failure (network,
/// timeout, body not JSON) is an `Err`, which the client reports as "unreachable".
pub trait Http: Send + Sync {
    fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Value, ()>>;
}

pub struct ReqwestHttp(reqwest::Client);

impl ReqwestHttp {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self(reqwest::Client::builder().user_agent("LightningCCP").build()?))
    }
}

impl Http for ReqwestHttp {
    fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Value, ()>> {
        Box::pin(async move {
            let resp = self.0.get(url).query(params).timeout(Duration::from_secs(15)).send().await.map_err(|_| ())?;
            let body = resp.bytes().await.map_err(|_| ())?; // content type ignored, like aiohttp content_type=None
            serde_json::from_slice(&body).map_err(|_| ())
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
        let url = format!("{BASE_URL}{method}");
        for attempt in 0..self.retries {
            let payload = {
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
            // non-str comment: not a BotError, so the cogs reply "Something went wrong".
            let Value::Object(mut object) = payload else { anyhow::bail!("Codeforces payload is not a JSON object") };
            if object.get("status").and_then(Value::as_str) == Some("OK") {
                return Ok(serde_json::from_value(object.remove("result").unwrap_or(Value::Null))?);
            }
            let comment = match object.get("comment") {
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
    async fn call_object<T: DeserializeOwned>(&self, method: &str, params: Params) -> anyhow::Result<T> {
        let result: Value = self.call(method, params).await?;
        anyhow::ensure!(result.is_object(), "Codeforces result is not a JSON object");
        Ok(serde_json::from_value(result)?)
    }

    pub async fn problemset(&self) -> anyhow::Result<Vec<Problem>> {
        #[derive(Deserialize)]
        struct Data {
            problems: Vec<Problem>,
        }
        Ok(self.call_object::<Data>("problemset.problems", vec![]).await?.problems)
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
        struct Member {
            handle: String,
        }
        #[derive(Deserialize)]
        struct Party {
            members: Vec<Member>,
        }
        #[derive(Deserialize)]
        struct Row {
            party: Party,
            // Read only on matched rows, like Python: an unmatched row may lack them or hold anything.
            #[serde(default)]
            rank: Value,
            #[serde(default)]
            points: Value,
            #[serde(default)]
            penalty: Value,
        }
        #[derive(Deserialize)]
        struct Data {
            rows: Vec<Row>,
        }
        let wanted: Vec<String> = handles.iter().map(|h| h.to_lowercase()).collect();
        let data: Data = self.call_object("contest.standings", vec![("contestId", contest_id.to_string())]).await?;
        data.rows
            .into_iter()
            .filter(|r| r.party.members.iter().any(|m| wanted.contains(&m.handle.to_lowercase())))
            .map(|r| {
                let bad = || anyhow::anyhow!("malformed standings row");
                Ok(Standing {
                    rank: r.rank.as_i64().ok_or_else(bad)?,
                    handles: r.party.members.into_iter().map(|m| m.handle).collect(),
                    points: r.points.as_f64().ok_or_else(bad)?,
                    penalty: r.penalty.as_i64().ok_or_else(bad)?,
                })
            })
            .collect()
    }

    pub async fn rating_history(&self, handle: &str) -> anyhow::Result<Vec<RatingChange>> {
        self.call("user.rating", vec![("handle", handle.into())]).await
    }
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
        responses: StdMutex<VecDeque<Result<Value, ()>>>,
        calls: Calls,
    }

    impl Http for FakeHttp {
        fn get<'a>(&'a self, url: &'a str, params: &'a Params) -> BoxFuture<'a, Result<Value, ()>> {
            self.calls.lock().unwrap().push((url.to_string(), params.clone()));
            let r = self.responses.lock().unwrap().pop_front().expect("no queued response");
            Box::pin(async move { r })
        }
    }

    fn ok(result: Value) -> Result<Value, ()> {
        Ok(json!({"status": "OK", "result": result}))
    }

    fn failed(comment: &str) -> Result<Value, ()> {
        Ok(json!({"status": "FAILED", "comment": comment}))
    }

    fn make_client(responses: Vec<Result<Value, ()>>) -> (CodeforcesClient, Calls) {
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
        for bad in [json!([1]), json!("x"), json!(7), json!(null), json!({"status": "FAILED", "comment": null}), json!({"status": "FAILED", "comment": 5})] {
            let (c, _) = make_client(vec![Ok(bad.clone())]);
            let e = c.user_info("a").await.unwrap_err();
            assert!(e.downcast_ref::<BotError>().is_none(), "{bad}: {e}");
        }
        // missing comment is still "unknown"; an OK payload with no result (KeyError) is not a BotError
        let (c, _) = make_client(vec![Ok(json!({"status": "FAILED"}))]);
        assert_eq!(c.user_info("a").await.unwrap_err().to_string(), "Codeforces error: unknown");
        let (c, _) = make_client(vec![Ok(json!({"status": "OK"}))]);
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
        let (c, _) = make_client(vec![Ok(json!({"status": "FAILED"}))]);
        assert_eq!(c.contests().await.unwrap_err().to_string(), "Codeforces error: unknown");
    }

    #[tokio::test]
    async fn non_json_body_maps_to_codeforces_error() {
        // maintenance pages are HTML: the HTTP layer reports Err
        let (c, _) = make_client(vec![Err(())]);
        let e = c.problemset().await.unwrap_err();
        assert!(is_cf_error(&e));
        assert_eq!(e.to_string(), "Codeforces is unreachable or down for maintenance.");
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
