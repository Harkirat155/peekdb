//! Cross-cutting integration test: drives the `serve` (MCP JSON-RPC-over-stdio) path
//! directly, end to end, against REAL Postgres + Mongo containers — proving enforcement
//! behavior matches the direct-CLI path (tests/integration_postgres.rs,
//! tests/integration_mongo.rs) rather than trusting that a check added to one code path
//! also covers the other.
//!
//! Requires `docker compose -f docker-compose.test.yml up -d`. Gated `#[ignore]`; run with:
//!     cargo test --test integration_mcp_serve -- --ignored

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

const PG_URL: &str = "host=localhost port=55432 user=testuser password=testpass dbname=testdb";
const MONGO_URI: &str = "mongodb://localhost:57017";
const MONGO_DB: &str = "testdb";

fn seed_data() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "DROP TABLE IF EXISTS widgets; \
                 CREATE TABLE widgets (id serial primary key, name text); \
                 INSERT INTO widgets (name) VALUES ('alpha');",
            )
            .await
            .unwrap();

        let mclient = mongodb::Client::with_uri_str(MONGO_URI).await.unwrap();
        let coll = mclient
            .database(MONGO_DB)
            .collection::<mongodb::bson::Document>("widgets");
        coll.drop(None).await.ok();
        coll.insert_many(vec![mongodb::bson::doc! {"name": "alpha"}], None)
            .await
            .unwrap();
    });
}

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

#[test]
#[ignore]
fn mcp_serve_enforces_read_only_identically_to_the_direct_cli() {
    seed_data();

    let mut config = tempfile::NamedTempFile::new().unwrap();
    write!(
        config,
        r#"
        [[database]]
        name = "pg"
        url = "host=localhost port=55432 user=testuser password=testpass dbname=testdb"
        no_tls = true

        [[mongo]]
        name = "mg"
        uri = "mongodb://localhost:57017/testdb"
        "#
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_mcp-db-readonly");
    let mut child = Command::new(bin)
        .args(["serve", "--config", config.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp-db-readonly serve");

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());

    send_frame(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
    );
    let resp = read_frame(&mut reader);
    assert_eq!(resp["result"]["protocolVersion"], "2024-11-05");

    send_frame(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    );
    let resp = read_frame(&mut reader);
    let tools: Vec<String> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(tools.contains(&"sql_query".to_string()));
    assert!(tools.contains(&"mongo_query".to_string()));

    // Allowed SQL query succeeds over MCP, same as the direct `query` subcommand.
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "sql_query", "arguments": {"statement": "SELECT * FROM widgets"}}
        }),
    );
    let resp = read_frame(&mut reader);
    assert_ne!(resp["result"]["isError"], json!(true));
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("alpha"));

    // Blocked SQL statement is rejected via the MCP path too — same enforcement layer.
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "sql_query", "arguments": {"statement": "DROP TABLE widgets"}}
        }),
    );
    let resp = read_frame(&mut reader);
    assert_eq!(resp["result"]["isError"], json!(true));
    assert!(resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Blocked"));

    // Allowed mongo find succeeds over MCP.
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": {"name": "mongo_query", "arguments": {"collection": "widgets", "operation": "find"}}
        }),
    );
    let resp = read_frame(&mut reader);
    assert_ne!(resp["result"]["isError"], json!(true));

    // Blocked mongo $out is rejected via the MCP path too.
    send_frame(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0", "id": 6, "method": "tools/call",
            "params": {
                "name": "mongo_query",
                "arguments": {
                    "collection": "widgets",
                    "operation": "aggregate",
                    "pipeline": "[{\"$out\":\"widgets_copy\"}]"
                }
            }
        }),
    );
    let resp = read_frame(&mut reader);
    assert_eq!(resp["result"]["isError"], json!(true));

    drop(stdin); // EOF on stdin — server should shut down cleanly.
    let status = child.wait().expect("wait for server to exit");
    assert!(status.success());
}
