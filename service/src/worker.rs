use std::time::Duration;

use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    app::AppState,
    assets,
    collector::{thread, user},
    metrics, notification,
    repository::watch,
};

pub async fn run(state: AppState, cancellation: CancellationToken) -> anyhow::Result<()> {
    let mut scheduler = time::interval(Duration::from_secs(2));
    scheduler.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

    info!("worker role started");

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                info!("worker role stopping");
                return Ok(());
            }
            _ = scheduler.tick() => {
                metrics::worker_cycle();
                loop {
                    match assets::process_one(&state).await {
                        Ok(true) => metrics::asset_job(),
                        Ok(false) => break,
                        Err(error) => {
                            warn!(error = %error, "asset job failed; it will be retried on a later cycle");
                            break;
                        }
                    }
                }
                while notification::worker::process_one(&state).await? {
                    metrics::notification_job();
                }
                // Expire stale login sessions and clear their protocol
                // contexts on every cycle (cheap: one indexed query).
                if let Err(error) = crate::bot::session::expire_stale_sessions(&state).await {
                    warn!(error = %error, "login session cleanup failed");
                }
                let claimed = watch::claim_due(&state.pool, state.config.database_backend).await?;
                if let Some(watch_target) = claimed {
                    match watch_target.target_type.as_str() {
                        "thread" => {
                            if let Err(error) = thread::run(&state, watch_target).await {
                                metrics::crawl_failed();
                                warn!(error = %error, "thread crawl failed");
                            } else {
                                metrics::crawl_succeeded();
                            }
                        }
                        "user" => {
                            if let Err(error) = user::run(&state, watch_target).await {
                                metrics::crawl_failed();
                                warn!(error = %error, "user crawl failed");
                            } else {
                                metrics::crawl_succeeded();
                            }
                        }
                        _ => warn!(watch_id = watch_target.id, "unknown watch type"),
                    }
                }
            }
        }
    }
}
