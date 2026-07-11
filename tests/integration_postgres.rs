//! Exhaustive integration tests against a REAL Postgres container.
//!
//! Requires `docker compose -f docker-compose.test.yml up -d` (see repo root). Gated
//! `#[ignore]` so plain `cargo test` stays fast/offline; run explicitly with:
//!     cargo test --test integration_postgres -- --ignored --test-threads=1
//! (single-threaded: tests share one `widgets` table and reseed it, so they must not
//! interleave).

use peekdb::db::postgres::PostgresDb;
use peekdb::db::traits::{Database, DbError};

const PG_URL: &str = "host=localhost port=55432 user=testuser password=testpass dbname=testdb";

async fn raw_client() -> tokio_postgres::Client {
    let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
        .await
        .expect(
            "connect to test postgres container — is `docker compose -f docker-compose.test.yml up -d` running?",
        );
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn setup_widgets() {
    let client = raw_client().await;
    client
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS widgets;
            CREATE TABLE widgets (id serial primary key, name text NOT NULL, note text);
            INSERT INTO widgets (name, note) VALUES ('alpha', NULL);
            INSERT INTO widgets (name, note) VALUES ('beta', 'plain note');
            INSERT INTO widgets (name, note) VALUES ('gamma', 'row three');
            "#,
        )
        .await
        .expect("seed widgets table");
}

fn db() -> PostgresDb {
    PostgresDb::new("test", PG_URL, true)
}

#[tokio::test]
#[ignore]
async fn select_returns_rows_with_correct_columns_and_null_handling() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("SELECT id, name, note FROM widgets ORDER BY id", 500)
        .await
        .unwrap();
    assert_eq!(result.columns, vec!["id", "name", "note"]);
    assert_eq!(result.row_count, 3);
    assert_eq!(result.rows[0][1], Some("alpha".to_string()));
    assert_eq!(
        result.rows[0][2], None,
        "NULL note must round-trip as None, not the string \"NULL\""
    );
    assert!(!result.truncated);
}

#[tokio::test]
#[ignore]
async fn max_rows_truncates_and_flags_truncated() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("SELECT * FROM widgets ORDER BY id", 2)
        .await
        .unwrap();
    assert_eq!(result.row_count, 2);
    assert!(result.truncated);
}

#[tokio::test]
#[ignore]
async fn max_rows_not_flagged_truncated_when_exact_fit() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("SELECT * FROM widgets ORDER BY id", 3)
        .await
        .unwrap();
    assert_eq!(result.row_count, 3);
    assert!(!result.truncated);
}

#[tokio::test]
#[ignore]
async fn cte_query_is_allowed() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query(
            "WITH x AS (SELECT * FROM widgets) SELECT count(*) AS c FROM x",
            500,
        )
        .await
        .unwrap();
    assert_eq!(result.rows[0][0], Some("3".to_string()));
}

#[tokio::test]
#[ignore]
async fn window_function_query_is_allowed() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query(
            "SELECT name, row_number() OVER (ORDER BY id) AS rn FROM widgets",
            500,
        )
        .await
        .unwrap();
    assert_eq!(result.row_count, 3);
}

#[tokio::test]
#[ignore]
async fn explain_analyze_is_allowed() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("EXPLAIN ANALYZE SELECT * FROM widgets", 500)
        .await
        .unwrap();
    assert!(!result.rows.is_empty());
}

#[tokio::test]
#[ignore]
async fn multiline_query_is_allowed() {
    setup_widgets().await;
    let d = db();
    let sql = "SELECT *\nFROM widgets\nWHERE id > 0\nORDER BY id";
    let result = d.execute_query(sql, 500).await.unwrap();
    assert_eq!(result.row_count, 3);
}

#[tokio::test]
#[ignore]
async fn non_ascii_result_does_not_panic_when_printed() {
    // Regression test for the byte-index `truncate()` panic (fixed in src/cli.rs) —
    // exercised here against a REAL query result from Postgres, not a synthetic string.
    let client = raw_client().await;
    client
        .batch_execute(
            "DROP TABLE IF EXISTS widgets_utf8; \
             CREATE TABLE widgets_utf8 (id serial primary key, note text);",
        )
        .await
        .unwrap();
    let long_japanese = "日".repeat(200);
    client
        .execute(
            "INSERT INTO widgets_utf8 (note) VALUES ($1)",
            &[&long_japanese],
        )
        .await
        .unwrap();

    let d = db();
    let result = d
        .execute_query("SELECT id, note FROM widgets_utf8", 500)
        .await
        .unwrap();
    assert_eq!(result.row_count, 1);
    peekdb::cli::print_pg_result(&result, peekdb::cli::OutputFormat::Table);
    peekdb::cli::print_pg_result(&result, peekdb::cli::OutputFormat::Json);
}

#[tokio::test]
#[ignore]
async fn disallowed_statements_are_rejected_end_to_end_and_table_is_untouched() {
    setup_widgets().await;
    let d = db();
    let statements = [
        "INSERT INTO widgets (name) VALUES ('x')",
        "UPDATE widgets SET name = 'x'",
        "DELETE FROM widgets",
        "DROP TABLE widgets",
        "CREATE TABLE integration_test_t2 (id int)",
        "ALTER TABLE widgets ADD COLUMN extra int",
        "GRANT SELECT ON widgets TO PUBLIC",
        "TRUNCATE widgets",
        "COPY widgets TO STDOUT",
        "VACUUM widgets",
        "SET search_path = public",
    ];
    for sql in statements {
        let result = d.execute_query(sql, 500).await;
        assert!(
            matches!(result, Err(DbError::NotAllowedStatement { .. })),
            "{sql:?} should be rejected, got {result:?}"
        );
    }

    // Confirm none of the attempts actually reached Postgres.
    let check = d
        .execute_query("SELECT count(*) AS c FROM widgets", 500)
        .await
        .unwrap();
    assert_eq!(check.rows[0][0], Some("3".to_string()));
}

#[tokio::test]
#[ignore]
async fn interior_semicolon_statement_chaining_is_rejected() {
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("SELECT 1; INSERT INTO widgets (name) VALUES ('x')", 500)
        .await;
    assert!(matches!(result, Err(DbError::MultipleStatements)));
}

#[tokio::test]
#[ignore]
async fn string_literal_containing_a_keyword_now_succeeds_end_to_end() {
    // Regression test for the fixed false positive: the old blocklist scanned the
    // whole statement text (including string literals) and wrongly rejected this.
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query("SELECT 'DROP TABLE users' AS example", 500)
        .await
        .unwrap();
    assert_eq!(result.rows[0][0], Some("DROP TABLE users".to_string()));
}

#[tokio::test]
#[ignore]
async fn explain_analyze_wrapping_a_write_is_still_rejected_end_to_end() {
    // The allowlist must not be fooled by EXPLAIN's own leading keyword — the
    // statement EXPLAIN wraps has to be checked too.
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query(
            "EXPLAIN ANALYZE INSERT INTO widgets (name) VALUES ('smuggled')",
            500,
        )
        .await;
    assert!(matches!(result, Err(DbError::NotAllowedStatement { .. })));

    let check = d
        .execute_query("SELECT count(*) AS c FROM widgets", 500)
        .await
        .unwrap();
    assert_eq!(check.rows[0][0], Some("3".to_string()));
}

#[tokio::test]
#[ignore]
async fn data_modifying_cte_is_rejected_before_postgres() {
    // Classic smuggle: WITH starts allowlisted, CTE body writes. Must fail client-side
    // (EmbeddedWrite), not only via the READ ONLY transaction backstop.
    setup_widgets().await;
    let d = db();
    let result = d
        .execute_query(
            "WITH d AS (DELETE FROM widgets RETURNING *) SELECT count(*) FROM d",
            500,
        )
        .await;
    assert!(
        matches!(result, Err(DbError::EmbeddedWrite { .. })),
        "expected EmbeddedWrite, got {result:?}"
    );

    let check = d
        .execute_query("SELECT count(*) AS c FROM widgets", 500)
        .await
        .unwrap();
    assert_eq!(check.rows[0][0], Some("3".to_string()));
}

#[tokio::test]
#[ignore]
async fn statement_timeout_actually_aborts_a_slow_query() {
    // Proves the connect-time `SET SESSION statement_timeout` really takes effect —
    // not just that the config plumbing compiles. pg_sleep(2) with a 200ms timeout
    // must fail, not hang for 2 seconds.
    let d = PostgresDb::new("test", PG_URL, true).with_statement_timeout_ms(200);
    let start = std::time::Instant::now();
    let result = d.execute_query("SELECT pg_sleep(2)", 500).await;
    assert!(
        result.is_err(),
        "expected the slow query to be aborted by statement_timeout"
    );
    assert!(
        start.elapsed() < std::time::Duration::from_secs(1),
        "query should have been aborted well before the full 2s sleep completed"
    );
}

#[tokio::test]
#[ignore]
async fn read_only_transaction_backstop_rejects_write_independent_of_app_level_check() {
    // Proves the actual backstop mechanism (not just the app-level blocklist): even if
    // a write statement somehow reached `simple_query` inside the wrapping transaction,
    // Postgres itself refuses it because the transaction was opened READ ONLY.
    setup_widgets().await;
    let client = raw_client().await;
    client
        .simple_query("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")
        .await
        .unwrap();
    client
        .simple_query("BEGIN TRANSACTION READ ONLY")
        .await
        .unwrap();
    let write_result = client
        .simple_query("INSERT INTO widgets (name) VALUES ('should-fail')")
        .await;
    assert!(
        write_result.is_err(),
        "Postgres itself must reject a write inside a READ ONLY transaction"
    );
    let _ = client.simple_query("ROLLBACK").await;

    let d = db();
    let check = d
        .execute_query("SELECT count(*) AS c FROM widgets", 500)
        .await
        .unwrap();
    assert_eq!(check.rows[0][0], Some("3".to_string()));
}

#[tokio::test]
#[ignore]
async fn empty_statement_is_rejected() {
    let d = db();
    let result = d.execute_query("   ", 500).await;
    assert!(matches!(result, Err(DbError::QueryFailed(_))));
}
