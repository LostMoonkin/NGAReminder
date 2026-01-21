use std::collections::HashMap;
use std::error::Error;
use config_holder::ConfigHolder;
use crate::model::nga_thread::NGAThread;

mod crawler;
mod model;
mod config_holder;
mod monitor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let config_holder = ConfigHolder::new("./config/config.json".to_string()).await?;
    let crawler = crawler::Crawler::new(config_holder);
    let thread = crawler.fetch_thread_with_page(45907597, 1).await?;
    println!("{:#?}", thread);
    Ok(())
}
