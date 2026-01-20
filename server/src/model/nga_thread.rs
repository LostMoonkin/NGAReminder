use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct NGAThread {
    #[serde(default)]
    tid: u64,
    #[serde(rename(deserialize = "tsubject"))]
    title: String,
    #[serde(rename(deserialize = "tauthor"))]
    author_name: String,
    #[serde(rename(deserialize = "tauthorid"))]
    author_uid: u64,
    #[serde(rename(deserialize = "vrows"))]
    total_posts: u64,
    #[serde(rename(deserialize = "totalPage"))]
    total_pages: u64,
    #[serde(rename(deserialize = "result"))]
    posts: Vec<NAGPost>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NAGPost {
    tid: u64,
    pid: u64,
    content: String,
    #[serde(rename(deserialize = "postdate"))]
    post_date: String,
    #[serde(rename(deserialize = "postdatetimestamp"))]
    post_timestamp: u64,
    #[serde(rename(deserialize = "lou"))]
    post_number: u64,
    author: PostAuthor,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostAuthor {
    #[serde(rename(deserialize = "username"))]
    author_name: String,
    #[serde(rename(deserialize = "uid"))]
    author_uid: u64,
}