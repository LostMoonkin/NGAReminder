use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect};
use sha2::{Digest, Sha256};
use sqlx::{Any, Row, Transaction};
use thiserror::Error;
use uuid::Uuid;

use crate::{app::AppState, config::AssetsConfig, domain::thread::ParsedPost, markup};

const ALLOWED_IMAGE_HOSTS: &[&str] = &[
    "img.nga.cn",
    "img4.nga.178.com",
    "img.nga.178.com",
    "img6.nga.cn",
    "img7.nga.cn",
    "img8.nga.cn",
];

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("invalid asset URL")]
    InvalidUrl,
    #[error("asset host is not allowed")]
    HostNotAllowed,
    #[error("asset response was not successful")]
    Http(StatusCode),
    #[error("asset exceeds configured size limit")]
    TooLarge,
    #[error("asset filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("asset HTTP request failed")]
    Request(#[from] reqwest::Error),
    #[error("asset database operation failed")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone, Debug)]
pub struct AssetStore {
    config: AssetsConfig,
}

impl AssetStore {
    pub fn new(config: AssetsConfig) -> Self {
        Self { config }
    }

    pub fn content_path(&self, hash: &str, extension: &str) -> Result<PathBuf, AssetError> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(AssetError::InvalidUrl);
        }
        let extension = safe_extension(extension);
        Ok(self
            .config
            .storage_path
            .join(&hash[..2])
            .join(format!("{hash}.{extension}")))
    }

    pub async fn store_bytes(
        &self,
        bytes: &[u8],
        extension: &str,
    ) -> Result<(String, PathBuf), AssetError> {
        if bytes.len() as u64 > self.config.max_download_bytes {
            return Err(AssetError::TooLarge);
        }
        let hash = hex_hash(bytes);
        let path = self.content_path(&hash, extension)?;
        if !path.is_file() {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let temporary = path.with_extension(format!("part-{}", Uuid::new_v4()));
            tokio::fs::write(&temporary, bytes).await?;
            tokio::fs::rename(&temporary, &path).await?;
        }
        Ok((hash, path))
    }

    pub fn relative_path(&self, path: &Path) -> Result<String, AssetError> {
        let relative = path
            .strip_prefix(&self.config.storage_path)
            .map_err(|_| AssetError::InvalidUrl)?;
        validate_relative_path(relative)
    }
}

pub async fn record_post_assets(
    tx: &mut Transaction<'_, Any>,
    post_id: &str,
    post: &ParsedPost,
    download_enabled: bool,
) -> Result<(), sqlx::Error> {
    let mut references = markup::image_urls(&post.content_raw)
        .into_iter()
        .map(|source_url| (source_url, None, None, "inline".to_owned()))
        .collect::<Vec<_>>();
    references.extend(post.asset_refs.iter().map(|asset| {
        (
            asset.source_url.clone(),
            asset.original_name.clone(),
            asset.mime_type.clone(),
            asset.usage.clone(),
        )
    }));
    let mut seen = std::collections::HashSet::new();
    for (appearance_order, (source_url, original_name, mime_type, usage)) in references
        .into_iter()
        .filter(|(source_url, ..)| seen.insert(source_url.clone()))
        .enumerate()
    {
        let asset_id = Uuid::new_v4().to_string();
        let status = if download_enabled {
            "pending"
        } else {
            "remote_only"
        };
        sqlx::query(
            "INSERT INTO assets (id, source_url, original_name, mime_type, download_status)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (source_url) DO UPDATE SET source_url = EXCLUDED.source_url",
        )
        .bind(&asset_id)
        .bind(&source_url)
        .bind(original_name.or_else(|| original_name_from_url(&source_url)))
        .bind(mime_type)
        .bind(status)
        .execute(&mut **tx)
        .await?;
        let existing_id: String = sqlx::query_scalar("SELECT id FROM assets WHERE source_url = $1")
            .bind(&source_url)
            .fetch_one(&mut **tx)
            .await?;
        sqlx::query(
            "INSERT INTO post_assets (post_id, asset_id, appearance_order, usage)
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(post_id)
        .bind(existing_id)
        .bind(appearance_order as i32)
        .bind(usage)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

pub async fn process_one(state: &AppState) -> anyhow::Result<bool> {
    if !state.config.assets.download_enabled {
        return Ok(false);
    }
    let Some(row) = sqlx::query(
        "SELECT id, source_url, original_name FROM assets
         WHERE download_status = 'pending' ORDER BY first_seen_at LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?
    else {
        return Ok(false);
    };
    let id: String = row.get("id");
    let source_url: String = row.get("source_url");
    let original_name: Option<String> = row.get("original_name");
    sqlx::query("UPDATE assets SET download_status = 'downloading' WHERE id = $1")
        .bind(&id)
        .execute(&state.pool)
        .await?;

    let result = download_asset(&state.config.assets, &source_url, original_name.as_deref()).await;
    match result {
        Ok((hash, relative_path, mime_type, size)) => {
            sqlx::query(
                "UPDATE assets SET content_hash = $1, mime_type = $2, size_bytes = $3,
                 local_relative_path = $4, download_status = 'ready', downloaded_at = CURRENT_TIMESTAMP,
                 last_error_kind = NULL WHERE id = $5",
            )
            .bind(hash)
            .bind(mime_type)
            .bind(size as i64)
            .bind(relative_path)
            .bind(id)
            .execute(&state.pool)
            .await?;
        }
        Err(error) => {
            sqlx::query(
                "UPDATE assets SET download_status = 'failed', last_error_kind = $1 WHERE id = $2",
            )
            .bind(asset_error_kind(&error))
            .bind(id)
            .execute(&state.pool)
            .await?;
        }
    }
    Ok(true)
}

async fn download_asset(
    config: &AssetsConfig,
    source_url: &str,
    original_name: Option<&str>,
) -> Result<(String, String, Option<String>, usize), AssetError> {
    let url = Url::parse(source_url).map_err(|_| AssetError::InvalidUrl)?;
    validate_remote_url(&url)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(redirect::Policy::none())
        .build()?;
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(AssetError::Http(response.status()));
    }
    if response
        .content_length()
        .is_some_and(|size| size > config.max_download_bytes)
    {
        return Err(AssetError::TooLarge);
    }
    let mime_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if bytes.len() as u64 + chunk.len() as u64 > config.max_download_bytes {
            return Err(AssetError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let store = AssetStore::new(config.clone());
    let extension = original_name
        .and_then(|name| name.rsplit('.').next())
        .or_else(|| mime_type.as_deref().and_then(extension_from_mime))
        .unwrap_or("bin");
    let (hash, path) = store.store_bytes(&bytes, extension).await?;
    let relative_path = store.relative_path(&path)?;
    Ok((hash, relative_path, mime_type, bytes.len()))
}

pub fn validate_remote_url(url: &Url) -> Result<(), AssetError> {
    if url.scheme() != "https" {
        return Err(AssetError::InvalidUrl);
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(AssetError::InvalidUrl);
    }
    let Some(host) = url.host_str() else {
        return Err(AssetError::InvalidUrl);
    };
    if !ALLOWED_IMAGE_HOSTS.contains(&host) {
        return Err(AssetError::HostNotAllowed);
    }
    Ok(())
}

pub fn validate_relative_path(path: &Path) -> Result<String, AssetError> {
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(AssetError::InvalidUrl);
    }
    let value = path
        .to_str()
        .ok_or(AssetError::InvalidUrl)?
        .replace('\\', "/");
    if value.is_empty() || value.starts_with('/') {
        return Err(AssetError::InvalidUrl);
    }
    Ok(value)
}

fn original_name_from_url(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()?
        .path_segments()?
        .next_back()
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(160).collect())
}

fn safe_extension(value: &str) -> String {
    let value: String = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(10)
        .collect();
    if value.is_empty() {
        "bin".to_owned()
    } else {
        value.to_ascii_lowercase()
    }
}

fn extension_from_mime(mime: &str) -> Option<&'static str> {
    match mime.split(';').next()?.trim() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "audio/mpeg" => Some("mp3"),
        "video/mp4" => Some("mp4"),
        _ => None,
    }
}

fn hex_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn asset_error_kind(error: &AssetError) -> &'static str {
    match error {
        AssetError::InvalidUrl => "invalid_url",
        AssetError::HostNotAllowed => "host_not_allowed",
        AssetError::Http(_) => "http_error",
        AssetError::TooLarge => "too_large",
        AssetError::Io(_) => "io_error",
        AssetError::Request(_) => "request_error",
        AssetError::Database(_) => "database_error",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sqlx::any::AnyPoolOptions;

    use super::{
        AssetStore, hex_hash, record_post_assets, validate_relative_path, validate_remote_url,
    };
    use crate::{
        config::AssetsConfig,
        domain::thread::{ParsedPost, PostAssetReference, PostKind},
    };

    #[test]
    fn content_addressing_and_path_safety_are_stable() {
        let root = std::env::temp_dir().join(format!("nga-assets-{}", uuid::Uuid::new_v4()));
        let store = AssetStore::new(AssetsConfig {
            download_enabled: true,
            storage_path: root.clone(),
            max_download_bytes: 1024,
        });
        let hash = hex_hash(b"hello");
        assert_eq!(
            store
                .content_path(&hash, "../jpg")
                .expect("path must build"),
            root.join("2c").join(format!("{hash}.jpg"))
        );
        assert!(validate_relative_path(Path::new("../escape")).is_err());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn only_allowlisted_https_image_hosts_are_downloadable() {
        assert!(validate_remote_url(&"https://img.nga.cn/a.jpg".parse().unwrap()).is_ok());
        assert!(validate_remote_url(&"http://img.nga.cn/a.jpg".parse().unwrap()).is_err());
        assert!(validate_remote_url(&"https://example.com/a.jpg".parse().unwrap()).is_err());
    }

    #[tokio::test]
    async fn image_metadata_is_linked_to_posts_and_is_idempotent() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations/sqlite")
            .run(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO threads (tid, fid, title, forum_name, author_uid, author_name) VALUES (1, 2, 't', 'f', 3, 'u')").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO posts (id, tid, floor_number, post_kind, author_uid, author_name, content_raw, page_number, raw_payload) VALUES ('p', 1, 0, 'topic', 3, 'u', 'body', 1, '')").execute(&pool).await.unwrap();
        let post = ParsedPost {
            tid: 1,
            pid: None,
            floor_number: 0,
            kind: PostKind::Topic,
            parent_pid: None,
            parent_is_topic: false,
            author_uid: 3,
            author_name: "u".to_owned(),
            subject: String::new(),
            content_raw: "[img]https://img.nga.cn/a.jpg[/img]".to_owned(),
            published_at_unix: None,
            page_number: 1,
            raw_payload: String::new(),
            asset_refs: vec![PostAssetReference {
                source_url: "https://img.nga.cn/attachments/asset.pdf".to_owned(),
                original_name: Some("asset.pdf".to_owned()),
                mime_type: Some("application/pdf".to_owned()),
                size_bytes: Some(123),
                usage: "attachment".to_owned(),
            }],
        };
        let mut tx = pool.begin().await.unwrap();
        record_post_assets(&mut tx, "p", &post, false)
            .await
            .unwrap();
        record_post_assets(&mut tx, "p", &post, false)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets")
            .fetch_one(&pool)
            .await
            .unwrap();
        let links: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM post_assets")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(assets, 2);
        assert_eq!(links, 2);
    }
}
