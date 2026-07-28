#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserProfile {
    pub uid: i64,
    pub username: String,
    pub group_id: Option<i32>,
    pub avatar: Option<String>,
    pub registered_at_unix: Option<i64>,
    pub last_post_at_unix: Option<i64>,
    pub remote_post_count: Option<i64>,
    pub signature: Option<String>,
    pub raw_payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserTopicCandidate {
    pub tid: i64,
    pub postdate: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserReplyCandidate {
    pub tid: i64,
    pub pid: i64,
    pub postdate: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserListPage<T> {
    pub total_pages: i32,
    pub candidates: Vec<T>,
}
