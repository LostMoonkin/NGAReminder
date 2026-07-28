use std::{
    collections::HashMap,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

use anyhow::Context;
use serde::Serialize;
use sqlx::{AnyPool, Row};
use time::OffsetDateTime;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{config::AssetsConfig, markup};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Markdown,
    Zip,
}

impl ExportFormat {
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value.unwrap_or("markdown") {
            "markdown" | "md" => Some(Self::Markdown),
            "zip" => Some(Self::Zip),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct ExportArtifact {
    pub filename: String,
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Serialize)]
struct ExportMetadata {
    target_type: &'static str,
    target_id: i64,
    title: String,
    generated_at_unix: i64,
    post_count: usize,
    asset_count: usize,
}

#[derive(Debug)]
struct ExportAsset {
    archive_path: String,
    local_path: PathBuf,
}

#[derive(Debug)]
struct PostRow {
    id: String,
    tid: i64,
    pid: Option<i64>,
    floor_number: Option<i32>,
    post_kind: String,
    author_uid: i64,
    author_name: String,
    subject: String,
    content_raw: String,
    page_number: i32,
}

pub async fn thread(
    pool: &AnyPool,
    tid: i64,
    format: ExportFormat,
    assets_config: &AssetsConfig,
) -> anyhow::Result<Option<ExportArtifact>> {
    let Some(row) = sqlx::query(
        "SELECT tid, title, forum_name, author_uid, author_name, coverage
         FROM threads WHERE tid = $1",
    )
    .bind(tid)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let title: String = row.get("title");
    let forum_name: String = row.get("forum_name");
    let author_uid: i64 = row.get("author_uid");
    let author_name: String = row.get("author_name");
    let coverage: String = row.get("coverage");
    let posts = load_posts(pool, "p.tid = $1", &[tid]).await?;
    build_artifact(
        pool,
        DocumentInput {
            target_type: "thread",
            target_id: tid,
            title: title.clone(),
            header: format!(
                "_Forum: {forum_name} · Author: {author_name} (UID {author_uid}) · Coverage: {coverage}_"
            ),
            posts,
        },
        format,
        assets_config,
    )
    .await
    .map(Some)
}

pub async fn user(
    pool: &AnyPool,
    uid: i64,
    format: ExportFormat,
    assets_config: &AssetsConfig,
) -> anyhow::Result<Option<ExportArtifact>> {
    let posts = load_posts(pool, "p.author_uid = $1", &[uid]).await?;
    if posts.is_empty() {
        return Ok(None);
    }
    build_artifact(
        pool,
        DocumentInput {
            target_type: "user",
            target_id: uid,
            title: format!("NGA user {uid}"),
            header: format!("_Author UID: {uid}_"),
            posts,
        },
        format,
        assets_config,
    )
    .await
    .map(Some)
}

struct DocumentInput {
    target_type: &'static str,
    target_id: i64,
    title: String,
    header: String,
    posts: Vec<PostRow>,
}

async fn load_posts(
    pool: &AnyPool,
    predicate: &str,
    bind_id: &[i64],
) -> anyhow::Result<Vec<PostRow>> {
    let query = format!(
        "SELECT p.id, p.tid, p.pid, p.floor_number, p.post_kind,
                p.author_uid, p.author_name, p.subject, p.content_raw, p.page_number
         FROM posts p WHERE {predicate}
         ORDER BY p.tid, p.floor_number, p.page_number, COALESCE(p.pid, 0), p.id"
    );
    let mut request = sqlx::query(&query);
    for value in bind_id {
        request = request.bind(value);
    }
    let rows = request.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| PostRow {
            id: row.get("id"),
            tid: row.get("tid"),
            pid: row.get("pid"),
            floor_number: row.get("floor_number"),
            post_kind: row.get("post_kind"),
            author_uid: row.get("author_uid"),
            author_name: row.get("author_name"),
            subject: row.get("subject"),
            content_raw: row.get("content_raw"),
            page_number: row.get("page_number"),
        })
        .collect())
}

async fn build_artifact(
    pool: &AnyPool,
    input: DocumentInput,
    format: ExportFormat,
    assets_config: &AssetsConfig,
) -> anyhow::Result<ExportArtifact> {
    let post_ids: Vec<String> = input.posts.iter().map(|post| post.id.clone()).collect();
    let asset_rows = load_assets_for_posts(pool, &post_ids).await?;
    let mut asset_links = HashMap::new();
    let mut export_assets = Vec::new();
    for (source_url, local_path) in asset_rows {
        if let Some(local_path) = local_path {
            let relative = safe_local_path(&assets_config.storage_path, &local_path)?;
            let archive_path = format!("assets/{relative}");
            let full_path = assets_config.storage_path.join(&relative);
            if format == ExportFormat::Zip && full_path.is_file() {
                asset_links.insert(source_url, archive_path.clone());
                export_assets.push(ExportAsset {
                    archive_path,
                    local_path: full_path,
                });
            }
        }
    }

    let mut markdown = format!("# {}\n\n{}\n\n", input.title, input.header);
    let mut current_tid = None;
    for post in &input.posts {
        if input.target_type == "user" && current_tid != Some(post.tid) {
            current_tid = Some(post.tid);
            markdown.push_str(&format!("## TID {}\n\n", post.tid));
        }
        let level = if post.post_kind == "comment" {
            "###"
        } else {
            "##"
        };
        let floor = post.floor_number.unwrap_or_default();
        let pid = post
            .pid
            .map_or(String::new(), |pid| format!(" · PID {pid}"));
        let subject = if post.subject.trim().is_empty() {
            String::new()
        } else {
            format!(" · {}", post.subject.trim())
        };
        markdown.push_str(&format!(
            "{level} #{} · {} (UID {}){}{}\n\n",
            floor, post.author_name, post.author_uid, pid, subject
        ));
        let rendered = markup::render_markdown(&post.content_raw, &asset_links);
        markdown.push_str(rendered.trim_end());
        markdown.push_str(&format!(
            "\n\n_Page {} · TID {}_\n\n",
            post.page_number, post.tid
        ));
    }
    if input.posts.is_empty() {
        markdown.push_str("_No posts have been persisted for this target yet._\n");
    }

    let metadata = ExportMetadata {
        target_type: input.target_type,
        target_id: input.target_id,
        title: input.title,
        generated_at_unix: OffsetDateTime::now_utc().unix_timestamp(),
        post_count: input.posts.len(),
        asset_count: export_assets.len(),
    };
    let filename_base = format!("{}-{}", input.target_type, input.target_id);
    match format {
        ExportFormat::Markdown => Ok(ExportArtifact {
            filename: format!("{filename_base}.md"),
            content_type: "text/markdown; charset=utf-8",
            bytes: markdown.into_bytes(),
        }),
        ExportFormat::Zip => zip_artifact(filename_base, markdown, metadata, export_assets).await,
    }
}

async fn load_assets_for_posts(
    pool: &AnyPool,
    post_ids: &[String],
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    if post_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=post_ids.len())
        .map(|value| format!("${value}"))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        "SELECT DISTINCT a.source_url, a.local_relative_path, a.download_status
         FROM assets a JOIN post_assets pa ON pa.asset_id = a.id
         WHERE pa.post_id IN ({placeholders})"
    );
    let mut request = sqlx::query(&query);
    for post_id in post_ids {
        request = request.bind(post_id);
    }
    let rows = request.fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let source_url: String = row.get("source_url");
            let status: String = row.get("download_status");
            let local_path: Option<String> = row.get("local_relative_path");
            if status == "ready" {
                (source_url, local_path)
            } else {
                (source_url, None)
            }
        })
        .collect())
}

async fn zip_artifact(
    filename_base: String,
    markdown: String,
    metadata: ExportMetadata,
    assets: Vec<ExportAsset>,
) -> anyhow::Result<ExportArtifact> {
    let mut cursor = Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut cursor);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(format!("{filename_base}.md"), options)?;
    zip.write_all(markdown.as_bytes())?;
    zip.start_file("metadata.json", options)?;
    zip.write_all(&serde_json::to_vec_pretty(&metadata)?)?;
    for asset in assets {
        let bytes = tokio::fs::read(&asset.local_path)
            .await
            .with_context(|| format!("failed to read asset {}", asset.local_path.display()))?;
        zip.start_file(asset.archive_path, options)?;
        zip.write_all(&bytes)?;
    }
    zip.finish()?;
    Ok(ExportArtifact {
        filename: format!("{filename_base}.zip"),
        content_type: "application/zip",
        bytes: cursor.into_inner(),
    })
}

fn safe_local_path(storage_path: &Path, relative: &str) -> anyhow::Result<String> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!("unsafe asset path")
    }
    let full = storage_path.join(path);
    let canonical_root = storage_path
        .canonicalize()
        .unwrap_or_else(|_| storage_path.to_path_buf());
    let canonical_parent = full
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .unwrap_or_else(|| storage_path.to_path_buf());
    if !canonical_parent.starts_with(&canonical_root) {
        anyhow::bail!("asset path escapes storage root")
    }
    if full.exists() && !full.canonicalize()?.starts_with(&canonical_root) {
        anyhow::bail!("asset path escapes storage root")
    }
    Ok(path.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use sqlx::any::AnyPoolOptions;
    use zip::ZipArchive;

    use super::{ExportFormat, safe_local_path, thread};
    use crate::config::AssetsConfig;

    #[test]
    fn rejects_export_path_traversal() {
        let root = std::env::temp_dir().join("nga-export-root");
        assert!(safe_local_path(&root, "../secret.txt").is_err());
        assert_eq!(
            safe_local_path(&root, "aa/hash.jpg").unwrap(),
            "aa/hash.jpg"
        );
    }

    #[tokio::test]
    async fn thread_markdown_and_zip_exports_include_rendered_posts_and_assets() {
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
        sqlx::query(
            "INSERT INTO threads (tid, fid, title, forum_name, author_uid, author_name)
             VALUES (1001, 3001, 'Export title', 'Forum', 2001, 'Author')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO posts
             (id, tid, floor_number, post_kind, author_uid, author_name, content_raw, page_number, raw_payload)
             VALUES ('post-1', 1001, 0, 'topic', 2001, 'Author',
                     '[b]正文[/b][img]https://img.nga.cn/a.jpg[/img]', 1, '')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let root = std::env::temp_dir().join(format!("nga-export-{}", uuid::Uuid::new_v4()));
        let local = root.join("aa/hash.jpg");
        tokio::fs::create_dir_all(local.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&local, b"asset").await.unwrap();
        sqlx::query(
            "INSERT INTO assets
             (id, source_url, content_hash, local_relative_path, download_status)
             VALUES ('asset-1', 'https://img.nga.cn/a.jpg', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 'aa/hash.jpg', 'ready')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO post_assets (post_id, asset_id, appearance_order)
             VALUES ('post-1', 'asset-1', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let config = AssetsConfig {
            download_enabled: true,
            storage_path: root.clone(),
            max_download_bytes: 1024,
        };

        let markdown = thread(&pool, 1001, ExportFormat::Markdown, &config)
            .await
            .unwrap()
            .unwrap();
        let markdown = String::from_utf8(markdown.bytes).unwrap();
        assert!(markdown.contains("**正文**"));
        assert!(markdown.contains("https://img.nga.cn/a.jpg"));

        let zip = thread(&pool, 1001, ExportFormat::Zip, &config)
            .await
            .unwrap()
            .unwrap();
        let mut archive = ZipArchive::new(Cursor::new(zip.bytes)).unwrap();
        assert!(archive.by_name("thread-1001.md").is_ok());
        assert!(archive.by_name("metadata.json").is_ok());
        assert!(archive.by_name("assets/aa/hash.jpg").is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
