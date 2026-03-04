mod config;
mod http;
mod probe;
mod proto;
mod scheduler;
mod state;

use anyhow::Context;
use clap::Parser;
use std::future::IntoFuture;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(name = "kino")]
#[command(about = "Asynchronous probe service for ephemeral VM validation")]
struct Cli {
    #[arg(long, value_name = "PATH")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let app_config = config::load_from_file(&cli.config)
        .with_context(|| format!("failed to load config from {}", cli.config.display()))?;

    let built_probes = probe::build_probes(&app_config.probes)
        .await
        .context("failed to build probes")?;

    let shared_probes = built_probes
        .into_iter()
        .map(Arc::new)
        .collect::<Vec<Arc<probe::ProbeDefinition>>>();

    let store = state::ProbeStore::new(&shared_probes);
    let probe_tasks = scheduler::spawn_probe_tasks(shared_probes, &store);

    let router = http::build_router(store);
    let listener = tokio::net::TcpListener::bind(app_config.server_addr)
        .await
        .with_context(|| format!("failed to bind to {}", app_config.server_addr))?;

    eprintln!("kino listening on {}", app_config.server_addr);

    let server = axum::serve(listener, router).into_future();
    tokio::pin!(server);

    let server_result = tokio::select! {
        result = &mut server => Some(result),
        () = shutdown_signal() => None,
    };

    for task in probe_tasks {
        task.abort();
    }

    if let Some(result) = server_result {
        result.context("http server terminated unexpectedly")?;
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());

        if let Ok(mut stream) = signal {
            let _ = stream.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
