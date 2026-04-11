use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use clap::ValueEnum;
use organon_core::{
    config::OrgConfig,
    graph::{entity_matches_filter, FindFilter, Graph},
};
use serde::{Deserialize, Serialize};

use crate::python::python_run_with_env;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Vector,
    Fts,
    Hybrid,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub score: f64,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchPage {
    pub items: Vec<SearchHit>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}

pub fn python_env(config: &OrgConfig) -> Vec<(String, String)> {
    vec![
        (
            "ORGANON_VECTORS_DB".to_string(),
            config.indexer.vectors_path.clone(),
        ),
        (
            "ORGANON_EMBED_MODEL".to_string(),
            config.indexer.embed_model.clone(),
        ),
        (
            "ORGANON_OLLAMA_MODEL".to_string(),
            config.ollama.model.clone(),
        ),
        (
            "ORGANON_SUMMARIZE".to_string(),
            if config.indexer.summarize { "1" } else { "0" }.to_string(),
        ),
    ]
}

pub fn default_search_mode(config: &OrgConfig) -> SearchMode {
    match config.search.default_mode.as_str() {
        "fts" => SearchMode::Fts,
        "hybrid" => SearchMode::Hybrid,
        _ => SearchMode::Vector,
    }
}

pub fn search_entities(
    query: &str,
    limit: usize,
    offset: usize,
    dir: Option<&Path>,
    mode: SearchMode,
    metadata_filter: &FindFilter,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<SearchPage> {
    let path_prefix = dir.map(|p| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_string()
    });

    let mut merged: BTreeMap<String, (f64, bool, bool)> = BTreeMap::new();
    let desired = (limit + offset).max(1);
    let candidate_limit = if metadata_filter_requested(metadata_filter) {
        desired.saturating_mul(12)
    } else {
        desired.saturating_mul(4)
    };

    if matches!(mode, SearchMode::Vector | SearchMode::Hybrid) {
        let output = python_run_with_env(
            &[
                "-c",
                &format!(
                    "from ai.embeddings.store import search; import json; \
                     print(json.dumps(search({:?}, limit={}, db_path={:?}, path_prefix={})))",
                    query,
                    candidate_limit,
                    config.indexer.vectors_path,
                    path_prefix
                        .as_ref()
                        .map(|p| format!("{p:?}"))
                        .unwrap_or_else(|| "None".to_string())
                ),
            ],
            &python_env(config),
        )?;
        let results: Vec<serde_json::Value> = serde_json::from_str(&output)?;
        for row in results {
            if let Some(path) = row["path"].as_str() {
                let score = row["score"].as_f64().unwrap_or(0.0);
                merged
                    .entry(path.to_string())
                    .and_modify(|entry| {
                        entry.0 += score * 0.7;
                        entry.1 = true;
                    })
                    .or_insert((score * 0.7, true, false));
            }
        }
    }

    if matches!(mode, SearchMode::Fts | SearchMode::Hybrid) {
        let graph = Graph::open(db_path.to_string_lossy().as_ref())?;
        let mut results = graph.fts_search(query, candidate_limit)?;
        if let Some(prefix) = &path_prefix {
            results.retain(|(path, _)| path.starts_with(prefix));
        }
        for (path, rank) in results {
            let score = 1.0 / (1.0 + rank.max(0.0));
            let weight = if matches!(mode, SearchMode::Hybrid) {
                0.3
            } else {
                1.0
            };
            merged
                .entry(path)
                .and_modify(|entry| {
                    entry.0 += score * weight;
                    entry.2 = true;
                })
                .or_insert((score * weight, false, true));
        }
    }

    let mut results: Vec<_> = merged.into_iter().collect();
    results.sort_by(|a, b| b.1 .0.total_cmp(&a.1 .0));
    if metadata_filter_requested(metadata_filter) {
        let graph = Graph::open(db_path.to_string_lossy().as_ref())?;
        results = apply_metadata_filter(results, &graph, metadata_filter)?;
    }
    let total = results.len();
    let items = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(path, (score, from_vector, from_fts))| SearchHit {
            path,
            score,
            source: match (from_vector, from_fts) {
                (true, true) => "hybrid".to_string(),
                (true, false) => "vector".to_string(),
                (false, true) => "fts".to_string(),
                (false, false) => "-".to_string(),
            },
        })
        .collect();

    Ok(SearchPage {
        items,
        total,
        limit,
        offset,
        has_more: offset + limit < total,
    })
}

fn metadata_filter_requested(filter: &FindFilter) -> bool {
    filter.state.is_some()
        || filter.extension.is_some()
        || filter.created_after.is_some()
        || filter.modified_after.is_some()
        || filter.larger_than.is_some()
}

fn apply_metadata_filter(
    results: Vec<(String, (f64, bool, bool))>,
    graph: &Graph,
    filter: &FindFilter,
) -> Result<Vec<(String, (f64, bool, bool))>> {
    let mut filtered = Vec::with_capacity(results.len());
    for (path, meta) in results {
        if let Some(entity) = graph.get_by_path(&path)? {
            if entity_matches_filter(&entity, filter) {
                filtered.push((path, meta));
            }
        }
    }
    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use organon_core::{
        entity::{Entity, LifecycleState},
        graph::Graph,
    };
    use tempfile::NamedTempFile;

    use super::*;

    fn test_entity(path: &str, extension: &str, modified_at: i64) -> Entity {
        Entity {
            id: path.to_string(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap().to_string(),
            extension: Some(extension.to_string()),
            size_bytes: 64,
            created_at: modified_at - 10,
            modified_at,
            accessed_at: modified_at,
            lifecycle: LifecycleState::Active,
            content_hash: Some(format!("hash-{path}")),
            summary: None,
            git_author: None,
        }
    }

    #[test]
    fn apply_metadata_filter_keeps_matching_hits() {
        let file = NamedTempFile::new().unwrap();
        let graph = Graph::open(file.path().to_string_lossy().as_ref()).unwrap();
        graph.upsert(&test_entity("/tmp/a.rs", "rs", 200)).unwrap();
        graph.upsert(&test_entity("/tmp/b.py", "py", 100)).unwrap();

        let filtered = apply_metadata_filter(
            vec![
                ("/tmp/a.rs".to_string(), (0.9, true, false)),
                ("/tmp/b.py".to_string(), (0.8, true, false)),
            ],
            &graph,
            &FindFilter {
                extension: Some("rs".to_string()),
                modified_after: Some(150),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].0, "/tmp/a.rs");
    }
}
