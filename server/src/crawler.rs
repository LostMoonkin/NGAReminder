use crate::crawler::CrawlerError::ResponseContentError;
use crate::model::config::CrawlerConfig;
use crate::model::nga_thread::NGAThread;
use reqwest::header::COOKIE;
use serde_json::Value;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

#[derive(Debug)]
pub enum CrawlerError {
    HttpError(u16),
    ResponseContentError(String),
}

impl Display for CrawlerError {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            CrawlerError::HttpError(e) => {
                write!(f, "HTTP request failed code: {}", e)
            }
            ResponseContentError(e) => {
                write!(f, "Invalid HTTP response content: {}", e)
            }
        }
    }
}

impl Error for CrawlerError {}

#[derive(Clone)]
pub struct Crawler {
    client: reqwest::Client,
}

impl Crawler {
    pub fn new(user_agent: String, timeout: u64) -> Self {
        // panic if build client failed.
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(timeout))
            .build()
            .unwrap();
        Self { client }
    }

    pub async fn fetch_thread_with_page(
        &self,
        tid: u64,
        page: u64,
        config: &CrawlerConfig,
    ) -> Result<NGAThread, Box<dyn Error>> {
        let resp = self
            .client
            .post(config.api_url.clone())
            .form(&[("tid", tid), ("page", page)])
            .header(
                COOKIE,
                format!(
                    "ngaPassportUid={}; ngaPassportCid={}",
                    config.nga_passport_uid, config.nga_passport_cid
                ),
            )
            .send()
            .await?;
        if !resp.status().is_success() {
            println!(
                "Failed to fetch thread with page, Http status not ok {}: {}",
                tid,
                resp.status()
            );
            return Err(Box::new(CrawlerError::HttpError(resp.status().as_u16())));
        }
        let content = resp.text().await?;
        // check code and message from untyped value
        let value: Value = serde_json::from_str(&content)?;
        let optional_code = value.get("code").unwrap().as_u64();
        if optional_code.is_none() {
            println!(
                "Failed to fetch thread with page, invalid response data {}: {}",
                tid, value
            );
            return Err(Box::new(ResponseContentError(content)));
        }
        let mut thread_data: NGAThread = serde_json::from_value(value)?;
        // set tid as tid in argument
        thread_data.tid = tid;
        for post in &mut thread_data.posts {
            post.page = page;
            post.thread_title = thread_data.title.clone();
        }
        Ok(thread_data)
    }
}
