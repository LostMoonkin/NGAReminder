mod api;
mod app;
mod assets;
mod collector;
mod config;
mod crypto;
mod domain;
mod export;
mod markup;
mod nga;
mod notification;
mod observability;
mod repository;
mod schedule;
mod worker;

use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::task::JoinError;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{app::Application, config::AppConfig, observability::init_tracing};

#[derive(Debug, Parser)]
#[command(name = "nga-reminder", version, about = "NGA monitoring service")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Subcommand)]
enum Command {
    /// Run only the HTTP API.
    Serve,
    /// Run only background workers.
    Worker,
    /// Run the HTTP API and workers in one process.
    All,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Arc::new(AppConfig::load().context("failed to load configuration")?);
    init_tracing(&config.observability)?;

    let application = Application::build(config).await?;
    let cancellation = CancellationToken::new();
    let command = cli.command.unwrap_or(Command::All);

    info!(?command, "starting NGA Reminder");

    match command {
        Command::Serve => {
            run_server(application, cancellation).await?;
        }
        Command::Worker => {
            run_worker(application.state().clone(), cancellation).await?;
        }
        Command::All => {
            run_all(application, cancellation).await?;
        }
    }

    info!("NGA Reminder stopped");
    Ok(())
}

async fn run_server(
    application: Application,
    cancellation: CancellationToken,
) -> anyhow::Result<()> {
    let server_cancellation = cancellation.clone();
    let mut server = tokio::spawn(async move { application.run_http(server_cancellation).await });

    tokio::select! {
        result = &mut server => flatten_task_result(result),
        result = shutdown_signal() => {
            result?;
            cancellation.cancel();
            flatten_task_result(server.await)
        }
    }
}

async fn run_worker(state: app::AppState, cancellation: CancellationToken) -> anyhow::Result<()> {
    let worker_cancellation = cancellation.clone();
    let mut workers = tokio::spawn(worker::run(state, worker_cancellation));

    tokio::select! {
        result = &mut workers => flatten_task_result(result),
        result = shutdown_signal() => {
            result?;
            cancellation.cancel();
            flatten_task_result(workers.await)
        }
    }
}

async fn run_all(application: Application, cancellation: CancellationToken) -> anyhow::Result<()> {
    let state = application.state().clone();
    let server_cancellation = cancellation.clone();
    let worker_cancellation = cancellation.clone();

    let mut server = tokio::spawn(async move { application.run_http(server_cancellation).await });
    let mut workers = tokio::spawn(worker::run(state, worker_cancellation));

    tokio::select! {
        result = &mut server => {
            cancellation.cancel();
            flatten_task_result(result)?;
            flatten_task_result(workers.await)
        }
        result = &mut workers => {
            cancellation.cancel();
            flatten_task_result(result)?;
            flatten_task_result(server.await)
        }
        result = shutdown_signal() => {
            result?;
            cancellation.cancel();
            flatten_task_result(server.await)?;
            flatten_task_result(workers.await)
        }
    }
}

fn flatten_task_result(result: Result<anyhow::Result<()>, JoinError>) -> anyhow::Result<()> {
    result.context("service task panicked or was cancelled")?
}

async fn shutdown_signal() -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate =
            signal(SignalKind::terminate()).context("failed to register SIGTERM handler")?;

        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for Ctrl+C")?;
            }
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .context("failed to listen for Ctrl+C")?;

    Ok(())
}
