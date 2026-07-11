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
    /// Data-modifying SQL nested inside an otherwise allowlisted statement (e.g. a
    /// `WITH … AS (DELETE … RETURNING *)` CTE, or `WITH … INSERT …`). Rejected
    /// client-side so agents get a clear block instead of a Postgres READ ONLY error.
    #[error(
        "Blocked: write keyword '{keyword}' is not allowed inside this statement \
         (data-modifying CTEs and other embedded DML are rejected client-side)"
    )]
    EmbeddedWrite { keyword: String },
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

impl DbError {
    /// Stable machine code for agents / MCP structured errors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::BlockedKeyword { .. } => "blocked_keyword",
            Self::MultipleStatements => "multiple_statements",
            Self::NotAllowedStatement { .. } => "not_allowed_statement",
            Self::EmbeddedWrite { .. } => "embedded_write",
            Self::QueryFailed(_) => "query_failed",
            Self::ConnectionFailed(_) => "connection_failed",
            Self::NotAllowedObject { .. } => "not_allowed_object",
            Self::RateLimited(_) => "rate_limited",
            Self::ResponseTooLarge(_) => "response_too_large",
        }
    }

    /// Which enforcement layer produced this error (for agent debugging).
    pub fn layer(&self) -> &'static str {
        match self {
            Self::BlockedKeyword { .. } => "command_or_stage_allowlist",
            Self::MultipleStatements => "single_statement",
            Self::NotAllowedStatement { .. } => "statement_allowlist",
            Self::EmbeddedWrite { .. } => "embedded_write_scan",
            Self::QueryFailed(_) => "database",
            Self::ConnectionFailed(_) => "connection",
            Self::NotAllowedObject { .. } => "object_allowlist",
            Self::RateLimited(_) => "rate_limit",
            Self::ResponseTooLarge(_) => "response_cap",
        }
    }

    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::BlockedKeyword { .. } => {
                Some("Use only allowlisted read operations/commands for this backend")
            }
            Self::MultipleStatements => {
                Some("Send one statement per call; do not chain with semicolons")
            }
            Self::NotAllowedStatement { .. } => {
                Some("Leading keyword must be SELECT, WITH, EXPLAIN, SHOW, VALUES, or TABLE")
            }
            Self::EmbeddedWrite { .. } => {
                Some("Remove INSERT/UPDATE/DELETE/MERGE (and other writes) from CTEs and the main query body")
            }
            Self::NotAllowedObject { .. } => {
                Some("Query an object on this target's allowlist, or ask an operator to expand allow_tables/allow_collections")
            }
            Self::RateLimited(_) => Some("Back off and retry; lower max_rows or call rate"),
            Self::ResponseTooLarge(_) => {
                Some("Lower max_rows/limit or raise max_response_bytes on the target")
            }
            Self::QueryFailed(_) | Self::ConnectionFailed(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DbError;

    #[test]
    fn agent_meta_covers_embedded_write() {
        let e = DbError::EmbeddedWrite {
            keyword: "DELETE".into(),
        };
        assert_eq!(e.code(), "embedded_write");
        assert_eq!(e.layer(), "embedded_write_scan");
        assert!(e.hint().is_some());
        assert!(e.to_string().contains("DELETE"));
    }

    #[test]
    fn agent_meta_covers_allowlist_and_rate_limit() {
        assert_eq!(
            DbError::NotAllowedStatement {
                leading: "INSERT".into()
            }
            .code(),
            "not_allowed_statement"
        );
        assert_eq!(
            DbError::RateLimited("slow down".into()).layer(),
            "rate_limit"
        );
    }
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
