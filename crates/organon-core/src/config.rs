//! Organon configuration.
//!
//! Reads `~/.organon/config.toml` (or path in `ORGANON_CONFIG` env var).
//! All fields have sane defaults — the file is optional.

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
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            roots: vec![],
            index_interval_secs: 30,
            ignore_segments: vec![],
            use_git_timestamps: false,
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
    /// If true, summarize each file with ollama after embedding
    pub summarize: bool,
}

impl Default for IndexerConfig {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        Self {
            db_path: format!("{}/.organon/entities.db", home),
            vectors_path: format!("{}/.organon/vectors", home),
            embed_model: "BAAI/bge-small-en-v1.5".to_string(),
            max_file_size_mb: 100,
            summarize: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OllamaConfig {
    pub model: String,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self {
            model: "llama3.2".to_string(),
        }
    }
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
    pub search: SearchConfig,
    pub indexer: IndexerConfig,
    pub ollama: OllamaConfig,
    pub server: ServerConfig,
}

impl OrgConfig {
    /// Load from default location (`ORGANON_CONFIG` env → `~/.organon/config.toml`).
    /// Falls back to all-defaults if the file is absent.
    pub fn load() -> Self {
        let path = std::env::var("ORGANON_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(format!("{}/.organon/config.toml", home))
            });
        Self::load_from(&path).unwrap_or_else(|_| {
            debug!("config file not found or parse error, using defaults");
            Self::default()
        })
    }

    /// Load from a specific path. Returns error if file exists but is invalid TOML.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        // Env var overrides for critical paths
        let mut cfg: Self = toml::from_str(&text)?;
        if let Ok(db) = std::env::var("ORGANON_DB") {
            cfg.indexer.db_path = db;
        }
        if let Ok(model) = std::env::var("ORGANON_OLLAMA_MODEL") {
            cfg.ollama.model = model;
        }
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
