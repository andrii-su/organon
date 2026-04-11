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

/// Explanation block attached to a search hit when `explain=true`.
/// All fields are real signals from the ranking pipeline — no fabricated rationale.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SearchExplanation {
    /// Raw vector similarity (cosine, 0–1) before weight is applied
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_score: Option<f64>,
    /// vector_score × weight (0.7 hybrid, 1.0 vector-only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vector_contribution: Option<f64>,
    /// Raw BM25 rank from SQLite FTS5 (negative; lower = more relevant)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fts_rank: Option<f64>,
    /// Normalized FTS score: 1 / (1 + |rank|), range 0–1
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fts_score: Option<f64>,
    /// fts_score × weight (0.3 hybrid, 1.0 fts-only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fts_contribution: Option<f64>,
    /// Query terms (lowercase) found verbatim in the file path
    pub matched_terms: Vec<String>,
    /// True when at least one query term appears in the file path
    pub path_match: bool,
    /// Content snippet stored in the vector index (first ~512 chars of extracted text)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_preview: Option<String>,
    /// 2–5 human-readable reason lines summarising why this hit ranked here
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchHit {
    pub path: String,
    pub score: f64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<SearchExplanation>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SearchPage {
    pub items: Vec<SearchHit>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
    pub has_more: bool,
}

// Internal per-path accumulator used during merge phase.
#[derive(Default)]
pub(crate) struct MergeEntry {
    pub combined_score: f64,
    pub from_vector: bool,
    pub from_fts: bool,
    pub vector_raw_score: Option<f64>,
    pub fts_raw_rank: Option<f64>,
    pub fts_normalized_score: Option<f64>,
    pub text_preview: Option<String>,
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
    explain: bool,
) -> Result<SearchPage> {
    let path_prefix = dir.map(|p| {
        std::fs::canonicalize(p)
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .to_string()
    });

    let mut merged: BTreeMap<String, MergeEntry> = BTreeMap::new();
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
        let weight = if matches!(mode, SearchMode::Hybrid) {
            0.7
        } else {
            1.0
        };
        for row in results {
            if let Some(path) = row["path"].as_str() {
                let raw_score = row["score"].as_f64().unwrap_or(0.0);
                let preview = row["text_preview"].as_str().map(|s| s.to_string());
                merged
                    .entry(path.to_string())
                    .and_modify(|entry| {
                        entry.combined_score += raw_score * weight;
                        entry.from_vector = true;
                        if entry.vector_raw_score.is_none() {
                            entry.vector_raw_score = Some(raw_score);
                        }
                        if entry.text_preview.is_none() {
                            entry.text_preview = preview.clone();
                        }
                    })
                    .or_insert(MergeEntry {
                        combined_score: raw_score * weight,
                        from_vector: true,
                        from_fts: false,
                        vector_raw_score: Some(raw_score),
                        fts_raw_rank: None,
                        fts_normalized_score: None,
                        text_preview: preview,
                    });
            }
        }
    }

    if matches!(mode, SearchMode::Fts | SearchMode::Hybrid) {
        let graph = Graph::open(db_path.to_string_lossy().as_ref())?;
        let mut results = graph.fts_search(query, candidate_limit)?;
        if let Some(prefix) = &path_prefix {
            results.retain(|(path, _)| path.starts_with(prefix));
        }
        let weight = if matches!(mode, SearchMode::Hybrid) {
            0.3
        } else {
            1.0
        };
        for (path, rank) in results {
            let normalized = 1.0 / (1.0 + rank.max(0.0));
            merged
                .entry(path)
                .and_modify(|entry| {
                    entry.combined_score += normalized * weight;
                    entry.from_fts = true;
                    if entry.fts_raw_rank.is_none() {
                        entry.fts_raw_rank = Some(rank);
                        entry.fts_normalized_score = Some(normalized);
                    }
                })
                .or_insert(MergeEntry {
                    combined_score: normalized * weight,
                    from_vector: false,
                    from_fts: true,
                    vector_raw_score: None,
                    fts_raw_rank: Some(rank),
                    fts_normalized_score: Some(normalized),
                    text_preview: None,
                });
        }
    }

    let mut results: Vec<_> = merged.into_iter().collect();
    results.sort_by(|a, b| b.1.combined_score.total_cmp(&a.1.combined_score));
    if metadata_filter_requested(metadata_filter) {
        let graph = Graph::open(db_path.to_string_lossy().as_ref())?;
        results = apply_metadata_filter(results, &graph, metadata_filter)?;
    }
    let total = results.len();
    let items = results
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(path, entry)| {
            let source = match (entry.from_vector, entry.from_fts) {
                (true, true) => "hybrid".to_string(),
                (true, false) => "vector".to_string(),
                (false, true) => "fts".to_string(),
                (false, false) => "-".to_string(),
            };
            let explanation = if explain {
                Some(build_explanation(query, &path, &entry, mode))
            } else {
                None
            };
            SearchHit {
                score: entry.combined_score,
                path,
                source,
                explanation,
            }
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

/// Build a `SearchExplanation` from raw pipeline signals.
/// Only real signals are reported — no fabricated semantic rationale.
fn build_explanation(
    query: &str,
    path: &str,
    entry: &MergeEntry,
    mode: SearchMode,
) -> SearchExplanation {
    let query_terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    let path_lower = path.to_lowercase();
    let matched_terms: Vec<String> = query_terms
        .iter()
        .filter(|t| path_lower.contains(t.as_str()))
        .cloned()
        .collect();
    let path_match = !matched_terms.is_empty();

    let mut reasons: Vec<String> = Vec::new();

    match (entry.from_vector, entry.from_fts) {
        (true, true) => reasons.push("matched in both vector and FTS channels".to_string()),
        (true, false) => reasons.push("matched via semantic vector search".to_string()),
        (false, true) => reasons.push("matched via full-text search (FTS)".to_string()),
        _ => {}
    }

    if path_match {
        reasons.push(format!(
            "path contains query term(s): {}",
            matched_terms.join(", ")
        ));
    }

    if let Some(vs) = entry.vector_raw_score {
        let label = if vs >= 0.8 {
            "high"
        } else if vs >= 0.6 {
            "moderate"
        } else {
            "low"
        };
        reasons.push(format!("{label} semantic similarity ({vs:.3})"));
    }

    if let Some(fs) = entry.fts_normalized_score {
        let label = if fs >= 0.5 { "strong" } else { "weak" };
        reasons.push(format!("{label} FTS match (score {fs:.3})"));
    }

    let vector_weight = if matches!(mode, SearchMode::Hybrid) {
        0.7
    } else {
        1.0
    };
    let fts_weight = if matches!(mode, SearchMode::Hybrid) {
        0.3
    } else {
        1.0
    };

    SearchExplanation {
        vector_score: entry.vector_raw_score,
        vector_contribution: entry.vector_raw_score.map(|s| s * vector_weight),
        fts_rank: entry.fts_raw_rank,
        fts_score: entry.fts_normalized_score,
        fts_contribution: entry.fts_normalized_score.map(|s| s * fts_weight),
        matched_terms,
        path_match,
        text_preview: entry.text_preview.clone(),
        reasons,
    }
}

fn metadata_filter_requested(filter: &FindFilter) -> bool {
    filter.state.is_some()
        || filter.extension.is_some()
        || filter.created_after.is_some()
        || filter.modified_after.is_some()
        || filter.larger_than.is_some()
}

fn apply_metadata_filter(
    results: Vec<(String, MergeEntry)>,
    graph: &Graph,
    filter: &FindFilter,
) -> Result<Vec<(String, MergeEntry)>> {
    let mut filtered = Vec::with_capacity(results.len());
    for (path, entry) in results {
        if let Some(entity) = graph.get_by_path(&path)? {
            if entity_matches_filter(&entity, filter) {
                filtered.push((path, entry));
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
                (
                    "/tmp/a.rs".to_string(),
                    MergeEntry {
                        combined_score: 0.9,
                        from_vector: true,
                        ..Default::default()
                    },
                ),
                (
                    "/tmp/b.py".to_string(),
                    MergeEntry {
                        combined_score: 0.8,
                        from_vector: true,
                        ..Default::default()
                    },
                ),
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

    #[test]
    fn build_explanation_fts_only_hit() {
        let entry = MergeEntry {
            combined_score: 0.71,
            from_vector: false,
            from_fts: true,
            fts_raw_rank: Some(-1.4),
            fts_normalized_score: Some(1.0 / (1.0 + 1.4_f64.max(0.0))),
            ..Default::default()
        };
        let exp = build_explanation("auth token", "/src/auth/token.rs", &entry, SearchMode::Fts);
        assert!(
            !exp.matched_terms.is_empty(),
            "should match 'auth' and 'token' in path"
        );
        assert!(exp.path_match);
        assert!(exp.fts_score.is_some());
        assert!(exp.vector_score.is_none());
        assert!(exp.reasons.iter().any(|r| r.contains("full-text search")));
        assert!(exp.reasons.iter().any(|r| r.contains("path contains")));
    }

    #[test]
    fn build_explanation_vector_only_hit() {
        let entry = MergeEntry {
            combined_score: 0.85,
            from_vector: true,
            from_fts: false,
            vector_raw_score: Some(0.85),
            text_preview: Some("handles JWT token validation".to_string()),
            ..Default::default()
        };
        let exp = build_explanation("jwt auth", "/src/middleware.rs", &entry, SearchMode::Vector);
        assert!(exp.vector_score == Some(0.85));
        assert!(exp.fts_score.is_none());
        assert!(exp.text_preview.is_some());
        assert!(exp
            .reasons
            .iter()
            .any(|r| r.contains("semantic vector search")));
        assert!(exp
            .reasons
            .iter()
            .any(|r| r.contains("semantic similarity")));
    }

    #[test]
    fn build_explanation_hybrid_hit_shows_both_contributions() {
        let raw_vec = 0.82_f64;
        let raw_fts = -0.5_f64;
        let fts_norm = 1.0 / (1.0 + raw_fts.max(0.0));
        let entry = MergeEntry {
            combined_score: raw_vec * 0.7 + fts_norm * 0.3,
            from_vector: true,
            from_fts: true,
            vector_raw_score: Some(raw_vec),
            fts_raw_rank: Some(raw_fts),
            fts_normalized_score: Some(fts_norm),
            text_preview: Some("graph entity search".to_string()),
        };
        let exp = build_explanation("graph search", "/src/graph.rs", &entry, SearchMode::Hybrid);
        assert!(exp.vector_contribution.is_some());
        assert!(exp.fts_contribution.is_some());
        // In hybrid mode weights are 0.7 / 0.3
        assert!((exp.vector_contribution.unwrap() - raw_vec * 0.7).abs() < 1e-9);
        assert!((exp.fts_contribution.unwrap() - fts_norm * 0.3).abs() < 1e-9);
        assert!(exp
            .reasons
            .iter()
            .any(|r| r.contains("both vector and FTS")));
    }

    #[test]
    fn search_hit_explanation_omitted_when_explain_false() {
        // explanation field serializes as absent when None
        let hit = SearchHit {
            path: "/tmp/x.rs".to_string(),
            score: 0.5,
            source: "vector".to_string(),
            explanation: None,
        };
        let json = serde_json::to_value(&hit).unwrap();
        assert!(
            json.get("explanation").is_none(),
            "explanation should be absent"
        );
    }
}
