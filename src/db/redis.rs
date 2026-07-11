use super::traits::{DbError, RedisDatabase, RedisQuery, RedisResult};
use async_trait::async_trait;
use redis::aio::ConnectionManager;
use redis::Value;
use tokio::sync::Mutex;
use tracing::info;

/// Deny-by-default command allowlist — Redis has no read-only-transaction concept the
/// way Postgres does, so this is the ONLY enforcement layer (mirrors the same pattern
/// already proven by Mongo's operation allowlist). Anything not listed here — SET, DEL,
/// EXPIRE, FLUSHALL/FLUSHDB, scripting (EVAL/EVALSHA/FUNCTION/SCRIPT), transactions
/// (MULTI/EXEC/DISCARD/WATCH), pub/sub (SUBSCRIBE/PUBLISH), CONFIG SET, ACL, SHUTDOWN,
/// persistence (SAVE/BGSAVE/BGREWRITEAOF), RESTORE/MIGRATE, DEBUG, REPLICAOF/SLAVEOF,
/// FAILOVER, MODULE — is rejected by omission.
const ALLOWED_COMMANDS: &[&str] = &[
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "HGET",
    "HGETALL",
    "HMGET",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HRANDFIELD",
    "HSTRLEN",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SISMEMBER",
    "SMISMEMBER",
    "SCARD",
    "SRANDMEMBER",
    "SINTER",
    "SINTERCARD",
    "SUNION",
    "SDIFF",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZSCORE",
    "ZMSCORE",
    "ZCARD",
    "ZRANK",
    "ZREVRANK",
    "ZCOUNT",
    "ZLEXCOUNT",
    "SCAN",
    "HSCAN",
    "SSCAN",
    "ZSCAN",
    "TYPE",
    "TTL",
    "PTTL",
    "EXISTS",
    "DBSIZE",
    "RANDOMKEY",
    "KEYS",
    "PING",
    "ECHO",
    "GETBIT",
    "BITCOUNT",
    "BITPOS",
    "XLEN",
    "XRANGE",
    "XREVRANGE",
];

/// Composite commands where the verb alone is ambiguous (some subcommands write/mutate
/// server state) — only these specific safe subcommands are allowed.
const ALLOWED_SUBCOMMANDS: &[(&str, &[&str])] = &[
    ("CONFIG", &["GET"]),
    ("OBJECT", &["ENCODING", "REFCOUNT", "IDLETIME", "FREQ"]),
    ("MEMORY", &["USAGE", "STATS", "DOCTOR"]),
    ("CLIENT", &["INFO", "LIST", "GETNAME"]),
    ("XINFO", &["STREAM", "GROUPS", "CONSUMERS"]),
    ("CLUSTER", &["INFO", "NODES", "SLOTS", "SHARDS"]),
];

fn check_allowed_redis(command: &str, args: &[String]) -> Result<(), DbError> {
    let upper = command.to_uppercase();

    if ALLOWED_COMMANDS.contains(&upper.as_str()) {
        return Ok(());
    }

    if let Some((_, subs)) = ALLOWED_SUBCOMMANDS.iter().find(|(c, _)| *c == upper) {
        let sub = args.first().map(|s| s.to_uppercase()).unwrap_or_default();
        if subs.contains(&sub.as_str()) {
            return Ok(());
        }
        return Err(DbError::BlockedKeyword {
            keyword: format!("{upper} {sub}"),
        });
    }

    Err(DbError::BlockedKeyword { keyword: upper })
}

/// Converts a Redis RESP value into a JSON tree so results can be rendered/serialized
/// the same way Mongo documents are (one JSON string per top-level item).
fn redis_value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Nil => serde_json::Value::Null,
        Value::Int(i) => serde_json::Value::from(*i),
        Value::BulkString(bytes) => {
            serde_json::Value::String(String::from_utf8_lossy(bytes).into_owned())
        }
        Value::SimpleString(s) => serde_json::Value::String(s.clone()),
        Value::Okay => serde_json::Value::String("OK".to_string()),
        Value::Double(d) => serde_json::json!(*d),
        Value::Boolean(b) => serde_json::Value::Bool(*b),
        Value::Array(items) | Value::Set(items) => {
            serde_json::Value::Array(items.iter().map(redis_value_to_json).collect())
        }
        Value::Map(pairs) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in pairs {
                let key = match redis_value_to_json(k) {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                obj.insert(key, redis_value_to_json(val));
            }
            serde_json::Value::Object(obj)
        }
        other => serde_json::Value::String(format!("{other:?}")),
    }
}

pub struct RedisDb {
    name: String,
    url: String,
    conn: Mutex<Option<ConnectionManager>>,
}

impl RedisDb {
    /// Create a new (not-yet-connected) RedisDb. Connection happens lazily on first
    /// query. `url` selects plain vs. TLS via scheme (`redis://` vs `rediss://`).
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            conn: Mutex::new(None),
        }
    }

    /// Returns a cloned handle to the connection manager (cheap — it's an internal
    /// Arc-like handle designed to be cloned for concurrent use), connecting first if
    /// this is the first call.
    async fn ensure_connected(&self) -> Result<ConnectionManager, DbError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            info!("Connecting to Redis '{}'...", self.name);
            let client = redis::Client::open(self.url.as_str())
                .map_err(|e| DbError::ConnectionFailed(format!("'{}': {}", self.name, e)))?;
            let manager = client
                .get_connection_manager()
                .await
                .map_err(|e| DbError::ConnectionFailed(format!("'{}': {}", self.name, e)))?;
            info!("Redis '{}' connected.", self.name);
            *guard = Some(manager);
        }
        Ok(guard.as_ref().unwrap().clone())
    }
}

#[async_trait]
impl RedisDatabase for RedisDb {
    async fn execute_redis(&self, query: RedisQuery) -> Result<RedisResult, DbError> {
        check_allowed_redis(&query.command, &query.args)?;

        let mut conn = self.ensure_connected().await?;

        let mut cmd = redis::cmd(&query.command);
        for a in &query.args {
            cmd.arg(a);
        }

        let value: Value = cmd
            .query_async(&mut conn)
            .await
            .map_err(|e| DbError::QueryFailed(e.to_string()))?;

        let (values, truncated) = value_to_display(&value, &query);
        let count = values.len();
        Ok(RedisResult {
            values,
            count,
            truncated,
        })
    }
}

fn is_flat_map_command(query: &RedisQuery) -> bool {
    let upper = query.command.to_uppercase();
    upper == "HGETALL"
        || (upper == "CONFIG"
            && query
                .args
                .first()
                .is_some_and(|a| a.eq_ignore_ascii_case("GET")))
}

/// Converts a raw Redis reply into the displayed value list, applying the
/// HGETALL/CONFIG-GET pairing fixup and the `limit` truncation. Split out from
/// `execute_redis` so this transformation logic is unit-testable without a live server
/// — it's exactly the kind of thing that goes wrong quietly (see the HGETALL flattening
/// bug this shape was introduced to fix).
fn value_to_display(value: &Value, query: &RedisQuery) -> (Vec<String>, bool) {
    let json = redis_value_to_json(value);
    match json {
        // HGETALL/CONFIG GET are semantically key-value maps, but under the RESP2
        // protocol (redis-rs's default) they arrive as a flat alternating array — pair
        // them into a single object rather than flattening into separate per-item
        // results, which would otherwise interleave keys and values.
        serde_json::Value::Array(items) if is_flat_map_command(query) && items.len() % 2 == 0 => {
            let mut obj = serde_json::Map::new();
            for pair in items.chunks_exact(2) {
                let key = match &pair[0] {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                obj.insert(key, pair[1].clone());
            }
            (vec![serde_json::Value::Object(obj).to_string()], false)
        }
        serde_json::Value::Array(items) => {
            let truncated = items.len() > query.limit;
            let values: Vec<String> = items
                .into_iter()
                .take(query.limit)
                .map(|v| v.to_string())
                .collect();
            (values, truncated)
        }
        other => (vec![other.to_string()], false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hgetall_flat_array_reply_is_paired_into_one_object() {
        // Regression test: this is exactly what a RESP2 HGETALL reply looks like —
        // a flat array, NOT a Map. Flattening it per-item (as SMEMBERS/LRANGE do) would
        // wrongly interleave keys and values as 4 separate results instead of 1 hash.
        let value = Value::Array(vec![
            Value::BulkString(b"name".to_vec()),
            Value::BulkString(b"gamma".to_vec()),
            Value::BulkString(b"count".to_vec()),
            Value::BulkString(b"3".to_vec()),
        ]);
        let query = RedisQuery {
            command: "HGETALL".to_string(),
            args: vec![],
            limit: 100,
        };
        let (values, truncated) = value_to_display(&value, &query);
        assert_eq!(values.len(), 1);
        assert!(!truncated);
        let parsed: serde_json::Value = serde_json::from_str(&values[0]).unwrap();
        assert_eq!(parsed["name"], "gamma");
        assert_eq!(parsed["count"], "3");
    }

    #[test]
    fn config_get_flat_array_reply_is_paired_into_one_object() {
        let value = Value::Array(vec![
            Value::BulkString(b"maxmemory".to_vec()),
            Value::BulkString(b"0".to_vec()),
        ]);
        let query = RedisQuery {
            command: "CONFIG".to_string(),
            args: a(&["GET", "maxmemory"]),
            limit: 100,
        };
        let (values, _) = value_to_display(&value, &query);
        assert_eq!(values.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&values[0]).unwrap();
        assert_eq!(parsed["maxmemory"], "0");
    }

    #[test]
    fn smembers_array_reply_is_flattened_per_item_not_paired() {
        // A genuinely list-like command must NOT be paired, even with an even count.
        let value = Value::Array(vec![
            Value::BulkString(b"a".to_vec()),
            Value::BulkString(b"b".to_vec()),
        ]);
        let query = RedisQuery {
            command: "SMEMBERS".to_string(),
            args: vec![],
            limit: 100,
        };
        let (values, _) = value_to_display(&value, &query);
        assert_eq!(values, vec!["\"a\"".to_string(), "\"b\"".to_string()]);
    }

    #[test]
    fn scalar_string_reply_is_rendered_as_quoted_json() {
        let value = Value::BulkString(b"1".to_vec());
        let query = RedisQuery {
            command: "GET".to_string(),
            args: vec![],
            limit: 100,
        };
        let (values, truncated) = value_to_display(&value, &query);
        assert_eq!(values, vec!["\"1\"".to_string()]);
        assert!(!truncated);
    }

    #[test]
    fn nil_reply_is_one_null_value_not_zero_values() {
        let value = Value::Nil;
        let query = RedisQuery {
            command: "GET".to_string(),
            args: vec![],
            limit: 100,
        };
        let (values, _) = value_to_display(&value, &query);
        assert_eq!(values, vec!["null".to_string()]);
    }

    #[test]
    fn array_reply_over_limit_is_truncated() {
        let value = Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let query = RedisQuery {
            command: "LRANGE".to_string(),
            args: vec![],
            limit: 2,
        };
        let (values, truncated) = value_to_display(&value, &query);
        assert_eq!(values.len(), 2);
        assert!(truncated);
    }

    #[test]
    fn every_allowed_command_is_permitted() {
        for cmd in ALLOWED_COMMANDS {
            assert!(
                check_allowed_redis(cmd, &[]).is_ok(),
                "{cmd} should be allowed"
            );
            // Lowercase input must be treated the same (Redis commands are case-insensitive).
            assert!(check_allowed_redis(&cmd.to_lowercase(), &[]).is_ok());
        }
    }

    #[test]
    fn always_blocked_commands_are_rejected() {
        for cmd in [
            "SET",
            "DEL",
            "UNLINK",
            "EXPIRE",
            "PERSIST",
            "RENAME",
            "FLUSHALL",
            "FLUSHDB",
            "EVAL",
            "EVALSHA",
            "FCALL",
            "FUNCTION",
            "SCRIPT",
            "MULTI",
            "EXEC",
            "DISCARD",
            "WATCH",
            "SUBSCRIBE",
            "PUBLISH",
            "ACL",
            "SHUTDOWN",
            "SAVE",
            "BGSAVE",
            "BGREWRITEAOF",
            "RESTORE",
            "MIGRATE",
            "DEBUG",
            "REPLICAOF",
            "SLAVEOF",
            "FAILOVER",
            "MODULE",
        ] {
            assert!(
                matches!(
                    check_allowed_redis(cmd, &[]),
                    Err(DbError::BlockedKeyword { .. })
                ),
                "{cmd} should be blocked"
            );
        }
    }

    #[test]
    fn config_get_is_allowed_but_config_set_is_blocked() {
        assert!(check_allowed_redis("CONFIG", &a(&["GET", "maxmemory"])).is_ok());
        assert!(matches!(
            check_allowed_redis("CONFIG", &a(&["SET", "maxmemory", "100mb"])),
            Err(DbError::BlockedKeyword { .. })
        ));
    }

    #[test]
    fn config_with_no_subcommand_is_blocked() {
        assert!(matches!(
            check_allowed_redis("CONFIG", &[]),
            Err(DbError::BlockedKeyword { .. })
        ));
    }

    #[test]
    fn cluster_info_allowed_but_cluster_failover_blocked() {
        assert!(check_allowed_redis("CLUSTER", &a(&["INFO"])).is_ok());
        assert!(matches!(
            check_allowed_redis("CLUSTER", &a(&["FAILOVER"])),
            Err(DbError::BlockedKeyword { .. })
        ));
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert!(matches!(
            check_allowed_redis("NOTACOMMAND", &[]),
            Err(DbError::BlockedKeyword { .. })
        ));
    }

    #[test]
    fn redis_value_to_json_flattens_common_variants() {
        assert_eq!(redis_value_to_json(&Value::Nil), serde_json::Value::Null);
        assert_eq!(redis_value_to_json(&Value::Int(42)), serde_json::json!(42));
        assert_eq!(
            redis_value_to_json(&Value::BulkString(b"hello".to_vec())),
            serde_json::json!("hello")
        );
        assert_eq!(
            redis_value_to_json(&Value::Array(vec![Value::Int(1), Value::Int(2)])),
            serde_json::json!([1, 2])
        );
    }
}
