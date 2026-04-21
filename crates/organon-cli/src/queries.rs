//! Saved queries — named search/find definitions persisted in
//! `~/.organon/saved_queries.json`.
//!
//! Each entry captures a kind (`"find"` or `"search"`) plus the filter/search
//! parameters, so that `organon query run <name>` reproduces the exact command.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

// ── storage ───────────────────────────────────────────────────────────────────

pub fn queries_path() -> PathBuf {
    std::env::var("ORGANON_QUERIES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(format!("{home}/.organon/saved_queries.json"))
        })
}

// ── data model ────────────────────────────────────────────────────────────────

/// A persisted query definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedQuery {
    /// `"find"` or `"search"`
    pub kind: String,
    /// Free-text search query (search only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Search mode: vector / fts / hybrid (search only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub larger_than_mb: Option<u64>,
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: i64,
}

impl SavedQuery {
    pub fn summary(&self) -> String {
        let mut parts = vec![self.kind.clone()];
        if let Some(q) = &self.query {
            parts.push(format!("{q:?}"));
        }
        if let Some(s) = &self.state {
            parts.push(format!("state:{s}"));
        }
        if let Some(e) = &self.extension {
            parts.push(format!("ext:{e}"));
        }
        if let Some(m) = &self.mode {
            parts.push(format!("mode:{m}"));
        }
        if let Some(ma) = &self.modified_after {
            parts.push(format!("modified>{ma}"));
        }
        if let Some(ca) = &self.created_after {
            parts.push(format!("created>{ca}"));
        }
        if let Some(b) = self.larger_than_mb {
            parts.push(format!("size>{b}mb"));
        }
        parts.push(format!("limit:{}", self.limit));
        parts.join("  ")
    }
}

pub type QueryStore = BTreeMap<String, SavedQuery>;

pub fn load() -> Result<QueryStore> {
    let path = queries_path();
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&text)?)
}

pub fn persist(store: &QueryStore) -> Result<()> {
    let path = queries_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(store)? + "\n")?;
    Ok(())
}

pub fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn get(name: &str) -> Result<SavedQuery> {
    let store = load()?;
    store
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("no saved query named '{name}'"))
}

pub fn insert(name: &str, query: SavedQuery) -> Result<()> {
    let mut store = load()?;
    store.insert(name.to_string(), query);
    persist(&store)
}

pub fn remove(name: &str) -> Result<()> {
    let mut store = load()?;
    if store.remove(name).is_none() {
        bail!("no saved query named '{name}'");
    }
    persist(&store)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_find_query() -> SavedQuery {
        SavedQuery {
            kind: "find".into(),
            query: None,
            mode: None,
            state: Some("dormant".into()),
            extension: Some("rs".into()),
            created_after: None,
            modified_after: None,
            larger_than_mb: None,
            limit: 20,
            description: Some("stale Rust files".into()),
            created_at: 1_000_000,
        }
    }

    fn make_search_query() -> SavedQuery {
        SavedQuery {
            kind: "search".into(),
            query: Some("auth token".into()),
            mode: Some("hybrid".into()),
            state: None,
            extension: None,
            created_after: None,
            modified_after: None,
            larger_than_mb: None,
            limit: 10,
            description: None,
            created_at: 1_000_001,
        }
    }

    #[test]
    fn roundtrip_json() {
        let mut store: QueryStore = BTreeMap::new();
        store.insert("stale-rs".into(), make_find_query());
        store.insert("auth".into(), make_search_query());

        let json = serde_json::to_string_pretty(&store).unwrap();
        let restored: QueryStore = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.len(), 2);
        assert_eq!(restored["stale-rs"].kind, "find");
        assert_eq!(restored["auth"].query.as_deref(), Some("auth token"));
    }

    #[test]
    fn summary_find() {
        let s = make_find_query().summary();
        assert!(s.contains("find"));
        assert!(s.contains("state:dormant"));
        assert!(s.contains("ext:rs"));
    }

    #[test]
    fn summary_search() {
        let s = make_search_query().summary();
        assert!(s.contains("search"));
        assert!(s.contains("auth token"));
        assert!(s.contains("mode:hybrid"));
    }

    #[test]
    fn missing_name_errors() {
        // load on nonexistent path returns empty store
        std::env::set_var("ORGANON_QUERIES", "/tmp/organon_test_missing_queries.json");
        let _ = std::fs::remove_file("/tmp/organon_test_missing_queries.json");
        let store = load().unwrap();
        assert!(store.is_empty());
    }
}
