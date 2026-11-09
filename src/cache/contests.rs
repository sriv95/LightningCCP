//! Port of `src/cache/contests.py`: the contest list, kept in memory and in the DB.
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};

use crate::cf::client::CodeforcesClient;
use crate::cf::models::Contest;
use crate::errors::BotError;
use crate::logic::base::refresh_minutes;
use crate::store::base::Store;

pub const RETRY: Duration = Duration::from_secs(5 * 60);

/// Delay after a good refresh: 5 min while a contest runs, else 30 min.
pub fn refresh_delay(contests: &[Contest]) -> Duration {
    Duration::from_secs(refresh_minutes(contests) as u64 * 60)
}

pub struct ContestCache {
    cf: Arc<CodeforcesClient>,
    store: Arc<Store>,
    contests: RwLock<Arc<Vec<Contest>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl ContestCache {
    pub fn new(cf: Arc<CodeforcesClient>, store: Arc<Store>) -> Self {
        Self { cf, store, contests: RwLock::default(), task: Mutex::new(None) }
    }

    pub fn contests(&self) -> Arc<Vec<Contest>> {
        self.contests.read().unwrap().clone()
    }

    pub async fn load(&self) -> anyhow::Result<()> {
        *self.contests.write().unwrap() = Arc::new(self.store.load_contests().await?);
        Ok(())
    }

    pub async fn refresh(&self) -> anyhow::Result<()> {
        let contests = self.cf.contests().await?;
        if contests.is_empty() {
            // keep the old data; the tick retries in 5 minutes
            return Err(BotError::Codeforces("Codeforces returned no contests".into()).into());
        }
        self.store.replace_contests(&contests).await?;
        *self.contests.write().unwrap() = Arc::new(contests);
        Ok(())
    }

    /// One loop iteration; the delay before the next one.
    pub async fn tick(&self) -> Duration {
        match self.refresh().await {
            Ok(()) => refresh_delay(&self.contests()),
            Err(e) => {
                tracing::error!("Contest cache refresh failed; retrying in 5 minutes: {e:?}");
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
