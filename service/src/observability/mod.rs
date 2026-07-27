use anyhow::Context;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::ObservabilityConfig;

pub fn init_tracing(config: &ObservabilityConfig) -> anyhow::Result<()> {
    let filter = EnvFilter::try_new(&config.log_filter)
        .with_context(|| format!("invalid log filter: {}", config.log_filter))?;

    if config.log_json {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().json())
            .try_init()
            .context("failed to initialize JSON tracing subscriber")?;
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .try_init()
            .context("failed to initialize tracing subscriber")?;
    }

    Ok(())
}
