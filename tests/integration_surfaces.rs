//! Thorough surface coverage against docker-compose.test.yml:
//! doctor CLI, MCP resources, describe/sample MCP, max_response_bytes,
//! multi-backend discovery, and format table mode.
//!
//!     cargo test --test integration_surfaces -- --ignored --test-threads=1

use peekdb::db::postgres::PostgresDb;
use peekdb::db::traits::{Database, DbError};
use peekdb::policy::{ObjectAllowlist, TargetPolicy};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
// ObjectAllowlist used in allowlist tests

const PG_URL: &str = "host=localhost port=55432 user=testuser password=testpass dbname=testdb";

fn write_full_config(path: &std::path::Path) {
    std::fs::write(
        path,
        r#"
[[database]]
name = "pg"
url = "host=localhost port=55432 user=testuser password=testpass dbname=testdb"
no_tls = true
allow_literal_password = true

[[mongo]]
name = "mg"
uri = "mongodb://localhost:57017/testdb"

[[redis]]
name = "rd"
url = "redis://localhost:56379"
"#,
    )
    .unwrap();
}

async fn seed_widgets_pg() {
    let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
        .await
        .expect("postgres");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS widgets;
            CREATE TABLE widgets (id serial primary key, name text, email text);
            INSERT INTO widgets (name, email) VALUES
              ('alpha', 'a@x.com'),
              ('beta', 'b@x.com'),
              ('gamma', 'c@x.com');
            "#,
        )
        .await
        .unwrap();
}

// ── doctor CLI ────────────────────────────────────────────────────────────────

#[test]
#[ignore]
fn doctor_cli_probes_all_three_backends_ok() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    write_full_config(&cfg);

    let bin = env!("CARGO_BIN_EXE_peekdb");
    let out = Command::new(bin)
        .args(["doctor", "--config", cfg.to_str().unwrap()])
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "doctor failed: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("ok") || stderr.contains("ok") || stdout.contains("postgres"));
    // Should mention all three backends
    let combined = format!("{stdout}{stderr}");
    assert!(combined.contains("postgres") || combined.contains("pg"));
    assert!(combined.contains("mongo") || combined.contains("mg"));
    assert!(combined.contains("redis") || combined.contains("rd") || combined.contains("PING"));
}

// ── max_response_bytes live ───────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn max_response_bytes_truncates_live_pg_rows() {
    seed_widgets_pg().await;
    // Pad rows so estimate_query_result_bytes exceeds a tight cap.
    let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let pad = "x".repeat(200);
    client
        .execute(
            "UPDATE widgets SET name = $1 || id::text, email = $2 || id::text",
            &[&pad, &pad],
        )
        .await
        .unwrap();

    let policy = TargetPolicy {
        max_response_bytes: Some(120),
        ..Default::default()
    };
    let db = PostgresDb::new("t", PG_URL, true).with_safety(policy, None);
    match db
        .execute_query("SELECT id, name, email FROM widgets ORDER BY id", 500)
        .await
    {
        Ok(result) => {
            assert!(
                result.truncated || result.row_count < 3,
                "expected truncation under max_response_bytes, got row_count={} truncated={}",
                result.row_count,
                result.truncated
            );
        }
        Err(DbError::ResponseTooLarge(_)) => {
            // Cap may error when even one padded row exceeds the budget — still correct.
        }
        Err(e) => panic!("unexpected error under max_response_bytes: {e:?}"),
    }
}

// ── MCP helper ────────────────────────────────────────────────────────────────

fn send_frame(stdin: &mut impl Write, body: &Value) {
    let s = body.to_string();
    write!(stdin, "Content-Length: {}\r\n\r\n{}", s.len(), s).unwrap();
    stdin.flush().unwrap();
}

fn read_frame(reader: &mut impl BufRead) -> Value {
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length: ") {
            content_length = v.trim().parse().unwrap();
        }
    }
    let mut buf = vec![0u8; content_length];
    reader.read_exact(&mut buf).unwrap();
    serde_json::from_slice(&buf).unwrap()
}

fn spawn_mcp(
    config: &std::path::Path,
) -> (
    std::process::Child,
    impl Write,
    BufReader<std::process::ChildStdout>,
) {
    let bin = env!("CARGO_BIN_EXE_peekdb");
    let mut child = Command::new(bin)
        .args([
            "serve",
            "--config",
            config.to_str().unwrap(),
            "--no-audit-log",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let stdin = child.stdin.take().unwrap();
    let reader = BufReader::new(child.stdout.take().unwrap());
    (child, stdin, reader)
}

#[test]
#[ignore]
fn mcp_resources_list_and_read_schema() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(seed_widgets_pg());

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    write_full_config(&cfg);

    let (mut child, mut stdin, mut reader) = spawn_mcp(&cfg);

    send_frame(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let init = read_frame(&mut reader);
    assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
    assert!(
        init["result"]["capabilities"]["resources"].is_object()
            || init["result"]["capabilities"].get("resources").is_some(),
        "initialize should advertise resources capability"
    );

    send_frame(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":2,"method":"resources/list","params":{}}),
    );
    let list = read_frame(&mut reader);
    let resources = list["result"]["resources"]
        .as_array()
        .expect("resources array");
    assert!(
        resources
            .iter()
            .any(|r| r["uri"].as_str() == Some("peekdb://pg/schema")),
        "missing peekdb://pg/schema in {resources:?}"
    );
    assert!(
        resources
            .iter()
            .any(|r| r["uri"].as_str() == Some("peekdb://mg/schema")),
        "missing mongo schema uri"
    );
    assert!(
        resources
            .iter()
            .any(|r| r["uri"].as_str() == Some("peekdb://rd/schema")),
        "missing redis schema uri"
    );

    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":3,"method":"resources/read",
            "params":{"uri":"peekdb://pg/schema"}
        }),
    );
    let read = read_frame(&mut reader);
    let text = read["result"]["contents"][0]["text"]
        .as_str()
        .expect("resource text");
    assert!(
        text.contains("widgets") || text.contains("tables"),
        "schema resource should list tables: {text}"
    );

    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":4,"method":"resources/read",
            "params":{"uri":"peekdb://pg/tables/widgets"}
        }),
    );
    let table = read_frame(&mut reader);
    let ttext = table["result"]["contents"][0]["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        ttext.contains("name") || ttext.contains("columns") || ttext.contains("widgets"),
        "table resource: {ttext}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
#[ignore]
fn mcp_describe_object_and_table_format() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(seed_widgets_pg());

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    write_full_config(&cfg);
    let (mut child, mut stdin, mut reader) = spawn_mcp(&cfg);

    send_frame(
        &mut stdin,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
    );
    let _ = read_frame(&mut reader);

    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{
                "name":"describe_object",
                "arguments":{"database":"pg","object":"widgets"}
            }
        }),
    );
    let desc = read_frame(&mut reader);
    assert_ne!(desc["result"]["isError"], json!(true));
    let dtext = desc["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        dtext.contains("email") || dtext.contains("columns") || dtext.contains("name"),
        "describe_object: {dtext}"
    );

    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{
                "name":"sql_query",
                "arguments":{
                    "database":"pg",
                    "statement":"SELECT name FROM widgets ORDER BY id LIMIT 1",
                    "format":"table"
                }
            }
        }),
    );
    let table = read_frame(&mut reader);
    assert_ne!(table["result"]["isError"], json!(true));
    let ttext = table["result"]["content"][0]["text"].as_str().unwrap();
    // ASCII table uses box drawing or pipes
    assert!(
        ttext.contains("alpha")
            && (ttext.contains('│') || ttext.contains('|') || ttext.contains('+')),
        "table format expected, got: {ttext}"
    );

    // mongo list_objects + redis list_objects
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"list_objects","arguments":{"database":"mg"}}
        }),
    );
    let mg = read_frame(&mut reader);
    assert_ne!(mg["result"]["isError"], json!(true));

    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"list_objects","arguments":{"database":"rd","pattern":"*"}}
        }),
    );
    let rd = read_frame(&mut reader);
    assert_ne!(rd["result"]["isError"], json!(true));

    drop(stdin);
    let _ = child.wait();
}

#[test]
#[ignore]
fn query_cli_with_format_csv_against_live_pg() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(seed_widgets_pg());

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    write_full_config(&cfg);
    let bin = env!("CARGO_BIN_EXE_peekdb");
    let out = Command::new(bin)
        .args([
            "query",
            "--config",
            cfg.to_str().unwrap(),
            "--db",
            "pg",
            "--sql",
            "SELECT name FROM widgets ORDER BY id",
            "--format",
            "csv",
            "--no-audit-log",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("name"));
    assert!(stdout.contains("alpha"));
}

#[test]
#[ignore]
fn query_cli_json_and_vertical_formats_live() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(seed_widgets_pg());
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    write_full_config(&cfg);
    let bin = env!("CARGO_BIN_EXE_peekdb");

    for fmt in ["json", "vertical", "tsv"] {
        let out = Command::new(bin)
            .args([
                "query",
                "--config",
                cfg.to_str().unwrap(),
                "--db",
                "pg",
                "--sql",
                "SELECT id, name FROM widgets LIMIT 2",
                "--format",
                fmt,
                "--no-audit-log",
            ])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "format {fmt} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("alpha") || stdout.contains("1"),
            "format {fmt}: {stdout}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn allowlist_via_config_file_cli_path() {
    seed_widgets_pg().await;
    // also create secrets table
    let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute("DROP TABLE IF EXISTS peeks_secrets; CREATE TABLE peeks_secrets (id int);")
        .await
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    std::fs::write(
        &cfg,
        r#"
[[database]]
name = "pg"
url = "host=localhost port=55432 user=testuser password=testpass dbname=testdb"
no_tls = true
allow_literal_password = true
allow_tables = ["widgets"]
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_peekdb");
    let denied = Command::new(bin)
        .args([
            "query",
            "--config",
            cfg.to_str().unwrap(),
            "--sql",
            "SELECT * FROM peeks_secrets",
            "--no-audit-log",
        ])
        .output()
        .unwrap();
    assert!(!denied.status.success());
    let err = String::from_utf8_lossy(&denied.stderr);
    assert!(
        err.contains("allowlist")
            || err.contains("NotAllowed")
            || err.contains("not in the allowlist"),
        "stderr={err}"
    );

    let allowed = Command::new(bin)
        .args([
            "query",
            "--config",
            cfg.to_str().unwrap(),
            "--sql",
            "SELECT name FROM widgets LIMIT 1",
            "--no-audit-log",
        ])
        .output()
        .unwrap();
    assert!(
        allowed.status.success(),
        "widgets should be allowed: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
}

#[tokio::test]
#[ignore]
async fn redact_columns_via_config_file_cli() {
    seed_widgets_pg().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("databases.toml");
    std::fs::write(
        &cfg,
        r#"
[[database]]
name = "pg"
url = "host=localhost port=55432 user=testuser password=testpass dbname=testdb"
no_tls = true
allow_literal_password = true
redact_columns = ["email"]
"#,
    )
    .unwrap();
    let bin = env!("CARGO_BIN_EXE_peekdb");
    let out = Command::new(bin)
        .args([
            "query",
            "--config",
            cfg.to_str().unwrap(),
            "--sql",
            "SELECT name, email FROM widgets ORDER BY id LIMIT 1",
            "--format",
            "json",
            "--no-audit-log",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[REDACTED]") || stdout.contains("REDACTED"));
    assert!(!stdout.contains("a@x.com"));
}

#[tokio::test]
#[ignore]
async fn object_allowlist_check_on_live_db_trait() {
    seed_widgets_pg().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["widgets"]),
        ..Default::default()
    };
    let db = PostgresDb::new("t", PG_URL, true).with_safety(policy, None);
    assert!(db.check_object_access("widgets").is_ok());
    assert!(matches!(
        db.check_object_access("peeks_secrets"),
        Err(DbError::NotAllowedObject { .. })
    ));
}
