use config_holder::ConfigHolder;
use std::error::Error;

mod config_holder;
mod crawler;
mod model;
mod monitor;
mod notifier;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_holder = ConfigHolder::new("./config/config.json".to_string()).await?;
    let crawler_config = config_holder.get_crawler_config();
    let crawler = crawler::Crawler::new(crawler_config.user_agent, crawler_config.timeout);
    // let thread = crawler
    //     .fetch_thread_with_page(45907597, 1, &config_holder.get_crawler_config())
    //     .await?;
    // println!("{:#?}", thread);
    let mut monitor = monitor::NGAMonitor::new(config_holder, crawler);
    monitor.run().await;
    Ok(())
}
