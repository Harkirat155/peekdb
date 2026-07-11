//! Config path discovery and secret-safe URL display.
//!
//! Search order (first existing file wins):
//! 1. explicit `--config` / path argument
//! 2. `$PEEKDB_CONFIG`
//! 3. `./databases.toml` (cwd)
//! 4. `$XDG_CONFIG_HOME/peekdb/databases.toml` or `~/.config/peekdb/databases.toml`

use std::path::{Path, PathBuf};

/// Default config directory for peekdb user config.
pub fn config_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("peekdb");
        }
    }
    dirs_config_home().join("peekdb")
}

fn dirs_config_home() -> PathBuf {
    // Avoid a new dependency: mirror the common `dirs` crate home layout on Unix/macOS.
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".config");
        }
    }
    // Windows fallback — %USERPROFILE%\.config\peekdb still works if set.
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return PathBuf::from(profile).join(".config");
        }
    }
    PathBuf::from(".config")
}

/// Default path written by `peekdb init`.
pub fn default_config_file() -> PathBuf {
    config_dir().join("databases.toml")
}

/// Paths searched when no explicit `--config` is given (in order).
pub fn search_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(p) = std::env::var("PEEKDB_CONFIG") {
        if !p.is_empty() {
            paths.push(PathBuf::from(p));
        }
    }
    paths.push(PathBuf::from("databases.toml"));
    paths.push(default_config_file());
    paths
}

/// Resolve a config file path. `explicit` wins when present.
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p.to_path_buf());
        }
        return Err(format!("Cannot read {}: No such file", p.display()));
    }

    for p in search_paths() {
        if p.is_file() {
            return Ok(p);
        }
    }

    Err(format!(
        "No config found. Searched:\n{}\n\n\
         Run `peekdb init` to create {},\n\
         or pass --config <path>, or set PEEKDB_CONFIG.",
        search_paths()
            .iter()
            .map(|p| format!("  - {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n"),
        default_config_file().display()
    ))
}

/// Mask password segments in URL or DSN-style connection strings for display.
/// Never returns the raw secret.
pub fn mask_secret(conn: &str) -> String {
    if let Some(scheme_end) = conn.find("://") {
        let scheme = &conn[..scheme_end + 3];
        let rest = &conn[scheme_end + 3..];
        if let Some(at) = rest.find('@') {
            let userinfo = &rest[..at];
            let host = &rest[at..];
            if let Some(colon) = userinfo.find(':') {
                let user = &userinfo[..colon];
                return format!("{scheme}{user}:***{host}");
            }
        }
        return conn.to_string();
    }

    // libpq keyword form: password=secret → password=***
    let mut out = String::with_capacity(conn.len());
    let mut chars = conn.chars().peekable();
    while let Some(ch) = chars.next() {
        out.push(ch);
        // Detect "password=" (case-sensitive, as Postgres DSNs usually are lower).
        if out.ends_with("password=") {
            // Skip value (quoted or bare).
            if chars.peek() == Some(&'\'') {
                out.push('\'');
                chars.next();
                out.push_str("***");
                for c in chars.by_ref() {
                    if c == '\'' {
                        out.push('\'');
                        break;
                    }
                }
            } else {
                out.push_str("***");
                while chars.peek().is_some_and(|c| !c.is_whitespace()) {
                    chars.next();
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_url_with_password() {
        assert_eq!(
            mask_secret("postgres://user:hunter2@host:5432/db"),
            "postgres://user:***@host:5432/db"
        );
    }

    #[test]
    fn mask_secret_redis_password_only() {
        assert_eq!(
            mask_secret("rediss://:s3cret@redis.example:6380"),
            "rediss://:***@redis.example:6380"
        );
    }

    #[test]
    fn mask_secret_url_without_password_unchanged() {
        assert_eq!(
            mask_secret("postgres://user@host:5432/db"),
            "postgres://user@host:5432/db"
        );
    }

    #[test]
    fn mask_secret_dsn_password_keyword() {
        let masked = mask_secret("host=db user=app password=hunter2 dbname=app");
        assert!(masked.contains("password=***"));
        assert!(!masked.contains("hunter2"));
    }

    #[test]
    fn resolve_explicit_missing_file_errors() {
        let err = resolve_config_path(Some(Path::new("/nonexistent/peekdb-xyz.toml"))).unwrap_err();
        assert!(err.contains("Cannot read"));
    }
}
