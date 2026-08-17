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

// Keep every worker cycle fair. A continuously replenished asset or
// notification queue must not prevent due watches (or cancellation) from
// being observed indefinitely.
const MAX_ASSET_JOBS_PER_CYCLE: usize = 1;
const MAX_NOTIFICATION_JOBS_PER_CYCLE: usize = 1;

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
                for _ in 0..MAX_ASSET_JOBS_PER_CYCLE {
                    if cancellation.is_cancelled() {
                        info!("worker role stopping");
                        return Ok(());
                    }
                    // Let a claimed asset finish its normal bookkeeping. A
                    // crash is recoverable through the download lease, while
                    // graceful shutdown should avoid an unnecessary retry.
                    match assets::process_one(&state).await {
                        Ok(true) => metrics::asset_job(),
                        Ok(false) => break,
                        Err(error) => {
                            warn!(error = %error, "asset job failed; it will be retried on a later cycle");
                            break;
                        }
                    }
                }
                for _ in 0..MAX_NOTIFICATION_JOBS_PER_CYCLE {
                    if cancellation.is_cancelled() {
                        info!("worker role stopping");
                        return Ok(());
                    }
                    let processed = notification::worker::process_fair_batch(&state).await?;
                    if processed == 0 {
                        break;
                    }
                    for _ in 0..processed {
                        metrics::notification_job();
                    }
                }
                // Expire stale login sessions and clear their protocol
                // contexts on every cycle (cheap: one indexed query).
                if let Err(error) = crate::bot::session::expire_stale_sessions(&state).await {
                    warn!(error = %error, "login session cleanup failed");
                }
                if cancellation.is_cancelled() {
                    info!("worker role stopping");
                    return Ok(());
                }
                let claimed = watch::claim_due(&state.pool, state.config.database_backend).await?;
                if let Some(watch_target) = claimed {
                    let watch_id = watch_target.id.clone();
                    let target_type = watch_target.target_type.clone();
                    if !matches!(target_type.as_str(), "thread" | "user") {
                        warn!(watch_id = %watch_id, target_type, "unknown watch type");
                        continue;
                    }
                    let crawl = async {
                        match target_type.as_str() {
                            "thread" => thread::run(&state, watch_target)
                                .await
                                .map(|_| ())
                                .map_err(anyhow::Error::from),
                            "user" => user::run(&state, watch_target)
                                .await
                                .map(|_| ())
                                .map_err(anyhow::Error::from),
                            _ => unreachable!("watch type checked above"),
                        }
                    };
                    // Let a claimed crawl reach its normal success/failure
                    // bookkeeping before honoring shutdown.
                    let result = crawl.await;
                    if let Err(error) = result {
                        metrics::crawl_failed();
                        warn!(watch_id = %watch_id, target_type, error = %error, "watch crawl failed");
                    } else {
                        metrics::crawl_succeeded();
                    }
                }
            }
        }
    }
}
