use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub row_count: usize,
    pub truncated: bool,
    pub command_tag: Option<String>,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("Blocked: statement contains disallowed keyword '{keyword}'")]
    BlockedKeyword { keyword: String },
    #[error("Query failed: {0}")]
    QueryFailed(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}

#[async_trait]
pub trait Database: Send + Sync {
    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
}
