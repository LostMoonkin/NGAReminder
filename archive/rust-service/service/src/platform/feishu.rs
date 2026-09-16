//! Shared Feishu HTTP integrations that cannot use OpenLark's generated
//! request implementation directly.

use std::time::Duration;

use openlark_auth::AuthTokenProvider;
use openlark_core::auth::{TokenProvider, TokenRequest};
use openlark_core::config::Config as OpenLarkConfig;
use reqwest::{Client, StatusCode, redirect};
use serde::Deserialize;
use thiserror::Error;

use crate::platform::integration::FeishuCredentials;

const FEISHU_API_BASE_URL: &str = "https://open.feishu.cn";
const FEISHU_IMAGE_UPLOAD_PATH: &str = "/open-apis/im/v1/images";
const FEISHU_IMAGE_FIELD: &str = "image";
const FEISHU_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Reusable Feishu image uploader. OpenLark 0.20's generic multipart builder
/// always names the binary part `file`, while this endpoint requires `image`.
/// Keep this compatibility implementation until the SDK endpoint is fixed.
#[derive(Debug)]
pub struct FeishuImageUploader {
    client: Client,
    token_provider: AuthTokenProvider,
    upload_url: String,
}

impl FeishuImageUploader {
    pub fn new(credentials: &FeishuCredentials) -> Result<Self, FeishuImageUploadError> {
        let config = OpenLarkConfig::builder()
            .app_id(credentials.app_id.clone())
            .app_secret(credentials.app_secret.clone())
            .base_url(FEISHU_API_BASE_URL)
            .build();
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(redirect::Policy::none())
            .build()
            .map_err(FeishuImageUploadError::Request)?;
        Ok(Self {
            client,
            token_provider: AuthTokenProvider::new(config),
            upload_url: format!("{FEISHU_API_BASE_URL}{FEISHU_IMAGE_UPLOAD_PATH}"),
        })
    }

    pub async fn upload_message_image(
        &self,
        bytes: Vec<u8>,
        mime_type: &str,
        file_name: &str,
    ) -> Result<String, FeishuImageUploadError> {
        validate_image(&bytes, mime_type, file_name)?;
        let token = self
            .token_provider
            .get_token(TokenRequest::tenant())
            .await
            .map_err(|error| FeishuImageUploadError::Token(summarize(&error.to_string())))?;

        let file_part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name.to_owned())
            .mime_str(mime_type)
            .map_err(FeishuImageUploadError::Request)?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part(FEISHU_IMAGE_FIELD, file_part);
        let response = self
            .client
            .post(&self.upload_url)
            .bearer_auth(token)
            .multipart(form)
            .send()
            .await
            .map_err(FeishuImageUploadError::Request)?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(FeishuImageUploadError::Request)?;
        parse_upload_response(status, &body)
    }
}

#[derive(Debug, Error)]
pub enum FeishuImageUploadError {
    #[error("invalid Feishu image payload: {0}")]
    InvalidPayload(&'static str),
    #[error("failed to obtain Feishu tenant token: {0}")]
    Token(String),
    #[error("Feishu image upload request failed")]
    Request(#[source] reqwest::Error),
    #[error("Feishu image upload returned HTTP {status}: {summary}")]
    Http { status: StatusCode, summary: String },
    #[error("Feishu image upload API returned {code}: {message}")]
    Api { code: i64, message: String },
    #[error("Feishu image upload response omitted image_key")]
    MissingImageKey,
}

#[derive(Deserialize)]
struct ImageUploadResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    data: Option<ImageUploadData>,
}

#[derive(Deserialize)]
struct ImageUploadData {
    image_key: String,
}

fn validate_image(
    bytes: &[u8],
    mime_type: &str,
    file_name: &str,
) -> Result<(), FeishuImageUploadError> {
    if bytes.is_empty() {
        return Err(FeishuImageUploadError::InvalidPayload("image is empty"));
    }
    if bytes.len() > FEISHU_MAX_IMAGE_BYTES {
        return Err(FeishuImageUploadError::InvalidPayload(
            "image exceeds 10 MB",
        ));
    }
    if !matches!(
        mime_type,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/x-icon"
            | "image/tiff"
    ) {
        return Err(FeishuImageUploadError::InvalidPayload(
            "unsupported image MIME type",
        ));
    }
    if file_name.trim().is_empty() || file_name.contains(['\r', '\n']) {
        return Err(FeishuImageUploadError::InvalidPayload(
            "invalid image file name",
        ));
    }
    Ok(())
}

fn parse_upload_response(status: StatusCode, body: &str) -> Result<String, FeishuImageUploadError> {
    if !status.is_success() {
        return Err(FeishuImageUploadError::Http {
            status,
            summary: summarize(body),
        });
    }
    let response: ImageUploadResponse =
        serde_json::from_str(body).map_err(|_| FeishuImageUploadError::Http {
            status,
            summary: "invalid JSON response".to_owned(),
        })?;
    if response.code != 0 {
        return Err(FeishuImageUploadError::Api {
            code: response.code,
            message: summarize(&response.msg),
        });
    }
    response
        .data
        .map(|data| data.image_key)
        .filter(|key| !key.is_empty())
        .ok_or(FeishuImageUploadError::MissingImageKey)
}

fn summarize(value: &str) -> String {
    value.chars().take(512).collect()
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{
        FEISHU_IMAGE_FIELD, FeishuImageUploadError, parse_upload_response, validate_image,
    };

    #[test]
    fn image_endpoint_uses_the_required_multipart_field() {
        assert_eq!(FEISHU_IMAGE_FIELD, "image");
    }

    #[test]
    fn parses_successful_image_upload() {
        let key = parse_upload_response(
            StatusCode::OK,
            r#"{"code":0,"msg":"success","data":{"image_key":"img_v3_test"}}"#,
        )
        .expect("successful response must return the image key");
        assert_eq!(key, "img_v3_test");
    }

    #[test]
    fn preserves_feishu_api_error_code() {
        let error = parse_upload_response(
            StatusCode::OK,
            r#"{"code":234001,"msg":"Invalid request param.","data":null}"#,
        )
        .expect_err("API failure must not be treated as success");
        assert!(matches!(
            error,
            FeishuImageUploadError::Api { code: 234001, .. }
        ));
    }

    #[test]
    fn rejects_empty_and_unsupported_images() {
        assert!(validate_image(&[], "image/png", "captcha.png").is_err());
        assert!(validate_image(&[1], "text/plain", "captcha.txt").is_err());
    }
}
