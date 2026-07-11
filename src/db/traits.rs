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
    /// True when column/PII redaction modified at least one cell.
    pub redacted: bool,
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
    #[error("Blocked: object '{name}' is not in the allowlist for this target")]
    NotAllowedObject { name: String },
    #[error("Rate limited: {0}")]
    RateLimited(String),
    #[error("Response too large: {0}")]
    ResponseTooLarge(String),
}

#[async_trait]
pub trait Database: Send + Sync {
    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError>;

    /// Object-level allowlist check (tables). Default unrestricted.
    fn check_object_access(&self, _name: &str) -> Result<(), DbError> {
        Ok(())
    }
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
    pub redacted: bool,
}

#[async_trait]
pub trait MongoDatabase: Send + Sync {
    async fn execute_mongo(&self, query: MongoQuery) -> Result<MongoResult, DbError>;

    /// Collection-level allowlist check. Default unrestricted.
    fn check_object_access(&self, _name: &str) -> Result<(), DbError> {
        Ok(())
    }
}

// ── Redis types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct RedisQuery {
    pub command: String,
    pub args: Vec<String>,
    /// Caps the DISPLAYED result size for commands returning a list/set/hash-like
    /// value (e.g. SMEMBERS, HGETALL). Redis itself always executes the full command
    /// server-side — there's no cursor to stop early on a single-command call — so
    /// this truncates the response after the fact, the same semantics as Mongo's
    /// `limit` on `distinct`.
    pub limit: usize,
}

#[derive(Debug)]
pub struct RedisResult {
    pub values: Vec<String>,
    pub count: usize,
    pub truncated: bool,
    pub redacted: bool,
}

#[async_trait]
pub trait RedisDatabase: Send + Sync {
    async fn execute_redis(&self, query: RedisQuery) -> Result<RedisResult, DbError>;
}
