//! Pure safety policy helpers (allowlist, rate limit, redaction, response size).
//! Kept free of network I/O so unit tests can drive the real shipped logic.

use crate::db::traits::{DbError, QueryResult};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── Object allowlist ──────────────────────────────────────────────────────────

/// When `allowed` is empty, everything is permitted. When non-empty, only listed
/// object names (and bare names matching a `schema.name` entry's right-hand side)
/// pass.
#[derive(Debug, Clone, Default)]
pub struct ObjectAllowlist {
    allowed: HashSet<String>,
}

impl ObjectAllowlist {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let allowed: HashSet<String> = names
            .into_iter()
            .map(|s| s.into())
            .filter(|s: &String| !s.is_empty())
            .collect();
        Self { allowed }
    }

    pub fn is_unrestricted(&self) -> bool {
        self.allowed.is_empty()
    }

    /// Check a table/collection/key name. Bare `users` matches allowlist entry
    /// `users` or `public.users` (and vice versa for the right-hand segment).
    pub fn check(&self, name: &str) -> Result<(), DbError> {
        if self.is_unrestricted() {
            return Ok(());
        }
        if self.matches(name) {
            return Ok(());
        }
        Err(DbError::NotAllowedObject {
            name: name.to_string(),
        })
    }

    fn matches(&self, name: &str) -> bool {
        if self.allowed.contains(name) {
            return true;
        }
        // bare name vs schema.qualified
        if let Some((_schema, bare)) = name.split_once('.') {
            if self.allowed.contains(bare) {
                return true;
            }
        } else {
            for a in &self.allowed {
                if let Some((_s, bare)) = a.split_once('.') {
                    if bare == name {
                        return true;
                    }
                }
            }
        }
        // Redis-style prefix: entry `session:*` or `session:`
        for a in &self.allowed {
            if let Some(prefix) = a.strip_suffix('*') {
                if name.starts_with(prefix) {
                    return true;
                }
            } else if a.ends_with(':') && name.starts_with(a) {
                return true;
            }
        }
        false
    }

    /// Filter a list of object names to those allowed (unrestricted → all).
    pub fn filter_list(&self, names: Vec<String>) -> Vec<String> {
        if self.is_unrestricted() {
            return names;
        }
        names.into_iter().filter(|n| self.matches(n)).collect()
    }
}

/// Lightweight extraction of table-like identifiers after FROM / JOIN / TABLE.
/// Used only when an allowlist is configured — not a full SQL parser.
pub fn extract_sql_table_refs(sql: &str) -> Vec<String> {
    let upper = sql.to_ascii_uppercase();
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let keywords = ["FROM", "JOIN", "TABLE"];
    for kw in keywords {
        let mut search_from = 0usize;
        while let Some(rel) = upper[search_from..].find(kw) {
            let abs = search_from + rel;
            // word boundary: prev not alnum/_
            if abs > 0 {
                let prev = upper.as_bytes()[abs - 1] as char;
                if prev.is_ascii_alphanumeric() || prev == '_' {
                    search_from = abs + kw.len();
                    continue;
                }
            }
            let after = abs + kw.len();
            if after < upper.len() {
                let next = upper.as_bytes()[after] as char;
                if next.is_ascii_alphanumeric() || next == '_' {
                    search_from = abs + kw.len();
                    continue;
                }
            }
            // skip whitespace
            let mut i = after;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // optional opening quote
            let mut ident = String::new();
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'`') {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != q {
                    ident.push(bytes[i] as char);
                    i += 1;
                }
            } else {
                while i < bytes.len() {
                    let c = bytes[i] as char;
                    if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                        ident.push(c);
                        i += 1;
                    } else {
                        break;
                    }
                }
            }
            if !ident.is_empty()
                && !ident.eq_ignore_ascii_case("select")
                && !ident.eq_ignore_ascii_case("lateral")
                && !ident.eq_ignore_ascii_case("only")
            {
                // strip optional schema quotes already handled; take as-is
                out.push(ident);
            }
            search_from = abs + kw.len();
        }
    }
    out
}

pub fn check_sql_allowlist(sql: &str, allow: &ObjectAllowlist) -> Result<(), DbError> {
    if allow.is_unrestricted() {
        return Ok(());
    }
    let refs = extract_sql_table_refs(sql);
    if refs.is_empty() {
        // e.g. SELECT 1 — no object touch
        return Ok(());
    }
    for r in refs {
        allow.check(&r)?;
    }
    Ok(())
}

// ── Rate limiter ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct WindowCounter {
    events: VecDeque<(Instant, u64)>,
    window: Duration,
}

impl WindowCounter {
    fn new(window: Duration) -> Self {
        Self {
            events: VecDeque::new(),
            window,
        }
    }

    fn purge(&mut self, now: Instant) {
        while let Some(&(t, _)) = self.events.front() {
            if now.duration_since(t) > self.window {
                self.events.pop_front();
            } else {
                break;
            }
        }
    }

    fn total(&mut self, now: Instant) -> u64 {
        self.purge(now);
        self.events.iter().map(|(_, n)| n).sum()
    }

    fn record(&mut self, now: Instant, n: u64) {
        self.purge(now);
        self.events.push_back((now, n));
    }
}

/// Sliding-window rate limiter for calls and/or rows per minute (or custom window).
#[derive(Debug)]
pub struct RateLimiter {
    max_calls: Option<u64>,
    max_rows: Option<u64>,
    calls: Mutex<WindowCounter>,
    rows: Mutex<WindowCounter>,
}

impl RateLimiter {
    pub fn new(max_calls_per_minute: Option<u64>, max_rows_per_minute: Option<u64>) -> Option<Self> {
        if max_calls_per_minute.is_none() && max_rows_per_minute.is_none() {
            return None;
        }
        let window = Duration::from_secs(60);
        Some(Self {
            max_calls: max_calls_per_minute,
            max_rows: max_rows_per_minute,
            calls: Mutex::new(WindowCounter::new(window)),
            rows: Mutex::new(WindowCounter::new(window)),
        })
    }

    /// Call before executing a query.
    pub fn check_call(&self) -> Result<(), DbError> {
        let Some(max) = self.max_calls else {
            return Ok(());
        };
        let now = Instant::now();
        let mut g = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        let used = g.total(now);
        if used >= max {
            return Err(DbError::RateLimited(format!(
                "call limit {max}/min exceeded (used {used})"
            )));
        }
        Ok(())
    }

    pub fn record_call(&self) {
        if self.max_calls.is_none() {
            return;
        }
        let now = Instant::now();
        let mut g = self.calls.lock().unwrap_or_else(|e| e.into_inner());
        g.record(now, 1);
    }

    /// Call after a successful query with the number of rows/docs returned.
    pub fn check_and_record_rows(&self, row_count: u64) -> Result<(), DbError> {
        let Some(max) = self.max_rows else {
            return Ok(());
        };
        let now = Instant::now();
        let mut g = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let used = g.total(now);
        if used.saturating_add(row_count) > max {
            return Err(DbError::RateLimited(format!(
                "row limit {max}/min would be exceeded (used {used}, requested {row_count})"
            )));
        }
        g.record(now, row_count);
        Ok(())
    }
}

// ── Redaction ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct RedactRules {
    /// Exact column/field names (case-insensitive).
    pub columns: Vec<String>,
    /// Simple patterns: `*` wildcard, matched case-insensitively against column names.
    pub patterns: Vec<String>,
}

impl RedactRules {
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty() && self.patterns.is_empty()
    }

    pub fn matches_column(&self, col: &str) -> bool {
        let cl = col.to_ascii_lowercase();
        for c in &self.columns {
            if c.eq_ignore_ascii_case(col) {
                return true;
            }
        }
        for p in &self.patterns {
            if pattern_match(&p.to_ascii_lowercase(), &cl) {
                return true;
            }
        }
        false
    }
}

/// `*` matches any substring (glob-style, not full regex).
fn pattern_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return text == pattern;
    }
    let mut pos = text;
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if i == 0 && !pattern.starts_with('*') {
            if !pos.starts_with(part) {
                return false;
            }
            pos = &pos[part.len()..];
            continue;
        }
        if i == parts.len() - 1 && !pattern.ends_with('*') {
            return pos.ends_with(part);
        }
        if let Some(idx) = pos.find(part) {
            pos = &pos[idx + part.len()..];
        } else {
            return false;
        }
    }
    true
}

pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Redact matching columns in a QueryResult. Returns whether any cell was redacted.
pub fn redact_query_result(result: &mut QueryResult, rules: &RedactRules) -> bool {
    if rules.is_empty() {
        return false;
    }
    let mut idxs = Vec::new();
    for (i, col) in result.columns.iter().enumerate() {
        if rules.matches_column(col) {
            idxs.push(i);
        }
    }
    if idxs.is_empty() {
        return false;
    }
    let mut any = false;
    for row in &mut result.rows {
        for &i in &idxs {
            if let Some(cell) = row.get_mut(i) {
                if cell.as_ref().is_some_and(|s| s != REDACTED_PLACEHOLDER) {
                    *cell = Some(REDACTED_PLACEHOLDER.to_string());
                    any = true;
                }
            }
        }
    }
    any
}

/// Redact keys in JSON object documents (Mongo). Returns whether any value changed.
pub fn redact_json_documents(docs: &mut [String], rules: &RedactRules) -> bool {
    if rules.is_empty() {
        return false;
    }
    let mut any = false;
    for d in docs {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(d) {
            if redact_value(&mut v, rules) {
                any = true;
                *d = serde_json::to_string(&v).unwrap_or_else(|_| d.clone());
            }
        }
    }
    any
}

fn redact_value(v: &mut serde_json::Value, rules: &RedactRules) -> bool {
    match v {
        serde_json::Value::Object(map) => {
            let mut any = false;
            for (k, val) in map.iter_mut() {
                if rules.matches_column(k) {
                    *val = serde_json::Value::String(REDACTED_PLACEHOLDER.into());
                    any = true;
                } else if redact_value(val, rules) {
                    any = true;
                }
            }
            any
        }
        serde_json::Value::Array(arr) => {
            let mut any = false;
            for item in arr {
                if redact_value(item, rules) {
                    any = true;
                }
            }
            any
        }
        _ => false,
    }
}

// ── Max response size ─────────────────────────────────────────────────────────

/// Truncate query rows until approximate serialized size is under `max_bytes`.
/// Returns true if truncation happened. Errors only if a single row still exceeds.
pub fn cap_query_result_bytes(result: &mut QueryResult, max_bytes: usize) -> Result<bool, DbError> {
    let size = estimate_query_result_bytes(result);
    if size <= max_bytes {
        return Ok(false);
    }
    while result.rows.len() > 1 {
        result.rows.pop();
        result.row_count = result.rows.len();
        result.truncated = true;
        if estimate_query_result_bytes(result) <= max_bytes {
            return Ok(true);
        }
    }
    // one row still too big
    if estimate_query_result_bytes(result) > max_bytes {
        result.rows.clear();
        result.row_count = 0;
        result.truncated = true;
        return Err(DbError::ResponseTooLarge(format!(
            "result exceeds max_response_bytes ({max_bytes}); even a single row is too large"
        )));
    }
    Ok(true)
}

pub fn estimate_query_result_bytes(result: &QueryResult) -> usize {
    let mut n = result.columns.iter().map(|c| c.len() + 1).sum::<usize>();
    for row in &result.rows {
        for cell in row {
            n += cell.as_ref().map(|s| s.len()).unwrap_or(4) + 1;
        }
    }
    n
}

pub fn cap_string_list_bytes(items: &mut Vec<String>, max_bytes: usize) -> Result<bool, DbError> {
    let mut total: usize = items.iter().map(|s| s.len() + 1).sum();
    if total <= max_bytes {
        return Ok(false);
    }
    while items.len() > 1 {
        if let Some(last) = items.pop() {
            total = total.saturating_sub(last.len() + 1);
        }
        if total <= max_bytes {
            return Ok(true);
        }
    }
    if total > max_bytes {
        items.clear();
        return Err(DbError::ResponseTooLarge(format!(
            "result exceeds max_response_bytes ({max_bytes})"
        )));
    }
    Ok(true)
}

// ── Per-target policy bundle ──────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TargetPolicy {
    pub allow_objects: ObjectAllowlist,
    pub redact: RedactRules,
    pub max_response_bytes: Option<usize>,
}

/// Shared rate limiters keyed by target name (process-local).
#[derive(Debug, Default)]
pub struct RateLimitRegistry {
    inner: Mutex<HashMap<String, std::sync::Arc<RateLimiter>>>,
}

impl RateLimitRegistry {
    pub fn get_or_insert(
        &self,
        name: &str,
        max_calls: Option<u64>,
        max_rows: Option<u64>,
    ) -> Option<std::sync::Arc<RateLimiter>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = g.get(name) {
            return Some(existing.clone());
        }
        let lim = RateLimiter::new(max_calls, max_rows)?;
        let arc = std::sync::Arc::new(lim);
        g.insert(name.to_string(), arc.clone());
        Some(arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_empty_is_unrestricted() {
        let a = ObjectAllowlist::new(Vec::<String>::new());
        assert!(a.check("anything").is_ok());
    }

    #[test]
    fn allowlist_denies_unknown_and_allows_listed() {
        let a = ObjectAllowlist::new(vec!["users", "public.orders"]);
        assert!(a.check("users").is_ok());
        assert!(a.check("public.orders").is_ok());
        assert!(a.check("orders").is_ok()); // bare matches public.orders
        assert!(a.check("secrets").is_err());
    }

    #[test]
    fn extract_sql_table_refs_finds_from_and_join() {
        let refs = extract_sql_table_refs("SELECT * FROM users u JOIN public.orders o ON true");
        assert!(refs.iter().any(|r| r == "users"));
        assert!(refs.iter().any(|r| r == "public.orders"));
    }

    #[test]
    fn check_sql_allowlist_blocks_denied_table() {
        let a = ObjectAllowlist::new(vec!["users"]);
        assert!(check_sql_allowlist("SELECT 1 FROM secrets", &a).is_err());
        assert!(check_sql_allowlist("SELECT 1 FROM users", &a).is_ok());
        assert!(check_sql_allowlist("SELECT 1", &a).is_ok());
    }

    #[test]
    fn rate_limiter_blocks_excess_calls() {
        let lim = RateLimiter::new(Some(2), None).unwrap();
        assert!(lim.check_call().is_ok());
        lim.record_call();
        assert!(lim.check_call().is_ok());
        lim.record_call();
        assert!(lim.check_call().is_err());
    }

    #[test]
    fn rate_limiter_blocks_excess_rows() {
        let lim = RateLimiter::new(None, Some(10)).unwrap();
        assert!(lim.check_and_record_rows(6).is_ok());
        assert!(lim.check_and_record_rows(5).is_err());
        assert!(lim.check_and_record_rows(4).is_ok());
    }

    #[test]
    fn redact_query_result_masks_matching_columns() {
        let rules = RedactRules {
            columns: vec!["email".into()],
            patterns: vec!["*ssn*".into()],
        };
        let mut r = QueryResult {
            columns: vec!["id".into(), "email".into(), "user_ssn".into()],
            rows: vec![vec![
                Some("1".into()),
                Some("a@b.com".into()),
                Some("111".into()),
            ]],
            row_count: 1,
            truncated: false,
            command_tag: None,
            redacted: false,
        };
        assert!(redact_query_result(&mut r, &rules));
        assert_eq!(r.rows[0][0].as_deref(), Some("1"));
        assert_eq!(r.rows[0][1].as_deref(), Some(REDACTED_PLACEHOLDER));
        assert_eq!(r.rows[0][2].as_deref(), Some(REDACTED_PLACEHOLDER));
    }

    #[test]
    fn cap_query_result_truncates_rows() {
        let mut r = QueryResult {
            columns: vec!["x".into()],
            rows: (0..50)
                .map(|i| vec![Some(format!("value-{i}-{}", "z".repeat(20)))])
                .collect(),
            row_count: 50,
            truncated: false,
            command_tag: None,
            redacted: false,
        };
        let before = r.rows.len();
        let truncated = cap_query_result_bytes(&mut r, 200).unwrap();
        assert!(truncated);
        assert!(r.rows.len() < before);
        assert!(r.truncated);
        assert!(estimate_query_result_bytes(&r) <= 200);
    }

    #[test]
    fn pattern_match_glob_star() {
        assert!(pattern_match("*ssn*", "user_ssn"));
        assert!(pattern_match("email*", "email_address"));
        assert!(!pattern_match("email*", "mail"));
    }
}
