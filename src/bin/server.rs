use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

#[path = "server/aggregation_api.rs"]
mod aggregation_api;
#[path = "server/api.rs"]
mod api;
#[path = "server/api_rows.rs"]
mod api_rows;
#[path = "server/api_types.rs"]
mod api_types;
#[path = "server/app.rs"]
mod app;
#[path = "server/auth.rs"]
mod auth;
#[path = "server/diagnostics_api.rs"]
mod diagnostics_api;
#[path = "server/import_api.rs"]
mod import_api;
#[path = "server/job_api.rs"]
mod job_api;
#[path = "server/jobs.rs"]
mod jobs;
#[path = "server/metrics.rs"]
mod metrics;
#[path = "server/row_concurrency.rs"]
mod row_concurrency;
#[path = "server/similar_api.rs"]
mod similar_api;
#[path = "server/state.rs"]
mod state;

use app::router;
use state::AppState;

#[derive(Debug, Parser)]
#[command(about = "FrankensteinDB JSON API server")]
struct Args {
    /// Directory containing data.sqlite3 and Tantivy indexes.
    database: PathBuf,

    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,

    /// Optional API key. FRANKENSTEINDB_API_KEY is used when omitted.
    #[arg(long, env = "FRANKENSTEINDB_API_KEY", hide_env_values = true)]
    api_key: Option<String>,

    /// JSON file containing hashed API keys, scopes, validity windows, and table allowlists.
    #[arg(long, env = "FRANKENSTEINDB_API_KEY_CONFIG")]
    api_key_config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let state = AppState::open(args.database, args.api_key, args.api_key_config)?;
    #[cfg(unix)]
    tokio::spawn(reload_auth_on_sighup(state.clone()));
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("FrankensteinDB API listening on http://{}", args.listen);
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(unix)]
async fn reload_auth_on_sighup(state: AppState) {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut signals) = signal(SignalKind::hangup()) else {
        return;
    };
    while signals.recv().await.is_some() {
        match state.auth.reload() {
            Ok(count) => eprintln!("reloaded {count} API key(s)"),
            Err(error) => eprintln!("failed to reload API keys: {error:#}"),
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
