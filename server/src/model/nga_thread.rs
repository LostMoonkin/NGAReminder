use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NGAThread {
    pub code: u64,
    pub msg: String,
    #[serde(default)]
    pub tid: u64,
    #[serde(rename(deserialize = "tsubject"))]
    pub title: String,
    #[serde(rename(deserialize = "tauthor"))]
    pub author_name: String,
    #[serde(rename(deserialize = "tauthorid"))]
    pub author_uid: u64,
    #[serde(rename(deserialize = "vrows"))]
    pub total_posts: u64,
    #[serde(rename(deserialize = "totalPage"))]
    pub total_pages: u64,
    #[serde(rename(deserialize = "currentPage"))]
    pub current_page: u64,
    #[serde(rename(deserialize = "result"))]
    pub posts: Vec<NAGPost>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NAGPost {
    pub tid: u64,
    pub pid: u64,
    pub content: String,
    #[serde(rename(deserialize = "postdate"))]
    pub post_date: String,
    #[serde(rename(deserialize = "postdatetimestamp"))]
    pub post_timestamp: u64,
    #[serde(rename(deserialize = "lou"))]
    pub post_number: u64,
    pub author: PostAuthor,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostAuthor {
    #[serde(rename(deserialize = "username"))]
    pub author_name: String,
    #[serde(rename(deserialize = "uid"))]
    pub author_uid: u64,
}