//! Process-local caches for the MCP server.
//!
//! - **Schema cache**: TTL'd `list_objects` / `describe_object` results so agents
//!   do not re-hit catalog SQL every turn in a long session.
//! - **Result cache**: holds the full buffer from a query so `cursor` pages can
//!   return more rows without re-executing against the database.
//!
//! Both are intentionally process-local (same model as rate limits).

use crate::db::schema::ObjectDescription;
use crate::db::traits::{MongoResult, QueryResult, RedisResult};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Default how long discovery results stay warm.
pub const DEFAULT_SCHEMA_CACHE_TTL_SECS: u64 = 60;
/// Default how long a paged query buffer stays addressable by cursor.
pub const DEFAULT_RESULT_CACHE_TTL_SECS: u64 = 120;
/// Cap stored result buffers so a runaway agent cannot pin unbounded RAM.
const MAX_RESULT_ENTRIES: usize = 64;

// ── Schema cache ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct TimedList {
    at: Instant,
    names: Vec<String>,
}

#[derive(Debug)]
struct TimedDescribe {
    at: Instant,
    desc: ObjectDescription,
}

#[derive(Debug)]
pub struct SchemaCache {
    ttl: Duration,
    lists: Mutex<HashMap<String, TimedList>>,
    describes: Mutex<HashMap<(String, String), TimedDescribe>>,
}

impl SchemaCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            lists: Mutex::new(HashMap::new()),
            describes: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_env() -> Self {
        let secs = std::env::var("PEEKDB_SCHEMA_CACHE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_SCHEMA_CACHE_TTL_SECS);
        Self::new(Duration::from_secs(secs))
    }

    pub fn get_list(&self, target: &str) -> Option<Vec<String>> {
        let mut guard = self.lists.lock().ok()?;
        let entry = guard.get(target)?;
        if entry.at.elapsed() > self.ttl {
            guard.remove(target);
            return None;
        }
        Some(entry.names.clone())
    }

    pub fn put_list(&self, target: impl Into<String>, names: Vec<String>) {
        if let Ok(mut guard) = self.lists.lock() {
            guard.insert(
                target.into(),
                TimedList {
                    at: Instant::now(),
                    names,
                },
            );
        }
    }

    pub fn get_describe(&self, target: &str, object: &str) -> Option<ObjectDescription> {
        let key = (target.to_string(), object.to_string());
        let mut guard = self.describes.lock().ok()?;
        let entry = guard.get(&key)?;
        if entry.at.elapsed() > self.ttl {
            guard.remove(&key);
            return None;
        }
        Some(entry.desc.clone())
    }

    pub fn put_describe(
        &self,
        target: impl Into<String>,
        object: impl Into<String>,
        desc: ObjectDescription,
    ) {
        if let Ok(mut guard) = self.describes.lock() {
            guard.insert(
                (target.into(), object.into()),
                TimedDescribe {
                    at: Instant::now(),
                    desc,
                },
            );
        }
    }
}

// ── Result page cache ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum CachedPayload {
    Postgres {
        columns: Vec<String>,
        rows: Vec<Vec<Option<String>>>,
        target: String,
        /// True when the original execute hit max_rows / was truncated at source.
        source_truncated: bool,
        redacted: bool,
    },
    Mongo {
        documents: Vec<String>,
        target: String,
        source_truncated: bool,
        redacted: bool,
    },
    Redis {
        values: Vec<String>,
        target: String,
        source_truncated: bool,
        redacted: bool,
    },
}

impl CachedPayload {
    pub fn from_pg(result: &QueryResult, target: &str) -> Self {
        Self::Postgres {
            columns: result.columns.clone(),
            rows: result.rows.clone(),
            target: target.to_string(),
            source_truncated: result.truncated,
            redacted: result.redacted,
        }
    }

    pub fn from_mongo(result: &MongoResult, target: &str) -> Self {
        Self::Mongo {
            documents: result.documents.clone(),
            target: target.to_string(),
            source_truncated: result.truncated,
            redacted: result.redacted,
        }
    }

    pub fn from_redis(result: &RedisResult, target: &str) -> Self {
        Self::Redis {
            values: result.values.clone(),
            target: target.to_string(),
            source_truncated: result.truncated,
            redacted: result.redacted,
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Postgres { rows, .. } => rows.len(),
            Self::Mongo { documents, .. } => documents.len(),
            Self::Redis { values, .. } => values.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug)]
struct ResultEntry {
    at: Instant,
    payload: CachedPayload,
}

#[derive(Debug)]
pub struct ResultCache {
    ttl: Duration,
    entries: Mutex<HashMap<String, ResultEntry>>,
    next_id: AtomicU64,
}

/// Parsed `cursor` value: opaque id + byte/row offset into the buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor {
    pub id: String,
    pub offset: usize,
}

impl PageCursor {
    /// Encode as `id:offset` (id is alphanumeric only).
    pub fn encode(&self) -> String {
        format!("{}:{}", self.id, self.offset)
    }

    pub fn parse(s: &str) -> Option<Self> {
        let (id, off) = s.rsplit_once(':')?;
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let offset: usize = off.parse().ok()?;
        Some(Self {
            id: id.to_string(),
            offset,
        })
    }
}

/// One page sliced from a cached buffer.
#[derive(Debug)]
pub struct PageSlice {
    pub payload: CachedPayload,
    pub offset: usize,
    pub total: usize,
    pub next_cursor: Option<String>,
    /// True when this page does not exhaust the in-memory buffer.
    pub has_more: bool,
    /// True when the original query was truncated at the DB layer (more may exist server-side).
    pub source_truncated: bool,
}

impl ResultCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn from_env() -> Self {
        let secs = std::env::var("PEEKDB_RESULT_CACHE_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_RESULT_CACHE_TTL_SECS);
        Self::new(Duration::from_secs(secs))
    }

    fn purge_expired_locked(entries: &mut HashMap<String, ResultEntry>, ttl: Duration) {
        entries.retain(|_, e| e.at.elapsed() <= ttl);
    }

    /// Insert a full result buffer; returns the cache id used in cursors.
    pub fn store(&self, payload: CachedPayload) -> String {
        let id = format!("{:x}", self.next_id.fetch_add(1, Ordering::Relaxed));
        if let Ok(mut guard) = self.entries.lock() {
            Self::purge_expired_locked(&mut guard, self.ttl);
            // Drop oldest if over cap (simple: clear all expired already; if still full, remove arbitrary).
            while guard.len() >= MAX_RESULT_ENTRIES {
                if let Some(k) = guard.keys().next().cloned() {
                    guard.remove(&k);
                } else {
                    break;
                }
            }
            guard.insert(
                id.clone(),
                ResultEntry {
                    at: Instant::now(),
                    payload,
                },
            );
        }
        id
    }

    /// Slice `[offset, offset + page_size)` from a stored buffer.
    pub fn page(&self, cursor: &PageCursor, page_size: usize) -> Result<PageSlice, String> {
        let page_size = page_size.clamp(1, 5000);
        let mut guard = self
            .entries
            .lock()
            .map_err(|_| "result cache lock poisoned".to_string())?;
        Self::purge_expired_locked(&mut guard, self.ttl);
        let entry = guard.get(&cursor.id).ok_or_else(|| {
            "cursor expired or unknown — re-run the query without cursor to refresh".to_string()
        })?;
        if entry.at.elapsed() > self.ttl {
            guard.remove(&cursor.id);
            return Err("cursor expired — re-run the query without cursor to refresh".to_string());
        }

        let total = entry.payload.len();
        let offset = cursor.offset.min(total);
        let end = (offset + page_size).min(total);
        let has_more = end < total;
        let next_cursor = if has_more {
            Some(
                PageCursor {
                    id: cursor.id.clone(),
                    offset: end,
                }
                .encode(),
            )
        } else {
            None
        };

        let payload = match &entry.payload {
            CachedPayload::Postgres {
                columns,
                rows,
                target,
                source_truncated,
                redacted,
            } => CachedPayload::Postgres {
                columns: columns.clone(),
                rows: rows[offset..end].to_vec(),
                target: target.clone(),
                source_truncated: *source_truncated,
                redacted: *redacted,
            },
            CachedPayload::Mongo {
                documents,
                target,
                source_truncated,
                redacted,
            } => CachedPayload::Mongo {
                documents: documents[offset..end].to_vec(),
                target: target.clone(),
                source_truncated: *source_truncated,
                redacted: *redacted,
            },
            CachedPayload::Redis {
                values,
                target,
                source_truncated,
                redacted,
            } => CachedPayload::Redis {
                values: values[offset..end].to_vec(),
                target: target.clone(),
                source_truncated: *source_truncated,
                redacted: *redacted,
            },
        };

        let source_truncated = match &entry.payload {
            CachedPayload::Postgres {
                source_truncated, ..
            }
            | CachedPayload::Mongo {
                source_truncated, ..
            }
            | CachedPayload::Redis {
                source_truncated, ..
            } => *source_truncated,
        };

        Ok(PageSlice {
            payload,
            offset,
            total,
            next_cursor,
            has_more,
            source_truncated,
        })
    }

    /// Store full payload and return the first page (and cursor if needed).
    pub fn store_and_page(&self, payload: CachedPayload, page_size: usize) -> PageSlice {
        let total = payload.len();
        let page_size = page_size.clamp(1, 5000);
        // If everything fits on one page, skip storing (no cursor needed).
        if total <= page_size {
            let source_truncated = match &payload {
                CachedPayload::Postgres {
                    source_truncated, ..
                }
                | CachedPayload::Mongo {
                    source_truncated, ..
                }
                | CachedPayload::Redis {
                    source_truncated, ..
                } => *source_truncated,
            };
            return PageSlice {
                payload,
                offset: 0,
                total,
                next_cursor: None,
                has_more: false,
                source_truncated,
            };
        }
        let id = self.store(payload);
        let cursor = PageCursor { id, offset: 0 };
        // page() re-reads; store already happened
        self.page(&cursor, page_size)
            .expect("just-stored cursor must resolve")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_list_ttl_expires() {
        let cache = SchemaCache::new(Duration::from_millis(30));
        cache.put_list("t", vec!["a".into(), "b".into()]);
        assert_eq!(cache.get_list("t").unwrap().len(), 2);
        std::thread::sleep(Duration::from_millis(40));
        assert!(cache.get_list("t").is_none());
    }

    #[test]
    fn page_cursor_roundtrip() {
        let c = PageCursor {
            id: "a1b2".into(),
            offset: 42,
        };
        assert_eq!(PageCursor::parse(&c.encode()), Some(c));
        assert!(PageCursor::parse("bad").is_none());
        assert!(PageCursor::parse("has:colon:1").is_none()); // id must be alnum only via rsplit
                                                             // rsplit_once: "has:colon:1" -> id="has:colon" fails alnum check
        assert!(PageCursor::parse("abc:xyz").is_none());
    }

    #[test]
    fn result_cache_pages_without_losing_tail() {
        let cache = ResultCache::new(Duration::from_secs(60));
        let rows: Vec<Vec<Option<String>>> = (0..5).map(|i| vec![Some(i.to_string())]).collect();
        let payload = CachedPayload::Postgres {
            columns: vec!["n".into()],
            rows,
            target: "local".into(),
            source_truncated: false,
            redacted: false,
        };
        let first = cache.store_and_page(payload, 2);
        assert_eq!(first.total, 5);
        assert!(first.has_more);
        let next = first.next_cursor.expect("cursor");
        let cur = PageCursor::parse(&next).unwrap();
        assert_eq!(cur.offset, 2);
        let second = cache.page(&cur, 2).unwrap();
        assert_eq!(second.offset, 2);
        assert!(second.has_more);
        let third = cache
            .page(
                &PageCursor::parse(second.next_cursor.as_ref().unwrap()).unwrap(),
                2,
            )
            .unwrap();
        assert!(!third.has_more);
        assert!(third.next_cursor.is_none());
        if let CachedPayload::Postgres { rows, .. } = third.payload {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0].as_deref(), Some("4"));
        } else {
            panic!("expected postgres payload");
        }
    }

    #[test]
    fn small_result_skips_cursor() {
        let cache = ResultCache::new(Duration::from_secs(60));
        let payload = CachedPayload::Redis {
            values: vec!["a".into()],
            target: "c".into(),
            source_truncated: false,
            redacted: false,
        };
        let page = cache.store_and_page(payload, 100);
        assert!(page.next_cursor.is_none());
        assert!(!page.has_more);
    }
}
