//! Port of src/__main__.py: dotenv, logging at INFO, config, then the bot.
use lightningccp::{bot, config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let config = config::load_config_from_env()?;
    bot::run(config).await
}
