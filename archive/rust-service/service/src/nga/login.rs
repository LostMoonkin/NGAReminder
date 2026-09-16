#![allow(dead_code)] // stable error-model contract consumed by the login command
//! NGA web login protocol adapter (`nga_web_login_v1`). Versioned against the
//! current public login page; changes must be detected through fixtures, never
//! guessed at runtime. The adapter never persists or logs credentials.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rsa::{RsaPublicKey, pkcs1v15::Pkcs1v15Encrypt, pkcs8::DecodePublicKey};
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::OffsetDateTime;

use crate::bot::session::{CookiePair, LoginProtocolContext};

const LOGIN_ENTRY_URL: &str = "https://bbs.nga.cn/nuke.php?__lib=login&__act=account&login";
const ACCOUNT_PAGE_URL: &str = "https://bbs.nga.cn/nuke/account_copy.html?login";
const LOGIN_CHECK_CODE_URL: &str = "https://bbs.nga.cn/login_check_code.php";
const LOGIN_SUBMIT_URL: &str = "https://bbs.nga.cn/nuke.php";
const MAX_PAGE_BYTES: usize = 1_000_000;
const MAX_CAPTCHA_BYTES: usize = 1_000_000;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 3;
const PROTOCOL_VERSION: &str = "nga_web_login_v1";
const MAX_CANDIDATE_SEARCH_DEPTH: usize = 4;
const MAX_CANDIDATE_SEARCH_NODES: usize = 128;

#[derive(Debug, Error)]
pub enum NgaLoginError {
    #[error("renewal is not configured")]
    RenewalNotConfigured,
    #[error("owner binding is missing")]
    OwnerBindingMissing,
    #[error("login challenge preparation failed")]
    ChallengePrepare,
    #[error("login public key is invalid or changed")]
    PublicKeyInvalid,
    #[error("renewal credentials are invalid")]
    InvalidCredentials,
    #[error("captcha is required")]
    CaptchaRequired,
    #[error("captcha is invalid")]
    CaptchaInvalid,
    #[error("captcha expired")]
    CaptchaExpired,
    #[error("unsupported Tencent captcha")]
    UnsupportedTencentCaptcha,
    #[error("unsupported phone verification")]
    UnsupportedPhoneVerification,
    #[error("NGA login is busy")]
    Busy,
    #[error("NGA login returned HTTP {0}")]
    Http(reqwest::StatusCode),
    #[error("NGA login protocol changed")]
    ProtocolChanged,
    #[error("candidate cookie missing ({response_shape})")]
    CandidateCookieMissing { response_shape: String },
    #[error("candidate cookie invalid")]
    CandidateCookieInvalid,
    #[error("candidate UID mismatch")]
    CandidateUidMismatch,
    #[error("request failed")]
    Request(#[source] reqwest::Error),
}

impl NgaLoginError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RenewalNotConfigured => "renewal_not_configured",
            Self::OwnerBindingMissing => "owner_binding_missing",
            Self::ChallengePrepare => "login_challenge_prepare_failed",
            Self::PublicKeyInvalid => "login_public_key_invalid",
            Self::InvalidCredentials => "invalid_renewal_credentials",
            Self::CaptchaRequired => "captcha_required",
            Self::CaptchaInvalid => "captcha_invalid",
            Self::CaptchaExpired => "captcha_expired",
            Self::UnsupportedTencentCaptcha => "unsupported_tencent_captcha",
            Self::UnsupportedPhoneVerification => "unsupported_phone_verification",
            Self::Busy => "nga_login_busy",
            Self::Http(_) => "nga_login_http_error",
            Self::ProtocolChanged => "nga_login_protocol_changed",
            Self::CandidateCookieMissing { .. } => "candidate_cookie_missing",
            Self::CandidateCookieInvalid => "candidate_cookie_invalid",
            Self::CandidateUidMismatch => "candidate_uid_mismatch",
            Self::Request(_) => "nga_login_http_error",
        }
    }
}

/// Captcha image plus the encrypted protocol context needed to submit.
pub struct LoginChallenge {
    pub image_mime: String,
    pub image: Vec<u8>,
    pub context: LoginProtocolContext,
    pub expires_at: OffsetDateTime,
}

/// Result of submitting login credentials + captcha.
#[derive(Debug)]
pub enum LoginStep {
    CookieCandidate {
        passport_uid: SecretString,
        passport_cid: SecretString,
        cookie_header: SecretString,
    },
    UnsupportedChallenge {
        kind: String,
    },
}

/// A single request context: the adapter owns one HTTP client and a manual
/// cookie jar shared across challenge preparation and submission.
pub struct NgaWebLoginV1 {
    client: reqwest::Client,
    cookie_jar: Vec<CookiePair>,
}

impl NgaWebLoginV1 {
    pub fn new(user_agent: &str) -> Result<Self, NgaLoginError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() < MAX_REDIRECTS
                    && attempt.url().host_str() == Some("bbs.nga.cn")
                {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }))
            .user_agent(user_agent)
            .build()
            .map_err(NgaLoginError::Request)?;
        Ok(Self {
            client,
            cookie_jar: Vec::new(),
        })
    }

    fn cookie_header(&self) -> String {
        self.cookie_jar
            .iter()
            .map(|pair| format!("{}={}", pair.name, pair.value))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn capture_cookies(&mut self, headers: &reqwest::header::HeaderMap) {
        for value in headers.get_all(reqwest::header::SET_COOKIE) {
            let Ok(value) = value.to_str() else { continue };
            let Some((name, rest)) = value.split_once('=') else {
                continue;
            };
            let value = rest.split(';').next().unwrap_or("").to_owned();
            let name = name.trim().to_owned();
            if name.is_empty() {
                continue;
            }
            if let Some(existing) = self.cookie_jar.iter_mut().find(|pair| pair.name == name) {
                existing.value = value;
            } else {
                self.cookie_jar.push(CookiePair { name, value });
            }
        }
    }

    /// Prepare a login challenge: session cookies, RSA public key, fresh
    /// `rid`/`prid` and the captcha image. Does not touch credentials.
    pub async fn prepare_challenge(&mut self) -> Result<LoginChallenge, NgaLoginError> {
        // 1. Establish the login session.
        let entry = self
            .client
            .get(LOGIN_ENTRY_URL)
            .send()
            .await
            .map_err(NgaLoginError::Request)?;
        self.capture_cookies(entry.headers());
        if !entry.status().is_success() {
            return Err(NgaLoginError::Http(entry.status()));
        }
        drop(entry);

        // 2. Fetch the account page and extract the RSA public key.
        let page = self
            .client
            .get(ACCOUNT_PAGE_URL)
            .header(reqwest::header::REFERER, LOGIN_ENTRY_URL)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .send()
            .await
            .map_err(NgaLoginError::Request)?;
        if !page.status().is_success() {
            return Err(NgaLoginError::Http(page.status()));
        }
        self.capture_cookies(page.headers());
        let page_bytes = page.bytes().await.map_err(NgaLoginError::Request)?;
        if page_bytes.len() > MAX_PAGE_BYTES {
            return Err(NgaLoginError::ProtocolChanged);
        }
        let page_html = String::from_utf8_lossy(&page_bytes);
        let public_key_pem =
            extract_public_key_pem(&page_html).ok_or(NgaLoginError::PublicKeyInvalid)?;
        validate_public_key(&public_key_pem)?;

        // 3. Fresh challenge identifiers.
        let rid = format!("login{}", random_digits(17));
        let prid = format!("P{}", random_digits(17));

        // 4. Fetch the captcha image.
        let captcha = self
            .client
            .get(LOGIN_CHECK_CODE_URL)
            .query(&[("id", rid.as_str()), ("from", "login")])
            .header(reqwest::header::REFERER, ACCOUNT_PAGE_URL)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .send()
            .await
            .map_err(NgaLoginError::Request)?;
        if !captcha.status().is_success() {
            return Err(NgaLoginError::Http(captcha.status()));
        }
        self.capture_cookies(captcha.headers());
        let content_type = captcha
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if !content_type.starts_with("image/") {
            return Err(NgaLoginError::ProtocolChanged);
        }
        let image = captcha.bytes().await.map_err(NgaLoginError::Request)?;
        if image.is_empty() || image.len() > MAX_CAPTCHA_BYTES {
            return Err(NgaLoginError::ProtocolChanged);
        }

        let created_at = OffsetDateTime::now_utc();
        let expires_at = created_at + time::Duration::seconds(10 * 60);
        let context = LoginProtocolContext {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            created_at: format_timestamp(created_at),
            expires_at: format_timestamp(expires_at),
            rid,
            prid,
            public_key_pem,
            cookie_jar: self.cookie_jar.clone(),
            captcha_revision: 1,
        };
        Ok(LoginChallenge {
            image_mime: content_type,
            image: image.to_vec(),
            context,
            expires_at,
        })
    }

    /// Submit the login form with the captcha answer. Credentials are
    /// decrypted only here, by the caller, and never leave this call.
    pub async fn submit_login(
        &mut self,
        login_name: &str,
        password: &SecretString,
        context: &LoginProtocolContext,
        captcha_answer: &str,
    ) -> Result<LoginStep, NgaLoginError> {
        // Restore the exact challenge cookie jar.
        self.cookie_jar = context.cookie_jar.clone();

        let account_type = infer_account_type(login_name);
        let password_encrypted = encrypt_password(&context.public_key_pem, password)?;

        let form = reqwest::multipart::Form::new()
            .text("__lib", "login")
            .text("__output", "1")
            .text("app_id", "5004")
            .text("device", "")
            .text("trackid", "")
            .text("__act", "login")
            .text("__ngaClientChecksum", "")
            .text("name", login_name.to_owned())
            .text("type", account_type)
            .text("password", password_encrypted)
            .text("__inchst", "UTF-8")
            .text("rid", context.rid.clone())
            .text("captcha", captcha_answer.to_owned())
            .text("prid", context.prid.clone());

        let response = self
            .client
            .post(LOGIN_SUBMIT_URL)
            .header(reqwest::header::ORIGIN, "https://bbs.nga.cn")
            .header(reqwest::header::REFERER, ACCOUNT_PAGE_URL)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .multipart(form)
            .send()
            .await
            .map_err(NgaLoginError::Request)?;
        if !response.status().is_success() {
            return Err(NgaLoginError::Http(response.status()));
        }
        self.capture_cookies(response.headers());
        let body = response.bytes().await.map_err(NgaLoginError::Request)?;
        if body.len() > MAX_PAGE_BYTES {
            return Err(NgaLoginError::ProtocolChanged);
        }
        let step = match parse_login_response(&body) {
            Err(NgaLoginError::CandidateCookieMissing { response_shape }) => self
                .cookie_candidate()
                .map(|(uid, cid)| LoginStep::CookieCandidate {
                    passport_uid: SecretString::from(uid),
                    passport_cid: SecretString::from(cid),
                    cookie_header: SecretString::from(String::new()),
                })
                .ok_or(NgaLoginError::CandidateCookieMissing { response_shape }),
            result => result,
        }?;
        match step {
            LoginStep::CookieCandidate {
                passport_uid,
                passport_cid,
                ..
            } => {
                let cookie_header = self.cookie_header_for_candidate(
                    passport_uid.expose_secret(),
                    passport_cid.expose_secret(),
                );
                Ok(LoginStep::CookieCandidate {
                    passport_uid,
                    passport_cid,
                    cookie_header: SecretString::from(cookie_header),
                })
            }
            other => Ok(other),
        }
    }

    fn cookie_candidate(&self) -> Option<(String, String)> {
        let uid = self
            .cookie_jar
            .iter()
            .find(|pair| pair.name == "ngaPassportUid")?
            .value
            .as_str();
        let cid = self
            .cookie_jar
            .iter()
            .find(|pair| pair.name == "ngaPassportCid")?
            .value
            .as_str();
        valid_cookie_candidate(uid, cid)
    }

    fn cookie_header_for_candidate(&self, passport_uid: &str, passport_cid: &str) -> String {
        let mut pairs = self.cookie_jar.clone();
        for (name, value) in [
            ("ngaPassportUid", passport_uid),
            ("ngaPassportCid", passport_cid),
        ] {
            if let Some(existing) = pairs.iter_mut().find(|pair| pair.name == name) {
                existing.value = value.to_owned();
            } else {
                pairs.push(CookiePair {
                    name: name.to_owned(),
                    value: value.to_owned(),
                });
            }
        }
        pairs
            .into_iter()
            .map(|pair| format!("{}={}", pair.name, pair.value))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Parse a login response. Accepts direct JSON or a
/// `window.script_muti_get_var_store=<json>;` wrapper. Never executes JS.
fn parse_login_response(body: &[u8]) -> Result<LoginStep, NgaLoginError> {
    let mut text = String::from_utf8_lossy(body).into_owned();
    if let Some(start) = text.find("window.script_muti_get_var_store=") {
        text = text[start + "window.script_muti_get_var_store=".len()..].to_owned();
        text = text.trim_end_matches(';').trim().to_owned();
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| NgaLoginError::ProtocolChanged)?;

    if let Some(error) = value.get("error") {
        return map_login_error(error);
    }

    // data may be an array or an object with numeric keys.
    let data = value.get("data");
    let data3 = if let Some(array) = data.and_then(serde_json::Value::as_array) {
        array.get(3)
    } else if let Some(object) = data.and_then(serde_json::Value::as_object) {
        object.get("3")
    } else {
        None
    };
    let Some(data3) = data3 else {
        return Err(NgaLoginError::CandidateCookieMissing {
            response_shape: response_shape(&value, &serde_json::Value::Null),
        });
    };

    if let Some(candidate) = extract_cookie_candidate_bounded(data3) {
        return Ok(LoginStep::CookieCandidate {
            cookie_header: SecretString::from(format!(
                "ngaPassportUid={}; ngaPassportCid={}",
                candidate.0, candidate.1
            )),
            passport_uid: SecretString::from(candidate.0),
            passport_cid: SecretString::from(candidate.1),
        });
    }
    // Recognizable unsupported challenges.
    let kind = detect_unsupported_challenge(&value, data3);
    if let Some(kind) = kind {
        return Ok(LoginStep::UnsupportedChallenge { kind });
    }
    Err(NgaLoginError::CandidateCookieMissing {
        response_shape: response_shape(&value, data3),
    })
}

fn map_login_error(error: &serde_json::Value) -> Result<LoginStep, NgaLoginError> {
    // error may be string, array, nested array or object.
    let flattened = flatten_error(error);
    if flattened.is_empty() || flattened.iter().all(|item| item.trim().is_empty()) {
        // Old-style empty error: the response carried no usable result.
        return Err(NgaLoginError::CaptchaRequired);
    }
    let joined = flattened.join(" ").to_lowercase();
    if joined.contains("验证码") || joined.contains("captcha") {
        if joined.contains("错误") || joined.contains("invalid") {
            return Err(NgaLoginError::CaptchaInvalid);
        }
        if joined.contains("过期") || joined.contains("expired") {
            return Err(NgaLoginError::CaptchaExpired);
        }
        return Err(NgaLoginError::CaptchaInvalid);
    }
    if joined.contains("密码") || joined.contains("账号") || joined.contains("password") {
        return Err(NgaLoginError::InvalidCredentials);
    }
    if joined.contains("忙") || joined.contains("busy") {
        return Err(NgaLoginError::Busy);
    }
    if joined.contains("腾讯") || joined.contains("安全验证") {
        return Err(NgaLoginError::UnsupportedTencentCaptcha);
    }
    Err(NgaLoginError::ProtocolChanged)
}

fn flatten_error(error: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    match error {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                out.extend(flatten_error(item));
            }
        }
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if let Some(text) = value.as_str() {
                    out.push(text.to_owned());
                } else {
                    out.push(key.clone());
                }
            }
        }
        serde_json::Value::Null => {}
        other => out.push(other.to_string()),
    }
    out
}

fn detect_unsupported_challenge(
    value: &serde_json::Value,
    data3: &serde_json::Value,
) -> Option<String> {
    let text = serde_json::to_string(data3).unwrap_or_default();
    if text.contains("tencent") || text.contains("tcaptcha") || text.contains("腾讯") {
        return Some("tencent".to_owned());
    }
    if text.contains("phone") || text.contains("match_phone") || text.contains("手机") {
        return Some("match_phone".to_owned());
    }
    if value.get("captcha").is_some() || value.get("need_captcha").is_some() {
        return Some("image".to_owned());
    }
    None
}

/// Recognized explicit field combinations inside `data[3]`.
fn extract_cookie_candidate(data3: &serde_json::Value) -> Option<(String, String)> {
    let object = data3.as_object()?;
    for (uid_key, cid_key) in [
        ("uid", "token"),
        ("uid", "cid"),
        ("access_uid", "access_token"),
        ("ngaPassportUid", "ngaPassportCid"),
    ] {
        let Some(uid) = object.get(uid_key).and_then(candidate_uid_value) else {
            continue;
        };
        let Some(cid) = object.get(cid_key).and_then(|value| value.as_str()) else {
            continue;
        };
        if let Some(candidate) = valid_cookie_candidate(&uid, cid) {
            return Some(candidate);
        }
    }
    None
}

fn candidate_uid_value(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|uid| uid.to_string()))
}

fn valid_cookie_candidate(uid: &str, cid: &str) -> Option<(String, String)> {
    if uid.chars().all(|ch| ch.is_ascii_digit())
        && !uid.is_empty()
        && !cid.is_empty()
        && cid.len() <= 512
    {
        Some((uid.to_owned(), cid.to_owned()))
    } else {
        None
    }
}

/// Search only below the documented `data[3]` success node. The limits keep a
/// changed or hostile response from causing unbounded traversal, while still
/// accepting the wrapper objects used by NGA's web scripts.
fn extract_cookie_candidate_bounded(data3: &serde_json::Value) -> Option<(String, String)> {
    fn visit(
        value: &serde_json::Value,
        depth: usize,
        remaining: &mut usize,
    ) -> Option<(String, String)> {
        if *remaining == 0 || depth > MAX_CANDIDATE_SEARCH_DEPTH {
            return None;
        }
        *remaining -= 1;
        if let Some(candidate) = extract_cookie_candidate(value) {
            return Some(candidate);
        }
        match value {
            serde_json::Value::Array(items) => items
                .iter()
                .find_map(|item| visit(item, depth + 1, remaining)),
            serde_json::Value::Object(object) => object
                .values()
                .find_map(|item| visit(item, depth + 1, remaining)),
            _ => None,
        }
    }

    let mut remaining = MAX_CANDIDATE_SEARCH_NODES;
    visit(data3, 0, &mut remaining)
}

/// Return field names and a stable structural fingerprint without response
/// values. This is safe to put in diagnostics when the login protocol changes.
fn response_shape(value: &serde_json::Value, data3: &serde_json::Value) -> String {
    fn keys(value: &serde_json::Value) -> String {
        value
            .as_object()
            .map(|object| {
                object
                    .keys()
                    .take(16)
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| value_type(value).to_owned())
    }

    fn describe(value: &serde_json::Value, depth: usize, remaining: &mut usize, out: &mut String) {
        if *remaining == 0 || depth > MAX_CANDIDATE_SEARCH_DEPTH {
            out.push('!');
            return;
        }
        *remaining -= 1;
        match value {
            serde_json::Value::Null => out.push('n'),
            serde_json::Value::Bool(_) => out.push('b'),
            serde_json::Value::Number(_) => out.push('#'),
            serde_json::Value::String(_) => out.push('s'),
            serde_json::Value::Array(items) => {
                out.push('[');
                for item in items {
                    describe(item, depth + 1, remaining, out);
                    out.push(',');
                }
                out.push(']');
            }
            serde_json::Value::Object(object) => {
                out.push('{');
                for (key, item) in object {
                    out.push_str(key);
                    out.push(':');
                    describe(item, depth + 1, remaining, out);
                    out.push(',');
                }
                out.push('}');
            }
        }
    }

    let mut descriptor = String::new();
    let mut remaining = MAX_CANDIDATE_SEARCH_NODES;
    describe(value, 0, &mut remaining, &mut descriptor);
    let digest = Sha256::digest(descriptor.as_bytes());
    let fingerprint = digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(
        "shape={fingerprint}; top_keys=[{}]; data3_keys=[{}]",
        keys(value),
        keys(data3)
    )
}

fn value_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Extract the first PEM RSA public key from the account page HTML.
fn extract_public_key_pem(html: &str) -> Option<String> {
    let start = html.find("-----BEGIN PUBLIC KEY-----")?;
    let rest = &html[start..];
    let end = rest.find("-----END PUBLIC KEY-----")? + "-----END PUBLIC KEY-----".len();
    // NGA embeds the PEM in a JavaScript string. The current page uses both
    // an escaped newline and a JavaScript physical-line continuation (`\n\` +
    // CRLF/LF), while older fixtures use plain escaped newlines. Normalize
    // those representations before handing the PEM to the RSA parser.
    Some(
        rest[..end]
            .replace("\\n\\\r\n", "\n")
            .replace("\\n\\\n", "\n")
            .replace("\\n", "\n")
            .replace('\r', ""),
    )
}

fn validate_public_key(pem: &str) -> Result<(), NgaLoginError> {
    if pem.len() > 4096 {
        return Err(NgaLoginError::PublicKeyInvalid);
    }
    RsaPublicKey::from_public_key_pem(pem).map_err(|_| NgaLoginError::PublicKeyInvalid)?;
    Ok(())
}

fn encrypt_password(pem: &str, password: &SecretString) -> Result<String, NgaLoginError> {
    let public_key =
        RsaPublicKey::from_public_key_pem(pem).map_err(|_| NgaLoginError::PublicKeyInvalid)?;
    let encrypted = public_key
        .encrypt(
            &mut rsa::rand_core::OsRng,
            Pkcs1v15Encrypt,
            password.expose_secret().as_bytes(),
        )
        .map_err(|_| NgaLoginError::ProtocolChanged)?;
    Ok(STANDARD.encode(encrypted))
}

fn infer_account_type(login_name: &str) -> &'static str {
    let digits_only = login_name.chars().all(|ch| ch.is_ascii_digit());
    if digits_only && login_name.len() <= 9 {
        "id"
    } else if login_name.contains('@') {
        "mail"
    } else if digits_only && login_name.len() >= 10 {
        "phone"
    } else {
        ""
    }
}

fn random_digits(length: usize) -> String {
    // CSPRNG-backed via uuid v4 bytes; avoids an extra rand dependency.
    let mut out = String::with_capacity(length);
    while out.len() < length {
        let bytes = uuid::Uuid::new_v4().as_bytes().to_vec();
        for byte in bytes {
            if out.len() >= length {
                break;
            }
            out.push(char::from(b'0' + (byte % 10)));
        }
    }
    out
}

fn format_timestamp(when: OffsetDateTime) -> String {
    when.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::{
        LoginStep, NgaLoginError, NgaWebLoginV1, extract_cookie_candidate,
        extract_cookie_candidate_bounded, extract_public_key_pem, infer_account_type,
        map_login_error, parse_login_response, validate_public_key,
    };

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/nga")
                .join(name),
        )
        .expect("fixture must read")
    }

    #[test]
    fn account_type_inference_matches_documented_rules() {
        assert_eq!(infer_account_type("123456"), "id");
        assert_eq!(infer_account_type("13800000000"), "phone");
        assert_eq!(infer_account_type("user@example.com"), "mail");
        assert_eq!(infer_account_type("username"), "");
    }

    #[test]
    fn success_response_yields_cookie_candidate() {
        let step = parse_login_response(&fixture("login_success.json")).expect("must parse");
        match step {
            LoginStep::CookieCandidate {
                passport_uid,
                passport_cid,
                cookie_header,
            } => {
                assert_eq!(passport_uid.expose_secret(), "12345678");
                assert!(!passport_cid.expose_secret().is_empty());
                assert!(
                    cookie_header
                        .expose_secret()
                        .contains("ngaPassportUid=12345678")
                );
            }
            _ => panic!("expected cookie candidate"),
        }
    }

    #[test]
    fn wrapped_success_response_is_unwrapped() {
        let step =
            parse_login_response(&fixture("login_success_wrapped.json")).expect("must parse");
        assert!(matches!(step, LoginStep::CookieCandidate { .. }));
    }

    #[test]
    fn empty_old_style_error_maps_to_captcha_required() {
        let error =
            parse_login_response(&fixture("login_empty_error.json")).expect_err("must fail");
        assert!(matches!(error, NgaLoginError::CaptchaRequired));
    }

    #[test]
    fn wrong_captcha_maps_to_captcha_invalid() {
        let error =
            parse_login_response(&fixture("login_captcha_error.json")).expect_err("must fail");
        assert!(matches!(error, NgaLoginError::CaptchaInvalid));
    }

    #[test]
    fn wrong_password_maps_to_invalid_credentials() {
        let error =
            parse_login_response(&fixture("login_password_error.json")).expect_err("must fail");
        assert!(matches!(error, NgaLoginError::InvalidCredentials));
    }

    #[test]
    fn busy_maps_to_busy() {
        let error = parse_login_response(&fixture("login_busy.json")).expect_err("must fail");
        assert!(matches!(error, NgaLoginError::Busy));
    }

    #[test]
    fn missing_candidate_maps_to_protocol_changed() {
        let error =
            parse_login_response(&fixture("login_missing_candidate.json")).expect_err("must fail");
        let NgaLoginError::CandidateCookieMissing { response_shape } = error else {
            panic!("expected candidate-cookie-missing error");
        };
        assert!(response_shape.contains("shape="));
        assert!(response_shape.contains("data3_keys=[]"));
    }

    #[test]
    fn account_page_public_key_is_extracted() {
        let html = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/nga/account_page.html"),
        )
        .expect("fixture must read");
        let pem = extract_public_key_pem(&html).expect("public key must be found");
        assert!(pem.starts_with("-----BEGIN PUBLIC KEY-----"));
        assert!(pem.ends_with("-----END PUBLIC KEY-----"));
        assert!(!pem.contains("\\n"));
        validate_public_key(&pem).expect("fixture public key must be a valid RSA key");
    }

    #[tokio::test]
    #[ignore = "requires live access to NGA's public login and captcha endpoints"]
    async fn live_login_challenge_protocol_is_supported() {
        let mut adapter = NgaWebLoginV1::new("Mozilla/5.0 (compatible; NGA-Reminder-Test/0.1)")
            .expect("login adapter must initialize");
        let challenge = adapter
            .prepare_challenge()
            .await
            .expect("live NGA challenge must remain parseable");

        assert!(challenge.image_mime.starts_with("image/"));
        assert!(!challenge.image.is_empty());
        validate_public_key(&challenge.context.public_key_pem)
            .expect("live public key must be valid");
    }

    #[test]
    fn candidate_extraction_accepts_both_array_and_object() {
        let array = serde_json::json!([null, null, null, {"uid": "123", "token": "tok"}]);
        assert_eq!(
            extract_cookie_candidate(&array[3]),
            Some(("123".to_owned(), "tok".to_owned()))
        );
        let object = serde_json::json!({"3": {"ngaPassportUid": "9", "ngaPassportCid": "c"}});
        assert_eq!(
            extract_cookie_candidate(&object["3"]),
            Some(("9".to_owned(), "c".to_owned()))
        );
        let numeric_uid = serde_json::json!({"uid": 123456, "token": "tok"});
        assert_eq!(
            extract_cookie_candidate(&numeric_uid),
            Some(("123456".to_owned(), "tok".to_owned()))
        );
        // Non-numeric uid is rejected.
        assert!(extract_cookie_candidate(&serde_json::json!({"uid": "x", "cid": "y"})).is_none());
    }

    #[test]
    fn candidate_extraction_accepts_bounded_nested_wrappers() {
        let nested = serde_json::json!({
            "result": {
                "account": {
                    "uid": "123",
                    "token": "candidate-token"
                }
            }
        });
        assert_eq!(
            extract_cookie_candidate_bounded(&nested),
            Some(("123".to_owned(), "candidate-token".to_owned()))
        );

        let too_deep = serde_json::json!({
            "a": {"b": {"c": {"d": {"e": {
                "uid": "123", "token": "candidate-token"
            }}}}}
        });
        assert!(extract_cookie_candidate_bounded(&too_deep).is_none());
    }

    #[test]
    fn candidate_can_fall_back_to_explicit_response_cookies() {
        let mut adapter = NgaWebLoginV1::new("NGA-Reminder-Test").expect("adapter must initialize");
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append(
            reqwest::header::SET_COOKIE,
            "ngaPassportUid=123456; Path=/; HttpOnly"
                .parse()
                .expect("header must parse"),
        );
        headers.append(
            reqwest::header::SET_COOKIE,
            "ngaPassportCid=candidate-token; Path=/; HttpOnly"
                .parse()
                .expect("header must parse"),
        );
        headers.append(
            reqwest::header::SET_COOKIE,
            "login_session=fresh; Path=/; HttpOnly"
                .parse()
                .expect("header must parse"),
        );
        adapter.capture_cookies(&headers);
        assert_eq!(
            adapter.cookie_candidate(),
            Some(("123456".to_owned(), "candidate-token".to_owned()))
        );
        assert_eq!(
            adapter.cookie_header_for_candidate("123456", "candidate-token"),
            "ngaPassportUid=123456; ngaPassportCid=candidate-token; login_session=fresh"
        );
    }

    #[test]
    fn error_mapping_handles_object_and_array_shapes() {
        let object_error = serde_json::json!({"code": "1001", "error": {"msg": "验证码错误"}});
        assert!(matches!(
            map_login_error(object_error.get("error").unwrap()),
            Err(NgaLoginError::CaptchaInvalid)
        ));
        let array_error = serde_json::json!(["密码错误", "0"]);
        assert!(matches!(
            map_login_error(&array_error),
            Err(NgaLoginError::InvalidCredentials)
        ));
    }
}
