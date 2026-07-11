use async_trait::async_trait;
use thiserror::Error;

// ── PostgreSQL types ──────────────────────────────────────────────────────────

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
    /// Mongo's operation allowlist and (future) Redis's command allowlist reject via
    /// this variant. Postgres used to as well, before its blocklist became an allowlist
    /// (see `NotAllowedStatement`) — kept for those other backends' use.
    #[error("Blocked: statement contains disallowed keyword '{keyword}'")]
    BlockedKeyword { keyword: String },
    #[error(
        "Blocked: only one SQL statement per call is allowed (found ';' inside the statement)"
    )]
    MultipleStatements,
    #[error(
        "Not allowed: statement must start with SELECT, WITH, EXPLAIN, SHOW, VALUES, or TABLE \
         (found '{leading}')"
    )]
    NotAllowedStatement { leading: String },
    #[error("Query failed: {0}")]
    QueryFailed(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}

#[async_trait]
pub trait Database: Send + Sync {
    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;
}

// ── MongoDB types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MongoOperation {
    Find,
    Aggregate,
    Distinct,
    Count,
    ListCollections,
    ListIndexes,
}

impl MongoOperation {
    // Named to match the CLI/MCP-facing "operation" string, not the std FromStr trait.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, String> {
        match s.to_lowercase().as_str() {
            "find" | "" => Ok(MongoOperation::Find),
            "aggregate" => Ok(MongoOperation::Aggregate),
            "distinct" => Ok(MongoOperation::Distinct),
            "count" => Ok(MongoOperation::Count),
            "list_collections" => Ok(MongoOperation::ListCollections),
            "list_indexes" => Ok(MongoOperation::ListIndexes),
            other => Err(format!(
                "Unknown operation '{}'. Valid: find, aggregate, distinct, count, list_collections, list_indexes",
                other
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MongoQuery {
    pub collection: Option<String>,
    pub operation: MongoOperation,
    pub filter: Option<String>,
    pub projection: Option<String>,
    pub sort: Option<String>,
    pub limit: usize,
    pub pipeline: Option<String>,
    pub field: Option<String>,
}

#[derive(Debug)]
pub struct MongoResult {
    pub documents: Vec<String>,
    pub count: usize,
    pub truncated: bool,
}

#[async_trait]
pub trait MongoDatabase: Send + Sync {
    async fn execute_mongo(&self, query: MongoQuery) -> Result<MongoResult, DbError>;
}
