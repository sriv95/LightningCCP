//! Port of src/services.py: what the cogs reach through `bot.services`.
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use tokio::task::JoinHandle;

use crate::cache::contests::ContestCache;
use crate::cache::problems::ProblemCache;
use crate::cf::client::{CodeforcesClient, ReqwestHttp};
use crate::cogs::problems::Daily;
use crate::config::Config;
use crate::daily::DailyService;
use crate::mashup::service::MashupService;
use crate::matches::MatchService;
use crate::store::base::Store;

pub struct Services {
    pub cf: Arc<CodeforcesClient>,
    pub db: Arc<Store>,
    pub problems: Arc<ProblemCache>,
    pub contests: Arc<ContestCache>,
    pub matches: MatchService<CodeforcesClient, ProblemCache>,
    pub mashups: MashupService<CodeforcesClient, ProblemCache>,
    pub daily: Daily,
    /// The cog loops (`cog_load`), started once the gateway cache is ready; `close` aborts them.
    pub loops: Mutex<Vec<JoinHandle<()>>>,
    pub loops_started: AtomicBool,
}

impl Services {
    /// The first half of Python `setup_hook`: store, HTTP session (User-Agent "LightningCCP"), clients,
    /// then the caches and services in Python's order (problems, contests, matches, mashups, daily).
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let db = Arc::new(Store::connect(&config.db_path).await?); // opens and migrates
        let cf = Arc::new(CodeforcesClient::new(Arc::new(ReqwestHttp::new()?)));
        let problems = Arc::new(ProblemCache::new(cf.clone(), db.clone()));
        let contests = Arc::new(ContestCache::new(cf.clone(), db.clone()));
        let matches = MatchService::new(db.clone(), cf.clone(), problems.clone());
        let mashups = MashupService::new(db.clone(), cf.clone(), problems.clone());
        let daily = DailyService::new(db.clone(), cf.clone(), problems.clone());
        Ok(Self { cf, db, problems, contests, matches, mashups, daily, loops: Mutex::default(), loops_started: AtomicBool::new(false) })
    }

    /// `problems.start()` then `contests.start()`: load from the DB, then the refresh loop's first tick.
    pub async fn start_caches(&self) -> anyhow::Result<()> {
        self.problems.start().await?;
        self.contests.start().await
    }

    /// Python `close()` after `super().close()`: stop the caches, then the session, then the DB.
    pub async fn close(&self) {
        for handle in self.loops.lock().unwrap().drain(..) {
            handle.abort();
        }
        self.problems.stop();
        self.contests.stop();
        // The HTTP session closes when `cf` drops. `Store::close` takes `self`; a clone of the connection handle closes it.
        if let Err(e) = self.db.conn.clone().close().await {
            tracing::warn!("Could not close the database: {e}");
        }
    }
}
