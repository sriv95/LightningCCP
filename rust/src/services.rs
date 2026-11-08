//! Port of src/services.py: what the cogs reach through `bot.services`.
use std::sync::Arc;

use crate::cf::client::{CodeforcesClient, ReqwestHttp};
use crate::config::Config;
use crate::store::base::Store;

pub struct Services {
    pub cf: Arc<CodeforcesClient>,
    pub db: Store,
    // TODO(s8): pub problems: ProblemCache, pub contests: ContestCache.
    // TODO(s11): pub matches. TODO(s12): pub daily. TODO(s16): pub mashups.
}

impl Services {
    /// The first half of Python `setup_hook`: store, HTTP session (User-Agent "LightningCCP"), clients.
    pub async fn connect(config: &Config) -> anyhow::Result<Self> {
        let db = Store::connect(&config.db_path).await?; // opens and migrates
        let cf = Arc::new(CodeforcesClient::new(Arc::new(ReqwestHttp::new()?)));
        // TODO(s8, s11, s12, s16): build the caches and services over the store and `cf`.
        Ok(Self { db, cf })
    }

    /// `problems.start()` then `contests.start()`: load from the DB, then the refresh loop's first tick.
    pub async fn start_caches(&self) {
        // TODO(s8): self.problems.start().await; self.contests.start().await;
    }

    /// Python `close()` after `super().close()`: stop the caches, then the session, then the DB.
    pub async fn close(&self) {
        // TODO(s8): stop problems and contests.
        // The HTTP session closes when `cf` drops. `Store::close` takes `self`; a clone of the connection handle closes it.
        if let Err(e) = self.db.conn.clone().close().await {
            tracing::warn!("Could not close the database: {e}");
        }
    }
}
