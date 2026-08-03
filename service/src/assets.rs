use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use anyhow::Context as _;
use futures_util::StreamExt;
use reqwest::{Client, StatusCode, Url, redirect};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{Any, Row, Transaction};
use thiserror::Error;
use time::OffsetDateTime;
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

pub const DEFAULT_MAINTENANCE_RETENTION_SECONDS: u64 = 24 * 60 * 60;
const MAINTENANCE_EXAMPLE_LIMIT: usize = 20;

#[derive(Clone, Debug, Serialize)]
pub struct MaintenanceReport {
    pub scanned_at: String,
    pub retention_seconds: u64,
    pub database_assets: i64,
    pub ready_assets: i64,
    pub missing_file_count: usize,
    pub orphan_metadata_count: i64,
    pub orphan_file_count: usize,
    pub stale_orphan_file_count: usize,
    pub stale_temp_file_count: usize,
    pub missing_file_examples: Vec<String>,
    pub orphan_file_examples: Vec<String>,
    pub stale_temp_file_examples: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MaintenanceCleanupResult {
    pub repaired_missing_assets: usize,
    pub deleted_orphan_metadata: u64,
    pub deleted_orphan_files: usize,
    pub deleted_temp_files: usize,
    pub report: MaintenanceReport,
}

#[derive(Debug)]
struct MaintenanceScan {
    report: MaintenanceReport,
    missing_asset_ids: Vec<String>,
    stale_orphan_files: Vec<String>,
    stale_temp_files: Vec<String>,
}

#[derive(Debug, Default)]
struct FilesystemScan {
    orphan_files: Vec<String>,
    stale_orphan_files: Vec<String>,
    stale_temp_files: Vec<String>,
}

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
    let claimed = sqlx::query(
        "UPDATE assets SET download_status = 'downloading'
         WHERE id = $1 AND download_status = 'pending'",
    )
    .bind(&id)
    .execute(&state.pool)
    .await?;
    if claimed.rows_affected() == 0 {
        // Another worker claimed the row after our SELECT. Report progress so
        // this worker continues looking for another pending asset.
        return Ok(true);
    }

    let result = download_asset(&state.config.assets, &source_url, original_name.as_deref()).await;
    match result {
        Ok((hash, relative_path, mime_type, size)) => {
            let update = sqlx::query(
                "UPDATE assets SET content_hash = $1, mime_type = $2, size_bytes = $3,
                 local_relative_path = $4, download_status = 'ready', downloaded_at = CURRENT_TIMESTAMP,
                 last_error_kind = NULL WHERE id = $5",
            )
            .bind(hash)
            .bind(mime_type)
            .bind(size as i64)
            .bind(relative_path)
            .bind(&id)
            .execute(&state.pool)
            .await;
            if let Err(error) = update {
                // The claim is committed before the network request. Make the
                // job retryable if final metadata persistence fails.
                let _ = sqlx::query(
                    "UPDATE assets SET download_status = 'pending', last_error_kind = 'database_error'
                     WHERE id = $1 AND download_status = 'downloading'",
                )
                .bind(&id)
                .execute(&state.pool)
                .await;
                return Err(error.into());
            }
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

pub async fn maintenance_report(
    state: &AppState,
    retention_seconds: u64,
) -> anyhow::Result<MaintenanceReport> {
    Ok(scan_maintenance(state, retention_seconds).await?.report)
}

pub async fn cleanup_maintenance(
    state: &AppState,
    retention_seconds: u64,
) -> anyhow::Result<MaintenanceCleanupResult> {
    let scan = scan_maintenance(state, retention_seconds).await?;
    let retry_status = if state.config.assets.download_enabled {
        "pending"
    } else {
        "remote_only"
    };
    let mut transaction = state.pool.begin().await?;
    let mut repaired_missing_assets = 0;
    for id in &scan.missing_asset_ids {
        repaired_missing_assets += sqlx::query(
            "UPDATE assets SET content_hash = NULL, size_bytes = NULL,
             local_relative_path = NULL, download_status = $1,
             downloaded_at = NULL, last_error_kind = 'missing_local_file'
             WHERE id = $2 AND download_status = 'ready'",
        )
        .bind(retry_status)
        .bind(id)
        .execute(&mut *transaction)
        .await?
        .rows_affected() as usize;
    }
    let deleted_orphan_metadata = sqlx::query(
        "DELETE FROM assets
         WHERE download_status <> 'downloading'
           AND NOT EXISTS (
             SELECT 1 FROM post_assets relation WHERE relation.asset_id = assets.id
           )",
    )
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    transaction.commit().await?;

    // Re-read references after the database transaction. This prevents a file
    // that became referenced while the scan was running from being removed.
    let referenced_files = load_referenced_ready_paths(state).await?;
    let removable_orphans = scan
        .stale_orphan_files
        .into_iter()
        .filter(|path| !referenced_files.contains(path))
        .collect::<Vec<_>>();
    let root = state.config.assets.storage_path.clone();
    let temp_files = scan.stale_temp_files;
    let (deleted_orphan_files, deleted_temp_files) = tokio::task::spawn_blocking(move || {
        let orphan_count = remove_scanned_files(&root, &removable_orphans);
        let temp_count = remove_scanned_files(&root, &temp_files);
        remove_empty_asset_directories(&root);
        (orphan_count, temp_count)
    })
    .await
    .context("asset cleanup task failed")?;

    let report = maintenance_report(state, retention_seconds).await?;
    Ok(MaintenanceCleanupResult {
        repaired_missing_assets,
        deleted_orphan_metadata,
        deleted_orphan_files,
        deleted_temp_files,
        report,
    })
}

async fn scan_maintenance(
    state: &AppState,
    retention_seconds: u64,
) -> anyhow::Result<MaintenanceScan> {
    let database_assets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets")
        .fetch_one(&state.pool)
        .await?;
    let orphan_metadata_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM assets a
         WHERE NOT EXISTS (
           SELECT 1 FROM post_assets relation WHERE relation.asset_id = a.id
         )",
    )
    .fetch_one(&state.pool)
    .await?;
    let rows = sqlx::query(
        "SELECT id, local_relative_path,
         CASE WHEN EXISTS (
           SELECT 1 FROM post_assets relation WHERE relation.asset_id = assets.id
         ) THEN 1 ELSE 0 END AS has_post_reference
         FROM assets WHERE download_status = 'ready'",
    )
    .fetch_all(&state.pool)
    .await?;
    let ready_assets = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    let mut referenced_files = HashSet::new();
    let mut missing_asset_ids = Vec::new();
    let mut missing_file_examples = Vec::new();
    for row in rows {
        let id: String = row.get("id");
        let relative: Option<String> = row.get("local_relative_path");
        let has_post_reference = row.get::<i32, _>("has_post_reference") == 1;
        let valid = relative
            .as_deref()
            .and_then(|value| validate_relative_path(Path::new(value)).ok());
        let exists = valid.as_ref().is_some_and(|value| {
            is_regular_file_without_symlink(&state.config.assets.storage_path.join(value))
        });
        if let Some(valid) = valid {
            if has_post_reference {
                referenced_files.insert(valid.clone());
            }
            if !exists && missing_file_examples.len() < MAINTENANCE_EXAMPLE_LIMIT {
                missing_file_examples.push(valid);
            }
        } else if missing_file_examples.len() < MAINTENANCE_EXAMPLE_LIMIT {
            missing_file_examples.push(format!("asset:{id}:invalid_path"));
        }
        if !exists {
            missing_asset_ids.push(id);
        }
    }

    let root = state.config.assets.storage_path.clone();
    let filesystem = tokio::task::spawn_blocking(move || {
        scan_asset_filesystem(&root, &referenced_files, retention_seconds)
    })
    .await
    .context("asset maintenance scan task failed")??;
    Ok(MaintenanceScan {
        report: MaintenanceReport {
            scanned_at: OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            retention_seconds,
            database_assets,
            ready_assets,
            missing_file_count: missing_asset_ids.len(),
            orphan_metadata_count,
            orphan_file_count: filesystem.orphan_files.len(),
            stale_orphan_file_count: filesystem.stale_orphan_files.len(),
            stale_temp_file_count: filesystem.stale_temp_files.len(),
            missing_file_examples,
            orphan_file_examples: limited_examples(&filesystem.orphan_files),
            stale_temp_file_examples: limited_examples(&filesystem.stale_temp_files),
        },
        missing_asset_ids,
        stale_orphan_files: filesystem.stale_orphan_files,
        stale_temp_files: filesystem.stale_temp_files,
    })
}

async fn load_referenced_ready_paths(state: &AppState) -> anyhow::Result<HashSet<String>> {
    let rows = sqlx::query(
        "SELECT a.local_relative_path FROM assets a
         WHERE a.download_status = 'ready' AND a.local_relative_path IS NOT NULL
           AND EXISTS (
             SELECT 1 FROM post_assets relation WHERE relation.asset_id = a.id
           )",
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let value: String = row.get("local_relative_path");
            validate_relative_path(Path::new(&value)).ok()
        })
        .collect())
}

fn scan_asset_filesystem(
    root: &Path,
    referenced_files: &HashSet<String>,
    retention_seconds: u64,
) -> std::io::Result<FilesystemScan> {
    if !root.exists() {
        return Ok(FilesystemScan::default());
    }
    let mut result = FilesystemScan::default();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                directories.push(path);
                continue;
            }
            if !metadata.is_file() {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let Ok(relative) = validate_relative_path(relative) else {
                continue;
            };
            let stale = is_older_than(&metadata, retention_seconds);
            if relative == ".tmp" || relative.starts_with(".tmp/") {
                if stale {
                    result.stale_temp_files.push(relative);
                }
            } else if !referenced_files.contains(&relative) {
                result.orphan_files.push(relative.clone());
                if stale {
                    result.stale_orphan_files.push(relative);
                }
            }
        }
    }
    result.orphan_files.sort();
    result.stale_orphan_files.sort();
    result.stale_temp_files.sort();
    Ok(result)
}

fn is_older_than(metadata: &std::fs::Metadata, retention_seconds: u64) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age.as_secs() >= retention_seconds)
}

fn is_regular_file_without_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

fn remove_scanned_files(root: &Path, files: &[String]) -> usize {
    files
        .iter()
        .filter(|relative| {
            let Ok(relative) = validate_relative_path(Path::new(relative)) else {
                return false;
            };
            let path = root.join(relative);
            if !is_regular_file_without_symlink(&path) {
                return false;
            }
            std::fs::remove_file(path).is_ok()
        })
        .count()
}

fn remove_empty_asset_directories(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = std::fs::remove_dir(path);
        }
    }
}

fn limited_examples(values: &[String]) -> Vec<String> {
    values
        .iter()
        .take(MAINTENANCE_EXAMPLE_LIMIT)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, net::SocketAddr, path::Path, sync::Arc};

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;
    use sqlx::any::AnyPoolOptions;
    use tokio::sync::RwLock;

    use super::{
        AssetStore, cleanup_maintenance, hex_hash, maintenance_report, record_post_assets,
        validate_relative_path, validate_remote_url,
    };
    use crate::{
        app::AppState,
        config::{
            AppConfig, AssetsConfig, DatabaseBackend, ObservabilityConfig, PersistenceConfig,
            SchedulerConfig,
        },
        crypto::CredentialCipher,
        domain::thread::{ParsedPost, PostAssetReference, PostKind},
        nga::NgaClient,
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

    #[tokio::test]
    async fn different_source_urls_may_share_the_same_content_hash() {
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

        for (id, source_url) in [
            ("a", "https://img.nga.cn/a.jpg"),
            ("b", "https://img4.nga.178.com/a.jpg"),
        ] {
            sqlx::query(
                "INSERT INTO assets (id, source_url, content_hash, download_status)
                 VALUES ($1, $2, $3, 'ready')",
            )
            .bind(id)
            .bind(source_url)
            .bind("same-content")
            .execute(&pool)
            .await
            .unwrap();
        }

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assets WHERE content_hash = $1")
            .bind("same-content")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn maintenance_repairs_missing_assets_and_removes_only_scanned_orphans() {
        let root = std::env::temp_dir().join(format!("nga-maintenance-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(root.join("aa")).await.unwrap();
        tokio::fs::create_dir_all(root.join("cc")).await.unwrap();
        tokio::fs::create_dir_all(root.join("dd")).await.unwrap();
        tokio::fs::create_dir_all(root.join(".tmp")).await.unwrap();
        tokio::fs::write(root.join("aa/referenced.jpg"), b"referenced")
            .await
            .unwrap();
        tokio::fs::write(root.join("cc/orphan.jpg"), b"orphan")
            .await
            .unwrap();
        tokio::fs::write(root.join("dd/metadata-orphan.jpg"), b"metadata orphan")
            .await
            .unwrap();
        tokio::fs::write(root.join(".tmp/stale.part"), b"temporary")
            .await
            .unwrap();

        let state = maintenance_state(root.clone()).await;
        sqlx::query("INSERT INTO threads (tid, fid, title, forum_name, author_uid, author_name) VALUES (1, 2, 't', 'f', 3, 'u')").execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO posts (id, tid, floor_number, post_kind, author_uid, author_name, content_raw, page_number, raw_payload) VALUES ('p', 1, 0, 'topic', 3, 'u', 'body', 1, '')").execute(&state.pool).await.unwrap();
        for (id, source_url, path, status) in [
            (
                "referenced",
                "https://img.nga.cn/referenced.jpg",
                Some("aa/referenced.jpg"),
                "ready",
            ),
            (
                "missing",
                "https://img.nga.cn/missing.jpg",
                Some("bb/missing.jpg"),
                "ready",
            ),
            (
                "metadata-orphan",
                "https://img.nga.cn/orphan.jpg",
                Some("dd/metadata-orphan.jpg"),
                "ready",
            ),
        ] {
            sqlx::query(
                "INSERT INTO assets (id, source_url, local_relative_path, download_status)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(id)
            .bind(source_url)
            .bind(path)
            .bind(status)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        for (asset, order) in [("referenced", 0), ("missing", 1)] {
            sqlx::query(
                "INSERT INTO post_assets (post_id, asset_id, appearance_order)
                 VALUES ('p', $1, $2)",
            )
            .bind(asset)
            .bind(order)
            .execute(&state.pool)
            .await
            .unwrap();
        }

        let report = maintenance_report(&state, 0).await.unwrap();
        assert_eq!(report.missing_file_count, 1);
        assert_eq!(report.orphan_metadata_count, 1);
        assert_eq!(report.orphan_file_count, 2);
        assert_eq!(report.stale_temp_file_count, 1);

        let result = cleanup_maintenance(&state, 0).await.unwrap();
        assert_eq!(result.repaired_missing_assets, 1);
        assert_eq!(result.deleted_orphan_metadata, 1);
        assert_eq!(result.deleted_orphan_files, 2);
        assert_eq!(result.deleted_temp_files, 1);
        let missing_status: String =
            sqlx::query_scalar("SELECT download_status FROM assets WHERE id = 'missing'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(missing_status, "pending");
        assert!(root.join("aa/referenced.jpg").is_file());
        assert!(!root.join("cc/orphan.jpg").exists());
        assert!(!root.join("dd/metadata-orphan.jpg").exists());
        assert!(!root.join(".tmp/stale.part").exists());

        state.pool.close().await;
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn maintenance_state(root: std::path::PathBuf) -> AppState {
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
        let key = STANDARD.encode([7_u8; 32]);
        let config = Arc::new(AppConfig {
            bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
            database_backend: DatabaseBackend::Sqlite,
            database_url: SecretString::from("postgres://unused"),
            sqlite_path: ":memory:".into(),
            database_max_connections: 1,
            api_token: SecretString::from("test-token"),
            admin_password: SecretString::from("test-password"),
            credential_encryption_key: SecretString::from(key.clone()),
            nga_user_agent: "test".to_owned(),
            run_migrations: false,
            persistence: PersistenceConfig {
                store_raw_payload: false,
            },
            assets: AssetsConfig {
                download_enabled: true,
                storage_path: root,
                max_download_bytes: 1024,
            },
            scheduler: SchedulerConfig {
                default_interval_seconds: 60,
                timezone_offset: time::UtcOffset::UTC,
            },
            observability: ObservabilityConfig {
                log_filter: "info".to_owned(),
                log_json: false,
            },
        });
        AppState {
            pool,
            config,
            credential_cipher: Arc::new(CredentialCipher::from_base64(&key).unwrap()),
            nga_client: NgaClient::new("test".to_owned()).unwrap(),
            admin_sessions: Arc::new(RwLock::new(HashSet::new())),
            platform_updates: tokio::sync::watch::channel(()).0,
        }
    }
}
