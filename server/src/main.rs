use config_holder::ConfigHolder;
use std::error::Error;
use std::sync::Arc;

mod config_holder;
mod crawler;
mod model;
mod monitor;
mod notifier;
mod web_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_holder = ConfigHolder::new("./config/config.json".to_string()).await?;
    let shared_config = Arc::new(config_holder);

    // Clone Arc for tasks
    let web_config = Arc::clone(&shared_config);
    let monitor_config = Arc::clone(&shared_config);

    // Get crawler info before spawning
    let crawler_cfg = shared_config.get_crawler_config().await;
    let crawler = crawler::Crawler::new(crawler_cfg.user_agent, crawler_cfg.timeout);

    // Spawn web server task
    let web_handle = tokio::spawn(async move {
        web_server::run_server(web_config).await;
    });

    // Spawn monitor task
    let monitor_handle = tokio::spawn(async move {
        let mut monitor = monitor::NGAMonitor::new(monitor_config, crawler).await;
        monitor.run().await;
    });

    // Wait for both tasks (they run indefinitely)
    tokio::select! {
        _ = web_handle => {
            println!("Web server stopped");
        }
        _ = monitor_handle => {
            println!("Monitor stopped");
        }
    }

    Ok(())
}
