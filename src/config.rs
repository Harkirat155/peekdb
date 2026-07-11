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
}

#[derive(Debug, Deserialize, Clone)]
pub struct MongoConfig {
    pub name: String,
    pub uri: String,
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

        Ok(config)
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
}
