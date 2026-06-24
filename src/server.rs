use crate::db::traits::{Database, QueryResult};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
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

pub struct Server {
    databases: HashMap<String, Arc<dyn Database>>,
    /// Sorted list of names for stable display
    db_names: Vec<String>,
    default_max_rows: usize,
}

impl Server {
    pub fn new(databases: HashMap<String, Arc<dyn Database>>, default_max_rows: usize) -> Self {
        let mut db_names: Vec<String> = databases.keys().cloned().collect();
        db_names.sort();
        Self { databases, db_names, default_max_rows }
    }

    fn single_db(&self) -> Option<&str> {
        if self.db_names.len() == 1 { Some(&self.db_names[0]) } else { None }
    }

    fn resolve_db<'a>(&'a self, requested: Option<&str>) -> Result<Arc<dyn Database>, String> {
        let name = match requested {
            Some(n) => n,
            None => match self.single_db() {
                Some(n) => n,
                None => return Err(format!(
                    "No 'database' specified. Available: {}",
                    self.db_names.join(", ")
                )),
            },
        };

        self.databases.get(name)
            .cloned()
            .ok_or_else(|| format!(
                "Unknown database '{}'. Available: {}",
                name, self.db_names.join(", ")
            ))
    }
}

async fn read_message(reader: &mut BufReader<tokio::io::Stdin>) -> Option<String> {
    let mut content_length: usize = 0;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.is_empty() { break; }
                if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }
            Err(e) => { error!("stdin read: {}", e); return None; }
        }
    }

    if content_length == 0 { return None; }

    let mut body = vec![0u8; content_length];
    match reader.read_exact(&mut body).await {
        Ok(_) => String::from_utf8(body).ok(),
        Err(e) => { error!("stdin body: {}", e); None }
    }
}

async fn write_message(stdout: &mut tokio::io::Stdout, json: &str) -> std::io::Result<()> {
    let bytes = json.as_bytes();
    stdout.write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes()).await?;
    stdout.write_all(bytes).await?;
    stdout.flush().await
}

pub async fn run(databases: HashMap<String, Arc<dyn Database>>, default_max_rows: usize) {
    let server = Arc::new(Server::new(databases, default_max_rows));
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    info!(
        "MCP server ready — {} database(s): {}",
        server.db_names.len(),
        server.db_names.join(", ")
    );

    loop {
        let raw = match read_message(&mut reader).await {
            Some(s) => s,
            None => { info!("stdin closed — shutting down"); break; }
        };

        debug!("<<< {}", raw);

        let response = match serde_json::from_str::<JsonRpcRequest>(&raw) {
            Ok(req) => dispatch(req, &server).await,
            Err(e) => Some(JsonRpcResponse::err(Value::Null, -32700, format!("Parse error: {}", e))),
        };

        if let Some(resp) = response {
            let json = serde_json::to_string(&resp).expect("serialize failed");
            debug!(">>> {}", json);
            if let Err(e) = write_message(&mut stdout, &json).await {
                error!("stdout write: {}", e);
                break;
            }
        }
    }
}

async fn dispatch(req: JsonRpcRequest, server: &Arc<Server>) -> Option<JsonRpcResponse> {
    match req.method.as_str() {
        "initialize" => Some(handle_initialize(req.id)),
        "ping" => Some(JsonRpcResponse::ok(req.id, json!({}))),
        "tools/list" => Some(handle_tools_list(req.id, server)),
        "tools/call" => Some(handle_tools_call(req.id, req.params, server).await),
        m if m.starts_with("notifications/") => { debug!("notification: {}", m); None }
        unknown => {
            if req.id == Value::Null {
                debug!("unknown notification: {}", unknown);
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

fn handle_tools_list(id: Value, server: &Server) -> JsonRpcResponse {
    let multi = server.db_names.len() > 1;

    let mut properties = json!({
        "statement": {
            "type": "string",
            "description": "The SQL statement to execute (read-only only)"
        },
        "max_rows": {
            "type": "integer",
            "description": "Maximum rows to return (1–5000). Defaults to server max_rows setting.",
            "minimum": 1,
            "maximum": 5000
        }
    });

    if multi {
        properties["database"] = json!({
            "type": "string",
            "description": format!("Target database name. Available: {}", server.db_names.join(", ")),
            "enum": server.db_names
        });
    } else {
        properties["database"] = json!({
            "type": "string",
            "description": format!("Database name (only '{}' is configured)", server.db_names[0]),
            "enum": server.db_names,
            "default": server.db_names[0]
        });
    }

    let required = if multi {
        json!(["statement", "database"])
    } else {
        json!(["statement"])
    };

    let db_list = server.db_names.join(", ");
    let description = format!(
        "Execute a read-only SQL query against a connected database.\n\
         Databases: {db_list}\n\n\
         Allowed: SELECT, EXPLAIN, EXPLAIN ANALYZE, SHOW, WITH...SELECT.\n\
         Blocked: INSERT, UPDATE, DELETE, DROP, CREATE, ALTER, TRUNCATE, GRANT, REVOKE, \
         COPY, VACUUM, ANALYZE (standalone), CALL, DO, EXEC, CLUSTER, REINDEX, REFRESH, IMPORT.\n\n\
         The session is also enforced READ ONLY at the connection level as a second layer."
    );

    JsonRpcResponse::ok(id, json!({
        "tools": [{
            "name": "sql_query",
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": properties,
                "required": required
            }
        }]
    }))
}

async fn handle_tools_call(id: Value, params: Value, server: &Arc<Server>) -> JsonRpcResponse {
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
        _ => return JsonRpcResponse::ok(id, json!({
            "content": [{"type": "text", "text": "Error: 'statement' is required and must be non-empty"}],
            "isError": true
        })),
    };

    let db_name = args.get("database").and_then(|v| v.as_str());
    let db = match server.resolve_db(db_name) {
        Ok(db) => db,
        Err(msg) => return JsonRpcResponse::ok(id, json!({
            "content": [{"type": "text", "text": format!("Error: {}", msg)}],
            "isError": true
        })),
    };

    let max_rows = args.get("max_rows")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(5000))
        .unwrap_or(server.default_max_rows);

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

    let mut widths: Vec<usize> = result.columns.iter().map(|c| c.len()).collect();
    for row in &result.rows {
        for (i, cell) in row.iter().enumerate() {
            if let Some(w) = widths.get_mut(i) {
                *w = (*w).max(cell.as_deref().unwrap_or("NULL").len());
            }
        }
    }

    let border = format!(
        "+{}+",
        widths.iter().map(|w| "-".repeat(w + 2)).collect::<Vec<_>>().join("+")
    );
    let mut out = String::with_capacity(256);

    out.push_str(&border);
    out.push('\n');
    out.push('|');
    out.push_str(
        &result.columns.iter().enumerate()
            .map(|(i, c)| format!(" {:w$} ", c, w = widths[i]))
            .collect::<Vec<_>>().join("|")
    );
    out.push_str("|\n");
    out.push_str(&border);
    out.push('\n');

    for row in &result.rows {
        out.push('|');
        out.push_str(
            &row.iter().enumerate()
                .map(|(i, cell)| format!(" {:w$} ", cell.as_deref().unwrap_or("NULL"), w = widths[i]))
                .collect::<Vec<_>>().join("|")
        );
        out.push_str("|\n");
    }

    out.push_str(&border);
    out.push('\n');
    if result.truncated {
        out.push_str(&format!(
            "{} row(s) shown (truncated — add LIMIT or increase max_rows up to 5000)\n",
            result.row_count
        ));
    } else {
        out.push_str(&format!("{} row(s)\n", result.row_count));
    }
    out
}
