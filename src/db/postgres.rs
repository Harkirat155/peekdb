use super::traits::{Database, DbError, QueryResult};
use crate::policy::{
    cap_query_result_bytes, check_sql_allowlist, redact_query_result, RateLimiter, TargetPolicy,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_postgres::{NoTls, SimpleQueryMessage};
use tracing::{debug, error, info, warn};

// Only statements whose LEADING keyword is one of these are allowed through. This
// replaced a keyword-blocklist (which scanned the whole statement text, including
// inside string literals — see the git history / CHANGELOG for the false-positive bug
// that caused). Read-only enforcement's real backstop is still the
// `BEGIN TRANSACTION READ ONLY ... ROLLBACK` wrapping in `execute_query`: this allowlist
// is defense-in-depth, not the sole guarantee.
//
// TABLE is Postgres shorthand for `SELECT * FROM`; VALUES is a literal row constructor.
// Known, accepted gap: a leading `WITH` permits `WITH d AS (DELETE FROM t RETURNING *)
// SELECT * FROM d` — a CTE body can write. Closing that fully needs a real SQL parser
// (out of scope here); the read-only transaction wrapper is what actually stops it.
const ALLOWED_LEADING_KEYWORDS: &[&str] = &["SELECT", "WITH", "EXPLAIN", "SHOW", "VALUES", "TABLE"];

// Bare/legacy EXPLAIN option words (e.g. `EXPLAIN ANALYZE ...`) and FORMAT's value
// words, skipped over when locating the statement EXPLAIN actually wraps — so
// `EXPLAIN ANALYZE INSERT ...` is correctly seen as leading with INSERT, not EXPLAIN.
const EXPLAIN_MODIFIER_WORDS: &[&str] = &[
    "ANALYZE",
    "ANALYSE",
    "VERBOSE",
    "COSTS",
    "SETTINGS",
    "BUFFERS",
    "WAL",
    "TIMING",
    "SUMMARY",
    "FORMAT",
    "GENERIC_PLAN",
    "TEXT",
    "XML",
    "JSON",
    "YAML",
    "TRUE",
    "FALSE",
    "ON",
    "OFF",
];

/// Applied at connect time via `SET SESSION statement_timeout`, not in the checked
/// query path — it's a session setting, not a user statement, so it isn't itself
/// subject to (and shouldn't be confused with) the `check_allowed` allowlist.
const DEFAULT_STATEMENT_TIMEOUT_MS: u64 = 30_000;

pub struct PostgresDb {
    name: String,
    url: String,
    no_tls: bool,
    statement_timeout_ms: u64,
    // None = not yet connected (or connection was dropped)
    client: Mutex<Option<tokio_postgres::Client>>,
    policy: TargetPolicy,
    rate: Option<Arc<RateLimiter>>,
}

impl PostgresDb {
    /// Create a new (not-yet-connected) PostgresDb. Connection happens lazily on first query.
    pub fn new(name: impl Into<String>, url: impl Into<String>, no_tls: bool) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            no_tls,
            statement_timeout_ms: DEFAULT_STATEMENT_TIMEOUT_MS,
            client: Mutex::new(None),
            policy: TargetPolicy::default(),
            rate: None,
        }
    }

    /// Overrides the default 30s server-side `statement_timeout` for this connection —
    /// guards against a runaway/looping agent hammering a database with an expensive
    /// query. Call-frequency limits are applied via [`Self::with_safety`].
    pub fn with_statement_timeout_ms(mut self, ms: u64) -> Self {
        self.statement_timeout_ms = ms;
        self
    }

    pub fn with_safety(mut self, policy: TargetPolicy, rate: Option<Arc<RateLimiter>>) -> Self {
        self.policy = policy;
        self.rate = rate;
        self
    }

    pub fn allowlist(&self) -> &crate::policy::ObjectAllowlist {
        &self.policy.allow_objects
    }

    async fn ensure_connected(
        guard: &mut tokio::sync::MutexGuard<'_, Option<tokio_postgres::Client>>,
        name: &str,
        url: &str,
        no_tls: bool,
        statement_timeout_ms: u64,
    ) -> Result<(), DbError> {
        if guard.is_some() {
            return Ok(());
        }

        info!("Connecting to '{}'...", name);
        let client = if no_tls {
            connect_plain(url).await?
        } else {
            #[cfg(feature = "tls")]
            {
                connect_tls(url).await?
            }
            #[cfg(not(feature = "tls"))]
            {
                connect_plain(url).await?
            }
        };

        client
            .simple_query("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
            .await
            .map_err(|e| {
                DbError::ConnectionFailed(format!("'{}': failed to set READ ONLY: {}", name, e))
            })?;

        client
            .simple_query(&format!(
                "SET SESSION statement_timeout = {statement_timeout_ms}"
            ))
            .await
            .map_err(|e| {
                DbError::ConnectionFailed(format!(
                    "'{}': failed to set statement_timeout: {}",
                    name, e
                ))
            })?;

        info!(
            "'{}' connected (READ ONLY, statement_timeout={}ms).",
            name, statement_timeout_ms
        );
        **guard = Some(client);
        Ok(())
    }

    fn check_allowed(sql: &str) -> Result<(), DbError> {
        let trimmed = sql.trim_start();

        let body = if leading_word(trimmed).eq_ignore_ascii_case("EXPLAIN") {
            strip_leading_explain(trimmed)
        } else {
            trimmed
        };

        // Permit a parenthesized top-level query, e.g. "(SELECT 1) UNION (SELECT 2)".
        let mut body = body.trim_start();
        while let Some(stripped) = body.strip_prefix('(') {
            body = stripped.trim_start();
        }

        let leading = leading_word(body);
        if !leading.is_empty()
            && ALLOWED_LEADING_KEYWORDS
                .iter()
                .any(|k| leading.eq_ignore_ascii_case(k))
        {
            Ok(())
        } else {
            Err(DbError::NotAllowedStatement {
                leading: leading.to_uppercase(),
            })
        }
    }
}

async fn connect_plain(url: &str) -> Result<tokio_postgres::Client, DbError> {
    let config: tokio_postgres::Config = url
        .parse()
        .map_err(|e| DbError::ConnectionFailed(format!("Invalid URL: {}", e)))?;
    let (client, connection) = config
        .connect(NoTls)
        .await
        .map_err(|e| DbError::ConnectionFailed(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("Connection dropped: {}", e);
        }
    });
    Ok(client)
}

#[cfg(feature = "tls")]
async fn connect_tls(url: &str) -> Result<tokio_postgres::Client, DbError> {
    let config: tokio_postgres::Config = url
        .parse()
        .map_err(|e| DbError::ConnectionFailed(format!("Invalid URL: {}", e)))?;
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| DbError::ConnectionFailed(format!("TLS init: {}", e)))?;
    let tls = postgres_native_tls::MakeTlsConnector::new(connector);
    let (client, connection) = config
        .connect(tls)
        .await
        .map_err(|e| DbError::ConnectionFailed(e.to_string()))?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("Connection dropped: {}", e);
        }
    });
    Ok(client)
}

/// The leading run of identifier-like characters in `s` (char-boundary safe — this
/// slices `s` at `char_indices()` offsets, never raw byte offsets computed from a
/// separately-cased copy, which is what caused a real panic elsewhere in this codebase
/// for non-ASCII text; see `cli::truncate`).
fn leading_word(s: &str) -> &str {
    let end = s
        .char_indices()
        .find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    &s[..end]
}

/// Given a string starting with "EXPLAIN" (case-insensitive), returns the slice
/// starting at whatever statement EXPLAIN wraps — after skipping its parenthesized
/// option list, e.g. `(ANALYZE, BUFFERS)`, or a legacy sequence of bare option words
/// like `ANALYZE VERBOSE` / `FORMAT JSON`.
fn strip_leading_explain(sql: &str) -> &str {
    let first = leading_word(sql);
    let mut rest = &sql[first.len()..];

    let after_ws = rest.trim_start();
    if let Some(after_paren) = after_ws.strip_prefix('(') {
        return match after_paren.find(')') {
            Some(idx) => &after_paren[idx + 1..],
            None => after_ws, // unterminated '(' — leave as-is, the allowlist check below rejects it
        };
    }

    loop {
        let t = rest.trim_start();
        let t = t.strip_prefix(',').map(|s| s.trim_start()).unwrap_or(t);
        let word = leading_word(t);
        if word.is_empty()
            || !EXPLAIN_MODIFIER_WORDS
                .iter()
                .any(|m| word.eq_ignore_ascii_case(m))
        {
            return t;
        }
        rest = &t[word.len()..];
    }
}

#[async_trait]
impl Database for PostgresDb {
    fn check_object_access(&self, name: &str) -> Result<(), DbError> {
        self.policy.allow_objects.check(name)
    }

    async fn execute_query(&self, sql: &str, max_rows: usize) -> Result<QueryResult, DbError> {
        // Strip a single trailing semicolon, then reject any interior semicolons.
        // One statement per call: prevents chaining (e.g. "SET ...; INSERT ...").
        let sql = sql.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Err(DbError::QueryFailed("Empty statement".into()));
        }
        if sql.contains(';') {
            return Err(DbError::MultipleStatements);
        }

        Self::check_allowed(sql)?;
        check_sql_allowlist(sql, &self.policy.allow_objects)?;
        if let Some(rate) = &self.rate {
            rate.check_call()?;
            rate.record_call();
        }

        debug!("[{}] {}", self.name, sql);

        let mut guard = self.client.lock().await;
        Self::ensure_connected(
            &mut guard,
            &self.name,
            &self.url,
            self.no_tls,
            self.statement_timeout_ms,
        )
        .await?;

        // Every user statement runs inside an explicit read-only transaction that is
        // always rolled back. This is the primary enforcement layer:
        //   - BEGIN TRANSACTION READ ONLY means PostgreSQL itself enforces read-only,
        //     regardless of session settings.
        //   - ROLLBACK at the end undoes any SET LOCAL effects on the connection.
        // The session-level SET at connect time is a secondary belt-and-suspenders layer.
        let mut connection_dead = false;
        let result: Result<Vec<SimpleQueryMessage>, DbError> = {
            let client = guard.as_ref().unwrap();

            match client.simple_query("BEGIN TRANSACTION READ ONLY").await {
                Err(e) => {
                    connection_dead = e.is_closed();
                    Err(DbError::QueryFailed(format!("BEGIN failed: {}", e)))
                }
                Ok(_) => {
                    let r = client.simple_query(sql).await;
                    let rb = client.simple_query("ROLLBACK").await;

                    if let Err(ref e) = r {
                        if e.is_closed() {
                            connection_dead = true;
                        }
                    }
                    if let Err(ref e) = rb {
                        if e.is_closed() {
                            connection_dead = true;
                        } else {
                            warn!("[{}] ROLLBACK failed (non-fatal): {}", self.name, e);
                        }
                    }

                    r.map_err(|e| DbError::QueryFailed(e.to_string()))
                }
            }
            // `client` borrow ends here — safe to mutate `guard` below
        };

        if connection_dead {
            *guard = None;
        }

        let messages = result?;

        let mut columns: Vec<String> = Vec::new();
        let mut rows: Vec<Vec<Option<String>>> = Vec::new();
        let mut truncated = false;
        let mut command_tag: Option<String> = None;

        for message in messages {
            match message {
                SimpleQueryMessage::Row(row) => {
                    if columns.is_empty() {
                        columns = row.columns().iter().map(|c| c.name().to_string()).collect();
                    }
                    if rows.len() < max_rows {
                        let row_data: Vec<Option<String>> = (0..row.columns().len())
                            .map(|i| row.get(i).map(|s| s.to_string()))
                            .collect();
                        rows.push(row_data);
                    } else {
                        truncated = true;
                    }
                }
                SimpleQueryMessage::CommandComplete(tag) => {
                    command_tag = Some(tag.to_string());
                }
                _ => {}
            }
        }

        let row_count = rows.len();
        let mut result = QueryResult {
            columns,
            rows,
            row_count,
            truncated,
            command_tag,
            redacted: false,
        };

        if let Some(rate) = &self.rate {
            rate.check_and_record_rows(result.row_count as u64)?;
        }
        if redact_query_result(&mut result, &self.policy.redact) {
            result.redacted = true;
        }
        if let Some(max_b) = self.policy.max_response_bytes {
            if cap_query_result_bytes(&mut result, max_b)? {
                result.truncated = true;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_disallowed_leading_statement_is_rejected() {
        let samples: &[(&str, &str)] = &[
            ("INSERT", "INSERT INTO t VALUES (1)"),
            ("UPDATE", "UPDATE t SET x = 1"),
            ("DELETE", "DELETE FROM t"),
            ("MERGE", "MERGE INTO t USING s ON t.id = s.id"),
            ("DROP", "DROP TABLE t"),
            ("CREATE", "CREATE TABLE t (id int)"),
            ("ALTER", "ALTER TABLE t ADD COLUMN x int"),
            ("TRUNCATE", "TRUNCATE t"),
            ("GRANT", "GRANT SELECT ON t TO foo"),
            ("REVOKE", "REVOKE SELECT ON t FROM foo"),
            ("EXECUTE", "EXECUTE stmt"),
            ("EXEC", "EXEC stmt"),
            ("CALL", "CALL proc()"),
            ("DO", "DO $$ BEGIN NULL; END $$"),
            ("COPY", "COPY t TO STDOUT"),
            ("IMPORT", "IMPORT FOREIGN SCHEMA s"),
            ("VACUUM", "VACUUM t"),
            ("CLUSTER", "CLUSTER t"),
            ("REINDEX", "REINDEX TABLE t"),
            ("REFRESH", "REFRESH MATERIALIZED VIEW t"),
            ("COMMIT", "COMMIT"),
            ("ROLLBACK", "ROLLBACK"),
            ("SAVEPOINT", "SAVEPOINT s"),
            ("RELEASE", "RELEASE s"),
            ("SET", "SET search_path = public"),
            ("ANALYZE", "ANALYZE t"),
        ];
        for (leading, sql) in samples {
            match PostgresDb::check_allowed(sql) {
                Err(DbError::NotAllowedStatement { leading: found }) => {
                    assert_eq!(&found, leading)
                }
                other => panic!("expected {sql:?} to be rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn every_allowed_leading_statement_is_permitted() {
        for sql in [
            "SELECT * FROM t WHERE id = 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SHOW search_path",
            "VALUES (1), (2)",
            "TABLE widgets",
            "(SELECT 1) UNION (SELECT 2)",
        ] {
            assert!(
                PostgresDb::check_allowed(sql).is_ok(),
                "{sql:?} should be allowed"
            );
        }
    }

    #[test]
    fn explain_variants_wrapping_an_allowed_statement_are_permitted() {
        for sql in [
            "EXPLAIN SELECT 1",
            "EXPLAIN ANALYZE SELECT 1",
            "EXPLAIN ANALYZE VERBOSE SELECT 1",
            "EXPLAIN (ANALYZE, BUFFERS) SELECT 1",
            "EXPLAIN FORMAT JSON SELECT 1",
            "explain select 1", // case-insensitive
        ] {
            assert!(
                PostgresDb::check_allowed(sql).is_ok(),
                "{sql:?} should be allowed"
            );
        }
    }

    #[test]
    fn explain_wrapping_a_write_statement_is_still_rejected() {
        // The exact smuggling case an allowlist must not miss: EXPLAIN's own leading
        // keyword is allowed, but the statement it wraps must be checked too.
        for sql in [
            "EXPLAIN INSERT INTO widgets (name) VALUES ('x')",
            "EXPLAIN ANALYZE INSERT INTO widgets (name) VALUES ('x')",
            "EXPLAIN (ANALYZE) DELETE FROM widgets",
        ] {
            assert!(
                matches!(
                    PostgresDb::check_allowed(sql),
                    Err(DbError::NotAllowedStatement { .. })
                ),
                "{sql:?} should still be rejected"
            );
        }
    }

    #[test]
    fn string_literal_containing_a_keyword_is_no_longer_a_false_positive() {
        // This used to be wrongly rejected by the old blocklist (it scanned the whole
        // uppercased text, including inside string literals). The allowlist only
        // inspects the leading token, so this now correctly passes — a real bug fix,
        // not just a behavior change.
        assert!(PostgresDb::check_allowed("SELECT 'DROP TABLE users' AS example").is_ok());
    }

    #[test]
    fn unrecognized_leading_statement_is_rejected() {
        match PostgresDb::check_allowed("PRAGMA foo") {
            Err(DbError::NotAllowedStatement { leading }) => assert_eq!(leading, "PRAGMA"),
            other => panic!("expected rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn allowlist_blocks_on_real_execute_path_before_connect() {
        use crate::policy::{ObjectAllowlist, TargetPolicy};
        let policy = TargetPolicy {
            allow_objects: ObjectAllowlist::new(vec!["users"]),
            ..Default::default()
        };
        let db =
            PostgresDb::new("t", "postgres://u@127.0.0.1:1/db", true).with_safety(policy, None);
        let err = db
            .execute_query("SELECT * FROM secrets", 10)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DbError::NotAllowedObject { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn rate_limit_blocks_second_execute_on_real_db_path() {
        use crate::policy::{RateLimiter, TargetPolicy};
        use std::sync::Arc;
        let lim = Arc::new(RateLimiter::new(Some(1), None).unwrap());
        let db = PostgresDb::new("t", "postgres://u@127.0.0.1:1/db", true)
            .with_safety(TargetPolicy::default(), Some(lim));
        // First call may fail to connect but still consumes the call slot.
        let _ = db.execute_query("SELECT 1", 1).await;
        let err = db.execute_query("SELECT 1", 1).await.unwrap_err();
        assert!(matches!(err, DbError::RateLimited(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn catalog_list_sql_under_allowlist_is_not_object_blocked() {
        use crate::policy::{ObjectAllowlist, TargetPolicy};
        let policy = TargetPolicy {
            allow_objects: ObjectAllowlist::new(vec!["users"]),
            ..Default::default()
        };
        let db =
            PostgresDb::new("t", "postgres://u@127.0.0.1:1/db", true).with_safety(policy, None);
        let err = db
            .execute_query(
                "SELECT table_schema || '.' || table_name AS qname \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY 1",
                100,
            )
            .await
            .unwrap_err();
        // Must reach the network layer (connection failed), NOT allowlist.
        assert!(
            !matches!(err, DbError::NotAllowedObject { .. }),
            "catalog discovery SQL must not be allowlist-blocked, got {err:?}"
        );
        assert!(
            matches!(err, DbError::ConnectionFailed(_) | DbError::QueryFailed(_)),
            "expected connection/query failure after allowlist pass, got {err:?}"
        );
    }

    #[test]
    fn check_object_access_respects_allowlist() {
        use crate::db::traits::Database;
        use crate::policy::{ObjectAllowlist, TargetPolicy};
        let policy = TargetPolicy {
            allow_objects: ObjectAllowlist::new(vec!["users"]),
            ..Default::default()
        };
        let db =
            PostgresDb::new("t", "postgres://u@127.0.0.1:1/db", true).with_safety(policy, None);
        assert!(db.check_object_access("users").is_ok());
        assert!(db.check_object_access("public.users").is_ok());
        assert!(db.check_object_access("secrets").is_err());
    }
}
