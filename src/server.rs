use crate::db::traits::{Database, QueryResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    #[serde(default)]
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl JsonRpcResponse {
    fn ok(id: Value, result: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: Some(result), error: None }
    }

    fn err(id: Value, code: i32, message: String) -> Self {
        Self { jsonrpc: "2.0".into(), id, result: None, error: Some(RpcError { code, message }) }
    }
}

async fn read_message(reader: &mut BufReader<tokio::io::Stdin>) -> Option<String> {
    let mut content_length: usize = 0;

    // Read LSP-style headers: "Header: value\r\n" until "\r\n"
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => return None, // EOF
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() {
                    break; // blank line = end of headers
                }
                if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }
            Err(e) => {
                error!("stdin read error: {}", e);
                return None;
            }
        }
    }

    if content_length == 0 {
        return None;
    }

    let mut body = vec![0u8; content_length];
    match reader.read_exact(&mut body).await {
        Ok(_) => String::from_utf8(body).ok(),
        Err(e) => {
            error!("stdin body read error: {}", e);
            None
        }
    }
}

async fn write_message(stdout: &mut tokio::io::Stdout, json: &str) -> std::io::Result<()> {
    let bytes = json.as_bytes();
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    stdout.write_all(header.as_bytes()).await?;
    stdout.write_all(bytes).await?;
    stdout.flush().await?;
    Ok(())
}

pub async fn run(db: Arc<dyn Database>, default_max_rows: usize) {
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    info!("MCP server ready on stdio");

    loop {
        let raw = match read_message(&mut reader).await {
            Some(s) => s,
            None => {
                info!("stdin closed — shutting down");
                break;
            }
        };

        debug!("<<< {}", raw);

        let response = match serde_json::from_str::<JsonRpcRequest>(&raw) {
            Ok(req) => dispatch(req, &db, default_max_rows).await,
            Err(e) => Some(JsonRpcResponse::err(
                Value::Null,
                -32700,
                format!("Parse error: {}", e),
            )),
        };

        if let Some(resp) = response {
            let json = serde_json::to_string(&resp).expect("serialize failed");
            debug!(">>> {}", json);
            if let Err(e) = write_message(&mut stdout, &json).await {
                error!("stdout write error: {}", e);
                break;
            }
        }
    }
}

async fn dispatch(req: JsonRpcRequest, db: &Arc<dyn Database>, default_max_rows: usize) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => Some(handle_initialize(req.id)),
        "ping" => Some(JsonRpcResponse::ok(req.id, json!({}))),
        "tools/list" => Some(handle_tools_list(req.id)),
        "tools/call" => Some(handle_tools_call(req.id, req.params, db, default_max_rows).await),
        method if method.starts_with("notifications/") => {
            debug!("notification ignored: {}", method);
            None
        }
        unknown => {
            if req.id == Value::Null {
                debug!("unknown notification ignored: {}", unknown);
                None
            } else {
                warn!("unknown method: {}", unknown);
                Some(JsonRpcResponse::err(req.id, -32601, format!("Method not found: {}", unknown)))
            }
        }
    }
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::ok(id, json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "mcp-db-readonly",
            "version": env!("CARGO_PKG_VERSION")
        }
    }))
}

fn handle_tools_list(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::ok(id, json!({
        "tools": [{
            "name": "sql_query",
            "description": "Execute a read-only SQL query against the connected database.\n\nAllowed: SELECT, EXPLAIN, SHOW, WITH...SELECT, and other read-only statements.\nBlocked: INSERT, UPDATE, DELETE, DROP, CREATE, ALTER, TRUNCATE, GRANT, REVOKE, COPY, VACUUM, ANALYZE (standalone), CALL, DO, EXEC, CLUSTER, REINDEX, REFRESH, IMPORT.\n\nThe session is also enforced as READ ONLY at the connection level as a second layer of protection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "statement": {
                        "type": "string",
                        "description": "The SQL statement to execute (read-only only)"
                    },
                    "max_rows": {
                        "type": "integer",
                        "description": "Maximum rows to return. Default: server default (usually 500). Max: 5000.",
                        "minimum": 1,
                        "maximum": 5000
                    }
                },
                "required": ["statement"]
            }
        }]
    }))
}

async fn handle_tools_call(
    id: Value,
    params: Value,
    db: &Arc<dyn Database>,
    default_max_rows: usize,
) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    if tool_name != "sql_query" {
        return JsonRpcResponse::ok(id, json!({
            "content": [{"type": "text", "text": format!("Unknown tool: {}", tool_name)}],
            "isError": true
        }));
    }

    let statement = match args.get("statement").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return JsonRpcResponse::ok(id, json!({
                "content": [{"type": "text", "text": "Error: 'statement' is required and must be non-empty"}],
                "isError": true
            }));
        }
    };

    let max_rows = args.get("max_rows")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(5000))
        .unwrap_or(default_max_rows);

    match db.execute_query(&statement, max_rows).await {
        Ok(result) => JsonRpcResponse::ok(id, json!({
            "content": [{"type": "text", "text": format_result(&result)}]
        })),
        Err(e) => JsonRpcResponse::ok(id, json!({
            "content": [{"type": "text", "text": format!("Error: {}", e)}],
            "isError": true
        })),
    }
}

fn format_result(result: &QueryResult) -> String {
    if result.columns.is_empty() {
        return result.command_tag.clone()
            .unwrap_or_else(|| "OK — no rows returned".to_string());
    }

    // Column widths = max(header_len, max_value_len)
    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
    for row in &result.rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.as_deref().unwrap_or("NULL").len());
            }
        }
    }

    let border = widths.iter()
        .map(|w| "-".repeat(w + 2))
        .collect::<Vec<_>>()
        .join("+");
    let border = format!("+{}+", border);

    let mut out = String::with_capacity(256);
    out.push_str(&border);
    out.push('\n');

    // Header row
    let header: String = result.columns.iter().enumerate()
        .map(|(i, c)| format!(" {:w$} ", c, w = widths[i]))
        .collect::<Vec<_>>()
        .join("|");
    out.push('|');
    out.push_str(&header);
    out.push_str("|\n");
    out.push_str(&border);
    out.push('\n');

    // Data rows
    for row in &result.rows {
        let cells: String = row.iter().enumerate()
            .map(|(i, cell)| format!(" {:w$} ", cell.as_deref().unwrap_or("NULL"), w = widths[i]))
            .collect::<Vec<_>>()
            .join("|");
        out.push('|');
        out.push_str(&cells);
        out.push_str("|\n");
    }

    out.push_str(&border);
    out.push('\n');

    if result.truncated {
        out.push_str(&format!(
            "{} row(s) shown (result truncated — add LIMIT or increase max_rows up to 5000)\n",
            result.row_count
        ));
    } else {
        out.push_str(&format!("{} row(s)\n", result.row_count));
    }

    out
}
