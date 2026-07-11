//! End-to-end tests for Phase 2 safety + Phase 3 CLI output/schema paths against
//! the real docker-compose.test.yml containers.
//!
//!     cargo test --test integration_phase2_3 -- --ignored --test-threads=1

use peekdb::cli::{self, OutputFormat};
use peekdb::db::mongo::MongoDb;
use peekdb::db::postgres::PostgresDb;
use peekdb::db::redis::RedisDb;
use peekdb::db::schema::{
    describe_mongo_collection, describe_pg_table, list_mongo_collections, list_pg_tables,
    sample_mongo_collection, sample_pg_table,
};
use peekdb::db::traits::{
    Database, DbError, MongoDatabase, MongoOperation, MongoQuery, RedisDatabase, RedisQuery,
};
use peekdb::policy::{ObjectAllowlist, RateLimiter, RedactRules, TargetPolicy};
use std::sync::Arc;

const PG_URL: &str = "host=localhost port=55432 user=testuser password=testpass dbname=testdb";
const MONGO_URI: &str = "mongodb://localhost:57017";
const MONGO_DB: &str = "testdb";
const REDIS_URL: &str = "redis://localhost:56379";

async fn seed_pg() {
    let (client, connection) = tokio_postgres::connect(PG_URL, tokio_postgres::NoTls)
        .await
        .expect("postgres up?");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
        .batch_execute(
            r#"
            DROP TABLE IF EXISTS peeks_users;
            DROP TABLE IF EXISTS peeks_secrets;
            CREATE TABLE peeks_users (id serial primary key, email text, name text);
            CREATE TABLE peeks_secrets (id serial primary key, ssn text);
            INSERT INTO peeks_users (email, name) VALUES ('a@x.com', 'alice'), ('b@x.com', 'bob');
            INSERT INTO peeks_secrets (ssn) VALUES ('111-22-3333');
            "#,
        )
        .await
        .expect("seed pg");
}

async fn seed_mongo() {
    let client = mongodb::Client::with_uri_str(MONGO_URI)
        .await
        .expect("mongo up?");
    let db = client.database(MONGO_DB);
    let users = db.collection::<mongodb::bson::Document>("peeks_users");
    let secrets = db.collection::<mongodb::bson::Document>("peeks_secrets");
    users.drop(None).await.ok();
    secrets.drop(None).await.ok();
    users
        .insert_many(
            vec![
                mongodb::bson::doc! {"email": "a@x.com", "name": "alice"},
                mongodb::bson::doc! {"email": "b@x.com", "name": "bob"},
            ],
            None,
        )
        .await
        .unwrap();
    secrets
        .insert_one(mongodb::bson::doc! {"ssn": "111-22-3333"}, None)
        .await
        .unwrap();
}

async fn seed_redis() {
    let client = redis::Client::open(REDIS_URL).expect("redis url");
    let mut conn = client.get_connection_manager().await.expect("redis up?");
    let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();
    let _: () = redis::cmd("SET")
        .arg("session:abc")
        .arg("ok")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("SET")
        .arg("secret:key")
        .arg("nope")
        .query_async(&mut conn)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore]
async fn pg_allowlist_blocks_secrets_and_allows_users() {
    seed_pg().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["peeks_users"]),
        ..Default::default()
    };
    let db = PostgresDb::new("t", PG_URL, true).with_safety(policy, None);

    let ok = db
        .execute_query("SELECT name FROM peeks_users ORDER BY id", 10)
        .await
        .unwrap();
    assert_eq!(ok.row_count, 2);

    let err = db
        .execute_query("SELECT * FROM peeks_secrets", 10)
        .await
        .unwrap_err();
    assert!(
        matches!(err, DbError::NotAllowedObject { ref name } if name == "peeks_secrets"),
        "got {err:?}"
    );
}

#[tokio::test]
#[ignore]
async fn pg_discovery_under_allowlist_lists_only_allowed() {
    seed_pg().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["peeks_users"]),
        ..Default::default()
    };
    let db = PostgresDb::new("t", PG_URL, true).with_safety(policy, None);

    // Catalog list SQL must work (not NotAllowedObject on information_schema).
    let tables = list_pg_tables(&db)
        .await
        .expect("list tables under allowlist");
    assert!(
        tables.iter().any(|t| t.contains("peeks_users")),
        "expected peeks_users in {tables:?}"
    );
    assert!(
        !tables.iter().any(|t| t.contains("peeks_secrets")),
        "secrets must be filtered out of list: {tables:?}"
    );

    describe_pg_table(&db, "peeks_users")
        .await
        .expect("describe allowed table");
    let denied = describe_pg_table(&db, "peeks_secrets").await.unwrap_err();
    assert!(matches!(denied, DbError::NotAllowedObject { .. }));
}

#[tokio::test]
#[ignore]
async fn pg_redaction_masks_email_column() {
    seed_pg().await;
    let policy = TargetPolicy {
        redact: RedactRules {
            columns: vec!["email".into()],
            patterns: vec![],
        },
        ..Default::default()
    };
    let db = PostgresDb::new("t", PG_URL, true).with_safety(policy, None);
    let result = db
        .execute_query("SELECT id, email, name FROM peeks_users ORDER BY id", 10)
        .await
        .unwrap();
    assert!(result.redacted);
    // email column index 1
    assert_eq!(result.rows[0][1].as_deref(), Some("[REDACTED]"));
    assert_eq!(result.rows[0][2].as_deref(), Some("alice"));
}

#[tokio::test]
#[ignore]
async fn pg_rate_limit_blocks_second_call_live() {
    seed_pg().await;
    let lim = Arc::new(RateLimiter::new(Some(1), None).unwrap());
    let db = PostgresDb::new("t", PG_URL, true).with_safety(TargetPolicy::default(), Some(lim));
    db.execute_query("SELECT 1", 1).await.unwrap();
    let err = db.execute_query("SELECT 1", 1).await.unwrap_err();
    assert!(matches!(err, DbError::RateLimited(_)), "got {err:?}");
}

#[tokio::test]
#[ignore]
async fn pg_sample_and_formats_smoke() {
    seed_pg().await;
    let db = PostgresDb::new("t", PG_URL, true);
    let sample = sample_pg_table(&db, "peeks_users", 5).await.unwrap();
    assert!(sample.row_count >= 1);
    // formats must not panic
    cli::print_pg_result(&sample, OutputFormat::Table);
    cli::print_pg_result(&sample, OutputFormat::Vertical);
    cli::print_pg_result(&sample, OutputFormat::Csv);
    cli::print_pg_result(&sample, OutputFormat::Tsv);
    cli::print_pg_result(&sample, OutputFormat::Json);
}

#[tokio::test]
#[ignore]
async fn mongo_nested_out_in_facet_blocked_live() {
    seed_mongo().await;
    let db = MongoDb::new("t", format!("{MONGO_URI}/{MONGO_DB}"), MONGO_DB);
    let q = MongoQuery {
        collection: Some("peeks_users".into()),
        operation: MongoOperation::Aggregate,
        filter: None,
        projection: None,
        sort: None,
        limit: 10,
        pipeline: Some(r#"[{"$facet":{"sneaky":[{"$out":"peeks_evil"}]}}]"#.into()),
        field: None,
    };
    let err = db.execute_mongo(q).await.unwrap_err();
    assert!(
        matches!(err, DbError::BlockedKeyword { ref keyword } if keyword == "$out"),
        "got {err:?}"
    );
}

#[tokio::test]
#[ignore]
async fn mongo_lookup_denied_collection_blocked_live() {
    seed_mongo().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["peeks_users"]),
        ..Default::default()
    };
    let db =
        MongoDb::new("t", format!("{MONGO_URI}/{MONGO_DB}"), MONGO_DB).with_safety(policy, None);
    let q = MongoQuery {
        collection: Some("peeks_users".into()),
        operation: MongoOperation::Aggregate,
        filter: None,
        projection: None,
        sort: None,
        limit: 10,
        pipeline: Some(
            r#"[{"$lookup":{"from":"peeks_secrets","localField":"email","foreignField":"ssn","as":"x"}}]"#.into(),
        ),
        field: None,
    };
    let err = db.execute_mongo(q).await.unwrap_err();
    assert!(
        matches!(err, DbError::NotAllowedObject { ref name } if name == "peeks_secrets"),
        "got {err:?}"
    );
}

#[tokio::test]
#[ignore]
async fn mongo_list_and_sample_under_allowlist() {
    seed_mongo().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["peeks_users"]),
        ..Default::default()
    };
    let db =
        MongoDb::new("t", format!("{MONGO_URI}/{MONGO_DB}"), MONGO_DB).with_safety(policy, None);

    let colls = list_mongo_collections(&db).await.unwrap();
    assert!(colls.iter().any(|c| c == "peeks_users"));
    assert!(!colls.iter().any(|c| c == "peeks_secrets"));

    let sample = sample_mongo_collection(&db, "peeks_users", 5)
        .await
        .unwrap();
    assert!(sample.count >= 1);

    let denied = describe_mongo_collection(&db, "peeks_secrets")
        .await
        .unwrap_err();
    assert!(matches!(denied, DbError::NotAllowedObject { .. }));
}

#[tokio::test]
#[ignore]
async fn mongo_redaction_masks_email_field() {
    seed_mongo().await;
    let policy = TargetPolicy {
        redact: RedactRules {
            columns: vec!["email".into()],
            patterns: vec![],
        },
        ..Default::default()
    };
    let db =
        MongoDb::new("t", format!("{MONGO_URI}/{MONGO_DB}"), MONGO_DB).with_safety(policy, None);
    let result = db
        .execute_mongo(MongoQuery {
            collection: Some("peeks_users".into()),
            operation: MongoOperation::Find,
            filter: None,
            projection: None,
            sort: None,
            limit: 10,
            pipeline: None,
            field: None,
        })
        .await
        .unwrap();
    assert!(result.redacted);
    assert!(result.documents.iter().any(|d| d.contains("[REDACTED]")));
    assert!(!result.documents.iter().any(|d| d.contains("a@x.com")));
}

#[tokio::test]
#[ignore]
async fn redis_key_allowlist_and_scan_prefix() {
    seed_redis().await;
    let policy = TargetPolicy {
        allow_objects: ObjectAllowlist::new(vec!["session:*"]),
        ..Default::default()
    };
    let db = RedisDb::new("t", REDIS_URL).with_safety(policy, None);

    let ok = db
        .execute_redis(RedisQuery {
            command: "GET".into(),
            args: vec!["session:abc".into()],
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(ok.count, 1);

    let err = db
        .execute_redis(RedisQuery {
            command: "GET".into(),
            args: vec!["secret:key".into()],
            limit: 10,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, DbError::NotAllowedObject { .. }),
        "got {err:?}"
    );
}
