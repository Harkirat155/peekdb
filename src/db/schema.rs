//! Schema discovery helpers built on top of the existing read-only execute paths.
//! Using `execute_*` means list/describe/sample inherit allowlists, timeouts, and audit
//! automatically — no parallel unguarded connection path.

use super::traits::{
    Database, DbError, MongoDatabase, MongoOperation, MongoQuery, MongoResult, QueryResult,
    RedisDatabase, RedisQuery, RedisResult,
};
use serde::Serialize;
use serde_json::{json, Value};

/// Safe SQL identifier: `name` or `schema.name`, each part `[A-Za-z_][A-Za-z0-9_]*`.
pub fn validate_sql_ident(name: &str) -> Result<(), DbError> {
    if name.is_empty() {
        return Err(DbError::QueryFailed("empty identifier".into()));
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() > 2 {
        return Err(DbError::QueryFailed(
            "identifier must be table or schema.table".into(),
        ));
    }
    for part in parts {
        let mut chars = part.chars();
        let Some(first) = chars.next() else {
            return Err(DbError::QueryFailed("empty identifier part".into()));
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            return Err(DbError::QueryFailed(format!(
                "invalid identifier '{name}': each part must start with a letter or underscore"
            )));
        }
        if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(DbError::QueryFailed(format!(
                "invalid identifier '{name}': only letters, digits, underscore allowed"
            )));
        }
    }
    Ok(())
}

/// Quote a validated SQL identifier for Postgres.
pub fn quote_sql_ident(name: &str) -> Result<String, DbError> {
    validate_sql_ident(name)?;
    Ok(name
        .split('.')
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join("."))
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObjectDescription {
    pub name: String,
    pub kind: String,
    pub backend: &'static str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub columns: Vec<ColumnInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<IndexInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

// ── PostgreSQL ────────────────────────────────────────────────────────────────

pub async fn list_pg_tables(db: &dyn Database) -> Result<Vec<String>, DbError> {
    let sql = "SELECT table_schema || '.' || table_name AS qname \
               FROM information_schema.tables \
               WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
               ORDER BY 1";
    let result = db.execute_query(sql, 2000).await?;
    let names: Vec<String> = result
        .rows
        .into_iter()
        .filter_map(|r| r.first().cloned().flatten())
        .collect();
    // Allowlist filters discovery results (catalog SQL itself is exempt in check_sql_allowlist).
    Ok(names
        .into_iter()
        .filter(|n| db.check_object_access(n).is_ok())
        .collect())
}

pub async fn describe_pg_table(
    db: &dyn Database,
    table: &str,
) -> Result<ObjectDescription, DbError> {
    validate_sql_ident(table)?;
    db.check_object_access(table)?;

    let (schema_filter, name_filter) = match table.split_once('.') {
        Some((s, t)) => (s.to_string(), t.to_string()),
        None => ("public".to_string(), table.to_string()),
    };

    // Parameters via string interpolation after validate_sql_ident (safe charset only).
    let col_sql = format!(
        "SELECT column_name, data_type, is_nullable, column_default \
         FROM information_schema.columns \
         WHERE table_schema = '{schema_filter}' AND table_name = '{name_filter}' \
         ORDER BY ordinal_position"
    );
    let cols = db.execute_query(&col_sql, 500).await?;
    if cols.row_count == 0 {
        return Err(DbError::QueryFailed(format!(
            "table or view not found: {table}"
        )));
    }

    let columns: Vec<ColumnInfo> = cols
        .rows
        .iter()
        .map(|row| ColumnInfo {
            name: row.first().and_then(|c| c.clone()).unwrap_or_default(),
            data_type: row.get(1).and_then(|c| c.clone()).unwrap_or_default(),
            nullable: row
                .get(2)
                .and_then(|c| c.as_ref())
                .map(|s| s.eq_ignore_ascii_case("YES")),
            default: row.get(3).and_then(|c| c.clone()),
        })
        .collect();

    let idx_sql = format!(
        "SELECT indexname, indexdef FROM pg_indexes \
         WHERE schemaname = '{schema_filter}' AND tablename = '{name_filter}' \
         ORDER BY 1"
    );
    let idx = db.execute_query(&idx_sql, 200).await?;
    let indexes: Vec<IndexInfo> = idx
        .rows
        .iter()
        .map(|row| IndexInfo {
            name: row.first().and_then(|c| c.clone()).unwrap_or_default(),
            definition: row.get(1).and_then(|c| c.clone()),
        })
        .collect();

    Ok(ObjectDescription {
        name: if table.contains('.') {
            table.to_string()
        } else {
            format!("public.{table}")
        },
        kind: "table".into(),
        backend: "postgres",
        columns,
        indexes,
        extra: None,
    })
}

pub async fn sample_pg_table(
    db: &dyn Database,
    table: &str,
    limit: usize,
) -> Result<QueryResult, DbError> {
    validate_sql_ident(table)?;
    db.check_object_access(table)?;
    let quoted = quote_sql_ident(table)?;
    let lim = limit.clamp(1, 100);
    let sql = format!("SELECT * FROM {quoted} LIMIT {lim}");
    db.execute_query(&sql, lim).await
}

// ── MongoDB ───────────────────────────────────────────────────────────────────

pub async fn list_mongo_collections(db: &dyn MongoDatabase) -> Result<Vec<String>, DbError> {
    let result = db
        .execute_mongo(MongoQuery {
            collection: None,
            operation: MongoOperation::ListCollections,
            filter: None,
            projection: None,
            sort: None,
            limit: 1000,
            pipeline: None,
            field: None,
        })
        .await?;
    // execute_mongo already filters list_collections when allowlist is set; re-check for safety.
    Ok(result
        .documents
        .into_iter()
        .map(|s| s.trim_matches('"').to_string())
        .filter(|n| db.check_object_access(n).is_ok())
        .collect())
}

pub async fn describe_mongo_collection(
    db: &dyn MongoDatabase,
    collection: &str,
) -> Result<ObjectDescription, DbError> {
    if collection.is_empty() {
        return Err(DbError::QueryFailed("collection is required".into()));
    }
    db.check_object_access(collection)?;

    let sample = db
        .execute_mongo(MongoQuery {
            collection: Some(collection.to_string()),
            operation: MongoOperation::Find,
            filter: None,
            projection: None,
            sort: None,
            limit: 5,
            pipeline: None,
            field: None,
        })
        .await?;

    let indexes = db
        .execute_mongo(MongoQuery {
            collection: Some(collection.to_string()),
            operation: MongoOperation::ListIndexes,
            filter: None,
            projection: None,
            sort: None,
            limit: 100,
            pipeline: None,
            field: None,
        })
        .await?;

    // Infer field names from sample docs (shallow keys only).
    let mut fields: Vec<String> = Vec::new();
    for doc in &sample.documents {
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(doc) {
            for k in map.keys() {
                if !fields.contains(k) {
                    fields.push(k.clone());
                }
            }
        }
    }
    fields.sort();

    let columns: Vec<ColumnInfo> = fields
        .into_iter()
        .map(|name| ColumnInfo {
            name,
            data_type: "mixed".into(),
            nullable: None,
            default: None,
        })
        .collect();

    let index_infos: Vec<IndexInfo> = indexes
        .documents
        .iter()
        .filter_map(|d| {
            let v: Value = serde_json::from_str(d).ok()?;
            Some(IndexInfo {
                name: v
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("(index)")
                    .to_string(),
                definition: Some(d.clone()),
            })
        })
        .collect();

    Ok(ObjectDescription {
        name: collection.to_string(),
        kind: "collection".into(),
        backend: "mongo",
        columns,
        indexes: index_infos,
        extra: Some(json!({
            "sample_docs": sample.count,
            "note": "columns inferred from up to 5 sample documents (shallow keys)"
        })),
    })
}

pub async fn sample_mongo_collection(
    db: &dyn MongoDatabase,
    collection: &str,
    limit: usize,
) -> Result<MongoResult, DbError> {
    if collection.is_empty() {
        return Err(DbError::QueryFailed("collection is required".into()));
    }
    db.check_object_access(collection)?;
    db.execute_mongo(MongoQuery {
        collection: Some(collection.to_string()),
        operation: MongoOperation::Find,
        filter: None,
        projection: None,
        sort: None,
        limit: limit.clamp(1, 100),
        pipeline: None,
        field: None,
    })
    .await
}

// ── Redis ─────────────────────────────────────────────────────────────────────

pub async fn scan_redis_keys(
    db: &dyn RedisDatabase,
    pattern: &str,
    limit: usize,
) -> Result<RedisResult, DbError> {
    let match_pat = if pattern.is_empty() { "*" } else { pattern };
    // Single SCAN iteration with COUNT; agents can refine the pattern.
    // Full keyspace walks are intentionally not automated.
    let count = limit.clamp(1, 200).to_string();
    db.execute_redis(RedisQuery {
        command: "SCAN".into(),
        args: vec![
            "0".into(),
            "MATCH".into(),
            match_pat.into(),
            "COUNT".into(),
            count,
        ],
        limit: limit.clamp(1, 200),
    })
    .await
}

pub async fn describe_redis_key(
    db: &dyn RedisDatabase,
    key: &str,
) -> Result<ObjectDescription, DbError> {
    if key.is_empty() {
        return Err(DbError::QueryFailed("key is required".into()));
    }
    let typ = db
        .execute_redis(RedisQuery {
            command: "TYPE".into(),
            args: vec![key.to_string()],
            limit: 1,
        })
        .await?;
    let ttl = db
        .execute_redis(RedisQuery {
            command: "TTL".into(),
            args: vec![key.to_string()],
            limit: 1,
        })
        .await?;
    let type_s = typ
        .values
        .first()
        .cloned()
        .unwrap_or_else(|| "\"none\"".into());
    let ttl_s = ttl.values.first().cloned().unwrap_or_else(|| "null".into());

    Ok(ObjectDescription {
        name: key.to_string(),
        kind: "key".into(),
        backend: "redis",
        columns: vec![],
        indexes: vec![],
        extra: Some(json!({
            "type": serde_json::from_str::<Value>(&type_s).unwrap_or(Value::String(type_s)),
            "ttl": serde_json::from_str::<Value>(&ttl_s).unwrap_or(Value::String(ttl_s)),
        })),
    })
}

// ── MCP result envelopes ──────────────────────────────────────────────────────

/// Optional pagination fields attached to MCP query envelopes.
#[derive(Debug, Clone, Default)]
pub struct PageMeta {
    pub offset: Option<usize>,
    pub total_buffered: Option<usize>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
    pub source_truncated: Option<bool>,
    pub cached: bool,
}

pub fn query_result_json(
    result: &QueryResult,
    target: &str,
    backend: &str,
    duration_ms: u128,
) -> Value {
    query_result_json_with_page(result, target, backend, duration_ms, &PageMeta::default())
}

pub fn query_result_json_with_page(
    result: &QueryResult,
    target: &str,
    backend: &str,
    duration_ms: u128,
    page: &PageMeta,
) -> Value {
    let rows: Vec<Value> = result
        .rows
        .iter()
        .map(|row| {
            Value::Array(
                row.iter()
                    .map(|c| match c {
                        Some(s) => Value::String(s.clone()),
                        None => Value::Null,
                    })
                    .collect(),
            )
        })
        .collect();
    let mut meta = json!({
        "target": target,
        "backend": backend,
        "row_count": result.row_count,
        "truncated": result.truncated,
        "duration_ms": duration_ms
    });
    attach_page_meta(&mut meta, page);
    json!({
        "columns": result.columns,
        "rows": rows,
        "meta": meta
    })
}

pub fn mongo_result_json(result: &MongoResult, target: &str, duration_ms: u128) -> Value {
    mongo_result_json_with_page(result, target, duration_ms, &PageMeta::default())
}

pub fn mongo_result_json_with_page(
    result: &MongoResult,
    target: &str,
    duration_ms: u128,
    page: &PageMeta,
) -> Value {
    let docs: Vec<Value> = result
        .documents
        .iter()
        .filter_map(|s| serde_json::from_str(s).ok())
        .collect();
    let mut meta = json!({
        "target": target,
        "backend": "mongo",
        "row_count": result.count,
        "truncated": result.truncated,
        "duration_ms": duration_ms
    });
    attach_page_meta(&mut meta, page);
    json!({
        "documents": docs,
        "meta": meta
    })
}

pub fn redis_result_json(result: &RedisResult, target: &str, duration_ms: u128) -> Value {
    redis_result_json_with_page(result, target, duration_ms, &PageMeta::default())
}

pub fn redis_result_json_with_page(
    result: &RedisResult,
    target: &str,
    duration_ms: u128,
    page: &PageMeta,
) -> Value {
    let values: Vec<Value> = result
        .values
        .iter()
        .map(|s| serde_json::from_str(s).unwrap_or(Value::String(s.clone())))
        .collect();
    let mut meta = json!({
        "target": target,
        "backend": "redis",
        "row_count": result.count,
        "truncated": result.truncated,
        "duration_ms": duration_ms
    });
    attach_page_meta(&mut meta, page);
    json!({
        "values": values,
        "meta": meta
    })
}

fn attach_page_meta(meta: &mut Value, page: &PageMeta) {
    let obj = meta.as_object_mut().expect("meta object");
    if let Some(o) = page.offset {
        obj.insert("offset".into(), json!(o));
    }
    if let Some(t) = page.total_buffered {
        obj.insert("total_buffered".into(), json!(t));
    }
    if let Some(ref c) = page.next_cursor {
        obj.insert("next_cursor".into(), json!(c));
    }
    if page.has_more {
        obj.insert("has_more".into(), json!(true));
    }
    if let Some(st) = page.source_truncated {
        obj.insert("source_truncated".into(), json!(st));
    }
    if page.cached {
        obj.insert("cached".into(), json!(true));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ObjectAllowlist;
    use async_trait::async_trait;

    #[test]
    fn validate_sql_ident_accepts_simple_and_qualified() {
        assert!(validate_sql_ident("users").is_ok());
        assert!(validate_sql_ident("public.users").is_ok());
        assert!(validate_sql_ident("_private").is_ok());
    }

    #[test]
    fn validate_sql_ident_rejects_injection_shapes() {
        assert!(validate_sql_ident("users; drop").is_err());
        assert!(validate_sql_ident("users--").is_err());
        assert!(validate_sql_ident("a.b.c").is_err());
        assert!(validate_sql_ident("").is_err());
        assert!(validate_sql_ident("1users").is_err());
    }

    #[test]
    fn quote_sql_ident_wraps_parts() {
        assert_eq!(quote_sql_ident("users").unwrap(), "\"users\"");
        assert_eq!(
            quote_sql_ident("public.users").unwrap(),
            "\"public\".\"users\""
        );
    }

    /// Fake DB that returns a fixed table list and enforces an allowlist on access checks.
    struct FakeListDb {
        allow: ObjectAllowlist,
        tables: Vec<String>,
    }

    #[async_trait]
    impl Database for FakeListDb {
        fn check_object_access(&self, name: &str) -> Result<(), DbError> {
            self.allow.check(name)
        }

        async fn execute_query(
            &self,
            _sql: &str,
            _max_rows: usize,
        ) -> Result<QueryResult, DbError> {
            Ok(QueryResult {
                columns: vec!["qname".into()],
                rows: self.tables.iter().map(|t| vec![Some(t.clone())]).collect(),
                row_count: self.tables.len(),
                truncated: false,
                command_tag: None,
                redacted: false,
            })
        }
    }

    #[tokio::test]
    async fn list_pg_tables_filters_to_allowlist() {
        let db = FakeListDb {
            allow: ObjectAllowlist::new(vec!["users", "public.orders"]),
            tables: vec![
                "public.users".into(),
                "public.orders".into(),
                "public.secrets".into(),
            ],
        };
        let listed = list_pg_tables(&db).await.unwrap();
        assert!(listed.iter().any(|t| t == "public.users"));
        assert!(listed.iter().any(|t| t == "public.orders"));
        assert!(!listed.iter().any(|t| t.contains("secrets")));
    }

    #[tokio::test]
    async fn describe_pg_table_denied_object_before_catalog_query() {
        let db = FakeListDb {
            allow: ObjectAllowlist::new(vec!["users"]),
            tables: vec![],
        };
        let err = describe_pg_table(&db, "secrets").await.unwrap_err();
        assert!(
            matches!(err, DbError::NotAllowedObject { ref name } if name == "secrets"),
            "got {err:?}"
        );
    }
}
