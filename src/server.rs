use crate::db::traits::{
    Database, MongoDatabase, MongoOperation, MongoQuery, MongoResult, QueryResult,
};
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
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }
    fn err(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(RpcError { code, message }),
        }
    }
}

pub struct Server {
    databases: HashMap<String, Arc<dyn Database>>,
    db_names: Vec<String>,
    mongo_databases: HashMap<String, Arc<dyn MongoDatabase>>,
    mongo_names: Vec<String>,
    default_max_rows: usize,
}

impl Server {
    pub fn new(
        databases: HashMap<String, Arc<dyn Database>>,
        mongo_databases: HashMap<String, Arc<dyn MongoDatabase>>,
        default_max_rows: usize,
    ) -> Self {
        let mut db_names: Vec<String> = databases.keys().cloned().collect();
        db_names.sort();
        let mut mongo_names: Vec<String> = mongo_databases.keys().cloned().collect();
        mongo_names.sort();
        Self {
            databases,
            db_names,
            mongo_databases,
            mongo_names,
            default_max_rows,
        }
    }

    fn resolve_db(&self, requested: Option<&str>) -> Result<Arc<dyn Database>, String> {
        let name = match requested {
            Some(n) => n,
            None if self.db_names.len() == 1 => &self.db_names[0],
            None => {
                return Err(format!(
                    "No 'database' specified. Available: {}",
                    self.db_names.join(", ")
                ))
            }
        };
        self.databases.get(name).cloned().ok_or_else(|| {
            format!(
                "Unknown database '{}'. Available: {}",
                name,
                self.db_names.join(", ")
            )
        })
    }

    fn resolve_mongo(&self, requested: Option<&str>) -> Result<Arc<dyn MongoDatabase>, String> {
        let name = match requested {
            Some(n) => n,
            None if self.mongo_names.len() == 1 => &self.mongo_names[0],
            None => {
                return Err(format!(
                    "No 'database' specified. Available MongoDB databases: {}",
                    self.mongo_names.join(", ")
                ))
            }
        };
        self.mongo_databases.get(name).cloned().ok_or_else(|| {
            format!(
                "Unknown MongoDB database '{}'. Available: {}",
                name,
                self.mongo_names.join(", ")
            )
        })
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
                if trimmed.is_empty() {
                    break;
                }
                if let Some(val) = trimmed.strip_prefix("Content-Length: ") {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }
            Err(e) => {
                error!("stdin read: {}", e);
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
            error!("stdin body: {}", e);
            None
        }
    }
}

async fn write_message(stdout: &mut tokio::io::Stdout, json: &str) -> std::io::Result<()> {
    let bytes = json.as_bytes();
    stdout
        .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await?;
    stdout.write_all(bytes).await?;
    stdout.flush().await
}

pub async fn run(
    databases: HashMap<String, Arc<dyn Database>>,
    mongo_databases: HashMap<String, Arc<dyn MongoDatabase>>,
    default_max_rows: usize,
) {
    let server = Arc::new(Server::new(databases, mongo_databases, default_max_rows));
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut summary = Vec::new();
    if !server.db_names.is_empty() {
        summary.push(format!("PG: {}", server.db_names.join(", ")));
    }
    if !server.mongo_names.is_empty() {
        summary.push(format!("Mongo: {}", server.mongo_names.join(", ")));
    }
    info!("MCP server ready — {}", summary.join(" | "));

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
            Ok(req) => dispatch(req, &server).await,
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
        m if m.starts_with("notifications/") => {
            debug!("notification: {}", m);
            None
        }
        unknown => {
            if req.id == Value::Null {
                debug!("unknown notification: {}", unknown);
                None
            } else {
                warn!("unknown method: {}", unknown);
                Some(JsonRpcResponse::err(
                    req.id,
                    -32601,
                    format!("Method not found: {}", unknown),
                ))
            }
        }
    }
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "mcp-db-readonly",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Value, server: &Server) -> JsonRpcResponse {
    let mut tools = Vec::new();

    // sql_query tool — only if postgres databases are configured
    if !server.db_names.is_empty() {
        let multi_pg = server.db_names.len() > 1;
        let mut props = json!({
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
        if multi_pg {
            props["database"] = json!({
                "type": "string",
                "description": format!("Target database. Available: {}", server.db_names.join(", ")),
                "enum": server.db_names
            });
        } else {
            props["database"] = json!({
                "type": "string",
                "description": format!("Database (only '{}' is configured)", server.db_names[0]),
                "enum": server.db_names,
                "default": server.db_names[0]
            });
        }
        let required = if multi_pg {
            json!(["statement", "database"])
        } else {
            json!(["statement"])
        };
        let db_list = server.db_names.join(", ");
        tools.push(json!({
            "name": "sql_query",
            "description": format!(
                "Execute a read-only SQL query against a PostgreSQL database.\n\
                 Databases: {db_list}\n\n\
                 Allowed: SELECT, EXPLAIN, EXPLAIN ANALYZE, SHOW, WITH...SELECT.\n\
                 Blocked: INSERT, UPDATE, DELETE, MERGE, DROP, CREATE, ALTER, TRUNCATE, GRANT, REVOKE, \
                 COPY, VACUUM, ANALYZE (standalone), CALL, DO, EXEC, CLUSTER, REINDEX, REFRESH, IMPORT, \
                 SET, COMMIT, ROLLBACK, SAVEPOINT, RELEASE.\n\n\
                 Every query runs inside BEGIN TRANSACTION READ ONLY ... ROLLBACK. \
                 Only one statement per call is allowed."
            ),
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": required
            }
        }));
    }

    // mongo_query tool — only if MongoDB databases are configured
    if !server.mongo_names.is_empty() {
        let multi_mg = server.mongo_names.len() > 1;
        let mut props = json!({
            "collection": {
                "type": "string",
                "description": "Collection name (required for find/aggregate/distinct/count/list_indexes)"
            },
            "operation": {
                "type": "string",
                "enum": ["find", "aggregate", "distinct", "count", "list_collections", "list_indexes"],
                "description": "Operation to perform (default: find)"
            },
            "filter": {
                "type": "string",
                "description": "MongoDB filter as JSON object (e.g. {\"status\": \"active\"}). Default: {} (all documents)."
            },
            "projection": {
                "type": "string",
                "description": "Fields to include/exclude as JSON (e.g. {\"name\": 1, \"_id\": 0})"
            },
            "sort": {
                "type": "string",
                "description": "Sort order as JSON (e.g. {\"created_at\": -1})"
            },
            "pipeline": {
                "type": "string",
                "description": "Aggregation pipeline as JSON array (e.g. [{\"$match\": {...}}, {\"$group\": {...}}]). Required for aggregate. $out and $merge are blocked."
            },
            "field": {
                "type": "string",
                "description": "Field name for the distinct operation"
            },
            "limit": {
                "type": "integer",
                "description": "Max documents to return (default 100, max 1000)",
                "minimum": 1,
                "maximum": 1000
            }
        });
        if multi_mg {
            props["database"] = json!({
                "type": "string",
                "description": format!("Target MongoDB database. Available: {}", server.mongo_names.join(", ")),
                "enum": server.mongo_names
            });
        } else {
            props["database"] = json!({
                "type": "string",
                "description": format!("MongoDB database (only '{}' is configured)", server.mongo_names[0]),
                "enum": server.mongo_names,
                "default": server.mongo_names[0]
            });
        }
        let required: Value = if multi_mg {
            json!(["database", "operation"])
        } else {
            json!(["operation"])
        };
        let mongo_list = server.mongo_names.join(", ");
        tools.push(json!({
            "name": "mongo_query",
            "description": format!(
                "Query a MongoDB database (read-only).\n\
                 Databases: {mongo_list}\n\n\
                 Operations: find, aggregate, distinct, count, list_collections, list_indexes.\n\
                 Blocked: $out and $merge pipeline stages (they write data).\n\
                 Output uses MongoDB Extended JSON (ObjectId as {{\"$oid\": \"...\"}}). \
                 Collection is required for all operations except list_collections."
            ),
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": required
            }
        }));
    }

    JsonRpcResponse::ok(id, json!({ "tools": tools }))
}

async fn handle_tools_call(id: Value, params: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "sql_query" => handle_sql_query(id, args, server).await,
        "mongo_query" => handle_mongo_query(id, args, server).await,
        other => JsonRpcResponse::ok(
            id,
            json!({
                "content": [{"type": "text", "text": format!("Unknown tool: {}", other)}],
                "isError": true
            }),
        ),
    }
}

async fn handle_sql_query(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let statement = match args.get("statement").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return JsonRpcResponse::ok(
                id,
                json!({
                    "content": [{"type": "text", "text": "Error: 'statement' is required and must be non-empty"}],
                    "isError": true
                }),
            )
        }
    };

    let db_name = args.get("database").and_then(|v| v.as_str());
    let db = match server.resolve_db(db_name) {
        Ok(db) => db,
        Err(msg) => {
            return JsonRpcResponse::ok(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("Error: {}", msg)}],
                    "isError": true
                }),
            )
        }
    };

    let max_rows = args
        .get("max_rows")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(5000))
        .unwrap_or(server.default_max_rows);

    match db.execute_query(&statement, max_rows).await {
        Ok(result) => JsonRpcResponse::ok(
            id,
            json!({
                "content": [{"type": "text", "text": format_result(&result)}]
            }),
        ),
        Err(e) => JsonRpcResponse::ok(
            id,
            json!({
                "content": [{"type": "text", "text": format!("Error: {}", e)}],
                "isError": true
            }),
        ),
    }
}

async fn handle_mongo_query(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let db_name = args.get("database").and_then(|v| v.as_str());
    let db = match server.resolve_mongo(db_name) {
        Ok(db) => db,
        Err(msg) => {
            return JsonRpcResponse::ok(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("Error: {}", msg)}],
                    "isError": true
                }),
            )
        }
    };

    let operation_str = args
        .get("operation")
        .and_then(|v| v.as_str())
        .unwrap_or("find");
    let operation = match MongoOperation::from_str(operation_str) {
        Ok(op) => op,
        Err(msg) => {
            return JsonRpcResponse::ok(
                id,
                json!({
                    "content": [{"type": "text", "text": format!("Error: {}", msg)}],
                    "isError": true
                }),
            )
        }
    };

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(1000))
        .unwrap_or(100);

    let query = MongoQuery {
        collection: args
            .get("collection")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        operation,
        filter: args
            .get("filter")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        projection: args
            .get("projection")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        sort: args
            .get("sort")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        pipeline: args
            .get("pipeline")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        field: args
            .get("field")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        limit,
    };

    match db.execute_mongo(query).await {
        Ok(result) => JsonRpcResponse::ok(
            id,
            json!({
                "content": [{"type": "text", "text": format_mongo_result(&result)}]
            }),
        ),
        Err(e) => JsonRpcResponse::ok(
            id,
            json!({
                "content": [{"type": "text", "text": format!("Error: {}", e)}],
                "isError": true
            }),
        ),
    }
}

fn format_mongo_result(result: &MongoResult) -> String {
    if result.documents.is_empty() {
        return "0 documents".to_string();
    }
    let mut out = String::new();
    for (i, doc) in result.documents.iter().enumerate() {
        out.push_str(&format!("[{}] {}\n", i + 1, doc));
    }
    if result.truncated {
        out.push_str(&format!(
            "\n{} document(s) shown (truncated — increase limit up to 1000)\n",
            result.count
        ));
    } else {
        out.push_str(&format!("\n{} document(s)\n", result.count));
    }
    out
}

fn format_result(result: &QueryResult) -> String {
    if result.columns.is_empty() {
        return result
            .command_tag
            .clone()
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
        widths
            .iter()
            .map(|w| "-".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    let mut out = String::with_capacity(256);

    out.push_str(&border);
    out.push('\n');
    out.push('|');
    out.push_str(
        &result
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!(" {:w$} ", c, w = widths[i]))
            .collect::<Vec<_>>()
            .join("|"),
    );
    out.push_str("|\n");
    out.push_str(&border);
    out.push('\n');

    for row in &result.rows {
        out.push('|');
        out.push_str(
            &row.iter()
                .enumerate()
                .map(|(i, cell)| {
                    format!(" {:w$} ", cell.as_deref().unwrap_or("NULL"), w = widths[i])
                })
                .collect::<Vec<_>>()
                .join("|"),
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
