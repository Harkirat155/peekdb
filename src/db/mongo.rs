use super::traits::{DbError, MongoDatabase, MongoOperation, MongoQuery, MongoResult};
use crate::policy::{
    cap_string_list_bytes, redact_json_documents, RateLimiter, TargetPolicy,
};
use async_trait::async_trait;
use futures::TryStreamExt;
use mongodb::{
    bson::{Bson, Document},
    options::{AggregateOptions, ClientOptions, FindOptions},
    Client,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;

/// Per-query `maxTimeMS` — guards against a runaway/looping agent driving an expensive
/// find/aggregate. Applied per-call via `FindOptions`/`AggregateOptions`, not at connect
/// time (Mongo has no server-side per-connection default the way Postgres does).
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 30_000;

pub struct MongoDb {
    name: String,
    uri: String,
    dbname: String,
    query_timeout_ms: u64,
    client: Mutex<Option<Client>>,
    policy: TargetPolicy,
    rate: Option<Arc<RateLimiter>>,
}

impl MongoDb {
    pub fn new(name: impl Into<String>, uri: impl Into<String>, dbname: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            uri: uri.into(),
            dbname: dbname.into(),
            query_timeout_ms: DEFAULT_QUERY_TIMEOUT_MS,
            client: Mutex::new(None),
            policy: TargetPolicy::default(),
            rate: None,
        }
    }

    /// Overrides the default 30s `maxTimeMS` for this connection's find/aggregate calls.
    pub fn with_query_timeout_ms(mut self, ms: u64) -> Self {
        self.query_timeout_ms = ms;
        self
    }

    pub fn with_safety(mut self, policy: TargetPolicy, rate: Option<Arc<RateLimiter>>) -> Self {
        self.policy = policy;
        self.rate = rate;
        self
    }

    async fn get_db(&self) -> Result<mongodb::Database, DbError> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            info!("Connecting to MongoDB '{}'...", self.name);
            let options = ClientOptions::parse(&self.uri)
                .await
                .map_err(|e| DbError::ConnectionFailed(format!("{}: {}", self.name, e)))?;
            let client = Client::with_options(options)
                .map_err(|e| DbError::ConnectionFailed(format!("{}: {}", self.name, e)))?;
            *guard = Some(client);
            info!("MongoDB '{}' client created.", self.name);
        }
        Ok(guard.as_ref().unwrap().database(&self.dbname))
    }
}

/// Parse a JSON string into a BSON Document. Empty/None → empty Document.
fn parse_doc(json: Option<&str>) -> Result<Document, DbError> {
    match json {
        None | Some("") | Some("{}") => return Ok(Document::new()),
        _ => {}
    }
    let val: serde_json::Value = serde_json::from_str(json.unwrap())
        .map_err(|e| DbError::QueryFailed(format!("JSON parse: {}", e)))?;
    let bson = mongodb::bson::to_bson(&val)
        .map_err(|e| DbError::QueryFailed(format!("BSON convert: {}", e)))?;
    match bson {
        Bson::Document(d) => Ok(d),
        _ => Err(DbError::QueryFailed(
            "Expected a JSON object ({...})".into(),
        )),
    }
}

/// Reject `$out` / `$merge` at any depth (including inside `$facet` / `$lookup` sub-pipelines).
fn check_pipeline_readonly(pipeline: &[Document]) -> Result<(), DbError> {
    for stage in pipeline {
        walk_bson_for_write_stages(&Bson::Document(stage.clone()))?;
    }
    Ok(())
}

fn walk_bson_for_write_stages(value: &Bson) -> Result<(), DbError> {
    match value {
        Bson::Document(doc) => {
            if doc.contains_key("$out") {
                return Err(DbError::BlockedKeyword {
                    keyword: "$out".into(),
                });
            }
            if doc.contains_key("$merge") {
                return Err(DbError::BlockedKeyword {
                    keyword: "$merge".into(),
                });
            }
            for (_k, v) in doc.iter() {
                walk_bson_for_write_stages(v)?;
            }
            Ok(())
        }
        Bson::Array(items) => {
            for item in items {
                walk_bson_for_write_stages(item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn bson_to_json_string(b: &Bson) -> String {
    serde_json::to_string(b).unwrap_or_else(|_| "null".to_string())
}

fn doc_to_json_string(d: &Document) -> String {
    serde_json::to_string(d).unwrap_or_else(|_| "{}".to_string())
}

#[async_trait]
impl MongoDatabase for MongoDb {
    async fn execute_mongo(&self, query: MongoQuery) -> Result<MongoResult, DbError> {
        if let Some(rate) = &self.rate {
            rate.check_call()?;
            rate.record_call();
        }
        if let Some(coll) = query.collection.as_deref() {
            self.policy.allow_objects.check(coll)?;
        }

        let db = self.get_db().await?;
        let max = query.limit;

        let require_coll = || {
            query.collection.as_deref().ok_or_else(|| {
                DbError::QueryFailed("'collection' is required for this operation".into())
            })
        };

        let mut result = match query.operation {
            MongoOperation::ListCollections => {
                let names = db
                    .list_collection_names(None)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;
                let docs: Vec<String> = names.iter().map(|n| format!("\"{}\"", n)).collect();
                let count = docs.len();
                MongoResult {
                    documents: docs,
                    count,
                    redacted: false,
                    truncated: false,
                }
            }

            MongoOperation::Count => {
                let filter = parse_doc(query.filter.as_deref())?;
                let coll = db.collection::<Document>(require_coll()?);
                let count = coll
                    .count_documents(filter, None)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;
                MongoResult {
                    documents: vec![count.to_string()],
                    count: 1,
                    redacted: false,
                    truncated: false,
                }
            }

            MongoOperation::Distinct => {
                let field = query.field.as_deref().ok_or_else(|| {
                    DbError::QueryFailed("'field' is required for distinct".into())
                })?;
                let filter = parse_doc(query.filter.as_deref())?;
                let coll = db.collection::<Document>(require_coll()?);
                let values = coll
                    .distinct(field, filter, None)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;
                let truncated = values.len() > max;
                let docs: Vec<String> = values.iter().take(max).map(bson_to_json_string).collect();
                let count = docs.len();
                MongoResult {
                    documents: docs,
                    count,
                    redacted: false,
                    truncated,
                }
            }

            MongoOperation::Find => {
                let filter = parse_doc(query.filter.as_deref())?;
                let projection = if query
                    .projection
                    .as_deref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                {
                    Some(parse_doc(query.projection.as_deref())?)
                } else {
                    None
                };
                let sort = if query
                    .sort
                    .as_deref()
                    .map(|s| !s.is_empty())
                    .unwrap_or(false)
                {
                    Some(parse_doc(query.sort.as_deref())?)
                } else {
                    None
                };

                let opts = FindOptions::builder()
                    .limit((max as i64) + 1)
                    .projection(projection)
                    .sort(sort)
                    .max_time(Duration::from_millis(self.query_timeout_ms))
                    .build();

                let mut cursor = db
                    .collection::<Document>(require_coll()?)
                    .find(filter, opts)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;

                let mut docs = Vec::new();
                let mut truncated = false;
                while let Some(doc) = cursor
                    .try_next()
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?
                {
                    if docs.len() >= max {
                        truncated = true;
                        break;
                    }
                    docs.push(doc_to_json_string(&doc));
                }
                let count = docs.len();
                MongoResult {
                    documents: docs,
                    count,
                    redacted: false,
                    truncated,
                }
            }

            MongoOperation::Aggregate => {
                let pipeline_str = query.pipeline.as_deref().ok_or_else(|| {
                    DbError::QueryFailed("'pipeline' (JSON array) is required for aggregate".into())
                })?;
                let stages: Vec<serde_json::Value> = serde_json::from_str(pipeline_str)
                    .map_err(|e| DbError::QueryFailed(format!("pipeline JSON parse: {}", e)))?;

                let pipeline: Vec<Document> = stages
                    .iter()
                    .map(|v| {
                        let bson = mongodb::bson::to_bson(v)
                            .map_err(|e| DbError::QueryFailed(format!("BSON: {}", e)))?;
                        match bson {
                            Bson::Document(d) => Ok(d),
                            _ => Err(DbError::QueryFailed(
                                "Each pipeline stage must be a JSON object".into(),
                            )),
                        }
                    })
                    .collect::<Result<_, _>>()?;

                check_pipeline_readonly(&pipeline)?;

                let agg_opts = AggregateOptions::builder()
                    .max_time(Duration::from_millis(self.query_timeout_ms))
                    .build();

                let mut cursor = db
                    .collection::<Document>(require_coll()?)
                    .aggregate(pipeline, agg_opts)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;

                let mut docs = Vec::new();
                let mut truncated = false;
                while let Some(doc) = cursor
                    .try_next()
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?
                {
                    if docs.len() >= max {
                        truncated = true;
                        break;
                    }
                    docs.push(doc_to_json_string(&doc));
                }
                let count = docs.len();
                MongoResult {
                    documents: docs,
                    count,
                    redacted: false,
                    truncated,
                }
            }

            MongoOperation::ListIndexes => {
                let mut cursor = db
                    .collection::<Document>(require_coll()?)
                    .list_indexes(None)
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?;

                let mut docs = Vec::new();
                while let Some(index) = cursor
                    .try_next()
                    .await
                    .map_err(|e| DbError::QueryFailed(e.to_string()))?
                {
                    let name = index
                        .options
                        .as_ref()
                        .and_then(|o| o.name.as_deref())
                        .unwrap_or("(unnamed)");
                    let entry = mongodb::bson::doc! {
                        "name": name,
                        "keys": index.keys,
                    };
                    docs.push(doc_to_json_string(&entry));
                }
                let count = docs.len();
                MongoResult {
                    documents: docs,
                    count,
                    redacted: false,
                    truncated: false,
                }
            }

        };

        // Allowlist filters list_collections output (discovery inherits execute path).
        if matches!(query.operation, MongoOperation::ListCollections)
            && !self.policy.allow_objects.is_unrestricted()
        {
            let names: Vec<String> = result
                .documents
                .iter()
                .map(|s| s.trim_matches('"').to_string())
                .collect();
            let filtered = self.policy.allow_objects.filter_list(names);
            result.documents = filtered.iter().map(|n| format!("\"{n}\"")).collect();
            result.count = result.documents.len();
        }

        if let Some(rate) = &self.rate {
            rate.check_and_record_rows(result.count as u64)?;
        }
        if redact_json_documents(&mut result.documents, &self.policy.redact) {
            result.redacted = true;
        }
        if let Some(max_b) = self.policy.max_response_bytes {
            if cap_string_list_bytes(&mut result.documents, max_b)? {
                result.truncated = true;
                result.count = result.documents.len();
            }
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use mongodb::bson::doc;

    #[test]
    fn parse_doc_none_and_empty_variants_yield_empty_document() {
        assert_eq!(parse_doc(None).unwrap(), Document::new());
        assert_eq!(parse_doc(Some("")).unwrap(), Document::new());
        assert_eq!(parse_doc(Some("{}")).unwrap(), Document::new());
    }

    #[test]
    fn parse_doc_parses_a_json_object() {
        let d = parse_doc(Some(r#"{"status":"active","count":3}"#)).unwrap();
        assert_eq!(d.get_str("status").unwrap(), "active");
        let count = d.get("count").unwrap();
        assert_eq!(
            count.as_i32().map(i64::from).or_else(|| count.as_i64()),
            Some(3)
        );
    }

    #[test]
    fn parse_doc_rejects_non_object_json() {
        assert!(parse_doc(Some("[1,2,3]")).is_err());
        assert!(parse_doc(Some("not json")).is_err());
    }

    #[test]
    fn pipeline_without_out_or_merge_is_allowed() {
        let pipeline = vec![doc! {"$match": {"status": "active"}}, doc! {"$limit": 10}];
        assert!(check_pipeline_readonly(&pipeline).is_ok());
    }

    #[test]
    fn pipeline_with_out_stage_is_blocked() {
        let pipeline = vec![doc! {"$match": {}}, doc! {"$out": "other_collection"}];
        match check_pipeline_readonly(&pipeline) {
            Err(DbError::BlockedKeyword { keyword }) => assert_eq!(keyword, "$out"),
            other => panic!("expected $out to be blocked, got {other:?}"),
        }
    }

    #[test]
    fn pipeline_with_merge_stage_is_blocked() {
        let pipeline = vec![doc! {"$merge": {"into": "other_collection"}}];
        match check_pipeline_readonly(&pipeline) {
            Err(DbError::BlockedKeyword { keyword }) => assert_eq!(keyword, "$merge"),
            other => panic!("expected $merge to be blocked, got {other:?}"),
        }
    }

    #[test]
    fn nested_out_inside_facet_is_blocked() {
        let pipeline = vec![doc! {
            "$facet": {
                "sneaky": [{"$out": "other_collection"}]
            }
        }];
        match check_pipeline_readonly(&pipeline) {
            Err(DbError::BlockedKeyword { keyword }) => assert_eq!(keyword, "$out"),
            other => panic!("expected nested $out under $facet to be blocked, got {other:?}"),
        }
    }

    #[test]
    fn nested_merge_inside_lookup_pipeline_is_blocked() {
        let pipeline = vec![doc! {
            "$lookup": {
                "from": "other",
                "pipeline": [
                    {"$match": {}},
                    {"$merge": {"into": "sink"}}
                ],
                "as": "joined"
            }
        }];
        match check_pipeline_readonly(&pipeline) {
            Err(DbError::BlockedKeyword { keyword }) => assert_eq!(keyword, "$merge"),
            other => panic!("expected nested $merge under $lookup to be blocked, got {other:?}"),
        }
    }

    #[test]
    fn deep_nested_out_under_facet_and_lookup_is_blocked() {
        let pipeline = vec![doc! {
            "$facet": {
                "branch": [{
                    "$lookup": {
                        "from": "x",
                        "pipeline": [{"$out": "evil"}],
                        "as": "y"
                    }
                }]
            }
        }];
        match check_pipeline_readonly(&pipeline) {
            Err(DbError::BlockedKeyword { keyword }) => assert_eq!(keyword, "$out"),
            other => panic!("expected deeply nested $out to be blocked, got {other:?}"),
        }
    }
}
