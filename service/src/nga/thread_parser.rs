use serde_json::{Map, Value};
use thiserror::Error;

use crate::domain::thread::{ParsedPost, PostKind, ThreadMetadata, ThreadPage};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ThreadParseError {
    #[error("missing or invalid field: {0}")]
    Field(&'static str),
    #[error("response TID {actual} does not match requested TID {expected}")]
    TidMismatch { expected: i64, actual: i64 },
    #[error("NGA returned invalid pagination metadata")]
    Pagination,
    #[error("NGA returned an empty successful thread page")]
    EmptyPage,
    #[error("failed to preserve raw post JSON")]
    RawJson,
}

pub fn parse_thread_page(
    value: &Value,
    requested_tid: i64,
) -> Result<ThreadPage, ThreadParseError> {
    let object = value
        .as_object()
        .ok_or(ThreadParseError::Field("response"))?;
    let current_page = required_i32(object, "currentPage")?;
    let total_pages = required_i32(object, "totalPage")?;
    let per_page = required_i32(object, "perPage")?;
    let vrows = required_i32(object, "vrows")?;
    if current_page < 1
        || total_pages < 1
        || current_page > total_pages
        || per_page < 1
        || vrows < 1
    {
        return Err(ThreadParseError::Pagination);
    }

    let raw_posts = object
        .get("result")
        .and_then(Value::as_array)
        .ok_or(ThreadParseError::Field("result"))?;
    if raw_posts.is_empty() {
        return Err(ThreadParseError::EmptyPage);
    }

    let first = raw_posts[0]
        .as_object()
        .ok_or(ThreadParseError::Field("result[]"))?;
    let response_tid = required_i64(first, "tid")?;
    if response_tid != requested_tid {
        return Err(ThreadParseError::TidMismatch {
            expected: requested_tid,
            actual: response_tid,
        });
    }

    let metadata = ThreadMetadata {
        tid: requested_tid,
        fid: required_i64(object, "fid")?,
        title: string_or_default(object, "tsubject"),
        forum_name: string_or_default(object, "forum_name"),
        author_uid: required_i64(object, "tauthorid")?,
        author_name: string_or_default(object, "tauthor"),
        total_pages,
        per_page,
        vrows,
    };

    let mut posts = Vec::new();
    for raw_post in raw_posts {
        let post = parse_post(raw_post, requested_tid, current_page, None)?;
        let parent_pid = post.pid;
        let parent_is_topic = post.kind == PostKind::Topic;
        posts.push(post);

        if let Some(comments) = raw_post.get("comments").and_then(Value::as_array) {
            for raw_comment in comments {
                posts.push(parse_post(
                    raw_comment,
                    requested_tid,
                    current_page,
                    Some((parent_pid, parent_is_topic)),
                )?);
            }
        }
    }

    Ok(ThreadPage {
        metadata,
        current_page,
        posts,
    })
}

fn parse_post(
    value: &Value,
    requested_tid: i64,
    page_number: i32,
    parent: Option<(Option<i64>, bool)>,
) -> Result<ParsedPost, ThreadParseError> {
    let object = value
        .as_object()
        .ok_or(ThreadParseError::Field("result[]"))?;
    let tid = required_i64(object, "tid")?;
    if tid != requested_tid {
        return Err(ThreadParseError::TidMismatch {
            expected: requested_tid,
            actual: tid,
        });
    }

    let floor_number = required_i32(object, "lou")?;
    let raw_pid = optional_i64(object, "pid")?;
    let kind = if parent.is_some() {
        PostKind::Comment
    } else if floor_number == 0 {
        PostKind::Topic
    } else {
        PostKind::Reply
    };
    let pid = match kind {
        PostKind::Topic => raw_pid.filter(|pid| *pid != 0),
        PostKind::Reply | PostKind::Comment => Some(
            raw_pid
                .filter(|pid| *pid != 0)
                .ok_or(ThreadParseError::Field("pid"))?,
        ),
    };
    if kind == PostKind::Reply && floor_number < 1 {
        return Err(ThreadParseError::Field("lou"));
    }

    let author = object
        .get("author")
        .and_then(Value::as_object)
        .ok_or(ThreadParseError::Field("author"))?;
    let (parent_pid, parent_is_topic) = parent.unwrap_or((None, false));

    Ok(ParsedPost {
        tid,
        pid,
        floor_number,
        kind,
        parent_pid,
        parent_is_topic,
        author_uid: required_i64(author, "uid")?,
        author_name: string_or_default(author, "username"),
        subject: string_or_default(object, "subject"),
        content_raw: string_or_default(object, "content"),
        published_at_unix: optional_i64(object, "postdatetimestamp")?,
        page_number,
        raw_payload: serde_json::to_string(value).map_err(|_| ThreadParseError::RawJson)?,
    })
}

fn required_i32(object: &Map<String, Value>, field: &'static str) -> Result<i32, ThreadParseError> {
    let value = required_i64(object, field)?;
    i32::try_from(value).map_err(|_| ThreadParseError::Field(field))
}

fn required_i64(object: &Map<String, Value>, field: &'static str) -> Result<i64, ThreadParseError> {
    optional_i64(object, field)?.ok_or(ThreadParseError::Field(field))
}

fn optional_i64(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<i64>, ThreadParseError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() || value == "" {
        return Ok(None);
    }
    if let Some(value) = value.as_i64() {
        return Ok(Some(value));
    }
    if let Some(value) = value.as_str().and_then(|value| value.parse::<i64>().ok()) {
        return Ok(Some(value));
    }
    Err(ThreadParseError::Field(field))
}

fn string_or_default(object: &Map<String, Value>, field: &str) -> String {
    object
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::parse_thread_page;
    use crate::domain::thread::PostKind;

    #[test]
    fn parses_topic_and_reply() {
        let page = parse_thread_page(&fixture("thread_page_success.json"), 1001)
            .expect("fixture must parse");

        assert_eq!(page.metadata.vrows, 2);
        assert_eq!(page.posts.len(), 2);
        assert_eq!(page.posts[0].kind, PostKind::Topic);
        assert_eq!(page.posts[0].pid, None);
        assert_eq!(page.posts[1].kind, PostKind::Reply);
        assert_eq!(page.posts[1].floor_number, 1);
    }

    #[test]
    fn flattens_comments_with_containment_parent() {
        let page = parse_thread_page(&fixture("thread_comments_hot_post.json"), 1001)
            .expect("fixture must parse");

        assert_eq!(page.posts.len(), 4);
        let comments: Vec<_> = page
            .posts
            .iter()
            .filter(|post| post.kind == PostKind::Comment)
            .collect();
        assert_eq!(comments.len(), 2);
        assert!(comments.iter().all(|post| post.parent_pid == Some(4001)));
    }

    #[test]
    fn preserves_attachment_payload() {
        let page = parse_thread_page(&fixture("thread_attachments.json"), 1004)
            .expect("fixture must parse");
        assert!(page.posts[0].raw_payload.contains("asset-1.jpg"));
    }

    fn fixture(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nga")
            .join(name);
        serde_json::from_slice(&std::fs::read(path).expect("fixture must be readable"))
            .expect("fixture must be JSON")
    }
}
