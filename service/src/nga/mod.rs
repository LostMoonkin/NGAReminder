pub mod thread_parser;
pub mod user_parser;

use std::{sync::Arc, time::Duration};

use reqwest::{
    Client, RequestBuilder, StatusCode,
    header::{ACCEPT, ACCEPT_LANGUAGE, CONTENT_TYPE, COOKIE, ORIGIN, REFERER, USER_AGENT},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;
use tokio::time::Instant;

const BASE_URL: &str = "https://bbs.nga.cn";
const REQUEST_INTERVAL: Duration = Duration::from_millis(500);
const USER_BUSY_ATTEMPTS: usize = 10;
const USER_BUSY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct NgaClient {
    client: Client,
    user_agent: String,
    base_url: String,
    request_gate: Arc<Mutex<Instant>>,
}

#[derive(Debug, Error)]
pub enum AuthCheckError {
    #[error("NGA request failed")]
    Request(#[source] reqwest::Error),
    #[error("NGA returned HTTP {0}")]
    Http(StatusCode),
    #[error("NGA credentials were rejected")]
    Unauthorized,
    #[error("NGA remained busy after retries")]
    Busy,
}

#[derive(Debug, Error)]
pub enum NgaRequestError {
    #[error("NGA request failed")]
    Request(#[source] reqwest::Error),
    #[error("NGA returned HTTP {0}")]
    Http(StatusCode),
    #[error("NGA credentials were rejected")]
    Unauthorized,
    #[error("NGA remained busy after retries")]
    Busy,
    #[error("NGA thread was not found")]
    NotFound,
    #[error("NGA thread is pending review")]
    PendingReview,
    #[error("NGA returned business error {code}")]
    Business { code: i64 },
    #[error("NGA response could not be decoded")]
    Decode(#[source] serde_json::Error),
}

#[derive(Debug, Serialize)]
pub struct AuthCheck {
    pub uid: i64,
    pub valid: bool,
}

#[derive(Deserialize)]
struct NgaEnvelope {
    code: i64,
    #[serde(default)]
    msg: String,
}

impl NgaClient {
    pub fn new(user_agent: String) -> anyhow::Result<Self> {
        Self::with_base_url(user_agent, BASE_URL)
    }

    fn with_base_url(user_agent: String, base_url: &str) -> anyhow::Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self {
            client,
            user_agent,
            base_url: base_url.trim_end_matches('/').to_owned(),
            request_gate: Arc::new(Mutex::new(Instant::now() - REQUEST_INTERVAL)),
        })
    }

    pub async fn check_credentials(
        &self,
        passport_uid: &str,
        passport_cid: &str,
    ) -> Result<AuthCheck, AuthCheckError> {
        let uid = passport_uid
            .parse::<i64>()
            .map_err(|_| AuthCheckError::Unauthorized)?;

        for attempt in 0..USER_BUSY_ATTEMPTS {
            self.wait_for_request_slot().await;
            let request = self.client.get(format!(
                "{}/thread.php?searchpost=1&authorid={uid}&__output=12",
                self.base_url
            ));
            let response = self
                .common_headers(request, passport_uid, passport_cid)
                .send()
                .await
                .map_err(AuthCheckError::Request)?;

            if response.status() != StatusCode::OK {
                return Err(AuthCheckError::Http(response.status()));
            }
            let envelope: NgaEnvelope = response.json().await.map_err(AuthCheckError::Request)?;

            match classify_envelope(&envelope) {
                Ok(()) => return Ok(AuthCheck { uid, valid: true }),
                Err(AuthCheckError::Busy) if attempt < 9 => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                result => return result.map(|()| AuthCheck { uid, valid: true }),
            }
        }

        Err(AuthCheckError::Busy)
    }

    pub async fn fetch_thread_page(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        tid: i64,
        page: i32,
    ) -> Result<Value, NgaRequestError> {
        for attempt in 0_u64..3 {
            let result = self
                .fetch_thread_page_once(passport_uid, passport_cid, tid, page)
                .await;
            let retryable = matches!(
                &result,
                Err(NgaRequestError::Request(_))
                    | Err(NgaRequestError::Http(StatusCode::TOO_MANY_REQUESTS))
            ) || matches!(&result, Err(NgaRequestError::Http(status)) if status.is_server_error());
            if !retryable || attempt == 2 {
                return result;
            }
            let jitter_ms = (tid.unsigned_abs() + page as u64 * 31 + attempt * 17) % 250;
            tokio::time::sleep(Duration::from_millis(
                500 * 2_u64.pow(attempt as u32) + jitter_ms,
            ))
            .await;
        }
        unreachable!("thread request retry loop always returns")
    }

    pub async fn fetch_post_by_pid(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        tid: i64,
        pid: i64,
    ) -> Result<Value, NgaRequestError> {
        for attempt in 0_u64..3 {
            self.wait_for_request_slot().await;
            let request = self
                .client
                .post(format!(
                    "{}/app_api.php?__lib=post&__act=list",
                    self.base_url
                ))
                .form(&[("tid", tid.to_string()), ("pid", pid.to_string())]);
            let result = self.send_json(request, passport_uid, passport_cid).await;
            let retryable = retryable_data_error(&result);
            if !retryable || attempt == 2 {
                return result;
            }
            self.retry_delay(tid, attempt).await;
        }
        unreachable!("post request retry loop always returns")
    }

    pub async fn fetch_user_topics(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        uid: i64,
        page: i32,
    ) -> Result<Value, NgaRequestError> {
        self.fetch_user_list(passport_uid, passport_cid, uid, page, false)
            .await
    }

    pub async fn fetch_user_replies(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        uid: i64,
        page: i32,
    ) -> Result<Value, NgaRequestError> {
        self.fetch_user_list(passport_uid, passport_cid, uid, page, true)
            .await
    }

    pub async fn fetch_user_profile(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        uid: i64,
    ) -> Result<Vec<u8>, NgaRequestError> {
        self.wait_for_request_slot().await;
        let request = self
            .client
            .get(format!("{}/nuke.php?func=ucp&uid={uid}", self.base_url));
        let response = self
            .common_headers(request, passport_uid, passport_cid)
            .send()
            .await
            .map_err(NgaRequestError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(NgaRequestError::Http(response.status()));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(NgaRequestError::Request)
    }

    async fn fetch_user_list(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        uid: i64,
        page: i32,
        replies: bool,
    ) -> Result<Value, NgaRequestError> {
        for attempt in 0..10 {
            self.wait_for_request_slot().await;
            let search = if replies { "searchpost=1&" } else { "" };
            let request = self.client.get(format!(
                "{}/thread.php?{search}authorid={uid}&__output=12&page={page}",
                self.base_url
            ));
            let result = self.send_json(request, passport_uid, passport_cid).await;
            match result {
                Err(NgaRequestError::Busy) if attempt + 1 < USER_BUSY_ATTEMPTS => {
                    tokio::time::sleep(USER_BUSY_DELAY).await;
                }
                result => return result,
            }
        }
        Err(NgaRequestError::Busy)
    }

    async fn fetch_thread_page_once(
        &self,
        passport_uid: &str,
        passport_cid: &str,
        tid: i64,
        page: i32,
    ) -> Result<Value, NgaRequestError> {
        self.wait_for_request_slot().await;
        let request = self
            .client
            .post(format!(
                "{}/app_api.php?__lib=post&__act=list",
                self.base_url
            ))
            .form(&[("tid", tid.to_string()), ("page", page.to_string())]);
        self.send_json(request, passport_uid, passport_cid).await
    }

    async fn send_json(
        &self,
        request: RequestBuilder,
        passport_uid: &str,
        passport_cid: &str,
    ) -> Result<Value, NgaRequestError> {
        let response = self
            .common_headers(request, passport_uid, passport_cid)
            .send()
            .await
            .map_err(NgaRequestError::Request)?;
        if response.status() != StatusCode::OK {
            return Err(NgaRequestError::Http(response.status()));
        }
        let bytes = response.bytes().await.map_err(NgaRequestError::Request)?;
        let value: Value = decode_json(&bytes)?;
        classify_data_envelope(&value)?;
        Ok(value)
    }

    fn common_headers(
        &self,
        request: RequestBuilder,
        passport_uid: &str,
        passport_cid: &str,
    ) -> RequestBuilder {
        request
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(USER_AGENT, &self.user_agent)
            .header(ACCEPT, "application/json, text/javascript, */*; q=0.01")
            .header(ACCEPT_LANGUAGE, "en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7")
            .header(
                COOKIE,
                format!("ngaPassportUid={passport_uid}; ngaPassportCid={passport_cid}"),
            )
            .header(ORIGIN, &self.base_url)
            .header(REFERER, format!("{}/", self.base_url))
    }

    async fn wait_for_request_slot(&self) {
        let mut last_request = self.request_gate.lock().await;
        let elapsed = last_request.elapsed();
        if elapsed < REQUEST_INTERVAL {
            tokio::time::sleep(REQUEST_INTERVAL - elapsed).await;
        }
        *last_request = Instant::now();
    }

    async fn retry_delay(&self, seed: i64, attempt: u64) {
        let jitter_ms = (seed.unsigned_abs() + attempt * 17) % 250;
        tokio::time::sleep(Duration::from_millis(
            500 * 2_u64.pow(attempt as u32) + jitter_ms,
        ))
        .await;
    }
}

fn retryable_data_error(result: &Result<Value, NgaRequestError>) -> bool {
    matches!(
        result,
        Err(NgaRequestError::Request(_))
            | Err(NgaRequestError::Http(StatusCode::TOO_MANY_REQUESTS))
    ) || matches!(result, Err(NgaRequestError::Http(status)) if status.is_server_error())
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, NgaRequestError> {
    serde_json::from_slice(bytes).map_err(NgaRequestError::Decode)
}

fn classify_data_envelope(value: &Value) -> Result<(), NgaRequestError> {
    let code = value
        .get("code")
        .and_then(|code| {
            code.as_i64()
                .or_else(|| code.as_str().and_then(|code| code.parse().ok()))
        })
        .ok_or(NgaRequestError::Business { code: -1 })?;
    let message = value.get("msg").and_then(Value::as_str).unwrap_or_default();
    match code {
        0 => Ok(()),
        14 => Err(NgaRequestError::NotFound),
        51 => Err(NgaRequestError::PendingReview),
        46 => Err(NgaRequestError::Unauthorized),
        2048 if message.contains("服务器忙") => Err(NgaRequestError::Busy),
        2048 if message.contains("必须登录") || message.contains("请登录") => {
            Err(NgaRequestError::Unauthorized)
        }
        code => Err(NgaRequestError::Business { code }),
    }
}

fn classify_envelope(envelope: &NgaEnvelope) -> Result<(), AuthCheckError> {
    if envelope.code == 0 {
        return Ok(());
    }
    if envelope.code == 2048 && envelope.msg.contains("服务器忙") {
        return Err(AuthCheckError::Busy);
    }
    if envelope.code == 46
        || (envelope.code == 2048
            && (envelope.msg.contains("必须登录") || envelope.msg.contains("请登录")))
    {
        return Err(AuthCheckError::Unauthorized);
    }
    Err(AuthCheckError::Unauthorized)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{
        AuthCheckError, NgaEnvelope, NgaRequestError, USER_BUSY_ATTEMPTS, classify_data_envelope,
        classify_envelope,
    };

    #[test]
    fn distinguishes_busy_from_login_required() {
        let busy = NgaEnvelope {
            code: 2048,
            msg: "服务器忙,请稍后重试".to_owned(),
        };
        let login = NgaEnvelope {
            code: 2048,
            msg: "你必须登录".to_owned(),
        };

        assert!(matches!(
            classify_envelope(&busy),
            Err(AuthCheckError::Busy)
        ));
        assert!(matches!(
            classify_envelope(&login),
            Err(AuthCheckError::Unauthorized)
        ));
    }

    #[test]
    fn classifies_thread_business_errors() {
        let missing: Value = fixture("invalid_tid_14.json");
        assert!(matches!(
            classify_data_envelope(&missing),
            Err(NgaRequestError::NotFound)
        ));

        let pending_review: Value = fixture("thread_pending_review_51.json");
        assert!(matches!(
            classify_data_envelope(&pending_review),
            Err(NgaRequestError::PendingReview)
        ));

        let unauthorized: Value = fixture("missing_auth_46.json");
        assert!(matches!(
            classify_data_envelope(&unauthorized),
            Err(NgaRequestError::Unauthorized)
        ));

        let busy: Value = fixture("busy_2048.json");
        assert!(matches!(
            classify_data_envelope(&busy),
            Err(NgaRequestError::Busy)
        ));
    }

    #[test]
    fn user_busy_policy_has_ten_total_attempts() {
        let retry_attempts = (0..USER_BUSY_ATTEMPTS)
            .filter(|attempt| attempt + 1 < USER_BUSY_ATTEMPTS)
            .count();
        assert_eq!(retry_attempts, 9);
        assert_eq!(USER_BUSY_ATTEMPTS, 10);
    }

    #[test]
    fn m0_thread_fixtures_match_frozen_contract() {
        let page: Value = fixture("thread_page_success.json");
        assert_eq!(page["code"], 0);
        let posts = page["result"].as_array().expect("result must be an array");
        assert_eq!(posts[0]["pid"], 0);
        assert_eq!(posts[0]["lou"], 0);

        let comments: Value = fixture("thread_comments_hot_post.json");
        assert!(comments["hot_post"].is_array());
        assert!(
            comments["result"]
                .as_array()
                .expect("result must be an array")
                .iter()
                .any(|post| post["comments"].is_array())
        );

        let attachments: Value = fixture("thread_attachments.json");
        assert!(
            attachments["result"]
                .as_array()
                .expect("result must be an array")
                .iter()
                .any(|post| post["attches"].is_array())
        );
    }

    #[test]
    fn m0_user_fixtures_match_frozen_contract() {
        let replies: Value = fixture("user_replies_success.json");
        assert_eq!(replies["code"], 0);
        assert_eq!(replies["result"]["__R__ROWS_PAGE"], 20);
        assert!(
            replies["result"]["__T"]
                .as_array()
                .expect("__T must be an array")
                .iter()
                .all(|item| item["__P"].is_object())
        );

        let topics: Value = fixture("user_topics_page_1.json");
        assert!(topics["result"]["__T"].is_array());

        let busy: Value = fixture("busy_2048.json");
        assert_eq!(busy["code"], 2048);
        assert!(
            busy["msg"]
                .as_str()
                .expect("busy message must be text")
                .contains("服务器忙")
        );
    }

    fn fixture(name: &str) -> Value {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nga")
            .join(name);
        let raw = std::fs::read_to_string(root).expect("fixture must be readable");
        serde_json::from_str(&raw).expect("fixture must be valid JSON")
    }
}
