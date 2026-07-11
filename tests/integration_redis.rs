//! Exhaustive integration tests against a REAL Redis container.
//!
//! Requires `docker compose -f docker-compose.test.yml up -d` (see repo root). Gated
//! `#[ignore]` so plain `cargo test` stays fast/offline; run explicitly with:
//!     cargo test --test integration_redis -- --ignored --test-threads=1
//! (single-threaded: tests share one keyspace and reseed it via FLUSHDB, so they must
//! not interleave).

use peekdb::db::redis::RedisDb;
use peekdb::db::traits::{DbError, RedisDatabase, RedisQuery};

const REDIS_URL: &str = "redis://localhost:56379";

async fn raw_conn() -> redis::aio::ConnectionManager {
    let client = redis::Client::open(REDIS_URL).expect("parse REDIS_URL");
    client.get_connection_manager().await.expect(
        "connect to test redis container — is `docker compose -f docker-compose.test.yml up -d` running?",
    )
}

async fn setup_widgets() {
    let mut conn = raw_conn().await;
    let _: () = redis::cmd("FLUSHDB").query_async(&mut conn).await.unwrap();
    let _: () = redis::cmd("SET")
        .arg("widgets:alpha")
        .arg("1")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("SET")
        .arg("widgets:beta")
        .arg("2")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("HSET")
        .arg("widgets:hash")
        .arg("name")
        .arg("gamma")
        .arg("count")
        .arg("3")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("SADD")
        .arg("widgets:set")
        .arg("a")
        .arg("b")
        .arg("c")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("ZADD")
        .arg("widgets:zset")
        .arg("1")
        .arg("a")
        .arg("2")
        .arg("b")
        .query_async(&mut conn)
        .await
        .unwrap();
    let _: () = redis::cmd("RPUSH")
        .arg("widgets:list")
        .arg("x")
        .arg("y")
        .arg("z")
        .query_async(&mut conn)
        .await
        .unwrap();
}

fn db() -> RedisDb {
    RedisDb::new("test", REDIS_URL)
}

fn q(command: &str, args: &[&str]) -> RedisQuery {
    RedisQuery {
        command: command.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        limit: 100,
    }
}

#[tokio::test]
#[ignore]
async fn get_returns_the_stored_value() {
    // Values are always rendered as JSON text (matching Mongo's per-document JSON
    // strings), so a plain string value comes back quoted.
    setup_widgets().await;
    let result = db()
        .execute_redis(q("GET", &["widgets:alpha"]))
        .await
        .unwrap();
    assert_eq!(result.values, vec!["\"1\"".to_string()]);
}

#[tokio::test]
#[ignore]
async fn get_is_case_insensitive() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("get", &["widgets:alpha"]))
        .await
        .unwrap();
    assert_eq!(result.values, vec!["\"1\"".to_string()]);
}

#[tokio::test]
#[ignore]
async fn mget_returns_multiple_values() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("MGET", &["widgets:alpha", "widgets:beta"]))
        .await
        .unwrap();
    assert_eq!(
        result.values,
        vec!["\"1\"".to_string(), "\"2\"".to_string()]
    );
}

#[tokio::test]
#[ignore]
async fn get_of_missing_key_returns_nil() {
    // A missing key is one result whose value is JSON null, not zero results — GET is a
    // scalar command, not a collection.
    setup_widgets().await;
    let result = db()
        .execute_redis(q("GET", &["widgets:does-not-exist"]))
        .await
        .unwrap();
    assert_eq!(result.count, 1);
    assert_eq!(result.values, vec!["null".to_string()]);
}

#[tokio::test]
#[ignore]
async fn hgetall_returns_hash_fields_as_a_single_object_not_flattened() {
    // Under RESP2 (redis-rs's default), HGETALL replies as a flat alternating array —
    // this must be paired back into one object, not flattened into 4 separate items.
    setup_widgets().await;
    let result = db()
        .execute_redis(q("HGETALL", &["widgets:hash"]))
        .await
        .unwrap();
    assert_eq!(result.count, 1);
    let parsed: serde_json::Value = serde_json::from_str(&result.values[0]).unwrap();
    assert_eq!(parsed["name"], "gamma");
    assert_eq!(parsed["count"], "3");
}

#[tokio::test]
#[ignore]
async fn hget_returns_a_single_field() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("HGET", &["widgets:hash", "name"]))
        .await
        .unwrap();
    assert_eq!(result.values, vec!["\"gamma\"".to_string()]);
}

#[tokio::test]
#[ignore]
async fn smembers_returns_all_set_members() {
    setup_widgets().await;
    let mut result = db()
        .execute_redis(q("SMEMBERS", &["widgets:set"]))
        .await
        .unwrap();
    result.values.sort();
    assert_eq!(
        result.values,
        vec![
            "\"a\"".to_string(),
            "\"b\"".to_string(),
            "\"c\"".to_string()
        ]
    );
}

#[tokio::test]
#[ignore]
async fn zrange_returns_sorted_set_members() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("ZRANGE", &["widgets:zset", "0", "-1"]))
        .await
        .unwrap();
    assert_eq!(
        result.values,
        vec!["\"a\"".to_string(), "\"b\"".to_string()]
    );
}

#[tokio::test]
#[ignore]
async fn lrange_returns_list_elements_in_order() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("LRANGE", &["widgets:list", "0", "-1"]))
        .await
        .unwrap();
    assert_eq!(
        result.values,
        vec![
            "\"x\"".to_string(),
            "\"y\"".to_string(),
            "\"z\"".to_string()
        ]
    );
}

#[tokio::test]
#[ignore]
async fn type_ttl_exists_dbsize_are_allowed() {
    setup_widgets().await;
    let d = db();
    assert_eq!(
        d.execute_redis(q("TYPE", &["widgets:alpha"]))
            .await
            .unwrap()
            .values,
        vec!["\"string\""]
    );
    assert!(d.execute_redis(q("TTL", &["widgets:alpha"])).await.is_ok());
    assert_eq!(
        d.execute_redis(q("EXISTS", &["widgets:alpha"]))
            .await
            .unwrap()
            .values,
        vec!["1"]
    );
    assert!(d.execute_redis(q("DBSIZE", &[])).await.is_ok());
}

#[tokio::test]
#[ignore]
async fn keys_pattern_matches_seeded_keys() {
    setup_widgets().await;
    let mut result = db().execute_redis(q("KEYS", &["widgets:*"])).await.unwrap();
    result.values.sort();
    assert_eq!(result.count, 6);
}

#[tokio::test]
#[ignore]
async fn limit_truncates_and_flags_truncated() {
    setup_widgets().await;
    let mut query = q("SMEMBERS", &["widgets:set"]);
    query.limit = 1;
    let result = db().execute_redis(query).await.unwrap();
    assert_eq!(result.count, 1);
    assert!(result.truncated);
}

#[tokio::test]
#[ignore]
async fn config_get_is_allowed_but_config_set_is_blocked_end_to_end() {
    let d = db();
    assert!(d
        .execute_redis(q("CONFIG", &["GET", "maxmemory"]))
        .await
        .is_ok());
    let result = d
        .execute_redis(q("CONFIG", &["SET", "maxmemory", "100mb"]))
        .await;
    assert!(matches!(result, Err(DbError::BlockedKeyword { .. })));
}

#[tokio::test]
#[ignore]
async fn always_blocked_commands_are_rejected_end_to_end_and_data_is_untouched() {
    setup_widgets().await;
    let d = db();
    let attempts: &[(&str, &[&str])] = &[
        ("SET", &["widgets:alpha", "tampered"]),
        ("DEL", &["widgets:alpha"]),
        ("EXPIRE", &["widgets:alpha", "10"]),
        ("FLUSHALL", &[]),
        ("FLUSHDB", &[]),
        ("EVAL", &["return 1", "0"]),
        ("MULTI", &[]),
        ("SUBSCRIBE", &["channel"]),
        ("SHUTDOWN", &["NOSAVE"]),
        ("RENAME", &["widgets:alpha", "widgets:renamed"]),
    ];
    for (cmd, args) in attempts {
        let result = d.execute_redis(q(cmd, args)).await;
        assert!(
            matches!(result, Err(DbError::BlockedKeyword { .. })),
            "{cmd:?} should be blocked, got {result:?}"
        );
    }

    // Confirm none of the attempts actually reached Redis.
    let check = d.execute_redis(q("GET", &["widgets:alpha"])).await.unwrap();
    assert_eq!(check.values, vec!["\"1\"".to_string()]);
}

#[tokio::test]
#[ignore]
async fn find_without_matching_key_pattern_returns_empty_not_error() {
    setup_widgets().await;
    let result = db()
        .execute_redis(q("KEYS", &["no-such-prefix:*"]))
        .await
        .unwrap();
    assert_eq!(result.count, 0);
    assert!(!result.truncated);
}
