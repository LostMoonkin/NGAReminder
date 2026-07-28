use std::time::Duration;

use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::{
    app::AppState,
    collector::{thread, user},
    notification,
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
                while notification::worker::process_one(&state).await? {}
                let claimed = watch::claim_due(&state.pool, state.config.database_backend).await?;
                if let Some(watch_target) = claimed {
                    match watch_target.target_type.as_str() {
                        "thread" => {
                            if let Err(error) = thread::run(&state, watch_target).await {
                                warn!(error = %error, "thread crawl failed");
                            }
                        }
                        "user" => {
                            if let Err(error) = user::run(&state, watch_target).await {
                                warn!(error = %error, "user crawl failed");
                            }
                        }
                        _ => warn!(watch_id = watch_target.id, "unknown watch type"),
                    }
                }
            }
        }
    }
}
