use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Config {
    pub monitor: MonitorConfig,
    pub crawler: CrawlerConfig,
    pub notifier: NotifierConfig,
    pub web: WebConfig,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct WebConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitorConfig {
    pub fetch_posts_parallel_limit: u32,
    pub monitor_duration: u64,
    pub monitored_threads: Vec<MonitoredThread>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CrawlerConfig {
    pub api_url: String,
    pub nga_passport_uid: String,
    pub nga_passport_cid: String,
    pub user_agent: String,
    pub timeout: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MonitoredThread {
    pub tid: u64,
    pub author_notification: Vec<u64>,
    pub check_interval: u64,
    pub check_schedule: Option<Vec<CheckSchedule>>,
    pub enabled: bool,
    pub last_seen_post_number: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CheckSchedule {
    pub days: Vec<String>,
    pub description: String,
    pub start_time: String,
    pub end_time: String,
    pub interval: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NotifierConfig {
    pub bark: Option<BarkConfig>,
    pub console: Option<ConsoleConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BarkConfig {
    pub enabled: bool,
    pub server_url: String,
    pub device_key: String,
    pub bark_group: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ConsoleConfig {
    pub enabled: bool,
}
