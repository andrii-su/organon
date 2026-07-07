use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use anyhow::{bail, Result};
use axum::Router;
use chrono::NaiveDate;
use log::debug;
use organon_core::{
    config::OrgConfig,
    entity::Entity,
    graph::{entity_matches_filter, FindFilter, Graph},
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    schemars,
    schemars::JsonSchema,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::{
        stdio,
        streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        },
    },
    ErrorData as McpError, Json, RoleServer, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SearchMode {
    Vector,
    Fts,
    Hybrid,
}

/// Explanation block attached to a search hit when `explain=true`.
/// All fields are real signals from the ranking pipeline.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SearchExplanation {
    /// Raw vector similarity (0–1) before weight is applied
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
    /// Content snippet stored in the vector index
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_preview: Option<String>,
    /// 2–5 human-readable reason lines
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub path: String,
    pub score: f64,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<SearchExplanation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EntityRecord {
    pub id: String,
    pub path: String,
    pub name: String,
    pub extension: Option<String>,
    pub size_bytes: u64,
    pub created_at: i64,
    pub modified_at: i64,
    pub accessed_at: i64,
    pub lifecycle: String,
    pub content_hash: Option<String>,
    pub summary: Option<String>,
    pub git_author: Option<String>,
}

impl From<Entity> for EntityRecord {
    fn from(value: Entity) -> Self {
        Self {
            id: value.id,
            path: value.path,
            name: value.name,
            extension: value.extension,
            size_bytes: value.size_bytes,
            created_at: value.created_at,
            modified_at: value.modified_at,
            accessed_at: value.accessed_at,
            lifecycle: value.lifecycle.as_str().to_string(),
            content_hash: value.content_hash,
            summary: value.summary,
            git_author: value.git_author,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleRow {
    pub path: String,
    pub lifecycle: String,
    pub size_bytes: u64,
    pub modified_at: i64,
    pub accessed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphStats {
    pub total_entities: usize,
    pub total_relations: usize,
    pub total_bytes: u64,
    pub by_lifecycle: BTreeMap<String, usize>,
    pub db_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RelationGraph {
    pub nodes: Vec<String>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FileContent {
    pub path: String,
    pub content: String,
    pub chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchFilesResponse {
    pub items: Vec<SearchHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextPack {
    pub query: Option<String>,
    pub seed_path: Option<String>,
    pub scope: Option<String>,
    pub budget_chars: usize,
    pub items: Vec<ContextItem>,
    pub total_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ContextItem {
    pub path: String,
    pub score: f64,
    pub source: String,
    pub lifecycle: Option<String>,
    pub summary: Option<String>,
    pub snippet: Option<String>,
    pub relations: Vec<GraphEdge>,
    pub recent_history: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetEntityResponse {
    pub entity: Option<EntityRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LifecycleListResponse {
    pub items: Vec<LifecycleRow>,
}

#[derive(Clone)]
pub struct McpService {
    db_path: PathBuf,
    config: OrgConfig,
    scope: Option<PathBuf>,
}

#[derive(Clone)]
pub struct OrganonMcpServer {
    service: Arc<McpService>,
    #[allow(dead_code)] // used by #[tool_router] proc macro via tool_router()
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchFilesRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub path_prefix: Option<String>,
    pub mode: Option<SearchMode>,
    pub state: Option<String>,
    pub extension: Option<String>,
    pub created_after: Option<String>,
    pub modified_after: Option<String>,
    /// When true, each hit includes an explanation block with score breakdown,
    /// matched terms, and human-readable ranking reasons.
    pub explain: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BuildContextRequest {
    /// Task or topic query. Optional when path is provided.
    pub query: Option<String>,
    /// Seed file or directory.
    pub path: Option<String>,
    /// Optional narrower directory prefix. Cannot widen the MCP session scope.
    pub path_prefix: Option<String>,
    /// Approximate character budget for snippets and context records.
    pub budget_chars: Option<usize>,
    /// Max candidate files.
    pub limit: Option<usize>,
    pub mode: Option<SearchMode>,
    pub state: Option<String>,
    pub extension: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PathRequest {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LifecycleRequest {
    pub state: String,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GraphRequest {
    pub path: String,
    pub depth: Option<u8>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetHistoryRequest {
    /// Absolute path of the file to retrieve history for.
    pub path: String,
    /// Maximum number of entries to return (default 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HistoryResponse {
    pub path: String,
    /// History entries, newest first. Fields: event, recorded_at, old_lifecycle, new_lifecycle,
    /// old_path (rename), size_bytes, content_hash.
    pub entries: Vec<serde_json::Value>,
    pub total: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ImpactRequest {
    /// Absolute path of the file to analyse.
    pub path: String,
    /// BFS depth for reverse-dependency traversal (default 5).
    pub depth: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ImpactResponse {
    pub path: String,
    pub depth: u8,
    pub total: usize,
    pub direct_dependents: usize,
    /// "none" | "low" | "medium" | "high" based on total reverse-dep count.
    pub risk_level: String,
    /// Impact entries, sorted by depth then path. Fields: path, kind, depth.
    pub entries: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ListSavedQueriesRequest {
    // No parameters — returns all saved queries.
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListSavedQueriesResponse {
    /// All saved queries. Each entry includes all definition fields plus "name".
    pub queries: Vec<serde_json::Value>,
    pub total: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunSavedQueryRequest {
    /// Name of the saved query to run.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RunSavedQueryResponse {
    /// "find" or "search" — the kind of query that was run.
    pub kind: String,
    /// Result items. For find: entity objects. For search: search hit objects.
    pub items: Vec<serde_json::Value>,
    pub total: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FindDuplicatesRequest {
    // No parameters — returns exact duplicates only.
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FindDuplicatesResponse {
    /// Groups of files sharing the same content hash. Fields: content_hash, paths.
    pub exact: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchSimilarRequest {
    /// Absolute path of the reference file.
    pub path: String,
    /// Max results (default 10).
    pub limit: Option<usize>,
    /// Optional directory prefix to scope results.
    pub path_prefix: Option<String>,
}

pub struct BuildContextParams<'a> {
    pub query: Option<&'a str>,
    pub seed_path: Option<&'a str>,
    pub path_prefix: Option<&'a str>,
    pub budget_chars: usize,
    pub limit: usize,
    pub mode: SearchMode,
    pub metadata_filter: &'a FindFilter,
}

impl McpService {
    pub fn new(db_path: PathBuf, config: OrgConfig) -> Self {
        Self {
            db_path,
            config,
            scope: None,
        }
    }

    pub fn new_with_scope(db_path: PathBuf, config: OrgConfig, scope: Option<PathBuf>) -> Self {
        Self {
            db_path,
            config,
            scope,
        }
    }

    pub fn from_config(config: OrgConfig) -> Self {
        Self::new(PathBuf::from(&config.indexer.db_path), config)
    }

    pub fn from_config_with_scope(config: OrgConfig, scope: Option<PathBuf>) -> Self {
        Self::new_with_scope(PathBuf::from(&config.indexer.db_path), config, scope)
    }
}

impl McpService {
    pub fn get_entity(&self, path: &str) -> Result<Option<Entity>> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        self.open_graph()?.get_by_path(&path)
    }

    pub fn list_by_lifecycle(&self, state: &str, limit: usize) -> Result<Vec<LifecycleRow>> {
        let valid = ["born", "active", "dormant", "archived", "dead"];
        if !valid.contains(&state) {
            bail!("invalid state '{state}'");
        }

        let mut rows: Vec<_> = self
            .open_graph()?
            .all()?
            .into_iter()
            .filter(|entity| self.path_in_scope(&entity.path))
            .filter(|entity| entity.lifecycle.as_str() == state)
            .map(|entity| LifecycleRow {
                path: entity.path,
                lifecycle: entity.lifecycle.as_str().to_string(),
                size_bytes: entity.size_bytes,
                modified_at: entity.modified_at,
                accessed_at: entity.accessed_at,
            })
            .collect();

        rows.sort_by_key(|row| Reverse(row.accessed_at));
        rows.truncate(limit);
        Ok(rows)
    }

    pub fn graph_stats(&self) -> Result<GraphStats> {
        let graph = self.open_graph()?;
        let entities: Vec<_> = graph
            .all()?
            .into_iter()
            .filter(|entity| self.path_in_scope(&entity.path))
            .collect();
        let relations: Vec<_> = graph
            .all_relations()?
            .into_iter()
            .filter(|(from, to, _)| self.path_in_scope(from) && self.path_in_scope(to))
            .collect();

        let mut by_lifecycle = BTreeMap::new();
        let mut total_bytes = 0u64;
        for entity in &entities {
            *by_lifecycle
                .entry(entity.lifecycle.as_str().to_string())
                .or_insert(0) += 1;
            total_bytes += entity.size_bytes;
        }

        Ok(GraphStats {
            total_entities: entities.len(),
            total_relations: relations.len(),
            total_bytes,
            by_lifecycle,
            db_path: self.db_path.display().to_string(),
        })
    }

    pub fn get_history(&self, path: &str, limit: usize) -> Result<HistoryResponse> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        let entries = self.open_graph()?.get_history(&path, limit)?;
        let total = entries.len();
        let entries_json: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(HistoryResponse {
            path,
            entries: entries_json,
            total,
        })
    }

    pub fn get_impact(&self, path: &str, depth: u8) -> Result<ImpactResponse> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        let entries: Vec<_> = self
            .open_graph()?
            .reverse_deps(&path, depth)?
            .into_iter()
            .filter(|entry| self.path_in_scope(&entry.path))
            .collect();
        let direct_dependents = entries.iter().filter(|e| e.depth == 1).count();
        let total = entries.len();
        let risk_level = match total {
            0 => "none",
            1..=3 => "low",
            4..=10 => "medium",
            _ => "high",
        }
        .to_string();
        let entries_json = entries
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(ImpactResponse {
            total,
            direct_dependents,
            risk_level,
            path,
            depth,
            entries: entries_json,
        })
    }

    pub fn list_saved_queries(&self) -> Result<ListSavedQueriesResponse> {
        let path = self.saved_queries_path();
        if !path.exists() {
            return Ok(ListSavedQueriesResponse {
                queries: vec![],
                total: 0,
            });
        }
        let text = std::fs::read_to_string(&path)?;
        let store: serde_json::Value = serde_json::from_str(&text)?;
        let queries: Vec<serde_json::Value> = store
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(name, val)| {
                        let mut v = val.clone();
                        if let serde_json::Value::Object(ref mut obj) = v {
                            obj.insert("name".to_string(), serde_json::Value::String(name.clone()));
                        }
                        v
                    })
                    .collect()
            })
            .unwrap_or_default();
        let total = queries.len();
        Ok(ListSavedQueriesResponse { queries, total })
    }

    pub fn run_saved_query(&self, name: &str) -> Result<RunSavedQueryResponse> {
        let path = self.saved_queries_path();
        if !path.exists() {
            bail!("no saved queries file found");
        }
        let text = std::fs::read_to_string(&path)?;
        let store: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&text)?;
        let sq = store
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("no saved query named '{name}'"))?;

        let kind = sq["kind"].as_str().unwrap_or("find");
        let limit = sq.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;

        match kind {
            "find" => {
                let state = sq.get("state").and_then(|v| v.as_str()).map(str::to_string);
                let extension = sq
                    .get("extension")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let filter = build_find_filter(state, extension, None, None, limit)?;
                let graph = self.open_graph()?;
                let items: Vec<serde_json::Value> = graph
                    .find(&filter)?
                    .into_iter()
                    .filter(|e| self.path_in_scope(&e.path))
                    .filter_map(|e| serde_json::to_value(e).ok())
                    .collect();
                let total = items.len();
                Ok(RunSavedQueryResponse {
                    kind: "find".to_string(),
                    items,
                    total,
                })
            }
            "search" => {
                let query_str = sq
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let mode_str = sq.get("mode").and_then(|v| v.as_str()).unwrap_or("vector");
                let mode = match mode_str {
                    "fts" => SearchMode::Fts,
                    "hybrid" => SearchMode::Hybrid,
                    _ => SearchMode::Vector,
                };
                let state = sq.get("state").and_then(|v| v.as_str()).map(str::to_string);
                let extension = sq
                    .get("extension")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let filter = build_find_filter(state, extension, None, None, limit)?;
                let items: Vec<serde_json::Value> = self
                    .search_files(&query_str, limit, None, mode, &filter, false)?
                    .into_iter()
                    .filter_map(|h| serde_json::to_value(h).ok())
                    .collect();
                let total = items.len();
                Ok(RunSavedQueryResponse {
                    kind: "search".to_string(),
                    items,
                    total,
                })
            }
            other => bail!("unknown query kind '{other}'"),
        }
    }

    pub fn find_duplicates(&self) -> Result<FindDuplicatesResponse> {
        let graph = self.open_graph()?;
        let exact = graph
            .exact_duplicates()?
            .into_iter()
            .filter_map(|mut group| {
                group.paths.retain(|path| self.path_in_scope(path));
                (group.paths.len() > 1).then_some(group)
            })
            .map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(FindDuplicatesResponse { exact })
    }

    pub fn search_similar(
        &self,
        path: &str,
        limit: usize,
        path_prefix: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        let path_prefix = self.effective_prefix(path_prefix)?;
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.embeddings.store import search_by_path; import json; \
                 print(json.dumps(search_by_path({:?}, limit={limit}, db_path={:?}, path_prefix={})))",
                path,
                self.config.indexer.vectors_path,
                path_prefix
                    .as_deref()
                    .map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "None".to_string()),
            ),
        ])?;
        let rows: Vec<serde_json::Value> = serde_json::from_str(&output)?;
        Ok(rows
            .into_iter()
            .filter_map(|r| {
                Some(SearchHit {
                    path: r["path"].as_str()?.to_string(),
                    score: r["score"].as_f64().unwrap_or(0.0),
                    source: "vector".to_string(),
                    explanation: None,
                })
            })
            .collect())
    }

    pub fn get_graph(&self, path: &str, depth: u8) -> Result<RelationGraph> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        let graph = self.open_graph()?;
        let depth = depth.min(3);
        let mut visited = BTreeSet::new();
        let mut edges = Vec::new();
        let mut seen_edges = BTreeSet::new();
        let mut queue = VecDeque::from([(path, 0u8)]);

        while let Some((current, level)) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if level >= depth {
                continue;
            }

            for (from, to, kind) in graph.get_relations(&current)? {
                if !self.path_in_scope(&from) || !self.path_in_scope(&to) {
                    continue;
                }
                let edge_key = format!("{from}\n{to}\n{kind}");
                if seen_edges.insert(edge_key) {
                    edges.push(GraphEdge {
                        from: from.clone(),
                        to: to.clone(),
                        kind: kind.clone(),
                    });
                }
                let neighbor = if from == current { to } else { from };
                if !visited.contains(&neighbor) {
                    queue.push_back((neighbor, level + 1));
                }
            }
        }

        Ok(RelationGraph {
            nodes: visited.into_iter().collect(),
            edges,
        })
    }

    pub fn search_files(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
        mode: SearchMode,
        metadata_filter: &FindFilter,
        explain: bool,
    ) -> Result<Vec<SearchHit>> {
        let path_prefix = self.effective_prefix(path_prefix)?;
        let mut merged: BTreeMap<String, MergeEntry> = BTreeMap::new();
        let candidate_limit = if metadata_filter_requested(metadata_filter) {
            limit.saturating_mul(12).max(12)
        } else {
            limit.saturating_mul(4).max(4)
        };

        if matches!(mode, SearchMode::Vector | SearchMode::Hybrid) {
            let results = self.vector_search(query, candidate_limit, path_prefix.as_deref())?;
            let weight = if matches!(mode, SearchMode::Hybrid) {
                0.7
            } else {
                1.0
            };
            for row in results {
                merged
                    .entry(row.path)
                    .and_modify(|entry| {
                        entry.combined_score += row.score * weight;
                        entry.from_vector = true;
                        if entry.vector_raw_score.is_none() {
                            entry.vector_raw_score = Some(row.score);
                        }
                        if entry.text_preview.is_none() {
                            entry.text_preview = row.text_preview.clone();
                        }
                    })
                    .or_insert(MergeEntry {
                        combined_score: row.score * weight,
                        from_vector: true,
                        from_fts: false,
                        vector_raw_score: Some(row.score),
                        fts_raw_rank: None,
                        fts_normalized_score: None,
                        text_preview: row.text_preview,
                    });
            }
        }

        if matches!(mode, SearchMode::Fts | SearchMode::Hybrid) {
            let graph = self.open_graph()?;
            let mut results = graph.fts_search(query, candidate_limit)?;
            if let Some(prefix) = path_prefix.as_deref() {
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

        let mut rows: Vec<_> = merged.into_iter().collect();
        rows.sort_by(|a, b| b.1.combined_score.total_cmp(&a.1.combined_score));
        if metadata_filter_requested(metadata_filter) {
            let graph = self.open_graph()?;
            rows = apply_metadata_filter(rows, &graph, metadata_filter)?;
        }
        rows.truncate(limit);

        Ok(rows
            .into_iter()
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
            .collect())
    }

    pub fn build_context(&self, params: BuildContextParams<'_>) -> Result<ContextPack> {
        let BuildContextParams {
            query,
            seed_path,
            path_prefix,
            budget_chars,
            limit,
            mode,
            metadata_filter,
        } = params;
        if query.is_none() && seed_path.is_none() {
            bail!("provide query or path");
        }
        let seed_path = seed_path.map(canonical_path_string);
        if let Some(path) = &seed_path {
            self.ensure_in_scope(path)?;
        }
        let seed_prefix = seed_path.as_deref().map(path_scope_hint);
        let effective_prefix = self.effective_prefix(path_prefix.or(seed_prefix.as_deref()))?;
        let query = query
            .filter(|q| !q.trim().is_empty())
            .map(str::to_string)
            .or_else(|| seed_path.as_ref().map(|p| path_query_hint(p)));

        let mut hits = Vec::new();
        let mut total_candidates = 0;
        if let Some(query) = &query {
            hits = self.search_files(
                query,
                limit,
                effective_prefix.as_deref(),
                mode,
                metadata_filter,
                true,
            )?;
            total_candidates = hits.len();
        }
        if let Some(seed) = &seed_path {
            if self.path_in_scope(seed) && !hits.iter().any(|hit| hit.path == *seed) {
                hits.insert(
                    0,
                    SearchHit {
                        path: seed.clone(),
                        score: 1.0,
                        source: "seed".to_string(),
                        explanation: None,
                    },
                );
            }
        }

        let graph = self.open_graph()?;
        let mut remaining = budget_chars.max(512);
        let mut items = Vec::new();
        for hit in hits {
            if remaining == 0 || !self.path_in_scope(&hit.path) {
                continue;
            }
            let entity = graph.get_by_path(&hit.path)?;
            let snippet = hit
                .explanation
                .as_ref()
                .and_then(|exp| exp.text_preview.clone())
                .or_else(|| file_excerpt(&hit.path, remaining.min(1200)));
            let snippet_len = snippet.as_ref().map(|s| s.len()).unwrap_or(0);
            let relations = graph
                .get_relations(&hit.path)?
                .into_iter()
                .filter(|(from, to, _)| self.path_in_scope(from) && self.path_in_scope(to))
                .take(8)
                .map(|(from, to, kind)| GraphEdge { from, to, kind })
                .collect();
            let recent_history = graph
                .get_history(&hit.path, 3)?
                .into_iter()
                .filter_map(|entry| serde_json::to_value(entry).ok())
                .collect();
            let (lifecycle, summary) = entity
                .map(|e| (Some(e.lifecycle.as_str().to_string()), e.summary))
                .unwrap_or((None, None));

            remaining = remaining.saturating_sub(hit.path.len() + snippet_len + 256);
            items.push(ContextItem {
                path: hit.path,
                score: hit.score,
                source: hit.source,
                lifecycle,
                summary,
                snippet,
                relations,
                recent_history,
            });
        }

        Ok(ContextPack {
            query,
            seed_path,
            scope: effective_prefix,
            budget_chars,
            items,
            total_candidates,
        })
    }

    pub fn get_file_content(&self, path: &str) -> Result<FileContent> {
        let path = normalize_request_path(path);
        self.ensure_in_scope(&path)?;
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.extractor.extract import extract_text; import json; \
                 text = extract_text({path:?}); \
                 print(json.dumps({{'path': {path:?}, 'content': text or '', 'chars': len(text or '')}}))"
            ),
        ])?;
        let result: FileContent = serde_json::from_str(&output)?;
        Ok(result)
    }

    pub fn entities_resource(&self) -> Result<String> {
        let mut rows: Vec<_> = self
            .open_graph()?
            .all()?
            .into_iter()
            .filter(|entity| self.path_in_scope(&entity.path))
            .map(|entity| format!("{:8}  {}", entity.lifecycle.as_str(), entity.path))
            .collect();
        rows.sort();
        Ok(rows.join("\n"))
    }

    pub fn entity_resource(&self, path: &str) -> Result<String> {
        match self.get_entity(path)? {
            Some(entity) => Ok([
                format!("path: {}", entity.path),
                format!("name: {}", entity.name),
                format!("lifecycle: {}", entity.lifecycle.as_str()),
                format!("size_bytes: {}", entity.size_bytes),
                format!("created_at: {}", entity.created_at),
                format!("modified_at: {}", entity.modified_at),
                format!("accessed_at: {}", entity.accessed_at),
                format!("summary: {}", entity.summary.unwrap_or_default()),
                format!("git_author: {}", entity.git_author.unwrap_or_default()),
            ]
            .join("\n")),
            None => Ok(format!("not found: {path}")),
        }
    }

    fn vector_search(
        &self,
        query: &str,
        limit: usize,
        path_prefix: Option<&str>,
    ) -> Result<Vec<VectorRow>> {
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.embeddings.store import search; import json; \
                 print(json.dumps(search({query:?}, limit={limit}, db_path={:?}, path_prefix={})))",
                self.config.indexer.vectors_path,
                path_prefix
                    .map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "None".to_string())
            ),
        ])?;
        Ok(serde_json::from_str(&output)?)
    }

    fn open_graph(&self) -> Result<Graph> {
        if !self.db_path.exists() {
            bail!("DB not found: {}", self.db_path.display());
        }
        Graph::open(self.db_path.to_string_lossy().as_ref())
    }

    fn effective_prefix(&self, requested: Option<&str>) -> Result<Option<String>> {
        let requested = requested.map(canonical_path_string);
        match (&self.scope, requested) {
            (Some(scope), Some(prefix)) => {
                let scope = scope.to_string_lossy();
                if prefix == scope || prefix.starts_with(&format!("{scope}/")) {
                    Ok(Some(prefix))
                } else {
                    bail!("path_prefix is outside MCP session scope: {scope}");
                }
            }
            (Some(scope), None) => Ok(Some(scope.to_string_lossy().to_string())),
            (None, prefix) => Ok(prefix),
        }
    }

    fn ensure_in_scope(&self, path: &str) -> Result<()> {
        if self.path_in_scope(path) {
            Ok(())
        } else {
            bail!(
                "path is outside MCP session scope: {}",
                self.scope
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<global>".to_string())
            )
        }
    }

    fn path_in_scope(&self, path: &str) -> bool {
        match &self.scope {
            Some(scope) => {
                let scope = scope.to_string_lossy();
                path == scope || path.starts_with(&format!("{scope}/"))
            }
            None => true,
        }
    }

    /// Resolve the saved-queries file path.
    ///
    /// Precedence: explicit `ORGANON_QUERIES` override → `ORGANON_HOME`/saved_queries.json
    /// → `~/.organon/saved_queries.json`. Must match the CLI's `queries::queries_path`
    /// so both read the same store.
    fn saved_queries_path(&self) -> PathBuf {
        if let Ok(path) = std::env::var("ORGANON_QUERIES") {
            return PathBuf::from(path);
        }
        let home = std::env::var("ORGANON_HOME").unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            format!("{home}/.organon")
        });
        PathBuf::from(home).join("saved_queries.json")
    }

    fn python_run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(self.python_bin());
        cmd.args(args)
            .env("ORGANON_VECTORS_DB", &self.config.indexer.vectors_path)
            .env("ORGANON_EMBED_MODEL", &self.config.indexer.embed_model);
        let out = cmd.output()?;
        if !out.status.success() {
            bail!(
                "python error (exit {}):\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn python_bin(&self) -> PathBuf {
        if let Ok(exe) = std::env::current_exe() {
            let real = std::fs::canonicalize(&exe).unwrap_or(exe);
            let mut dir = real.parent().map(|p| p.to_path_buf());
            for _ in 0..4 {
                if let Some(d) = dir {
                    let candidate = d.join(".venv/bin/python");
                    if candidate.exists() {
                        debug!(
                            "python: using .venv from binary dir: {}",
                            candidate.display()
                        );
                        return candidate;
                    }
                    dir = d.parent().map(|p| p.to_path_buf());
                } else {
                    break;
                }
            }
        }

        let cwd_venv = PathBuf::from(".venv/bin/python");
        if cwd_venv.exists() {
            return cwd_venv;
        }
        PathBuf::from("python3")
    }
}

impl Default for McpService {
    fn default() -> Self {
        Self::from_config(OrgConfig::load())
    }
}

#[tool_router]
impl OrganonMcpServer {
    pub fn new(service: McpService) -> Self {
        Self {
            service: Arc::new(service),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Search files by semantic meaning. Returns structured results. Pass explain=true for per-hit score breakdown."
    )]
    async fn search_files(
        &self,
        Parameters(req): Parameters<SearchFilesRequest>,
    ) -> Result<Json<SearchFilesResponse>, String> {
        let mode = req.mode.unwrap_or(SearchMode::Vector);
        let explain = req.explain.unwrap_or(false);
        let metadata_filter = build_find_filter(
            req.state,
            req.extension,
            req.created_after,
            req.modified_after,
            req.limit.unwrap_or(10),
        )
        .map_err(|e| e.to_string())?;
        self.service
            .search_files(
                &req.query,
                req.limit.unwrap_or(10),
                req.path_prefix.as_deref(),
                mode,
                &metadata_filter,
                explain,
            )
            .map(|items| Json(SearchFilesResponse { items }))
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Build a scoped agent context pack from a task query and/or seed path. The session scope is always enforced."
    )]
    async fn build_context(
        &self,
        Parameters(req): Parameters<BuildContextRequest>,
    ) -> Result<Json<ContextPack>, String> {
        let limit = req.limit.unwrap_or(10);
        let metadata_filter = build_find_filter(req.state, req.extension, None, None, limit)
            .map_err(|e| e.to_string())?;
        self.service
            .build_context(BuildContextParams {
                query: req.query.as_deref(),
                seed_path: req.path.as_deref(),
                path_prefix: req.path_prefix.as_deref(),
                budget_chars: req.budget_chars.unwrap_or(12000),
                limit,
                mode: req.mode.unwrap_or(SearchMode::Hybrid),
                metadata_filter: &metadata_filter,
            })
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Get full metadata for file path. Returns structured entity object.")]
    async fn get_entity(
        &self,
        Parameters(req): Parameters<PathRequest>,
    ) -> Result<Json<GetEntityResponse>, String> {
        self.service
            .get_entity(&req.path)
            .map(|entity| {
                Json(GetEntityResponse {
                    entity: entity.map(EntityRecord::from),
                })
            })
            .map_err(|e| e.to_string())
    }

    #[tool(description = "List files by lifecycle state. Returns structured rows.")]
    async fn list_by_lifecycle(
        &self,
        Parameters(req): Parameters<LifecycleRequest>,
    ) -> Result<Json<LifecycleListResponse>, String> {
        self.service
            .list_by_lifecycle(&req.state, req.limit.unwrap_or(20))
            .map(|items| Json(LifecycleListResponse { items }))
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Return summary statistics for entity graph.")]
    async fn graph_stats(&self) -> Result<Json<GraphStats>, String> {
        self.service
            .graph_stats()
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Return relationship graph rooted at file path.")]
    async fn get_graph(
        &self,
        Parameters(req): Parameters<GraphRequest>,
    ) -> Result<Json<RelationGraph>, String> {
        self.service
            .get_graph(&req.path, req.depth.unwrap_or(1))
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Extract text content for file.")]
    async fn get_file_content(
        &self,
        Parameters(req): Parameters<PathRequest>,
    ) -> Result<Json<FileContent>, String> {
        self.service
            .get_file_content(&req.path)
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Return lifecycle and change history for a file. Events: created, modified, lifecycle, renamed, deleted."
    )]
    async fn get_history(
        &self,
        Parameters(req): Parameters<GetHistoryRequest>,
    ) -> Result<Json<HistoryResponse>, String> {
        self.service
            .get_history(&req.path, req.limit.unwrap_or(20))
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Reverse dependency analysis: show what files import/depend on a given file. Useful before rename, refactor, or delete."
    )]
    async fn get_impact(
        &self,
        Parameters(req): Parameters<ImpactRequest>,
    ) -> Result<Json<ImpactResponse>, String> {
        self.service
            .get_impact(&req.path, req.depth.unwrap_or(5))
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Find exact duplicates with the same content hash. Useful for cleanup.")]
    async fn find_duplicates(
        &self,
        Parameters(_req): Parameters<FindDuplicatesRequest>,
    ) -> Result<Json<FindDuplicatesResponse>, String> {
        self.service
            .find_duplicates()
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Find files semantically similar to a given file using its existing vector embedding."
    )]
    async fn search_similar(
        &self,
        Parameters(req): Parameters<SearchSimilarRequest>,
    ) -> Result<Json<SearchFilesResponse>, String> {
        self.service
            .search_similar(
                &req.path,
                req.limit.unwrap_or(10),
                req.path_prefix.as_deref(),
            )
            .map(|items| Json(SearchFilesResponse { items }))
            .map_err(|e| e.to_string())
    }

    #[tool(description = "List all saved named queries (created with `organon query save`).")]
    async fn list_saved_queries(
        &self,
        Parameters(_req): Parameters<ListSavedQueriesRequest>,
    ) -> Result<Json<ListSavedQueriesResponse>, String> {
        self.service
            .list_saved_queries()
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(
        description = "Run a saved named query and return its results (find or search depending on how it was saved)."
    )]
    async fn run_saved_query(
        &self,
        Parameters(req): Parameters<RunSavedQueryRequest>,
    ) -> Result<Json<RunSavedQueryResponse>, String> {
        self.service
            .run_saved_query(&req.name)
            .map(Json)
            .map_err(|e| e.to_string())
    }
}

#[tool_handler]
impl ServerHandler for OrganonMcpServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Local semantic filesystem graph. Tools return structured JSON objects. Resources expose entity lists."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![Resource::new("organon://entities", "entities")],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ReadResourceResult, McpError> {
        let text = if request.uri.as_str() == "organon://entities" {
            self.service.entities_resource().map_err(to_mcp_error)?
        } else if let Some(path) = request.uri.as_str().strip_prefix("organon://entity/") {
            self.service.entity_resource(path).map_err(to_mcp_error)?
        } else {
            return Err(McpError::resource_not_found(
                "resource_not_found",
                Some(serde_json::json!({ "uri": request.uri })),
            ));
        };

        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            text,
            request.uri.as_str(),
        )]))
    }
}

pub async fn serve_stdio(service: McpService) -> Result<()> {
    let server = OrganonMcpServer::new(service).serve(stdio()).await?;
    server.waiting().await?;
    Ok(())
}

pub async fn serve_streamable_http(service: McpService, host: String, port: u16) -> Result<()> {
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let transport = StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(OrganonMcpServer::new(service.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let router = Router::new().nest_service("/mcp", transport);
    debug!("mcp streamable http: http://{addr}/mcp");
    axum::serve(listener, router).await?;
    Ok(())
}

pub async fn serve_stdio_from_config(config: OrgConfig) -> Result<()> {
    serve_stdio(McpService::from_config(config)).await
}

pub async fn serve_streamable_http_from_config(config: OrgConfig) -> Result<()> {
    let host = config.server.host.clone();
    let port = config.server.port;
    serve_streamable_http(McpService::from_config(config), host, port).await
}

fn to_mcp_error(err: anyhow::Error) -> McpError {
    McpError::internal_error(
        "internal_error",
        Some(serde_json::json!({ "error": err.to_string() })),
    )
}

/// Raw row returned by the Python vector search function.
#[derive(Debug, Deserialize)]
struct VectorRow {
    path: String,
    score: f64,
    text_preview: Option<String>,
}

/// Internal per-path accumulator used during search merge phase.
#[derive(Default)]
struct MergeEntry {
    combined_score: f64,
    from_vector: bool,
    from_fts: bool,
    vector_raw_score: Option<f64>,
    fts_raw_rank: Option<f64>,
    fts_normalized_score: Option<f64>,
    text_preview: Option<String>,
}

fn metadata_filter_requested(filter: &FindFilter) -> bool {
    filter.state.is_some()
        || filter.extension.is_some()
        || filter.created_after.is_some()
        || filter.modified_after.is_some()
        || filter.larger_than.is_some()
}

fn apply_metadata_filter(
    rows: Vec<(String, MergeEntry)>,
    graph: &Graph,
    filter: &FindFilter,
) -> Result<Vec<(String, MergeEntry)>> {
    let mut filtered = Vec::with_capacity(rows.len());
    for (path, entry) in rows {
        if let Some(entity) = graph.get_by_path(&path)? {
            if entity_matches_filter(&entity, filter) {
                filtered.push((path, entry));
            }
        }
    }
    Ok(filtered)
}

/// Build a `SearchExplanation` from raw pipeline signals.
/// Only real signals are reported — no fabricated rationale.
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

fn build_find_filter(
    state: Option<String>,
    extension: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    limit: usize,
) -> Result<FindFilter> {
    Ok(FindFilter {
        state,
        extension: extension.map(|ext| ext.trim_start_matches('.').to_string()),
        created_after: created_after
            .as_deref()
            .map(parse_date_to_timestamp)
            .transpose()?,
        modified_after: modified_after
            .as_deref()
            .map(parse_date_to_timestamp)
            .transpose()?,
        larger_than: None,
        offset: 0,
        limit,
    })
}

fn parse_date_to_timestamp(date: &str) -> Result<i64> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")?;
    Ok(parsed
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight")
        .and_utc()
        .timestamp())
}

fn canonical_path_string(path: &str) -> String {
    let path = Path::new(path);
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn normalize_request_path(path: &str) -> String {
    canonical_path_string(path)
}

fn path_query_hint(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().replace(['_', '-', '.'], " "))
        .unwrap_or_else(|| path.to_string())
}

fn path_scope_hint(path: &str) -> String {
    let path = Path::new(path);
    if path.is_dir() {
        path.to_string_lossy().to_string()
    } else {
        path.parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_else(|| ".".to_string())
    }
}

fn file_excerpt(path: &str, max_chars: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.chars().take(max_chars).collect())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use organon_core::{
        config::OrgConfig,
        entity::{Entity, LifecycleState},
        graph::Graph,
    };
    use tempfile::NamedTempFile;

    use super::*;

    fn test_entity(path: &str) -> Entity {
        Entity {
            id: path.to_string(),
            path: path.to_string(),
            name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 42,
            created_at: 1,
            modified_at: 2,
            accessed_at: 3,
            lifecycle: LifecycleState::Active,
            content_hash: Some("hash".to_string()),
            summary: Some("summary".to_string()),
            git_author: Some("Alice".to_string()),
        }
    }

    fn temp_service() -> (McpService, Graph, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let graph = Graph::open(path.to_string_lossy().as_ref()).unwrap();
        let mut config = OrgConfig::default();
        config.indexer.db_path = path.display().to_string();
        (McpService::new(path, config), graph, file)
    }

    fn temp_scoped_service(scope: &str) -> (McpService, Graph, NamedTempFile) {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let graph = Graph::open(path.to_string_lossy().as_ref()).unwrap();
        let mut config = OrgConfig::default();
        config.indexer.db_path = path.display().to_string();
        (
            McpService::new_with_scope(path, config, Some(PathBuf::from(scope))),
            graph,
            file,
        )
    }

    #[test]
    fn get_entity_returns_row() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/main.rs")).unwrap();

        let entity = svc.get_entity("/tmp/main.rs").unwrap().unwrap();
        assert_eq!(entity.path, "/tmp/main.rs");
    }

    #[test]
    fn graph_stats_counts_entities_and_relations() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/a.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/b.rs")).unwrap();
        graph
            .upsert_relation("/tmp/a.rs", "/tmp/b.rs", "imports")
            .unwrap();

        let stats = svc.graph_stats().unwrap();
        assert_eq!(stats.total_entities, 2);
        assert_eq!(stats.total_relations, 1);
    }

    #[test]
    fn scoped_service_filters_stats_and_rejects_out_of_scope_paths() {
        let (svc, graph, _file) = temp_scoped_service("/tmp/work");
        graph.upsert(&test_entity("/tmp/work/a.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/other/b.rs")).unwrap();

        let stats = svc.graph_stats().unwrap();
        assert_eq!(stats.total_entities, 1);
        assert!(svc.get_entity("/tmp/other/b.rs").is_err());
    }

    #[test]
    fn get_graph_walks_edges() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/a.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/b.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/c.rs")).unwrap();
        graph
            .upsert_relation("/tmp/a.rs", "/tmp/b.rs", "imports")
            .unwrap();
        graph
            .upsert_relation("/tmp/b.rs", "/tmp/c.rs", "imports")
            .unwrap();

        let result = svc.get_graph("/tmp/a.rs", 2).unwrap();
        assert!(result.nodes.contains(&"/tmp/a.rs".to_string()));
        assert!(result.nodes.contains(&"/tmp/b.rs".to_string()));
        assert!(result.nodes.contains(&"/tmp/c.rs".to_string()));
        assert_eq!(result.edges.len(), 2);
    }

    #[test]
    fn search_files_fts_works() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/graph.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/notes.rs")).unwrap();
        graph
            .update_fts("/tmp/graph.rs", "graph.rs", "entity graph sqlite search")
            .unwrap();
        graph
            .update_fts("/tmp/notes.rs", "notes.rs", "gardening shopping list")
            .unwrap();

        let hits = svc
            .search_files(
                "graph sqlite",
                5,
                None,
                SearchMode::Fts,
                &FindFilter::default(),
                false,
            )
            .unwrap();
        assert_eq!(hits[0].path, "/tmp/graph.rs");
        assert!(
            hits[0].explanation.is_none(),
            "explain=false should not attach explanation"
        );
    }

    #[test]
    fn search_files_fts_explain_returns_explanation() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/graph.rs")).unwrap();
        graph.upsert(&test_entity("/tmp/notes.rs")).unwrap();
        graph
            .update_fts("/tmp/graph.rs", "graph.rs", "entity graph sqlite search")
            .unwrap();
        graph
            .update_fts("/tmp/notes.rs", "notes.rs", "gardening shopping list")
            .unwrap();

        let hits = svc
            .search_files(
                "graph sqlite",
                5,
                None,
                SearchMode::Fts,
                &FindFilter::default(),
                true,
            )
            .unwrap();
        assert_eq!(hits[0].path, "/tmp/graph.rs");
        let exp = hits[0]
            .explanation
            .as_ref()
            .expect("explanation present when explain=true");
        assert!(exp.fts_score.is_some(), "FTS hit should have fts_score");
        assert!(
            exp.vector_score.is_none(),
            "FTS-only hit should not have vector_score"
        );
        assert!(!exp.reasons.is_empty(), "reasons should be non-empty");
        assert!(
            exp.reasons.iter().any(|r| r.contains("full-text search")),
            "reasons should mention FTS"
        );
        // "graph" appears in the path "/tmp/graph.rs"
        assert!(exp.path_match, "query term 'graph' should match path");
    }

    #[test]
    fn entity_resource_formats_output() {
        let (svc, graph, _file) = temp_service();
        graph.upsert(&test_entity("/tmp/main.rs")).unwrap();

        let text = svc.entity_resource("/tmp/main.rs").unwrap();
        assert!(text.contains("path: /tmp/main.rs"));
        assert!(text.contains("lifecycle: active"));
    }

    #[tokio::test]
    async fn server_info_enables_resources() {
        let server = OrganonMcpServer::new(McpService::default());
        let info = server.get_info();
        assert!(info.capabilities.resources.is_some());
    }

    #[tokio::test]
    async fn tool_schemas_include_structured_outputs() {
        let server = OrganonMcpServer::new(McpService::default());
        let tools = server.tool_router.list_all();

        for name in [
            "search_files",
            "build_context",
            "get_entity",
            "list_by_lifecycle",
            "graph_stats",
            "get_graph",
            "get_file_content",
            "get_history",
            "get_impact",
            "find_duplicates",
            "search_similar",
            "list_saved_queries",
            "run_saved_query",
        ] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                tool.output_schema.is_some(),
                "missing output schema for {name}"
            );
        }
    }
}
