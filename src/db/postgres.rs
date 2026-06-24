use super::traits::{Database, DbError, QueryResult};
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_postgres::{NoTls, SimpleQueryMessage};
use std::sync::Arc;
use tracing::{debug, error, info};

// Write operations blocked regardless of session-level READ ONLY setting (defense-in-depth)
const BLOCKED_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "DROP", "CREATE", "ALTER", "TRUNCATE",
    "GRANT", "REVOKE", "EXECUTE", "EXEC", "CALL", "COPY",
    "VACUUM", "CLUSTER", "REINDEX", "REFRESH", "IMPORT",
];

// DO is a PL/pgSQL block executor — blocked always
// ANALYZE is special: blocked standalone but allowed inside EXPLAIN ANALYZE

pub struct PostgresDb {
    client: Arc<Mutex<tokio_postgres::Client>>,
}

impl PostgresDb {
    pub async fn connect(database_url: &str, no_tls: bool) -> Result<Self, DbError> {
        info!("Connecting to PostgreSQL...");

        let config: tokio_postgres::Config = database_url
            .parse()
            .map_err(|e| DbError::ConnectionFailed(format!("Invalid URL: {}", e)))?;

        let client = if no_tls {
            connect_plain(&config).await?
        } else {
            #[cfg(feature = "tls")]
            {
                connect_tls(&config).await?
            }
            #[cfg(not(feature = "tls"))]
            {
                connect_plain(&config).await?
            }
        };

        // Primary read-only enforcement at session level
        client
            .simple_query("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
            .await
            .map_err(|e| DbError::ConnectionFailed(format!("Failed to set READ ONLY: {}", e)))?;

        info!("Connected. Session enforced as READ ONLY.");

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
        })
    }

    fn check_blocked(sql: &str) -> Result<(), DbError> {
        for stmt in sql.split(';') {
            let upper = stmt.trim().to_uppercase();
            if upper.is_empty() {
                continue;
            }

            let first_word = upper.split_whitespace().next().unwrap_or("");
            let starts_with_explain = first_word == "EXPLAIN";

            // DO is always blocked (PL/pgSQL executor)
            if !starts_with_explain && keyword_found(&upper, "DO") {
                return Err(DbError::BlockedKeyword { keyword: "DO".to_string() });
            }

            for &kw in BLOCKED_KEYWORDS {
                if keyword_found(&upper, kw) {
                    return Err(DbError::BlockedKeyword { keyword: kw.to_string() });
                }
            }

            // ANALYZE blocked standalone; EXPLAIN ANALYZE is fine
            if !starts_with_explain && keyword_found(&upper, "ANALYZE") {
                return Err(DbError::BlockedKeyword { keyword: "ANALYZE".to_string() });
            }
        }
        Ok(())
    }
}

async fn connect_plain(config: &tokio_postgres::Config) -> Result<tokio_postgres::Client, DbError> {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .map_err(|e| DbError::ConnectionFailed(e.to_string()))?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("PostgreSQL connection dropped: {}", e);
        }
    });

    Ok(client)
}

#[cfg(feature = "tls")]
async fn connect_tls(config: &tokio_postgres::Config) -> Result<tokio_postgres::Client, DbError> {
    let connector = native_tls::TlsConnector::new()
        .map_err(|e| DbError::ConnectionFailed(format!("TLS init failed: {}", e)))?;
    let tls = postgres_native_tls::MakeTlsConnector::new(connector);

    let (client, connection) = config
        .connect(tls)
        .await
        .map_err(|e| DbError::ConnectionFailed(e.to_string()))?;

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            error!("PostgreSQL connection dropped: {}", e);
        }
    });

    Ok(client)
}

// Word-boundary check: keyword must not be flanked by identifier chars
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

        debug!("Executing: {}", sql.trim());

        let client = self.client.lock().await;
        let messages = client
            .simple_query(sql)
            .await
            .map_err(|e| DbError::QueryFailed(e.to_string()))?;

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
