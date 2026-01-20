use std::io::Error;
use std::time::Duration;
use reqwest::header::COOKIE;
use crate::config_holder::ConfigHolder;
use crate::model::nga_thread::NGAThread;

struct Crawler {
    client: reqwest::Client,
    config_holder: ConfigHolder,
}

impl Crawler {
    pub fn new(config_holder: ConfigHolder) -> Self {
        let crawler_config = config_holder.get_config().crawler;
        // panic if build client failed.
        let client = reqwest::Client::builder()
            .user_agent(crawler_config.user_agent)
            .timeout(Duration::from_secs(crawler_config.timeout))
            .build()
            .unwrap();
        Self {
            client,
            config_holder,
        }
    }

    pub async fn fetch_thread_with_page(&self, tid: u64, page: u64) -> Result<NGAThread, Box<dyn std::error::Error>> {
        let crawler_config = self.config_holder.get_config().crawler;
        let resp = self.client.post(crawler_config.api_url)
            .form(&[("tid", tid), ("page", page)])
            .header(COOKIE, format!("ngaPassportUid=${}; ngaPassportCid=${}", tid, page))
            .send()
            .await?;
        if !resp.status().is_success() {
        }
        Ok(NGAThread{})
    }
}