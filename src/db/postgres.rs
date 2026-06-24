use super::traits::{Database, DbError, QueryResult};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_postgres::{NoTls, SimpleQueryMessage};
use tracing::{debug, error, info};

const BLOCKED_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE",
    "GRANT", "REVOKE", "EXECUTE", "EXEC", "CALL", "COPY",
    "VACUUM", "CLUSTER", "REINDEX", "REFRESH", "IMPORT",
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
            .map_err(|e| DbError::ConnectionFailed(format!("'{}': failed to set READ ONLY: {}", name, e)))?;

        info!("'{}' connected (READ ONLY).", name);
        **guard = Some(client);
        Ok(())
    }

    fn check_blocked(sql: &str) -> Result<(), DbError> {
        for stmt in sql.split(';') {
            let upper = stmt.trim().to_uppercase();
            if upper.is_empty() {
                continue;
            }

            let first_word = upper.split_whitespace().next().unwrap_or("");
            let is_explain = first_word == "EXPLAIN";

            if !is_explain && keyword_found(&upper, "DO") {
                return Err(DbError::BlockedKeyword { keyword: "DO".to_string() });
            }
            for &kw in BLOCKED_KEYWORDS {
                if keyword_found(&upper, kw) {
                    return Err(DbError::BlockedKeyword { keyword: kw.to_string() });
                }
            }
            // ANALYZE standalone is blocked; EXPLAIN ANALYZE is fine
            if !is_explain && keyword_found(&upper, "ANALYZE") {
                return Err(DbError::BlockedKeyword { keyword: "ANALYZE".to_string() });
            }
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
        let after_ok = after_pos >= haystack.len() || !is_ident_char(haystack.as_bytes()[after_pos]);
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
        Self::check_blocked(sql)?;

        debug!("[{}] {}", self.name, sql.trim());

        let mut guard = self.client.lock().await;
        Self::ensure_connected(&mut guard, &self.name, &self.url, self.no_tls).await?;

        let client = guard.as_ref().unwrap();
        let messages = match client.simple_query(sql).await {
            Ok(m) => m,
            Err(e) => {
                // Mark connection as dead so the next call reconnects
                if e.is_closed() {
                    *guard = None;
                    return Err(DbError::QueryFailed(
                        format!("Connection lost (will reconnect on next query): {}", e)
                    ));
                }
                return Err(DbError::QueryFailed(e.to_string()));
            }
        };

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
        Ok(QueryResult { columns, rows, row_count, truncated, command_tag })
    }
}
