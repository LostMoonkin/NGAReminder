use crate::model::config::{Config, CrawlerConfig, MonitorConfig, NotifierConfig};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Mutex;

pub struct ConfigHolder {
    config_file_path: String,
    config: Config,
    file_lock: Mutex<i32>,
}

impl ConfigHolder {
    pub async fn new(config_file_path: String) -> Result<Self, Box<dyn Error>> {
        let content = tokio::fs::read(config_file_path.clone()).await?;

        let origin_config: Config = serde_json::from_slice(&content)?;
        let file_lock = Mutex::new(0);
        Ok(Self {
            config_file_path,
            config: origin_config,
            file_lock,
        })
    }

    pub fn get_all_config(&self) -> Config {
        self.config.clone()
    }

    pub fn get_crawler_config(&self) -> CrawlerConfig {
        self.config.crawler.clone()
    }

    pub fn get_monitor_config(&self) -> MonitorConfig {
        self.config.monitor.clone()
    }

    pub fn get_notifier_config(&self) -> NotifierConfig {
        self.config.notifier.clone()
    }

    pub async fn update_passport(
        &mut self,
        cid: String,
        uid: String,
    ) -> Result<(), Box<dyn Error>> {
        let _lock = self.file_lock.lock().unwrap();
        self.config.crawler.nga_passport_uid = uid;
        self.config.crawler.nga_passport_cid = cid;
        self.write_to_file().await
    }

    pub async fn update_post_last_seen(
        &mut self,
        tid_to_post_number: &HashMap<u64, u64>,
    ) -> Result<(), Box<dyn Error>> {
        let _lock = self.file_lock.lock().unwrap();
        for t in &mut self.config.monitor.monitored_threads {
            if tid_to_post_number.contains_key(&t.tid) {
                let last_seen = tid_to_post_number[&t.tid];
                t.last_seen_post_number = last_seen
            }
        }
        self.write_to_file().await
    }

    async fn write_to_file(&self) -> Result<(), Box<dyn Error>> {
        let config = self.config.clone();

        let config_json = serde_json::to_string_pretty(&config)?;
        tokio::fs::write(self.config_file_path.clone(), config_json).await?;
        Ok(())
    }
}
