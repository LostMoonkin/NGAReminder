use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ThreadMetadata {
    pub tid: i64,
    pub fid: i64,
    pub title: String,
    pub forum_name: String,
    pub author_uid: i64,
    pub author_name: String,
    pub total_pages: i32,
    pub per_page: i32,
    pub vrows: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostKind {
    Topic,
    Reply,
    Comment,
}

impl PostKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Topic => "topic",
            Self::Reply => "reply",
            Self::Comment => "comment",
        }
    }

    pub fn event_type(self) -> &'static str {
        match self {
            Self::Topic => "new_topic",
            Self::Reply | Self::Comment => "new_reply",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ParsedPost {
    pub tid: i64,
    pub pid: Option<i64>,
    pub floor_number: i32,
    pub kind: PostKind,
    pub parent_pid: Option<i64>,
    pub parent_is_topic: bool,
    pub author_uid: i64,
    pub author_name: String,
    pub subject: String,
    pub content_raw: String,
    pub published_at_unix: Option<i64>,
    pub page_number: i32,
    pub raw_payload: String,
}

#[derive(Clone, Debug)]
pub struct ThreadPage {
    pub metadata: ThreadMetadata,
    pub current_page: i32,
    pub posts: Vec<ParsedPost>,
}
