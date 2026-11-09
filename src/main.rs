//! Port of src/__main__.py: dotenv, logging at INFO, config, then the bot. `--bench` runs the offline benchmark.
use lightningccp::{bench, bot, config};

fn main() -> anyhow::Result<()> {
    // two workers: the bot is I/O bound (Python is single-threaded); the bench and the bot share this config
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--bench") {
        // JSON goes to stdout, so logs go to stderr and only warnings
        tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).with_writer(std::io::stderr).init();
        return bench::run(&args).await;
    }
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let config = config::load_config_from_env()?;
    bot::run(config).await
}
