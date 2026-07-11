//! Exhaustive integration tests against a REAL MongoDB container.
//!
//! Requires `docker compose -f docker-compose.test.yml up -d` (see repo root). Gated
//! `#[ignore]` so plain `cargo test` stays fast/offline; run explicitly with:
//!     cargo test --test integration_mongo -- --ignored --test-threads=1
//! (single-threaded: tests share one `widgets` collection and reseed it, so they must
//! not interleave).

use mongodb::bson::{doc, Document};
use mongodb::Client;
use peekdb::db::mongo::MongoDb;
use peekdb::db::traits::{DbError, MongoDatabase, MongoOperation, MongoQuery};

const MONGO_URI: &str = "mongodb://localhost:57017";
const MONGO_DB: &str = "testdb";

async fn raw_client() -> Client {
    Client::with_uri_str(MONGO_URI).await.expect(
        "connect to test mongo container — is `docker compose -f docker-compose.test.yml up -d` running?",
    )
}

async fn setup_widgets() {
    let client = raw_client().await;
    let db = client.database(MONGO_DB);
    let coll = db.collection::<Document>("widgets");
    coll.drop(None).await.ok();
    coll.insert_many(
        vec![
            doc! {"name": "alpha", "status": "active", "count": 1},
            doc! {"name": "beta", "status": "inactive", "count": 2},
            doc! {"name": "gamma", "status": "active", "count": 3},
        ],
        None,
    )
    .await
    .unwrap();
}

fn db() -> MongoDb {
    MongoDb::new("test", MONGO_URI, MONGO_DB)
}

fn find_query(collection: &str, filter: Option<&str>, limit: usize) -> MongoQuery {
    MongoQuery {
        collection: Some(collection.to_string()),
        operation: MongoOperation::Find,
        filter: filter.map(str::to_string),
        projection: None,
        sort: None,
        limit,
        pipeline: None,
        field: None,
    }
}

#[tokio::test]
#[ignore]
async fn find_with_no_filter_returns_all_documents() {
    setup_widgets().await;
    let result = db()
        .execute_mongo(find_query("widgets", None, 100))
        .await
        .unwrap();
    assert_eq!(result.count, 3);
    assert!(!result.truncated);
}

#[tokio::test]
#[ignore]
async fn find_with_filter_returns_matching_documents_only() {
    setup_widgets().await;
    let result = db()
        .execute_mongo(find_query("widgets", Some(r#"{"status":"active"}"#), 100))
        .await
        .unwrap();
    assert_eq!(result.count, 2);
    for doc in &result.documents {
        assert!(doc.contains("\"active\""));
    }
}

#[tokio::test]
#[ignore]
async fn find_with_sort_orders_results() {
    setup_widgets().await;
    let mut q = find_query("widgets", None, 100);
    q.sort = Some(r#"{"count":-1}"#.to_string());
    let result = db().execute_mongo(q).await.unwrap();
    assert_eq!(result.count, 3);
    // Descending by count: gamma(3), beta(2), alpha(1)
    assert!(result.documents[0].contains("gamma"));
    assert!(result.documents[2].contains("alpha"));
}

#[tokio::test]
#[ignore]
async fn find_with_projection_limits_returned_fields() {
    setup_widgets().await;
    let mut q = find_query("widgets", None, 1);
    q.projection = Some(r#"{"name":1,"_id":0}"#.to_string());
    let result = db().execute_mongo(q).await.unwrap();
    assert_eq!(result.count, 1);
    assert!(!result.documents[0].contains("status"));
    assert!(!result.documents[0].contains("_id"));
}

#[tokio::test]
#[ignore]
async fn find_limit_truncates_and_flags_truncated() {
    setup_widgets().await;
    let result = db()
        .execute_mongo(find_query("widgets", None, 2))
        .await
        .unwrap();
    assert_eq!(result.count, 2);
    assert!(result.truncated);
}

#[tokio::test]
#[ignore]
async fn count_operation_returns_matching_count() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::Count,
        filter: Some(r#"{"status":"active"}"#.to_string()),
        projection: None,
        sort: None,
        limit: 100,
        pipeline: None,
        field: None,
    };
    let result = db().execute_mongo(q).await.unwrap();
    assert_eq!(result.documents, vec!["2"]);
}

#[tokio::test]
#[ignore]
async fn distinct_operation_returns_unique_values() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::Distinct,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: None,
        field: Some("status".to_string()),
    };
    let mut result = db().execute_mongo(q).await.unwrap();
    result.documents.sort();
    assert_eq!(result.documents, vec!["\"active\"", "\"inactive\""]);
}

#[tokio::test]
#[ignore]
async fn list_collections_includes_seeded_collection() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: None,
        operation: MongoOperation::ListCollections,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: None,
        field: None,
    };
    let result = db().execute_mongo(q).await.unwrap();
    assert!(result.documents.contains(&"\"widgets\"".to_string()));
}

#[tokio::test]
#[ignore]
async fn list_indexes_includes_default_id_index() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::ListIndexes,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: None,
        field: None,
    };
    let result = db().execute_mongo(q).await.unwrap();
    assert!(result.documents.iter().any(|d| d.contains("_id_")));
}

#[tokio::test]
#[ignore]
async fn aggregate_simple_pipeline_is_allowed() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::Aggregate,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: Some(r#"[{"$match":{"status":"active"}},{"$count":"n"}]"#.to_string()),
        field: None,
    };
    let result = db().execute_mongo(q).await.unwrap();
    assert_eq!(result.count, 1);
    assert!(result.documents[0].contains("\"n\":2"));
}

#[tokio::test]
#[ignore]
async fn aggregate_with_out_stage_is_blocked_end_to_end() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::Aggregate,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: Some(r#"[{"$match":{}},{"$out":"widgets_copy"}]"#.to_string()),
        field: None,
    };
    let result = db().execute_mongo(q).await;
    assert!(matches!(result, Err(DbError::BlockedKeyword { .. })));

    // Confirm the $out never actually ran — no copy collection was created.
    let client = raw_client().await;
    let names = client
        .database(MONGO_DB)
        .list_collection_names(None)
        .await
        .unwrap();
    assert!(!names.contains(&"widgets_copy".to_string()));
}

#[tokio::test]
#[ignore]
async fn aggregate_with_merge_stage_is_blocked_end_to_end() {
    setup_widgets().await;
    let q = MongoQuery {
        collection: Some("widgets".to_string()),
        operation: MongoOperation::Aggregate,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: Some(r#"[{"$merge":{"into":"widgets_copy"}}]"#.to_string()),
        field: None,
    };
    let result = db().execute_mongo(q).await;
    assert!(matches!(result, Err(DbError::BlockedKeyword { .. })));
}

#[tokio::test]
#[ignore]
async fn find_without_collection_fails_with_clear_error() {
    let q = MongoQuery {
        collection: None,
        operation: MongoOperation::Find,
        filter: None,
        projection: None,
        sort: None,
        limit: 100,
        pipeline: None,
        field: None,
    };
    let result = db().execute_mongo(q).await;
    assert!(matches!(result, Err(DbError::QueryFailed(msg)) if msg.contains("collection")));
}
