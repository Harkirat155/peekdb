//! Named query snippets stored under the peekdb config dir.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FavoritesFile {
    #[serde(default)]
    pub favorites: BTreeMap<String, Favorite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorite {
    /// postgres | mongo | redis
    pub backend: String,
    /// Optional preferred target name
    #[serde(default)]
    pub db: Option<String>,
    /// SQL / JSON filter description / redis command line
    pub body: String,
    #[serde(default)]
    pub notes: Option<String>,
}

impl FavoritesFile {
    pub fn default_path() -> PathBuf {
        paths::config_dir().join("favorites.toml")
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        toml::from_str(&raw).map_err(|e| format!("Invalid favorites {}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Cannot create {}: {e}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).map_err(|e| format!("serialize favorites: {e}"))?;
        std::fs::write(path, raw).map_err(|e| format!("Cannot write {}: {e}", path.display()))
    }

    pub fn get(&self, name: &str) -> Option<&Favorite> {
        self.favorites.get(name)
    }

    pub fn set(&mut self, name: impl Into<String>, fav: Favorite) {
        self.favorites.insert(name.into(), fav);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.favorites.remove(name).is_some()
    }

    pub fn names(&self) -> Vec<&str> {
        self.favorites.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_favorites_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("favorites.toml");
        let mut f = FavoritesFile::default();
        f.set(
            "slow-orders",
            Favorite {
                backend: "postgres".into(),
                db: Some("staging".into()),
                body: "SELECT * FROM orders WHERE status = 'open' LIMIT 20".into(),
                notes: Some("daily check".into()),
            },
        );
        f.save(&path).unwrap();
        let loaded = FavoritesFile::load(&path).unwrap();
        assert_eq!(loaded.get("slow-orders").unwrap().backend, "postgres");
        assert!(loaded.get("slow-orders").unwrap().body.contains("orders"));
    }
}
