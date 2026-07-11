use crate::db::traits::{
    Database, DbError, MongoDatabase, MongoQuery, MongoResult, QueryResult, RedisDatabase,
    RedisQuery, RedisResult,
};
use async_trait::async_trait;
use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Serialize)]
pub struct AuditEvent {
    pub timestamp: String,
    pub source: &'static str,
    pub backend: &'static str,
    /// The config entry NAME only — never the connection string/URI. Logging the
    /// resolved URL here would turn the audit trail into a second credential leak.
    pub target: String,
    pub operation: String,
    pub outcome: &'static str,
    pub detail: Option<String>,
    pub row_count: Option<usize>,
    pub duration_ms: u128,
    pub truncated: bool,
    /// True when column/PII redaction modified the returned payload.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Append-only JSON-Lines audit sink, deliberately NOT routed through `tracing` — a
/// `RUST_LOG=error` setting must not be able to silently make the security record
/// disappear. Rotates to `<path>.<YYYY-MM-DD>` on the first write of a new UTC day
/// (day is derived from each event's own timestamp, not read from the system clock
/// directly, which keeps rotation testable without faking wall-clock time).
pub struct AuditLog {
    path: PathBuf,
    state: Mutex<AuditLogState>,
}

#[derive(Default)]
struct AuditLogState {
    file: Option<File>,
    day: String,
}

impl AuditLog {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state: Mutex::new(AuditLogState::default()),
        }
    }

    pub fn write(&self, event: &AuditEvent) {
        let today = event.timestamp.get(0..10).unwrap_or("").to_string();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        if state.day != today {
            if let Err(e) = self.rotate(&mut state, &today) {
                tracing::warn!("audit log: could not open {}: {}", self.path.display(), e);
                return;
            }
        }

        let Some(file) = state.file.as_mut() else {
            return;
        };
        match serde_json::to_string(event) {
            Ok(line) => {
                if let Err(e) = writeln!(file, "{line}") {
                    tracing::warn!("audit log: write to {} failed: {}", self.path.display(), e);
                }
            }
            Err(e) => tracing::warn!("audit log: failed to serialize event: {}", e),
        }
    }

    fn rotate(&self, state: &mut AuditLogState, today: &str) -> std::io::Result<()> {
        if !state.day.is_empty() && state.day != today {
            state.file = None; // close the previous day's handle before renaming it
            let rotated = self.path.with_extension(format!("jsonl.{}", state.day));
            let _ = std::fs::rename(&self.path, rotated);
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        state.file = Some(file);
        state.day = today.to_string();
        Ok(())
    }
}

/// Wraps a `Database` with audit logging. Constructed at the same site the inner trait
/// object is built (`build_pg_databases` / `run_serve`) so both the direct-CLI and MCP
/// `serve` paths get identical coverage — this is what proves the two entry points stay
/// in sync rather than one silently drifting from the other.
pub struct AuditedDatabase {
    inner: Arc<dyn Database>,
    name: String,
    source: &'static str,
    log: Arc<AuditLog>,
}

impl AuditedDatabase {
    pub fn new(
        inner: Arc<dyn Database>,
        name: impl Into<String>,
        source: &'static str,
        log: Arc<AuditLog>,
    ) -> Self {
        Self {
            inner,
            name: name.into(),
            source,
            log,
        }
    }
}

#[async_trait]
impl Database for AuditedDatabase {
    fn check_object_access(&self, name: &str) -> Result<(), DbError> {
        self.inner.check_object_access(name)
    }

    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        let start = Instant::now();
        let result = self.inner.execute_query(sql, max_rows).await;
        let duration_ms = start.elapsed().as_millis();

        let (outcome, detail, row_count, truncated, redacted) = match &result {
            Ok(r) => ("ok", None, Some(r.row_count), r.truncated, r.redacted),
            Err(
                e @ (DbError::BlockedKeyword { .. }
                | DbError::NotAllowedStatement { .. }
                | DbError::MultipleStatements
                | DbError::NotAllowedObject { .. }
                | DbError::RateLimited(_)),
            ) => ("blocked", Some(e.to_string()), None, false, false),
            Err(e) => ("error", Some(e.to_string()), None, false, false),
        };

        self.log.write(&AuditEvent {
            timestamp: now_rfc3339(),
            source: self.source,
            backend: "postgres",
            target: self.name.clone(),
            operation: sql.to_string(),
            outcome,
            detail,
            row_count,
            duration_ms,
            truncated,
            redacted,
        });

        result
    }
}

/// Mongo counterpart of `AuditedDatabase` — see its docs.
pub struct AuditedMongo {
    inner: Arc<dyn MongoDatabase>,
    name: String,
    source: &'static str,
    log: Arc<AuditLog>,
}

impl AuditedMongo {
    pub fn new(
        inner: Arc<dyn MongoDatabase>,
        name: impl Into<String>,
        source: &'static str,
        log: Arc<AuditLog>,
    ) -> Self {
        Self {
            inner,
            name: name.into(),
            source,
            log,
        }
    }
}

#[async_trait]
impl MongoDatabase for AuditedMongo {
    fn check_object_access(&self, name: &str) -> Result<(), DbError> {
        self.inner.check_object_access(name)
    }

    async fn execute_mongo(&self, query: MongoQuery) -> Result<MongoResult, DbError> {
        let operation_desc = format!(
            "operation={:?} collection={:?}",
            query.operation, query.collection
        );
        let start = Instant::now();
        let result = self.inner.execute_mongo(query).await;
        let duration_ms = start.elapsed().as_millis();

        let (outcome, detail, row_count, truncated, redacted) = match &result {
            Ok(r) => ("ok", None, Some(r.count), r.truncated, r.redacted),
            Err(
                e @ (DbError::BlockedKeyword { .. }
                | DbError::NotAllowedObject { .. }
                | DbError::RateLimited(_)),
            ) => ("blocked", Some(e.to_string()), None, false, false),
            Err(e) => ("error", Some(e.to_string()), None, false, false),
        };

        self.log.write(&AuditEvent {
            timestamp: now_rfc3339(),
            source: self.source,
            backend: "mongo",
            target: self.name.clone(),
            operation: operation_desc,
            outcome,
            detail,
            row_count,
            duration_ms,
            truncated,
            redacted,
        });

        result
    }
}

/// Redis counterpart of `AuditedDatabase`/`AuditedMongo` — see their docs.
pub struct AuditedRedis {
    inner: Arc<dyn RedisDatabase>,
    name: String,
    source: &'static str,
    log: Arc<AuditLog>,
}

impl AuditedRedis {
    pub fn new(
        inner: Arc<dyn RedisDatabase>,
        name: impl Into<String>,
        source: &'static str,
        log: Arc<AuditLog>,
    ) -> Self {
        Self {
            inner,
            name: name.into(),
            source,
            log,
        }
    }
}

#[async_trait]
impl RedisDatabase for AuditedRedis {
    async fn execute_redis(&self, query: RedisQuery) -> Result<RedisResult, DbError> {
        let operation_desc = format!("command={} args={:?}", query.command, query.args);
        let start = Instant::now();
        let result = self.inner.execute_redis(query).await;
        let duration_ms = start.elapsed().as_millis();

        let (outcome, detail, row_count, truncated, redacted) = match &result {
            Ok(r) => ("ok", None, Some(r.count), r.truncated, r.redacted),
            Err(
                e @ (DbError::BlockedKeyword { .. }
                | DbError::NotAllowedObject { .. }
                | DbError::RateLimited(_)),
            ) => ("blocked", Some(e.to_string()), None, false, false),
            Err(e) => ("error", Some(e.to_string()), None, false, false),
        };

        self.log.write(&AuditEvent {
            timestamp: now_rfc3339(),
            source: self.source,
            backend: "redis",
            target: self.name.clone(),
            operation: operation_desc,
            outcome,
            detail,
            row_count,
            duration_ms,
            truncated,
            redacted,
        });

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(timestamp: &str) -> AuditEvent {
        AuditEvent {
            timestamp: timestamp.to_string(),
            source: "cli",
            backend: "postgres",
            target: "test".to_string(),
            operation: "SELECT 1".to_string(),
            outcome: "ok",
            detail: None,
            row_count: Some(1),
            duration_ms: 5,
            truncated: false,
            redacted: false,
        }
    }

    #[test]
    fn write_creates_the_file_and_appends_json_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path);

        log.write(&event("2026-01-01T00:00:00Z"));
        log.write(&event("2026-01-01T01:00:00Z"));

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2);
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["source"], "cli");
        assert_eq!(parsed["backend"], "postgres");
        assert_eq!(parsed["target"], "test");
    }

    #[test]
    fn write_never_includes_a_connection_string_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path);
        log.write(&event("2026-01-01T00:00:00Z"));

        let contents = std::fs::read_to_string(&path).unwrap();
        // AuditEvent has no url/uri/password field at all, but assert on the
        // serialized text too so a future field addition can't silently reintroduce one.
        assert!(!contents.contains("://"));
        assert!(!contents.to_lowercase().contains("password"));
    }

    #[test]
    fn write_rotates_to_a_dated_file_on_a_new_day() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("audit.jsonl");
        let log = AuditLog::open(&path);

        log.write(&event("2026-01-01T23:00:00Z"));
        log.write(&event("2026-01-02T00:00:00Z"));

        let rotated = dir.path().join("audit.jsonl.2026-01-01");
        assert!(
            rotated.exists(),
            "expected the first day's file to be rotated aside"
        );
        let rotated_contents = std::fs::read_to_string(&rotated).unwrap();
        assert_eq!(rotated_contents.lines().count(), 1);

        let current_contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(current_contents.lines().count(), 1);
    }

    #[test]
    fn write_to_an_unwritable_path_does_not_panic() {
        let log = AuditLog::open("/nonexistent-dir-xyz/audit.jsonl");
        log.write(&event("2026-01-01T00:00:00Z")); // must not panic
    }
}
