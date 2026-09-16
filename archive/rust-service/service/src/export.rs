use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::Context as _;
use axum::body::{Body, Bytes};
use futures_util::{Stream, StreamExt, stream};
use serde::Serialize;
use sqlx::{AnyPool, Row};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{config::AssetsConfig, markup};

const EXPORT_BATCH_SIZE: i64 = 128;

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

pub struct ExportArtifact {
    pub filename: String,
    pub content_type: &'static str,
    pub content_length: Option<u64>,
    pub body: Body,
}

#[derive(Debug, Serialize)]
struct ExportMetadata {
    target_type: &'static str,
    target_id: i64,
    title: String,
    generated_at_unix: i64,
    post_count: i64,
    asset_count: usize,
}

#[derive(Clone, Copy, Debug)]
enum ExportTarget {
    Thread(i64),
    User(i64),
}

impl ExportTarget {
    fn target_type(self) -> &'static str {
        match self {
            Self::Thread(_) => "thread",
            Self::User(_) => "user",
        }
    }

    fn target_id(self) -> i64 {
        match self {
            Self::Thread(value) | Self::User(value) => value,
        }
    }

    fn predicate(self) -> &'static str {
        match self {
            Self::Thread(_) => "p.tid = $1",
            Self::User(_) => "p.author_uid = $1",
        }
    }
}

#[derive(Clone, Debug)]
struct DocumentSpec {
    target: ExportTarget,
    title: String,
    header: String,
}

#[derive(Clone, Debug, Default)]
struct PostCursor {
    tid: i64,
    floor_sort: i32,
    page_number: i32,
    pid_sort: i64,
    id: String,
}

#[derive(Debug)]
struct PostRow {
    id: String,
    tid: i64,
    pid: Option<i64>,
    floor_number: Option<i32>,
    floor_sort: i32,
    post_kind: String,
    author_uid: i64,
    author_name: String,
    subject: String,
    content_raw: String,
    page_number: i32,
    pid_sort: i64,
}

impl PostRow {
    fn cursor(&self) -> PostCursor {
        PostCursor {
            tid: self.tid,
            floor_sort: self.floor_sort,
            page_number: self.page_number,
            pid_sort: self.pid_sort,
            id: self.id.clone(),
        }
    }
}

#[derive(Debug)]
struct MarkdownStreamState {
    pool: AnyPool,
    target: ExportTarget,
    cursor: Option<PostCursor>,
    current_tid: Option<i64>,
}

#[derive(Debug)]
struct ExportAsset {
    archive_path: String,
    local_path: PathBuf,
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
    let spec = DocumentSpec {
        target: ExportTarget::Thread(tid),
        title: title.clone(),
        header: format!(
            "_Forum: {} · Author: {} (UID {}) · Coverage: {}_",
            row.get::<String, _>("forum_name"),
            row.get::<String, _>("author_name"),
            row.get::<i64, _>("author_uid"),
            row.get::<String, _>("coverage")
        ),
    };
    build_artifact(pool, spec, format, assets_config)
        .await
        .map(Some)
}

pub async fn user(
    pool: &AnyPool,
    uid: i64,
    format: ExportFormat,
    assets_config: &AssetsConfig,
) -> anyhow::Result<Option<ExportArtifact>> {
    let exists: Option<i32> =
        sqlx::query_scalar("SELECT 1 FROM posts WHERE author_uid = $1 LIMIT 1")
            .bind(uid)
            .fetch_optional(pool)
            .await?;
    if exists.is_none() {
        return Ok(None);
    }
    let spec = DocumentSpec {
        target: ExportTarget::User(uid),
        title: format!("NGA user {uid}"),
        header: format!("_Author UID: {uid}_"),
    };
    build_artifact(pool, spec, format, assets_config)
        .await
        .map(Some)
}

async fn build_artifact(
    pool: &AnyPool,
    spec: DocumentSpec,
    format: ExportFormat,
    assets_config: &AssetsConfig,
) -> anyhow::Result<ExportArtifact> {
    match format {
        ExportFormat::Markdown => Ok(markdown_artifact(pool, spec)),
        ExportFormat::Zip => zip_artifact(pool, spec, assets_config).await,
    }
}

fn markdown_artifact(pool: &AnyPool, spec: DocumentSpec) -> ExportArtifact {
    let filename = format!(
        "{}-{}.md",
        spec.target.target_type(),
        spec.target.target_id()
    );
    let header = Bytes::from(document_header(&spec));
    let state = MarkdownStreamState {
        pool: pool.clone(),
        target: spec.target,
        cursor: None,
        current_tid: None,
    };
    let chunks = stream::try_unfold(state, |mut state| async move {
        let rows = load_post_batch(&state.pool, state.target, state.cursor.as_ref()).await?;
        if rows.is_empty() {
            return Ok::<_, anyhow::Error>(None);
        }
        let chunk = render_post_rows(&rows, state.target, &mut state.current_tid, &HashMap::new());
        state.cursor = rows.last().map(PostRow::cursor);
        Ok(Some((Bytes::from(chunk), state)))
    });
    let body_stream = stream::once(async move { Ok::<Bytes, anyhow::Error>(header) }).chain(chunks);
    ExportArtifact {
        filename,
        content_type: "text/markdown; charset=utf-8",
        content_length: None,
        body: Body::from_stream(body_stream),
    }
}

async fn zip_artifact(
    pool: &AnyPool,
    spec: DocumentSpec,
    assets_config: &AssetsConfig,
) -> anyhow::Result<ExportArtifact> {
    let temp_dir = assets_config.storage_path.join(".tmp");
    tokio::fs::create_dir_all(&temp_dir)
        .await
        .with_context(|| {
            format!(
                "failed to create export temp directory {}",
                temp_dir.display()
            )
        })?;
    let token = Uuid::new_v4();
    let markdown_path = temp_dir.join(format!("export-{token}.md.part"));
    let zip_path = temp_dir.join(format!("export-{token}.zip.part"));
    let result = build_zip_files(pool, &spec, assets_config, &markdown_path, &zip_path).await;
    let _ = tokio::fs::remove_file(&markdown_path).await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&zip_path).await;
        return Err(error);
    }

    let file = match tokio::fs::File::open(&zip_path).await {
        Ok(file) => file,
        Err(error) => {
            let _ = tokio::fs::remove_file(&zip_path).await;
            return Err(error).context("failed to open generated ZIP");
        }
    };
    let content_length = file.metadata().await.ok().map(|value| value.len());
    let body = Body::from_stream(DeleteOnDropStream::new(file, zip_path));
    Ok(ExportArtifact {
        filename: format!(
            "{}-{}.zip",
            spec.target.target_type(),
            spec.target.target_id()
        ),
        content_type: "application/zip",
        content_length,
        body,
    })
}

async fn build_zip_files(
    pool: &AnyPool,
    spec: &DocumentSpec,
    assets_config: &AssetsConfig,
    markdown_path: &Path,
    zip_path: &Path,
) -> anyhow::Result<()> {
    let (asset_links, export_assets) = load_export_assets(pool, spec.target, assets_config).await?;
    write_markdown_file(pool, spec, &asset_links, markdown_path).await?;
    let post_count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM posts p WHERE {}",
        spec.target.predicate()
    ))
    .bind(spec.target.target_id())
    .fetch_one(pool)
    .await?;
    let metadata = ExportMetadata {
        target_type: spec.target.target_type(),
        target_id: spec.target.target_id(),
        title: spec.title.clone(),
        generated_at_unix: OffsetDateTime::now_utc().unix_timestamp(),
        post_count,
        asset_count: export_assets.len(),
    };
    let markdown_name = format!(
        "{}-{}.md",
        spec.target.target_type(),
        spec.target.target_id()
    );
    let markdown_path = markdown_path.to_path_buf();
    let zip_path = zip_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        write_zip_file(
            &zip_path,
            &markdown_path,
            &markdown_name,
            &metadata,
            &export_assets,
        )
    })
    .await
    .context("ZIP builder task failed")??;
    Ok(())
}

async fn write_markdown_file(
    pool: &AnyPool,
    spec: &DocumentSpec,
    asset_links: &HashMap<String, String>,
    path: &Path,
) -> anyhow::Result<()> {
    let mut file = tokio::fs::File::create(path).await?;
    file.write_all(document_header(spec).as_bytes()).await?;
    let mut cursor = None;
    let mut current_tid = None;
    loop {
        let rows = load_post_batch(pool, spec.target, cursor.as_ref()).await?;
        if rows.is_empty() {
            break;
        }
        let chunk = render_post_rows(&rows, spec.target, &mut current_tid, asset_links);
        file.write_all(chunk.as_bytes()).await?;
        cursor = rows.last().map(PostRow::cursor);
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

fn document_header(spec: &DocumentSpec) -> String {
    format!("# {}\n\n{}\n\n", spec.title, spec.header)
}

async fn load_post_batch(
    pool: &AnyPool,
    target: ExportTarget,
    cursor: Option<&PostCursor>,
) -> anyhow::Result<Vec<PostRow>> {
    let query = format!(
        "SELECT p.id, p.tid, p.pid, p.floor_number,
                COALESCE(p.floor_number, 0) AS floor_sort, p.post_kind,
                p.author_uid, p.author_name, p.subject, p.content_raw, p.page_number,
                COALESCE(p.pid, 0) AS pid_sort
         FROM posts p
         WHERE {} AND (
             $2 = 0 OR p.tid > $3
             OR (p.tid = $3 AND COALESCE(p.floor_number, 0) > $4)
             OR (p.tid = $3 AND COALESCE(p.floor_number, 0) = $4 AND p.page_number > $5)
             OR (p.tid = $3 AND COALESCE(p.floor_number, 0) = $4 AND p.page_number = $5
                 AND COALESCE(p.pid, 0) > $6)
             OR (p.tid = $3 AND COALESCE(p.floor_number, 0) = $4 AND p.page_number = $5
                 AND COALESCE(p.pid, 0) = $6 AND p.id > $7)
         )
         ORDER BY p.tid, COALESCE(p.floor_number, 0), p.page_number,
                  COALESCE(p.pid, 0), p.id
         LIMIT $8",
        target.predicate()
    );
    let empty = PostCursor::default();
    let value = cursor.unwrap_or(&empty);
    let rows = sqlx::query(&query)
        .bind(target.target_id())
        .bind(i32::from(cursor.is_some()))
        .bind(value.tid)
        .bind(value.floor_sort)
        .bind(value.page_number)
        .bind(value.pid_sort)
        .bind(&value.id)
        .bind(EXPORT_BATCH_SIZE)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| PostRow {
            id: row.get("id"),
            tid: row.get("tid"),
            pid: row.get("pid"),
            floor_number: row.get("floor_number"),
            floor_sort: row.get("floor_sort"),
            post_kind: row.get("post_kind"),
            author_uid: row.get("author_uid"),
            author_name: row.get("author_name"),
            subject: row.get("subject"),
            content_raw: row.get("content_raw"),
            page_number: row.get("page_number"),
            pid_sort: row.get("pid_sort"),
        })
        .collect())
}

fn render_post_rows(
    rows: &[PostRow],
    target: ExportTarget,
    current_tid: &mut Option<i64>,
    asset_links: &HashMap<String, String>,
) -> String {
    let mut markdown = String::new();
    for post in rows {
        if matches!(target, ExportTarget::User(_)) && *current_tid != Some(post.tid) {
            *current_tid = Some(post.tid);
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
        let rendered = markup::render_markdown(&post.content_raw, asset_links);
        markdown.push_str(rendered.trim_end());
        markdown.push_str(&format!(
            "\n\n_Page {} · TID {}_\n\n",
            post.page_number, post.tid
        ));
    }
    markdown
}

async fn load_export_assets(
    pool: &AnyPool,
    target: ExportTarget,
    assets_config: &AssetsConfig,
) -> anyhow::Result<(HashMap<String, String>, Vec<ExportAsset>)> {
    let query = format!(
        "SELECT DISTINCT a.source_url, a.local_relative_path
         FROM assets a
         JOIN post_assets pa ON pa.asset_id = a.id
         JOIN posts p ON p.id = pa.post_id
         WHERE {} AND a.download_status = 'ready'
           AND a.local_relative_path IS NOT NULL",
        target.predicate()
    );
    let rows = sqlx::query(&query)
        .bind(target.target_id())
        .fetch_all(pool)
        .await?;
    let mut links = HashMap::new();
    let mut assets = Vec::new();
    let mut seen_paths = HashSet::new();
    for row in rows {
        let source_url: String = row.get("source_url");
        let local_path: String = row.get("local_relative_path");
        let relative = safe_local_path(&assets_config.storage_path, &local_path)?;
        let archive_path = format!("assets/{relative}");
        let full_path = assets_config.storage_path.join(&relative);
        if full_path.is_file() {
            links.insert(source_url, archive_path.clone());
            if seen_paths.insert(relative) {
                assets.push(ExportAsset {
                    archive_path,
                    local_path: full_path,
                });
            }
        }
    }
    Ok((links, assets))
}

fn write_zip_file(
    output_path: &Path,
    markdown_path: &Path,
    markdown_name: &str,
    metadata: &ExportMetadata,
    assets: &[ExportAsset],
) -> anyhow::Result<()> {
    let output = std::fs::File::create(output_path)?;
    let mut zip = ZipWriter::new(output);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(markdown_name, options)?;
    let mut markdown = std::fs::File::open(markdown_path)?;
    std::io::copy(&mut markdown, &mut zip)?;
    zip.start_file("metadata.json", options)?;
    zip.write_all(&serde_json::to_vec_pretty(metadata)?)?;
    for asset in assets {
        zip.start_file(&asset.archive_path, options)?;
        let mut source = std::fs::File::open(&asset.local_path)?;
        std::io::copy(&mut source, &mut zip)?;
    }
    zip.finish()?;
    Ok(())
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

struct DeleteOnDropStream {
    inner: Pin<Box<ReaderStream<tokio::fs::File>>>,
    path: PathBuf,
}

impl DeleteOnDropStream {
    fn new(file: tokio::fs::File, path: PathBuf) -> Self {
        Self {
            inner: Box::pin(ReaderStream::new(file)),
            path,
        }
    }
}

impl Stream for DeleteOnDropStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

impl Drop for DeleteOnDropStream {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use http_body_util::BodyExt as _;
    use sqlx::any::AnyPoolOptions;
    use uuid::Uuid;
    use zip::ZipArchive;

    use super::{EXPORT_BATCH_SIZE, ExportFormat, safe_local_path, thread, user};
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
        let (pool, root, config) = export_fixture().await;

        let markdown = thread(&pool, 1001, ExportFormat::Markdown, &config)
            .await
            .unwrap()
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let markdown = String::from_utf8(markdown.to_vec()).unwrap();
        assert!(markdown.contains("**正文**"));
        assert!(markdown.contains("https://img.nga.cn/a.jpg"));

        let zip = thread(&pool, 1001, ExportFormat::Zip, &config)
            .await
            .unwrap()
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let mut archive = ZipArchive::new(Cursor::new(zip)).unwrap();
        assert!(archive.by_name("thread-1001.md").is_ok());
        assert!(archive.by_name("metadata.json").is_ok());
        assert!(archive.by_name("assets/aa/hash.jpg").is_ok());
        drop(archive);
        let temp_files = std::fs::read_dir(root.join(".tmp"))
            .unwrap()
            .filter_map(Result::ok)
            .count();
        assert_eq!(temp_files, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn user_markdown_streams_across_multiple_database_batches() {
        let (pool, root, config) = export_fixture().await;
        for index in 1..=EXPORT_BATCH_SIZE + 5 {
            sqlx::query(
                "INSERT INTO posts
                 (id, tid, pid, floor_number, post_kind, author_uid, author_name,
                  content_raw, page_number, raw_payload)
                 VALUES ($1, 1001, $2, $3, 'reply', 2001, 'Author', $4, 1, '')",
            )
            .bind(format!("post-extra-{index:04}"))
            .bind(10_000 + index)
            .bind(i32::try_from(index).unwrap())
            .bind(format!("batch marker {index}"))
            .execute(&pool)
            .await
            .unwrap();
        }
        let markdown = user(&pool, 2001, ExportFormat::Markdown, &config)
            .await
            .unwrap()
            .unwrap()
            .body
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let markdown = String::from_utf8(markdown.to_vec()).unwrap();
        assert!(markdown.contains("batch marker 1"));
        assert!(markdown.contains(&format!("batch marker {}", EXPORT_BATCH_SIZE + 5)));
        assert_eq!(markdown.matches("## TID 1001").count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn export_fixture() -> (sqlx::AnyPool, std::path::PathBuf, AssetsConfig) {
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
        let root = std::env::temp_dir().join(format!("nga-export-{}", Uuid::new_v4()));
        let local = root.join("aa/hash.jpg");
        tokio::fs::create_dir_all(local.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&local, b"asset").await.unwrap();
        sqlx::query(
            "INSERT INTO assets
             (id, source_url, content_hash, local_relative_path, download_status)
             VALUES ('asset-1', 'https://img.nga.cn/a.jpg',
                     'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
                     'aa/hash.jpg', 'ready')",
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
        (pool, root, config)
    }
}
