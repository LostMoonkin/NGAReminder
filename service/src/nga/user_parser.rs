//! (dead_code allowed: verified parser contract, covered by fixture tests; wired by later user-crawl enhancements)
#![allow(dead_code)]

use encoding_rs::GBK;
use serde_json::{Map, Value};
use thiserror::Error;

use crate::domain::user::{UserListPage, UserProfile, UserReplyCandidate, UserTopicCandidate};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum UserParseError {
    #[error("missing or invalid field: {0}")]
    Field(&'static str),
    #[error("user profile was not found")]
    ProfileNotFound,
    #[error("user profile UID does not match requested UID")]
    UidMismatch,
    #[error("failed to preserve user JSON")]
    RawJson,
}

pub fn parse_topic_list(
    value: &Value,
    watched_uid: i64,
) -> Result<UserListPage<UserTopicCandidate>, UserParseError> {
    let result = result_object(value)?;
    let total_pages = total_pages(result, "__T__ROWS_PAGE")?;
    let entries = array(result, "__T")?;
    let mut candidates = Vec::new();
    for entry in entries {
        let object = entry.as_object().ok_or(UserParseError::Field("__T[]"))?;
        if object.get("denied").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if optional_i64(object, "authorid")? != Some(watched_uid) {
            continue;
        }
        let tid = required_i64(object, "tid")?;
        let postdate = required_i64(object, "postdate")?;
        if tid > 0 && postdate > 0 {
            candidates.push(UserTopicCandidate { tid, postdate });
        }
    }
    Ok(UserListPage {
        total_pages,
        candidates,
    })
}

pub fn parse_reply_list(
    value: &Value,
    watched_uid: i64,
) -> Result<UserListPage<UserReplyCandidate>, UserParseError> {
    let result = result_object(value)?;
    let total_pages = total_pages(result, "__R__ROWS_PAGE")?;
    let entries = array(result, "__T")?;
    let mut candidates = Vec::new();
    for entry in entries {
        let Some(post) = entry.get("__P").and_then(Value::as_object) else {
            continue;
        };
        if optional_i64(post, "authorid")? != Some(watched_uid) {
            continue;
        }
        let tid = required_i64(post, "tid")?;
        let pid = required_i64(post, "pid")?;
        let postdate = required_i64(post, "postdate")?;
        if tid > 0 && pid > 0 && postdate > 0 {
            candidates.push(UserReplyCandidate { tid, pid, postdate });
        }
    }
    Ok(UserListPage {
        total_pages,
        candidates,
    })
}

pub fn parse_profile_gbk(bytes: &[u8], watched_uid: i64) -> Result<UserProfile, UserParseError> {
    let (decoded, _, _) = GBK.decode(bytes);
    let marker = "__UCPUSER";
    let marker_start = decoded
        .find(marker)
        .ok_or(UserParseError::ProfileNotFound)?;
    let after_marker = &decoded[marker_start + marker.len()..];
    let object_start = after_marker
        .find('{')
        .ok_or(UserParseError::ProfileNotFound)?;
    let json = extract_json_object(&after_marker[object_start..])?;
    let value: Value = serde_json::from_str(json).map_err(|_| UserParseError::ProfileNotFound)?;
    let object = value.as_object().ok_or(UserParseError::ProfileNotFound)?;
    let uid = required_i64(object, "uid")?;
    if uid != watched_uid {
        return Err(UserParseError::UidMismatch);
    }
    Ok(UserProfile {
        uid,
        username: string_or_default(object, "username"),
        group_id: optional_i64(object, "groupid")?
            .map(|value| i32::try_from(value).map_err(|_| UserParseError::Field("groupid")))
            .transpose()?,
        avatar: optional_string(object, "avatar"),
        registered_at_unix: optional_i64(object, "regdate")?,
        last_post_at_unix: optional_i64(object, "lastpost")?,
        remote_post_count: optional_i64(object, "posts")?,
        signature: optional_string(object, "sign"),
        raw_payload: serde_json::to_string(&value).map_err(|_| UserParseError::RawJson)?,
    })
}

fn extract_json_object(input: &str) -> Result<&str, UserParseError> {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        match character {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&input[..=index]);
                }
            }
            _ => {}
        }
    }
    Err(UserParseError::ProfileNotFound)
}

fn result_object(value: &Value) -> Result<&Map<String, Value>, UserParseError> {
    value
        .get("result")
        .and_then(Value::as_object)
        .ok_or(UserParseError::Field("result"))
}

fn array<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a [Value], UserParseError> {
    object
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or(UserParseError::Field(field))
}

fn total_pages(
    object: &Map<String, Value>,
    page_size_field: &'static str,
) -> Result<i32, UserParseError> {
    let rows = required_i64(object, "__ROWS")?;
    let page_size = required_i64(object, page_size_field)?;
    if rows < 0 || page_size < 1 {
        return Err(UserParseError::Field(page_size_field));
    }
    i32::try_from(((rows + page_size - 1) / page_size).max(1))
        .map_err(|_| UserParseError::Field("__ROWS"))
}

fn required_i64(object: &Map<String, Value>, field: &'static str) -> Result<i64, UserParseError> {
    optional_i64(object, field)?.ok_or(UserParseError::Field(field))
}

fn optional_i64(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<i64>, UserParseError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() || value == "" {
        return Ok(None);
    }
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        .map(Some)
        .ok_or(UserParseError::Field(field))
}

fn string_or_default(object: &Map<String, Value>, field: &str) -> String {
    object
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn optional_string(object: &Map<String, Value>, field: &str) -> Option<String> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::{parse_profile_gbk, parse_reply_list, parse_topic_list};

    #[test]
    fn topics_use_server_page_count_and_skip_denied_entries() {
        let page = parse_topic_list(&fixture("user_topics_page_1.json"), 2001)
            .expect("fixture must parse");
        assert_eq!(page.total_pages, 2);
        assert_eq!(page.candidates.len(), 1);
        assert_eq!(page.candidates[0].tid, 1001);
    }

    #[test]
    fn replies_only_accept_watched_author() {
        let page = parse_reply_list(&fixture("user_replies_success.json"), 2001)
            .expect("fixture must parse");
        assert_eq!(page.candidates.len(), 1);
        assert_eq!(page.candidates[0].pid, 4002);
    }

    #[test]
    fn profile_is_decoded_from_gbk_without_executing_script() {
        let bytes = fixture_bytes("user_profile_gbk.html");
        let profile = parse_profile_gbk(&bytes, 2001).expect("profile must parse");
        assert_eq!(profile.uid, 2001);
        assert_eq!(profile.username, "脱敏用户");
        assert_eq!(profile.remote_post_count, Some(123));
    }

    fn fixture(name: &str) -> Value {
        serde_json::from_slice(&fixture_bytes(name)).expect("fixture must be JSON")
    }

    fn fixture_bytes(name: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/nga")
            .join(name);
        std::fs::read(path).expect("fixture must be readable")
    }
}
