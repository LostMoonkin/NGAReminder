use std::collections::HashMap;
use std::error::Error;
use config_holder::ConfigHolder;
use crate::model::nga_thread::NGAThread;

mod crawler;
mod model;
mod config_holder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let content = tokio::fs::read("config/result.json").await?;
    let thread_data: NGAThread = serde_json::from_slice(&content)?;
    println!("{:#?}", thread_data);
    Ok(())
}
