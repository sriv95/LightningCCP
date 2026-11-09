//! Port of `src/cache/problems.py`: the problemset, kept in memory and in the DB.
use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::cf::client::CodeforcesClient;
use crate::cf::models::Problem;
use crate::errors::BotError;
use crate::store::base::Store;

pub const REFRESH: Duration = Duration::from_secs(6 * 3600);
pub const RETRY: Duration = Duration::from_secs(5 * 60);

#[derive(Default)]
struct State {
    problems: Arc<Vec<Problem>>,
    tags: Arc<HashSet<String>>,
}

pub struct ProblemCache {
    cf: Arc<CodeforcesClient>,
    store: Arc<Store>,
    state: RwLock<State>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ProblemCache {
    pub fn new(cf: Arc<CodeforcesClient>, store: Arc<Store>) -> Self {
        Self { cf, store, state: RwLock::default(), task: Mutex::new(None) }
    }

    fn set(&self, problems: Vec<Problem>) {
        let tags = problems.iter().flat_map(|p| p.tags.iter().cloned()).collect();
        *self.state.write().unwrap() = State { problems: Arc::new(problems), tags: Arc::new(tags) };
    }

    pub fn tags(&self) -> Arc<HashSet<String>> {
        self.state.read().unwrap().tags.clone()
    }

    /// The cached problems; `CacheNotReady` while empty.
    pub fn require(&self) -> Result<Arc<Vec<Problem>>, BotError> {
        let problems = self.state.read().unwrap().problems.clone();
        if problems.is_empty() { Err(BotError::CacheNotReady) } else { Ok(problems) }
    }

    pub async fn load(&self) -> anyhow::Result<()> {
        self.set(self.store.load_problems().await?);
        Ok(())
    }

    pub async fn refresh(&self) -> anyhow::Result<()> {
        let problems = self.cf.problemset().await?;
        if problems.is_empty() {
            // keep the old data; the tick retries in 5 minutes
            return Err(BotError::Codeforces("Codeforces returned no problems".into()).into());
        }
        self.store.replace_problems(&problems).await?;
        self.set(problems);
        Ok(())
    }

    /// One loop iteration; the delay before the next one.
    pub async fn tick(&self) -> Duration {
        match self.refresh().await {
            Ok(()) => REFRESH,
            Err(e) => {
                tracing::error!("Problem cache refresh failed; retrying in 5 minutes: {e:?}");
                RETRY
            }
        }
    }

    /// Load from the DB, then run the refresh loop (first tick immediately).
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        self.load().await?;
        let cache = self.clone();
        let handle = tokio::spawn(async move {
            loop {
                let started = Instant::now(); // discord.py: next run = tick start + (new) interval
                let delay = cache.tick().await;
                sleep_until(started + delay).await;
            }
        });
        *self.task.lock().unwrap() = Some(handle);
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(handle) = self.task.lock().unwrap().take() {
            handle.abort();
        }
    }
}
