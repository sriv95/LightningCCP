//! Port of src/daily.py: the daily problem, one per user per UTC day, solved for double the solo rating change.
//! Also the two read-only seams the problem cogs share: the Codeforces calls (`CfApi`) and the problem cache (`ProblemSource`).
use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::rngs::StdRng;
use rand::seq::IndexedRandom;
use rand::SeedableRng;

use crate::cache::problems::ProblemCache;
use crate::cf::client::CodeforcesClient;
use crate::cf::models::{Problem, Submission, User};
use crate::errors::BotError;
use crate::logic::base::{
    DAILY_MULTIPLIER, ProblemFilter, daily_streak, earliest_solve, filter_problems, floor_rating, solo_delta, solved_keys,
    utc_day,
};
use crate::store::base::Store;
use crate::store::matches::{DailyRow, DailySolve};
pub use crate::ui::embeds::a::DailyView;

/// The Codeforces calls the problem cogs make. `CodeforcesClient` implements it; tests use a fake.
pub trait CfApi: Send + Sync {
    fn user_info(&self, handle: &str) -> impl Future<Output = anyhow::Result<User>> + Send;
    /// Profiles of the given handles (Python `users_info`, default chunk 300); unknown handles are skipped.
    fn users_info(&self, handles: &[String]) -> impl Future<Output = anyhow::Result<Vec<User>>> + Send;
    fn user_status(&self, handle: &str, count: Option<i64>) -> impl Future<Output = anyhow::Result<Vec<Submission>>> + Send;
}

impl CfApi for CodeforcesClient {
    async fn user_info(&self, handle: &str) -> anyhow::Result<User> {
        CodeforcesClient::user_info(self, handle).await
    }
    async fn users_info(&self, handles: &[String]) -> anyhow::Result<Vec<User>> {
        CodeforcesClient::users_info(self, handles, 300).await
    }
    async fn user_status(&self, handle: &str, count: Option<i64>) -> anyhow::Result<Vec<Submission>> {
        CodeforcesClient::user_status(self, handle, count, None).await
    }
}

/// What the cogs read from the problem cache (Python `problems.problems`, `.require()`, `.tags`).
pub trait ProblemSource: Send + Sync {
    /// The cached problems, empty before the first load (Python `.problems`).
    fn problems(&self) -> Arc<Vec<Problem>>;
    /// The cached problems; `CacheNotReady` while empty.
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError>;
    fn tags(&self) -> Arc<HashSet<String>>;
}

impl ProblemSource for ProblemCache {
    fn problems(&self) -> Arc<Vec<Problem>> {
        self.require().unwrap_or_default()
    }
    fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        ProblemCache::require(self)
    }
    fn tags(&self) -> Arc<HashSet<String>> {
        ProblemCache::tags(self)
    }
}

/// Python `time.time`.
pub fn system_clock() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

pub struct DailyService<C, P> {
    store: Arc<Store>,
    cf: Arc<C>,
    problems: Arc<P>,
    clock: Box<dyn Fn() -> f64 + Send + Sync>,
    rng: Mutex<StdRng>,
}

impl<C: CfApi, P: ProblemSource> DailyService<C, P> {
    /// System clock and an OS-seeded rng.
    pub fn new(store: Arc<Store>, cf: Arc<C>, problems: Arc<P>) -> Self {
        Self::with(store, cf, problems, Box::new(system_clock), StdRng::from_rng(&mut rand::rng()))
    }

    /// Injected clock (Python `clock`) and rng (replaces the global `random` of src/daily.py:82).
    pub fn with(
        store: Arc<Store>,
        cf: Arc<C>,
        problems: Arc<P>,
        clock: Box<dyn Fn() -> f64 + Send + Sync>,
        rng: StdRng,
    ) -> Self {
        Self { store, cf, problems, clock, rng: Mutex::new(rng) }
    }

    fn problem(&self, row: &DailyRow) -> Option<Problem> {
        let key = row.problem_key.as_deref()?;
        self.problems.problems().iter().find(|p| p.key() == key).cloned()
    }

    async fn view(&self, row: DailyRow, delta: Option<i64>) -> anyhow::Result<DailyView> {
        let days = self.store.daily_days(row.guild_id, row.user_id).await?;
        let streak = daily_streak(&days, row.day);
        Ok(DailyView { problem: self.problem(&row), row, delta, streak })
    }

    async fn stored(&self, guild_id: i64, user_id: i64, day: i64) -> anyhow::Result<DailyRow> {
        self.store.get_daily(guild_id, user_id, day).await?.ok_or_else(|| anyhow::anyhow!("daily row missing"))
    }

    /// Issue today's problem, or check whether the user solved it since it was issued.
    ///
    /// `rating` picks the problem's rating (default: match rating floored to 100). Asking for a different
    /// rating than an unsolved daily has replaces it with a new problem.
    pub async fn claim(&self, guild_id: i64, channel_id: i64, user_id: i64, rating: Option<i64>) -> anyhow::Result<DailyView> {
        let linked = self.store.get_handle(guild_id, user_id).await?.ok_or_else(BotError::not_linked)?;
        let now = (self.clock)() as i64; // Python int(float)
        let day = utc_day(now);
        let row = self.store.get_daily(guild_id, user_id, day).await?;
        if let Some(row) = row.as_ref().filter(|r| r.solved_at.is_some()) {
            return Ok(DailyView { problem: self.problem(row), row: row.clone(), delta: None, streak: 0 });
        }
        let reroll = row.is_none() || rating.is_some_and(|r| Some(r) != row.as_ref().and_then(|r| r.rating));
        // A new problem needs every solved problem; a plain check only the recent submissions.
        let subs = self.cf.user_status(&linked.handle, if reroll { None } else { Some(100) }).await?;
        if let Some(row) = row {
            if let (Some(key), Some(since)) = (row.problem_key.as_deref(), row.issued_at)
                && let Some(solved_at) = earliest_solve(&subs, key, since)
            {
                return self.book(guild_id, channel_id, &linked.handle, row, solved_at, now).await;
            }
            if !reroll {
                return self.view(row, None).await;
            }
        }
        let rating = match rating {
            Some(r) => r,
            None => floor_rating(self.store.rating(guild_id, user_id).await?),
        };
        let pool = filter_problems(
            &self.problems.require()?,
            &ProblemFilter { min_rating: Some(rating), max_rating: Some(rating), solved: solved_keys(&subs), ..Default::default() },
        );
        let key = match pool.choose(&mut *self.rng.lock().unwrap()) {
            Some(p) => p.key(),
            None => return Err(BotError::Plain(format!("You have solved every problem rated {rating}.")).into()),
        };
        self.store.issue_daily(guild_id, user_id, day, &key, rating, now).await?;
        self.view(self.stored(guild_id, user_id, day).await?, None).await
    }

    async fn book(&self, guild_id: i64, channel_id: i64, handle: &str, row: DailyRow, solved_at: i64, now: i64) -> anyhow::Result<DailyView> {
        let mine = self.store.rating(guild_id, row.user_id).await?;
        let problem_rating = row.rating.ok_or_else(|| anyhow::anyhow!("issued daily has no rating"))?;
        let delta = DAILY_MULTIPLIER * solo_delta(mine, problem_rating, true);
        let booked = self
            .store
            .record_daily_solve(DailySolve {
                guild_id,
                channel_id,
                user_id: row.user_id,
                handle: handle.to_string(),
                day: row.day,
                solved_at,
                delta,
                now,
            })
            .await?;
        let row = self.stored(guild_id, row.user_id, row.day).await?;
        self.view(row, booked.then_some(delta)).await
    }
}
