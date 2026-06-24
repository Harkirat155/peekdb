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
            return Err(format!("{} contains no [[database]] or [[mongo]] entries", path.display()));
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
                return Err(format!("Duplicate name '{}' across [[database]]/[[mongo]] entries", m.name));
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
