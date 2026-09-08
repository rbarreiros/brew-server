mod config;
mod control;
mod dashboard;
mod fsnet;
mod monitor;
mod protocol;
mod router;
mod server;
mod state;
mod telemetry;

use config::Config;
use state::AppState;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "brew_server=info".into()))
        .init();

    let path = std::env::args().nth(1).unwrap_or_else(|| "brew-server.toml".to_owned());
    let config = Config::load(&path)?;
    let state = Arc::new(AppState::new(config));

    tokio::try_join!(
        server::run(state.clone()),
        telemetry::run(state.clone()),
        control::run(state.clone()),
        dashboard::run(state.clone()),
    )?;
    Ok(())
}
