use crate::db::schema::{
    describe_mongo_collection, describe_pg_table, describe_redis_key, list_mongo_collections,
    list_pg_tables, mongo_result_json, query_result_json, redis_result_json,
    sample_mongo_collection, sample_pg_table, scan_redis_keys,
};
use crate::db::traits::{
    Database, MongoDatabase, MongoOperation, MongoQuery, MongoResult, QueryResult, RedisDatabase,
    RedisQuery, RedisResult,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, error, info, warn};

#[derive(Clone, Copy)]
enum BackendKind {
    Postgres,
    Mongo,
    Redis,
}

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
    redis_databases: HashMap<String, Arc<dyn RedisDatabase>>,
    redis_names: Vec<String>,
    default_max_rows: usize,
}

impl Server {
    pub fn new(
        databases: HashMap<String, Arc<dyn Database>>,
        mongo_databases: HashMap<String, Arc<dyn MongoDatabase>>,
        redis_databases: HashMap<String, Arc<dyn RedisDatabase>>,
        default_max_rows: usize,
    ) -> Self {
        let mut db_names: Vec<String> = databases.keys().cloned().collect();
        db_names.sort();
        let mut mongo_names: Vec<String> = mongo_databases.keys().cloned().collect();
        mongo_names.sort();
        let mut redis_names: Vec<String> = redis_databases.keys().cloned().collect();
        redis_names.sort();
        Self {
            databases,
            db_names,
            mongo_databases,
            mongo_names,
            redis_databases,
            redis_names,
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

    fn resolve_redis(&self, requested: Option<&str>) -> Result<Arc<dyn RedisDatabase>, String> {
        let name = match requested {
            Some(n) => n,
            None if self.redis_names.len() == 1 => &self.redis_names[0],
            None => {
                return Err(format!(
                    "No 'database' specified. Available Redis instances: {}",
                    self.redis_names.join(", ")
                ))
            }
        };
        self.redis_databases.get(name).cloned().ok_or_else(|| {
            format!(
                "Unknown Redis instance '{}'. Available: {}",
                name,
                self.redis_names.join(", ")
            )
        })
    }

    /// Resolve a target name (unique across backends) to its kind + concrete handle name.
    fn resolve_target(&self, requested: Option<&str>) -> Result<(String, BackendKind), String> {
        let all = self.all_target_names();
        let name = match requested {
            Some(n) => n.to_string(),
            None if all.len() == 1 => all[0].clone(),
            None => {
                return Err(format!(
                    "No 'database' specified. Call list_targets first. Available: {}",
                    all.join(", ")
                ))
            }
        };
        if self.databases.contains_key(&name) {
            return Ok((name, BackendKind::Postgres));
        }
        if self.mongo_databases.contains_key(&name) {
            return Ok((name, BackendKind::Mongo));
        }
        if self.redis_databases.contains_key(&name) {
            return Ok((name, BackendKind::Redis));
        }
        Err(format!(
            "Unknown target '{}'. Available: {}",
            name,
            all.join(", ")
        ))
    }

    fn all_target_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        names.extend(self.db_names.iter().cloned());
        names.extend(self.mongo_names.iter().cloned());
        names.extend(self.redis_names.iter().cloned());
        names
    }

    fn list_targets_json(&self) -> Value {
        let mut targets = Vec::new();
        for n in &self.db_names {
            targets.push(json!({"name": n, "backend": "postgres"}));
        }
        for n in &self.mongo_names {
            targets.push(json!({"name": n, "backend": "mongo"}));
        }
        for n in &self.redis_names {
            targets.push(json!({"name": n, "backend": "redis"}));
        }
        json!({ "targets": targets })
    }
}

/// How a client frames messages; replies use the framing of the request they answer.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Framing {
    /// MCP stdio spec: one JSON message per line
    Lines,
    /// LSP-style `Content-Length: N` headers
    ContentLength,
}

async fn read_message(reader: &mut (impl AsyncBufRead + Unpin)) -> Option<(String, Framing)> {
    let mut content_length: usize = 0;

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => return None,
            Ok(_) => {
                let trimmed = line.trim_end_matches(['\r', '\n']);
                if trimmed.trim_start().starts_with('{') {
                    return Some((trimmed.trim().to_string(), Framing::Lines));
                }
                if trimmed.is_empty() {
                    if content_length == 0 {
                        continue;
                    }
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
        Ok(_) => String::from_utf8(body)
            .ok()
            .map(|b| (b, Framing::ContentLength)),
        Err(e) => {
            error!("stdin body: {}", e);
            None
        }
    }
}

async fn write_message(
    stdout: &mut tokio::io::Stdout,
    json: &str,
    framing: Framing,
) -> std::io::Result<()> {
    let bytes = json.as_bytes();
    match framing {
        Framing::Lines => {
            stdout.write_all(bytes).await?;
            stdout.write_all(b"\n").await?;
        }
        Framing::ContentLength => {
            stdout
                .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
                .await?;
            stdout.write_all(bytes).await?;
        }
    }
    stdout.flush().await
}

pub async fn run(
    databases: HashMap<String, Arc<dyn Database>>,
    mongo_databases: HashMap<String, Arc<dyn MongoDatabase>>,
    redis_databases: HashMap<String, Arc<dyn RedisDatabase>>,
    default_max_rows: usize,
) {
    let server = Arc::new(Server::new(
        databases,
        mongo_databases,
        redis_databases,
        default_max_rows,
    ));
    let mut reader = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    let mut summary = Vec::new();
    if !server.db_names.is_empty() {
        summary.push(format!("PG: {}", server.db_names.join(", ")));
    }
    if !server.mongo_names.is_empty() {
        summary.push(format!("Mongo: {}", server.mongo_names.join(", ")));
    }
    if !server.redis_names.is_empty() {
        summary.push(format!("Redis: {}", server.redis_names.join(", ")));
    }
    info!("MCP server ready — {}", summary.join(" | "));

    loop {
        let (raw, framing) = match read_message(&mut reader).await {
            Some(m) => m,
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
            if let Err(e) = write_message(&mut stdout, &json, framing).await {
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
        "resources/list" => Some(handle_resources_list(req.id, server)),
        "resources/read" => Some(handle_resources_read(req.id, req.params, server).await),
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
            "capabilities": {
                "tools": {},
                "resources": {}
            },
            "serverInfo": {
                "name": "peekdb",
                "version": env!("CARGO_PKG_VERSION")
            }
        }),
    )
}

fn handle_tools_list(id: Value, server: &Server) -> JsonRpcResponse {
    let mut tools = Vec::new();
    let all_targets = server.all_target_names();

    // Discovery first — short descriptions so agents start here.
    tools.push(json!({
        "name": "list_targets",
        "description": "List configured database targets (name + backend). Call this first when exploring.",
        "inputSchema": { "type": "object", "properties": {}, "required": [] }
    }));

    if !all_targets.is_empty() {
        let target_prop = target_property(&all_targets, "Target from list_targets");
        tools.push(json!({
            "name": "list_objects",
            "description": "List tables (Postgres), collections (Mongo), or SCAN keys (Redis) for a target. Prefer this over guessing names.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "database": target_prop,
                    "pattern": {
                        "type": "string",
                        "description": "Redis only: SCAN MATCH pattern (default '*'). Ignored for Postgres/Mongo."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max objects to return (default 100, max 200 for Redis SCAN).",
                        "minimum": 1,
                        "maximum": 2000
                    }
                },
                "required": if all_targets.len() > 1 { json!(["database"]) } else { json!([]) }
            }
        }));
        tools.push(json!({
            "name": "describe_object",
            "description": "Describe a table/view (Postgres), collection (Mongo, sample-inferred fields + indexes), or key (Redis TYPE/TTL).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "database": target_property(&all_targets, "Target from list_targets"),
                    "object": {
                        "type": "string",
                        "description": "Table (optionally schema.table), collection name, or Redis key"
                    }
                },
                "required": if all_targets.len() > 1 {
                    json!(["database", "object"])
                } else {
                    json!(["object"])
                }
            }
        }));
        tools.push(json!({
            "name": "sample_object",
            "description": "Sample rows/docs from a Postgres table or Mongo collection (safe identifier / collection name). Not for Redis — use redis_query.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "database": target_property(&all_targets, "Target from list_targets"),
                    "object": {
                        "type": "string",
                        "description": "Table (schema.table ok) or collection name"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max rows/docs (default 10, max 100)",
                        "minimum": 1,
                        "maximum": 100
                    }
                },
                "required": if all_targets.len() > 1 {
                    json!(["database", "object"])
                } else {
                    json!(["object"])
                }
            }
        }));
    }

    // sql_query tool — only if postgres databases are configured
    if !server.db_names.is_empty() {
        let multi_pg = server.db_names.len() > 1;
        let mut props = json!({
            "statement": {
                "type": "string",
                "description": "Read-only SQL (SELECT/WITH/EXPLAIN/SHOW/VALUES/TABLE only)"
            },
            "max_rows": {
                "type": "integer",
                "description": "Maximum rows to return (1–5000). Defaults to server max_rows setting.",
                "minimum": 1,
                "maximum": 5000
            },
            "format": {
                "type": "string",
                "enum": ["json", "table"],
                "description": "Result format. Default json (agent-friendly). Use table for ASCII."
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
                "Read-only SQL against Postgres ({db_list}). Prefer list_objects/describe_object first. \
                 Allowlist: SELECT/WITH/EXPLAIN/SHOW/VALUES/TABLE; plus READ ONLY transaction. Single statement only."
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
                "description": "Aggregation pipeline as JSON array. Required for aggregate. $out/$merge blocked."
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
            },
            "format": {
                "type": "string",
                "enum": ["json", "table"],
                "description": "Result format. Default json."
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
                "Read-only Mongo ({mongo_list}). Ops: find/aggregate/distinct/count/list_collections/list_indexes. \
                 Prefer list_objects/describe_object for exploration. $out/$merge blocked."
            ),
            "inputSchema": {
                "type": "object",
                "properties": props,
                "required": required
            }
        }));
    }

    // redis_query tool — only if Redis instances are configured
    if !server.redis_names.is_empty() {
        let multi_redis = server.redis_names.len() > 1;
        let mut props = json!({
            "command": {
                "type": "string",
                "description": "Redis command to execute, e.g. GET, HGETALL, SMEMBERS (read-only commands only)"
            },
            "args": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Arguments to the command, in order (e.g. [\"mykey\"] for GET, [\"myhash\", \"field\"] for HGET)"
            },
            "limit": {
                "type": "integer",
                "description": "Max items to display for list/set/hash-like results (default 100, max 1000)",
                "minimum": 1,
                "maximum": 1000
            },
            "format": {
                "type": "string",
                "enum": ["json", "table"],
                "description": "Result format. Default json."
            }
        });
        if multi_redis {
            props["database"] = json!({
                "type": "string",
                "description": format!("Target Redis instance. Available: {}", server.redis_names.join(", ")),
                "enum": server.redis_names
            });
        } else {
            props["database"] = json!({
                "type": "string",
                "description": format!("Redis instance (only '{}' is configured)", server.redis_names[0]),
                "enum": server.redis_names,
                "default": server.redis_names[0]
            });
        }
        let required: Value = if multi_redis {
            json!(["command", "database"])
        } else {
            json!(["command"])
        };
        let redis_list = server.redis_names.join(", ");
        tools.push(json!({
            "name": "redis_query",
            "description": format!(
                "Read-only Redis ({redis_list}). Allowlisted reads only (GET/HGETALL/SCAN/TTL/…). \
                 Writes rejected by omission. Prefer list_objects for key discovery."
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

fn target_property(names: &[String], description: &str) -> Value {
    if names.len() == 1 {
        json!({
            "type": "string",
            "description": format!("{description} (only '{}' configured)", names[0]),
            "enum": names,
            "default": names[0]
        })
    } else {
        json!({
            "type": "string",
            "description": format!("{description}. Available: {}", names.join(", ")),
            "enum": names
        })
    }
}

fn tool_ok(id: Value, body: Value) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        id,
        json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string())}]
        }),
    )
}

fn tool_err(id: Value, msg: impl std::fmt::Display) -> JsonRpcResponse {
    JsonRpcResponse::ok(
        id,
        json!({
            "content": [{"type": "text", "text": format!("Error: {msg}")}],
            "isError": true
        }),
    )
}

/// Structured error envelope for agents: stable `code` / `layer` / optional `hint`
/// plus human `message`. Still MCP `isError: true` with a single text content block.
fn tool_err_db(id: Value, err: &crate::db::traits::DbError) -> JsonRpcResponse {
    let mut body = json!({
        "error": {
            "message": err.to_string(),
            "code": err.code(),
            "layer": err.layer(),
        }
    });
    if let Some(hint) = err.hint() {
        body["error"]["hint"] = json!(hint);
    }
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| format!("Error: {err}"));
    JsonRpcResponse::ok(
        id,
        json!({
            "content": [{"type": "text", "text": text}],
            "isError": true
        }),
    )
}

fn wants_table(args: &Value) -> bool {
    args.get("format")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("table"))
}

async fn handle_tools_call(id: Value, params: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    match tool_name {
        "list_targets" => tool_ok(id, server.list_targets_json()),
        "list_objects" => handle_list_objects(id, args, server).await,
        "describe_object" => handle_describe_object(id, args, server).await,
        "sample_object" => handle_sample_object(id, args, server).await,
        "sql_query" => handle_sql_query(id, args, server).await,
        "mongo_query" => handle_mongo_query(id, args, server).await,
        "redis_query" => handle_redis_query(id, args, server).await,
        other => tool_err(id, format!("Unknown tool: {other}")),
    }
}

async fn handle_list_objects(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let db_name = args.get("database").and_then(|v| v.as_str());
    let (name, kind) = match server.resolve_target(db_name) {
        Ok(v) => v,
        Err(e) => return tool_err(id, e),
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(100);

    match kind {
        BackendKind::Postgres => {
            let db = server.databases.get(&name).unwrap().clone();
            match list_pg_tables(db.as_ref()).await {
                Ok(mut tables) => {
                    let total = tables.len();
                    if tables.len() > limit {
                        tables.truncate(limit);
                    }
                    tool_ok(
                        id,
                        json!({
                            "target": name,
                            "backend": "postgres",
                            "objects": tables,
                            "meta": { "count": total.min(limit), "total_found": total, "truncated": total > limit }
                        }),
                    )
                }
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Mongo => {
            let db = server.mongo_databases.get(&name).unwrap().clone();
            match list_mongo_collections(db.as_ref()).await {
                Ok(mut colls) => {
                    let total = colls.len();
                    if colls.len() > limit {
                        colls.truncate(limit);
                    }
                    tool_ok(
                        id,
                        json!({
                            "target": name,
                            "backend": "mongo",
                            "objects": colls,
                            "meta": { "count": total.min(limit), "total_found": total, "truncated": total > limit }
                        }),
                    )
                }
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Redis => {
            let db = server.redis_databases.get(&name).unwrap().clone();
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("*");
            match scan_redis_keys(db.as_ref(), pattern, limit).await {
                Ok(result) => tool_ok(
                    id,
                    json!({
                        "target": name,
                        "backend": "redis",
                        "scan": redis_result_json(&result, &name, 0),
                        "note": "Single SCAN cursor step — refine pattern rather than full keyspace dump"
                    }),
                ),
                Err(e) => tool_err_db(id, &e),
            }
        }
    }
}

async fn handle_describe_object(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let db_name = args.get("database").and_then(|v| v.as_str());
    let object = match args.get("object").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_err(id, "'object' is required"),
    };
    let (name, kind) = match server.resolve_target(db_name) {
        Ok(v) => v,
        Err(e) => return tool_err(id, e),
    };

    match kind {
        BackendKind::Postgres => {
            let db = server.databases.get(&name).unwrap().clone();
            match describe_pg_table(db.as_ref(), object).await {
                Ok(desc) => tool_ok(id, serde_json::to_value(desc).unwrap_or(json!({}))),
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Mongo => {
            let db = server.mongo_databases.get(&name).unwrap().clone();
            match describe_mongo_collection(db.as_ref(), object).await {
                Ok(desc) => tool_ok(id, serde_json::to_value(desc).unwrap_or(json!({}))),
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Redis => {
            let db = server.redis_databases.get(&name).unwrap().clone();
            match describe_redis_key(db.as_ref(), object).await {
                Ok(desc) => tool_ok(id, serde_json::to_value(desc).unwrap_or(json!({}))),
                Err(e) => tool_err_db(id, &e),
            }
        }
    }
}

async fn handle_sample_object(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let db_name = args.get("database").and_then(|v| v.as_str());
    let object = match args.get("object").and_then(|v| v.as_str()) {
        Some(s) if !s.is_empty() => s,
        _ => return tool_err(id, "'object' is required"),
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).clamp(1, 100))
        .unwrap_or(10);
    let (name, kind) = match server.resolve_target(db_name) {
        Ok(v) => v,
        Err(e) => return tool_err(id, e),
    };

    let start = Instant::now();
    match kind {
        BackendKind::Postgres => {
            let db = server.databases.get(&name).unwrap().clone();
            match sample_pg_table(db.as_ref(), object, limit).await {
                Ok(result) => {
                    let ms = start.elapsed().as_millis();
                    tool_ok(id, query_result_json(&result, &name, "postgres", ms))
                }
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Mongo => {
            let db = server.mongo_databases.get(&name).unwrap().clone();
            match sample_mongo_collection(db.as_ref(), object, limit).await {
                Ok(result) => {
                    let ms = start.elapsed().as_millis();
                    tool_ok(id, mongo_result_json(&result, &name, ms))
                }
                Err(e) => tool_err_db(id, &e),
            }
        }
        BackendKind::Redis => tool_err(
            id,
            "sample_object is not supported for Redis; use redis_query (GET/HGETALL/…) or describe_object",
        ),
    }
}

fn handle_resources_list(id: Value, server: &Server) -> JsonRpcResponse {
    let mut resources = Vec::new();
    for n in &server.db_names {
        resources.push(json!({
            "uri": format!("peekdb://{n}/schema"),
            "name": format!("{n} schema"),
            "description": format!("PostgreSQL tables for target '{n}'"),
            "mimeType": "application/json"
        }));
    }
    for n in &server.mongo_names {
        resources.push(json!({
            "uri": format!("peekdb://{n}/schema"),
            "name": format!("{n} schema"),
            "description": format!("MongoDB collections for target '{n}'"),
            "mimeType": "application/json"
        }));
    }
    for n in &server.redis_names {
        resources.push(json!({
            "uri": format!("peekdb://{n}/schema"),
            "name": format!("{n} schema"),
            "description": format!("Redis instance '{n}' (use tools for key SCAN)"),
            "mimeType": "application/json"
        }));
    }
    JsonRpcResponse::ok(id, json!({ "resources": resources }))
}

async fn handle_resources_read(id: Value, params: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => {
            return JsonRpcResponse::err(id, -32602, "resources/read requires 'uri'".into());
        }
    };

    // peekdb://{target}/schema or peekdb://{target}/tables/{name}
    let rest = match uri.strip_prefix("peekdb://") {
        Some(r) => r,
        None => {
            return JsonRpcResponse::err(id, -32602, format!("unsupported uri: {uri}"));
        }
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.is_empty() || parts[0].is_empty() {
        return JsonRpcResponse::err(id, -32602, format!("invalid uri: {uri}"));
    }
    let target = parts[0];
    let (name, kind) = match server.resolve_target(Some(target)) {
        Ok(v) => v,
        Err(e) => return JsonRpcResponse::err(id, -32602, e),
    };

    let body = if parts.len() == 2 && parts[1] == "schema" {
        match kind {
            BackendKind::Postgres => {
                let db = server.databases.get(&name).unwrap().clone();
                match list_pg_tables(db.as_ref()).await {
                    Ok(tables) => json!({"target": name, "backend": "postgres", "tables": tables}),
                    Err(e) => {
                        return JsonRpcResponse::err(id, -32000, e.to_string());
                    }
                }
            }
            BackendKind::Mongo => {
                let db = server.mongo_databases.get(&name).unwrap().clone();
                match list_mongo_collections(db.as_ref()).await {
                    Ok(c) => json!({"target": name, "backend": "mongo", "collections": c}),
                    Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            BackendKind::Redis => json!({
                "target": name,
                "backend": "redis",
                "note": "Use list_objects or redis_query SCAN; full keyspace is not exported as a resource"
            }),
        }
    } else if parts.len() == 3 && (parts[1] == "tables" || parts[1] == "collections") {
        let object = parts[2];
        match kind {
            BackendKind::Postgres => {
                let db = server.databases.get(&name).unwrap().clone();
                match describe_pg_table(db.as_ref(), object).await {
                    Ok(d) => serde_json::to_value(d).unwrap_or(json!({})),
                    Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            BackendKind::Mongo => {
                let db = server.mongo_databases.get(&name).unwrap().clone();
                match describe_mongo_collection(db.as_ref(), object).await {
                    Ok(d) => serde_json::to_value(d).unwrap_or(json!({})),
                    Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
            BackendKind::Redis => {
                let db = server.redis_databases.get(&name).unwrap().clone();
                match describe_redis_key(db.as_ref(), object).await {
                    Ok(d) => serde_json::to_value(d).unwrap_or(json!({})),
                    Err(e) => return JsonRpcResponse::err(id, -32000, e.to_string()),
                }
            }
        }
    } else {
        return JsonRpcResponse::err(
            id,
            -32602,
            format!("unsupported uri path: {uri} (use peekdb://target/schema or …/tables/name)"),
        );
    };

    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    JsonRpcResponse::ok(
        id,
        json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text
            }]
        }),
    )
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

    let target = db_name
        .map(|s| s.to_string())
        .or_else(|| server.db_names.first().cloned())
        .unwrap_or_else(|| "default".into());

    let start = Instant::now();
    match db.execute_query(&statement, max_rows).await {
        Ok(result) => {
            let ms = start.elapsed().as_millis();
            let text = if wants_table(&args) {
                format_result(&result)
            } else {
                serde_json::to_string_pretty(&query_result_json(&result, &target, "postgres", ms))
                    .unwrap_or_default()
            };
            JsonRpcResponse::ok(id, json!({ "content": [{"type": "text", "text": text}] }))
        }
        Err(e) => tool_err_db(id, &e),
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

    let target = db_name
        .map(|s| s.to_string())
        .or_else(|| server.mongo_names.first().cloned())
        .unwrap_or_else(|| "default".into());

    let start = Instant::now();
    match db.execute_mongo(query).await {
        Ok(result) => {
            let ms = start.elapsed().as_millis();
            let text = if wants_table(&args) {
                format_mongo_result(&result)
            } else {
                serde_json::to_string_pretty(&mongo_result_json(&result, &target, ms))
                    .unwrap_or_default()
            };
            JsonRpcResponse::ok(id, json!({ "content": [{"type": "text", "text": text}] }))
        }
        Err(e) => tool_err_db(id, &e),
    }
}

async fn handle_redis_query(id: Value, args: Value, server: &Arc<Server>) -> JsonRpcResponse {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.to_string(),
        _ => {
            return JsonRpcResponse::ok(
                id,
                json!({
                    "content": [{"type": "text", "text": "Error: 'command' is required and must be non-empty"}],
                    "isError": true
                }),
            )
        }
    };

    let db_name = args.get("database").and_then(|v| v.as_str());
    let db = match server.resolve_redis(db_name) {
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

    let cmd_args: Vec<String> = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).min(1000))
        .unwrap_or(100);

    let query = RedisQuery {
        command,
        args: cmd_args,
        limit,
    };

    let target = db_name
        .map(|s| s.to_string())
        .or_else(|| server.redis_names.first().cloned())
        .unwrap_or_else(|| "default".into());

    let start = Instant::now();
    match db.execute_redis(query).await {
        Ok(result) => {
            let ms = start.elapsed().as_millis();
            let text = if wants_table(&args) {
                format_redis_result(&result)
            } else {
                serde_json::to_string_pretty(&redis_result_json(&result, &target, ms))
                    .unwrap_or_default()
            };
            JsonRpcResponse::ok(id, json!({ "content": [{"type": "text", "text": text}] }))
        }
        Err(e) => tool_err_db(id, &e),
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

fn format_redis_result(result: &RedisResult) -> String {
    if result.values.is_empty() {
        return "(nil)".to_string();
    }
    let mut out = String::new();
    for (i, v) in result.values.iter().enumerate() {
        out.push_str(&format!("[{}] {}\n", i + 1, v));
    }
    if result.truncated {
        out.push_str(&format!(
            "\n{} value(s) shown (truncated — increase limit up to 1000)\n",
            result.count
        ));
    } else {
        out.push_str(&format!("\n{} value(s)\n", result.count));
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

#[cfg(test)]
mod tests {
    use super::{read_message, Framing};

    async fn read_all(input: &str) -> Vec<(String, Framing)> {
        let mut reader = tokio::io::BufReader::new(input.as_bytes());
        let mut out = Vec::new();
        while let Some(m) = read_message(&mut reader).await {
            out.push(m);
        }
        out
    }

    #[tokio::test]
    async fn reads_newline_delimited_messages_and_skips_blank_lines() {
        let msgs = read_all("{\"a\":1}\n\n  {\"b\":2}\r\n").await;
        assert_eq!(
            msgs,
            vec![
                ("{\"a\":1}".to_string(), Framing::Lines),
                ("{\"b\":2}".to_string(), Framing::Lines),
            ]
        );
    }

    #[tokio::test]
    async fn reads_content_length_messages() {
        let msgs = read_all("Content-Length: 7\r\n\r\n{\"a\":1}").await;
        assert_eq!(
            msgs,
            vec![("{\"a\":1}".to_string(), Framing::ContentLength)]
        );
    }
}
