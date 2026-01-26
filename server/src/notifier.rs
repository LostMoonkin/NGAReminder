use crate::model::config::{BarkConfig, ConsoleConfig};
use async_trait::async_trait;
use std::collections::HashMap;
use std::string::ToString;
use std::time::Duration;
use url::Url;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_BARK_GROUP: &str = "NGA Reminder";

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send_notification(
        &self,
        title: &String,
        message: &String,
        extra: Option<HashMap<String, String>>,
    ) -> bool;
}

pub struct ConsoleNotifier {
    config: ConsoleConfig,
}

impl ConsoleNotifier {
    pub fn new(config: ConsoleConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Notifier for ConsoleNotifier {
    async fn send_notification(
        &self,
        title: &String,
        message: &String,
        extra: Option<HashMap<String, String>>,
    ) -> bool {
        if !self.config.enabled {
            println!("ConsoleNotifier is disabled, Skipping notification.");
            return false;
        }
        let separator = "=".repeat(80);
        println!("\n{}", separator);
        println!("📱 NOTIFICATION");
        println!("{}", separator);
        println!("Title: {}", title);
        println!("Message: {}", message);
        if let Some(extra_map) = extra {
            if let Some(url) = extra_map.get("url") {
                if !url.is_empty() {
                    println!("URL: {}", url);
                }
            }
        }
        true
    }
}

pub struct BarkNotifier {
    client: reqwest::Client,
    config: BarkConfig,
}

impl BarkNotifier {
    pub fn new(config: BarkConfig) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(DEFAULT_TIMEOUT)
                .build()
                .unwrap(),
            config,
        }
    }
}

#[async_trait]
impl Notifier for BarkNotifier {
    async fn send_notification(
        &self,
        title: &String,
        message: &String,
        extra: Option<HashMap<String, String>>,
    ) -> bool {
        if !self.config.enabled {
            println!("BarkNotifier is disabled, Skipping notification.");
            return false;
        }
        let api_url = Url::parse(&*self.config.server_url);
        if api_url.is_err() {
            println!(
                "Invalid Bark URL: {}, Skipping notification.",
                self.config.server_url
            );
            return false;
        }
        let api_url = api_url.unwrap().join(&*self.config.device_key);
        if api_url.is_err() {
            println!(
                "Invalid Bark device key: {}, Skipping notification.",
                self.config.device_key
            );
            return false;
        }
        let extra_map = extra.unwrap_or_default();
        let api_url = api_url.unwrap();

        let mut body = HashMap::from([("title", title), ("body", message)]);
        if let Some(url) = extra_map.get("url") {
            if !url.is_empty() {
                body.insert("url", url);
            }
        }
        let group = extra_map
            .get("group")
            .cloned()
            .unwrap_or_else(|| DEFAULT_BARK_GROUP.to_string());
        body.insert("group", &group);
        let res = self.client.post(api_url).json(&body).send().await;
        match res {
            Ok(resp) => {
                if resp.status().is_success() {
                    return true;
                }
                println!(
                    "Bark notification failed with status: {}, data: {}",
                    resp.status(),
                    resp.text().await.unwrap_or("".to_string())
                );
            }
            Err(err) => {
                println!("Error sending Bark notification: {}", err);
            }
        }
        false
    }
}
