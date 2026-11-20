//! Query-time freshness of search hits, shared by the CLI and the MCP server.
//!
//! The Python bridge refreshes vector and FTS rows before answering a search
//! (see `ai/freshness.py`). This module is the Rust side: a final guard per hit
//! and parsing of the bridge's search output.

use std::path::Path;

use anyhow::Result;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::entity::LifecycleState;
use crate::graph::Graph;

/// What a search hit's file looks like now, compared with the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitState {
    /// Indexed and unchanged on disk.
    Live,
    /// Deleted on disk, or no longer an active entity: never return it.
    Gone,
    /// On disk, but its size or mtime differs from what the graph recorded.
    ChangedSinceIndexed,
}

/// Classify `path` for the final hit guard.
pub fn hit_state(graph: &Graph, path: &str) -> Result<HitState> {
    let Some(entity) = graph.get_by_path(path)? else {
        return Ok(HitState::Gone);
    };
    if entity.lifecycle == LifecycleState::Dead || crate::freshness::is_gone(Path::new(path)) {
        return Ok(HitState::Gone);
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(HitState::Gone);
    };
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    if meta.len() != entity.size_bytes || mtime.is_some_and(|m| m != entity.modified_at) {
        return Ok(HitState::ChangedSinceIndexed);
    }
    Ok(HitState::Live)
}

/// Counts reported by the bridge's query-time refresh.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshStats {
    pub refreshed: usize,
    pub removed: usize,
    /// Changed files left for later because the refresh budget ran out.
    pub pending: usize,
}

/// Bridge `search` output in either shape.
#[derive(Debug)]
pub struct BridgeSearch<T> {
    pub items: Vec<T>,
    /// `None` when the Python layer predates the refresh (plain list output).
    pub refresh: Option<RefreshStats>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Shape<T> {
    WithRefresh {
        items: Vec<T>,
        refresh: Option<RefreshStats>,
    },
    List(Vec<T>),
}

/// Parse a bridge search result: a JSON list (older Python layer) or
/// `{"items": [...], "refresh": {...}}`.
pub fn parse_bridge_search<T: DeserializeOwned>(output: &str) -> Result<BridgeSearch<T>> {
    Ok(match serde_json::from_str::<Shape<T>>(output)? {
        Shape::WithRefresh { items, refresh } => BridgeSearch { items, refresh },
        Shape::List(items) => BridgeSearch {
            items,
            refresh: None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::Entity;

    #[derive(Debug, serde::Deserialize)]
    struct Row {
        path: String,
    }

    #[test]
    fn parses_old_list_shape() {
        let out: BridgeSearch<Row> = parse_bridge_search(r#"[{"path":"/a","score":0.5}]"#).unwrap();
        assert_eq!(out.items.len(), 1);
        assert_eq!(out.items[0].path, "/a");
        assert_eq!(out.refresh, None);
    }

    #[test]
    fn parses_new_object_shape() {
        let out: BridgeSearch<Row> = parse_bridge_search(
            r#"{"items":[],"refresh":{"refreshed":1,"removed":2,"pending":3}}"#,
        )
        .unwrap();
        assert!(out.items.is_empty());
        assert_eq!(
            out.refresh,
            Some(RefreshStats {
                refreshed: 1,
                removed: 2,
                pending: 3
            })
        );
    }

    #[test]
    fn hit_state_live_gone_changed() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let graph = Graph::open(db.path().to_str().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::canonicalize(dir.path()).unwrap().join("a.py");
        std::fs::write(&file, "one\n").unwrap();
        let path = file.to_str().unwrap();
        graph.upsert(&Entity::from_path(path).unwrap()).unwrap();

        assert_eq!(hit_state(&graph, path).unwrap(), HitState::Live);
        std::fs::write(&file, "one plus more\n").unwrap();
        assert_eq!(
            hit_state(&graph, path).unwrap(),
            HitState::ChangedSinceIndexed
        );
        std::fs::remove_file(&file).unwrap();
        assert_eq!(hit_state(&graph, path).unwrap(), HitState::Gone);
        assert_eq!(
            hit_state(&graph, "/never/indexed.py").unwrap(),
            HitState::Gone
        );
    }
}
