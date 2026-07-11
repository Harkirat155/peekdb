use super::traits::{Database, DbError, QueryResult};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_postgres::{NoTls, SimpleQueryMessage};
use tracing::{debug, error, info, warn};

// Write operations and anything that mutates session / transaction state.
// ANALYZE is NOT here — it needs the "EXPLAIN ANALYZE is OK" special case below.
const BLOCKED_KEYWORDS: &[&str] = &[
    // DML writes
    "INSERT",
    "UPDATE",
    "DELETE",
    "MERGE",
    // DDL
    "DROP",
    "CREATE",
    "ALTER",
    "TRUNCATE",
    // Permissions
    "GRANT",
    "REVOKE",
    // Procedural execution
    "EXECUTE",
    "EXEC",
    "CALL",
    "DO",
    // File / bulk operations
    "COPY",
    "IMPORT",
    // Maintenance
    "VACUUM",
    "CLUSTER",
    "REINDEX",
    "REFRESH",
    // Transaction state manipulation — we own the transaction wrapper
    "COMMIT",
    "ROLLBACK",
    "SAVEPOINT",
    "RELEASE",
    // Session / transaction mode changes (e.g. SET TRANSACTION READ WRITE)
    "SET",
];

pub struct PostgresDb {
    name: String,
    url: String,
    no_tls: bool,
    // None = not yet connected (or connection was dropped)
    client: Mutex<Option<tokio_postgres::Client>>,
}

impl PostgresDb {
    /// Create a new (not-yet-connected) PostgresDb. Connection happens lazily on first query.
    pub fn new(name: impl Into<String>, url: impl Into<String>, no_tls: bool) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            no_tls,
            client: Mutex::new(None),
        }
    }

    async fn ensure_connected(
        guard: &mut tokio::sync::MutexGuard<'_, Option<tokio_postgres::Client>>,
        name: &str,
        url: &str,
        no_tls: bool,
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

        info!("'{}' connected (READ ONLY).", name);
        **guard = Some(client);
        Ok(())
    }

    fn check_blocked(sql: &str) -> Result<(), DbError> {
        let upper = sql.to_uppercase();
        let is_explain = upper.split_whitespace().next() == Some("EXPLAIN");

        for &kw in BLOCKED_KEYWORDS {
            if keyword_found(&upper, kw) {
                return Err(DbError::BlockedKeyword {
                    keyword: kw.to_string(),
                });
            }
        }
        // ANALYZE standalone is blocked; EXPLAIN ANALYZE is fine
        if !is_explain && keyword_found(&upper, "ANALYZE") {
            return Err(DbError::BlockedKeyword {
                keyword: "ANALYZE".to_string(),
            });
        }
        Ok(())
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

fn keyword_found(haystack: &str, keyword: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(keyword) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident_char(haystack.as_bytes()[abs - 1]);
        let after_pos = abs + keyword.len();
        let after_ok =
            after_pos >= haystack.len() || !is_ident_char(haystack.as_bytes()[after_pos]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[async_trait]
impl Database for PostgresDb {
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

        Self::check_blocked(sql)?;

        debug!("[{}] {}", self.name, sql);

        let mut guard = self.client.lock().await;
        Self::ensure_connected(&mut guard, &self.name, &self.url, self.no_tls).await?;

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
        Ok(QueryResult {
            columns,
            rows,
            row_count,
            truncated,
            command_tag,
        })
    }
}
