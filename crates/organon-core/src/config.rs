//! Organon configuration.
//!
//! Reads `$ORGANON_HOME/config.toml` (default `~/.organon/config.toml`, or the
//! path in `ORGANON_CONFIG`). All fields have sane defaults — the file is
//! optional. `ORGANON_HOME` moves every default path (config, DB, vectors,
//! daemons, workspaces), so it fully isolates a sandboxed run.

use std::path::{Path, PathBuf};

use anyhow::Result;
use log::debug;
use serde::{Deserialize, Serialize};

// ── sub-configs ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleConfig {
    /// Days until a file becomes dormant (default: 30)
    pub dormant_days: i64,
    /// Days until a file becomes archived (default: 90)
    pub archive_days: i64,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            dormant_days: 30,
            archive_days: 90,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    /// Additional directories to watch (besides CLI arg)
    pub roots: Vec<PathBuf>,
    /// Seconds between auto-index runs when `organon watch` spawns the indexer
    pub index_interval_secs: u64,
    /// Extra path segments to ignore (on top of built-in list)
    pub ignore_segments: Vec<String>,
    /// Use git log for created_at/modified_at when inside a git repo
    pub use_git_timestamps: bool,
    /// Seconds between disk ↔ graph reconciliation passes in `organon watch`
    /// (repairs events the watcher missed). 0 disables the periodic pass.
    pub reconcile_interval_secs: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            roots: vec![],
            index_interval_secs: 30,
            ignore_segments: vec![],
            use_git_timestamps: false,
            reconcile_interval_secs: 300,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HistoryConfig {
    /// Move entries older than this many days to the local cold table.
    pub archive_after_days: u64,
    /// Warn when active or archived history exceeds this many MiB.
    pub warn_at_mb: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            archive_after_days: 90,
            warn_at_mb: 100,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub default_limit: usize,
    /// "vector" | "fts" | "hybrid"
    pub default_mode: String,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            default_limit: 10,
            default_mode: "vector".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct IndexerConfig {
    pub db_path: String,
    pub vectors_path: String,
    pub embed_model: String,
    pub max_file_size_mb: u64,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        Self::in_home(&organon_home())
    }
}

impl IndexerConfig {
    /// Defaults with the DB and vectors under `home`.
    pub fn in_home(home: &Path) -> Self {
        Self {
            db_path: home.join("entities.db").to_string_lossy().into_owned(),
            vectors_path: home.join("vectors").to_string_lossy().into_owned(),
            embed_model: "BAAI/bge-small-en-v1.5".to_string(),
            max_file_size_mb: 100,
        }
    }
}

/// The organon home directory: `$ORGANON_HOME`, else `~/.organon`.
pub fn organon_home() -> PathBuf {
    organon_home_with(&|key| std::env::var(key).ok())
}

fn organon_home_with(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    env_path(env, "ORGANON_HOME").unwrap_or_else(|| {
        PathBuf::from(env("HOME").unwrap_or_else(|| ".".to_string())).join(".organon")
    })
}

/// A path from `key`: unset or empty → `None`; `~/` expands to `$HOME`;
/// relative paths are made absolute against the current directory, so the
/// Python layer (spawned in another directory) resolves the same place.
/// Same rules as `ai/common/paths.py`.
fn env_path(env: &dyn Fn(&str) -> Option<String>, key: &str) -> Option<PathBuf> {
    let raw = env(key).filter(|value| !value.is_empty())?;
    let path = match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(env("HOME").unwrap_or_default()).join(rest),
        None if raw == "~" => PathBuf::from(env("HOME").unwrap_or_default()),
        None => PathBuf::from(raw),
    };
    Some(if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7474,
        }
    }
}

// ── root config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgConfig {
    pub lifecycle: LifecycleConfig,
    pub watch: WatchConfig,
    pub history: HistoryConfig,
    pub search: SearchConfig,
    pub indexer: IndexerConfig,
    pub server: ServerConfig,
}

impl OrgConfig {
    /// Load from `ORGANON_CONFIG`, else `$ORGANON_HOME/config.toml`.
    /// Falls back to all-defaults if the file is absent or invalid.
    /// `ORGANON_DB` and `ORGANON_VECTORS_DB` override the paths either way.
    pub fn load() -> Self {
        Self::load_with(&|key| std::env::var(key).ok())
    }

    fn load_with(env: &dyn Fn(&str) -> Option<String>) -> Self {
        let home = organon_home_with(env);
        let path = env_path(env, "ORGANON_CONFIG").unwrap_or_else(|| home.join("config.toml"));
        let mut cfg = if path.exists() {
            Self::parse(&path).unwrap_or_else(|e| {
                debug!(
                    "config parse error in {}: {e}; using defaults",
                    path.display()
                );
                Self::defaults_in(&home)
            })
        } else {
            debug!("config file {} not found, using defaults", path.display());
            Self::defaults_in(&home)
        };
        cfg.apply_env(env);
        cfg
    }

    /// `ORGANON_DB` and `ORGANON_VECTORS_DB` override the configured paths.
    fn apply_env(&mut self, env: &dyn Fn(&str) -> Option<String>) {
        if let Some(db) = env_path(env, "ORGANON_DB") {
            self.indexer.db_path = db.to_string_lossy().into_owned();
        }
        if let Some(vectors) = env_path(env, "ORGANON_VECTORS_DB") {
            self.indexer.vectors_path = vectors.to_string_lossy().into_owned();
        }
    }

    fn defaults_in(home: &Path) -> Self {
        Self {
            indexer: IndexerConfig::in_home(home),
            ..Self::default()
        }
    }

    /// Load from a specific path (defaults if it does not exist), then apply
    /// `ORGANON_DB` / `ORGANON_VECTORS_DB`. Errors if the file is invalid TOML.
    pub fn load_from(path: &Path) -> Result<Self> {
        let mut cfg = if path.exists() {
            Self::parse(path)?
        } else {
            Self::default()
        };
        cfg.apply_env(&|key| std::env::var(key).ok());
        Ok(cfg)
    }

    fn parse(path: &Path) -> Result<Self> {
        let cfg = toml::from_str(&std::fs::read_to_string(path)?)?;
        debug!("config loaded from {}", path.display());
        Ok(cfg)
    }

    /// Write a default config file to the standard location (for `organon init`).
    pub fn write_default(path: &Path) -> Result<()> {
        let default = Self::default();
        let text = toml::to_string_pretty(&default)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn empty_values_are_ignored() {
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", ""),
            ("ORGANON_DB", ""),
            ("ORGANON_VECTORS_DB", ""),
            ("ORGANON_CONFIG", ""),
        ]);
        let cfg = OrgConfig::load_with(&env);
        assert_eq!(cfg.indexer.db_path, "/u/.organon/entities.db");
        assert_eq!(cfg.indexer.vectors_path, "/u/.organon/vectors");
    }

    #[test]
    fn tilde_expands_to_home() {
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", "~/sandbox"),
            ("ORGANON_DB", "~"),
        ]);
        assert_eq!(organon_home_with(&env), PathBuf::from("/u/sandbox"));
        assert_eq!(env_path(&env, "ORGANON_DB"), Some(PathBuf::from("/u")));
    }

    #[test]
    fn relative_home_is_made_absolute() {
        let env = env_of(&[("HOME", "/u"), ("ORGANON_HOME", "rel/home")]);
        let home = organon_home_with(&env);
        assert!(home.is_absolute(), "{}", home.display());
        assert_eq!(home, std::env::current_dir().unwrap().join("rel/home"));
    }

    #[test]
    fn home_defaults_to_dot_organon() {
        let env = env_of(&[("HOME", "/u")]);
        assert_eq!(organon_home_with(&env), PathBuf::from("/u/.organon"));
    }

    #[test]
    fn organon_home_moves_db_and_vectors_without_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("sandbox");
        let env = env_of(&[("HOME", "/u"), ("ORGANON_HOME", home.to_str().unwrap())]);

        let cfg = OrgConfig::load_with(&env);

        assert_eq!(
            cfg.indexer.db_path,
            home.join("entities.db").to_string_lossy()
        );
        assert_eq!(
            cfg.indexer.vectors_path,
            home.join("vectors").to_string_lossy()
        );
    }

    #[test]
    fn organon_home_config_file_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[indexer]\ndb_path = \"/custom/graph.db\"\n",
        )
        .unwrap();
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", dir.path().to_str().unwrap()),
        ]);

        assert_eq!(
            OrgConfig::load_with(&env).indexer.db_path,
            "/custom/graph.db"
        );
    }

    #[test]
    fn env_overrides_apply_without_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", dir.path().to_str().unwrap()),
            ("ORGANON_DB", "/x/e.db"),
            ("ORGANON_VECTORS_DB", "/x/vec"),
        ]);

        let cfg = OrgConfig::load_with(&env);

        assert_eq!(cfg.indexer.db_path, "/x/e.db");
        assert_eq!(cfg.indexer.vectors_path, "/x/vec");
    }

    #[test]
    fn explicit_config_path_wins_over_home() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("other.toml");
        std::fs::write(&file, "[indexer]\nvectors_path = \"/v\"\n").unwrap();
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", "/nowhere"),
            ("ORGANON_CONFIG", file.to_str().unwrap()),
        ]);

        assert_eq!(OrgConfig::load_with(&env).indexer.vectors_path, "/v");
    }

    #[test]
    fn invalid_config_falls_back_to_home_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "not = [valid").unwrap();
        let env = env_of(&[
            ("HOME", "/u"),
            ("ORGANON_HOME", dir.path().to_str().unwrap()),
        ]);

        assert_eq!(
            OrgConfig::load_with(&env).indexer.db_path,
            dir.path().join("entities.db").to_string_lossy()
        );
    }
}
