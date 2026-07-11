use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub database: Vec<DatabaseConfig>,
    #[serde(default)]
    pub mongo: Vec<MongoConfig>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub no_tls: bool,
    /// Explicit opt-out for the literal-password check below. Off by default —
    /// a user who genuinely wants a trivial local-dev password must say so.
    #[serde(default)]
    pub allow_literal_password: bool,
    /// Server-side `statement_timeout` in milliseconds. `None` uses `PostgresDb`'s
    /// built-in default (30s) rather than duplicating that default here.
    #[serde(default)]
    pub statement_timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MongoConfig {
    pub name: String,
    pub uri: String,
    #[serde(default)]
    pub allow_literal_password: bool,
    /// Per-query `maxTimeMS` in milliseconds. `None` uses `MongoDb`'s built-in default.
    #[serde(default)]
    pub query_timeout_ms: Option<u64>,
}

impl Config {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        let config: Self = toml::from_str(&raw)
            .map_err(|e| format!("Invalid config {}: {}", path.display(), e))?;

        if config.database.is_empty() && config.mongo.is_empty() {
            return Err(format!(
                "{} contains no [[database]] or [[mongo]] entries",
                path.display()
            ));
        }

        // Duplicate name detection across both postgres and mongo
        let mut seen = HashSet::new();
        for db in &config.database {
            if db.name.is_empty() {
                return Err("Database entry has an empty name".into());
            }
            if !seen.insert(db.name.clone()) {
                return Err(format!("Duplicate database name '{}' in config", db.name));
            }
        }
        for m in &config.mongo {
            if m.name.is_empty() {
                return Err("Mongo entry has an empty name".into());
            }
            if !seen.insert(m.name.clone()) {
                return Err(format!(
                    "Duplicate name '{}' across [[database]]/[[mongo]] entries",
                    m.name
                ));
            }
        }

        for db in &config.database {
            if !db.allow_literal_password {
                check_no_literal_password(&db.name, &db.url)?;
            }
        }
        for m in &config.mongo {
            if !m.allow_literal_password {
                check_no_literal_password(&m.name, &m.uri)?;
            }
        }

        Ok(config)
    }
}

/// Rejects a connection string that embeds a literal (non-`${VAR}`) password.
/// Understands both connection-string shapes used in this repo:
///   - URL form:      postgres://user:PASSWORD@host/db, mongodb://user:PASSWORD@host/db
///   - keyword/DSN form: host=... user=... password=PASSWORD ...
fn check_no_literal_password(name: &str, conn_str: &str) -> Result<(), String> {
    let password = extract_password(conn_str);
    let Some(password) = password else {
        return Ok(());
    };
    if is_safe_password_placeholder(&password) {
        return Ok(());
    }
    Err(format!(
        "database '{name}' has a literal password embedded in its connection string.\n\
         Replace it with an environment variable placeholder, e.g.:\n    \
         password=${{{name_upper}_DB_PASSWORD}}\n\
         then export {name_upper}_DB_PASSWORD=... (or add it to your .env).\n\
         See databases.toml.example. If you really want a literal password for local\n\
         dev, set allow_literal_password = true on this entry explicitly.",
        name_upper = name.to_uppercase().replace(['-', ' '], "_"),
    ))
}

fn is_safe_password_placeholder(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    match value.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        Some(inner) if !inner.is_empty() => {
            inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Extracts the password portion of a connection string, if any. Returns `None` when
/// there's no password segment at all (e.g. no userinfo, or userinfo with no colon).
fn extract_password(conn_str: &str) -> Option<String> {
    if let Some(scheme_end) = conn_str.find("://") {
        let after_scheme = &conn_str[scheme_end + 3..];
        let userinfo_end = after_scheme.find('@')?;
        let userinfo = &after_scheme[..userinfo_end];
        let colon = userinfo.find(':')?;
        return Some(userinfo[colon + 1..].to_string());
    }
    dsn_password(conn_str)
}

/// Parses a libpq-style keyword/value DSN (`key=value key2='quoted value'`) looking for
/// a `password=` token. Respects single-quoted values so a quoted value containing
/// spaces or `=` isn't split incorrectly.
fn dsn_password(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        chars.peek()?;

        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if chars.peek() != Some(&'=') {
            // Malformed token (no '=') — skip to next whitespace and continue.
            while chars.peek().is_some_and(|c| !c.is_whitespace()) {
                chars.next();
            }
            continue;
        }
        chars.next(); // consume '='

        let mut value = String::new();
        if chars.peek() == Some(&'\'') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\'' {
                    break;
                }
                value.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }

        if key == "password" {
            return Some(value);
        }
    }
}

/// Resolve `${VAR_NAME}` placeholders in a string from the environment.
/// Returns an error if any referenced variable is unset.
pub fn interpolate_env(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut var = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                var.push(c);
            }
            if !closed {
                return Err(format!("Unclosed '${{' in value: {}", s));
            }
            match std::env::var(&var) {
                Ok(val) => out.push_str(&val),
                Err(_) => return Err(format!("Environment variable '{}' is not set", var)),
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // Environment variables are process-global, so these tests use unique names
    // (rather than a shared one) to stay safe under `cargo test`'s default
    // multi-threaded runner.

    #[test]
    fn interpolate_env_no_placeholders_is_passthrough() {
        assert_eq!(
            interpolate_env("plain string, no vars").unwrap(),
            "plain string, no vars"
        );
    }

    #[test]
    fn interpolate_env_resolves_a_set_variable() {
        std::env::set_var("CFG_TEST_VAR_A", "resolved-value");
        let out = interpolate_env("prefix-${CFG_TEST_VAR_A}-suffix").unwrap();
        assert_eq!(out, "prefix-resolved-value-suffix");
        std::env::remove_var("CFG_TEST_VAR_A");
    }

    #[test]
    fn interpolate_env_resolves_multiple_variables() {
        std::env::set_var("CFG_TEST_VAR_B1", "one");
        std::env::set_var("CFG_TEST_VAR_B2", "two");
        let out = interpolate_env("${CFG_TEST_VAR_B1}-${CFG_TEST_VAR_B2}").unwrap();
        assert_eq!(out, "one-two");
        std::env::remove_var("CFG_TEST_VAR_B1");
        std::env::remove_var("CFG_TEST_VAR_B2");
    }

    #[test]
    fn interpolate_env_errors_on_unset_variable() {
        std::env::remove_var("CFG_TEST_VAR_UNSET");
        let err = interpolate_env("${CFG_TEST_VAR_UNSET}").unwrap_err();
        assert!(err.contains("CFG_TEST_VAR_UNSET"));
        assert!(err.contains("not set"));
    }

    #[test]
    fn interpolate_env_errors_on_unclosed_placeholder() {
        let err = interpolate_env("prefix-${UNCLOSED").unwrap_err();
        assert!(err.contains("Unclosed"));
    }

    fn write_temp_toml(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn from_file_rejects_empty_config() {
        let f = write_temp_toml("");
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("no [[database]] or [[mongo]] entries"));
    }

    #[test]
    fn from_file_rejects_malformed_toml() {
        let f = write_temp_toml("this is not [valid toml");
        assert!(Config::from_file(f.path()).is_err());
    }

    #[test]
    fn from_file_rejects_duplicate_database_names() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "dup"
            url = "postgres://a"

            [[database]]
            name = "dup"
            url = "postgres://b"
            "#,
        );
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("Duplicate database name 'dup'"));
    }

    #[test]
    fn from_file_rejects_name_collision_across_database_and_mongo() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "shared"
            url = "postgres://a"

            [[mongo]]
            name = "shared"
            uri = "mongodb://b"
            "#,
        );
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("Duplicate name 'shared'"));
    }

    #[test]
    fn from_file_accepts_a_valid_config() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "pg1"
            url = "postgres://a"

            [[mongo]]
            name = "mg1"
            uri = "mongodb://b"
            "#,
        );
        let config = Config::from_file(f.path()).unwrap();
        assert_eq!(config.database.len(), 1);
        assert_eq!(config.mongo.len(), 1);
    }

    // -- extract_password / dsn_password --

    #[test]
    fn extract_password_url_form_with_password() {
        assert_eq!(
            extract_password("postgres://user:secret@host:5432/db"),
            Some("secret".to_string())
        );
    }

    #[test]
    fn extract_password_url_form_no_userinfo() {
        assert_eq!(extract_password("postgres://host:5432/db"), None);
    }

    #[test]
    fn extract_password_url_form_user_with_no_password() {
        assert_eq!(extract_password("postgres://user@host:5432/db"), None);
    }

    #[test]
    fn extract_password_url_form_placeholder_password() {
        assert_eq!(
            extract_password("mongodb://user:${MONGO_PW}@host/db"),
            Some("${MONGO_PW}".to_string())
        );
    }

    #[test]
    fn extract_password_dsn_form_plain() {
        assert_eq!(
            extract_password("host=localhost port=5432 user=x password=secret dbname=db"),
            Some("secret".to_string())
        );
    }

    #[test]
    fn extract_password_dsn_form_quoted_value_with_special_chars() {
        // Mirrors the real-world warpgate DSN shape: user='name#svc:read'
        assert_eq!(
            extract_password(
                "host=h port=1 user='harkirat.singh2#hmsbff:read' password=${WARPGATE_PASSWORD}"
            ),
            Some("${WARPGATE_PASSWORD}".to_string())
        );
    }

    #[test]
    fn extract_password_dsn_form_no_password_key() {
        assert_eq!(extract_password("host=localhost dbname=db"), None);
    }

    // -- check_no_literal_password / Config::from_file integration --

    #[test]
    fn from_file_rejects_literal_password_in_url_form() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "staging"
            url = "postgres://app:hunter2@staging-db.example.com/app"
            "#,
        );
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("literal password"));
        assert!(err.contains("staging"));
    }

    #[test]
    fn from_file_rejects_literal_password_in_dsn_form() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "warpgate"
            url = "host=warpgate.example.com port=55432 user=svc password=OyoDev321 dbname=x"
            "#,
        );
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("literal password"));
    }

    #[test]
    fn from_file_rejects_literal_password_in_mongo_uri() {
        let f = write_temp_toml(
            r#"
            [[mongo]]
            name = "iris"
            uri = "mongodb://admin:supersecret@10.0.0.1:27017/iris?authSource=admin"
            "#,
        );
        let err = Config::from_file(f.path()).unwrap_err();
        assert!(err.contains("literal password"));
    }

    #[test]
    fn from_file_accepts_env_var_placeholder_password() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "staging"
            url = "postgres://app:${STAGING_DB_PASSWORD}@staging-db.example.com/app"
            "#,
        );
        assert!(Config::from_file(f.path()).is_ok());
    }

    #[test]
    fn from_file_accepts_no_password_at_all() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "local"
            url = "postgres://postgres@localhost:5432/mydb"
            "#,
        );
        assert!(Config::from_file(f.path()).is_ok());
    }

    #[test]
    fn from_file_accepts_literal_password_with_explicit_opt_out() {
        let f = write_temp_toml(
            r#"
            [[database]]
            name = "local"
            url = "postgres://postgres:postgres@localhost:5432/mydb"
            allow_literal_password = true
            "#,
        );
        assert!(Config::from_file(f.path()).is_ok());
    }
}
