use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
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
            session::local::LocalSessionManager, StreamableHttpServerConfig,
            StreamableHttpService,
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
pub struct SummaryResponse {
    pub path: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QueryGraphResult {
    pub results: Vec<serde_json::Value>,
    pub sql: String,
    pub mode: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SearchFilesResponse {
    pub items: Vec<SearchHit>,
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
pub struct PathRequest {
    pub path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummarizeFileRequest {
    pub path: String,
    pub model: Option<String>,
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
pub struct QueryGraphRequest {
    pub nl_query: String,
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
    /// When true, also find near-duplicates via embedding similarity (slower).
    pub near: Option<bool>,
    /// Similarity threshold for near-duplicates, 0–1 (default 0.95).
    pub threshold: Option<f64>,
    /// Max near-duplicate pairs to return (default 50).
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FindDuplicatesResponse {
    /// Groups of files sharing the same content hash. Fields: content_hash, paths.
    pub exact: Vec<serde_json::Value>,
    pub near: Option<Vec<serde_json::Value>>,
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

impl McpService {
    pub fn new(db_path: PathBuf, config: OrgConfig) -> Self {
        Self { db_path, config }
    }

    pub fn from_config(config: OrgConfig) -> Self {
        Self::new(PathBuf::from(&config.indexer.db_path), config)
    }
}

impl McpService {
    pub fn get_entity(&self, path: &str) -> Result<Option<Entity>> {
        self.open_graph()?.get_by_path(path)
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
            .filter(|entity| entity.lifecycle.as_str() == state)
            .map(|entity| LifecycleRow {
                path: entity.path,
                lifecycle: entity.lifecycle.as_str().to_string(),
                size_bytes: entity.size_bytes,
                modified_at: entity.modified_at,
                accessed_at: entity.accessed_at,
            })
            .collect();

        rows.sort_by(|a, b| b.accessed_at.cmp(&a.accessed_at));
        rows.truncate(limit);
        Ok(rows)
    }

    pub fn graph_stats(&self) -> Result<GraphStats> {
        let graph = self.open_graph()?;
        let entities = graph.all()?;
        let relations = graph.all_relations()?;

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
        let entries = self.open_graph()?.get_history(path, limit)?;
        let total = entries.len();
        let entries_json: Vec<serde_json::Value> = entries
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(HistoryResponse {
            path: path.to_string(),
            entries: entries_json,
            total,
        })
    }

    pub fn get_impact(&self, path: &str, depth: u8) -> Result<ImpactResponse> {
        let entries = self.open_graph()?.reverse_deps(path, depth)?;
        let direct_dependents = entries.iter().filter(|e| e.depth == 1).count();
        let total = entries.len();
        let risk_level = match total {
            0 => "none",
            1..=3 => "low",
            4..=10 => "medium",
            _ => "high",
        }.to_string();
        let entries_json = entries
            .into_iter()
            .map(|e| serde_json::to_value(e).unwrap_or(serde_json::Value::Null))
            .collect();
        Ok(ImpactResponse {
            total,
            direct_dependents,
            risk_level,
            path: path.to_string(),
            depth,
            entries: entries_json,
        })
    }

    pub fn list_saved_queries(&self) -> Result<ListSavedQueriesResponse> {
        let path = self.saved_queries_path();
        if !path.exists() {
            return Ok(ListSavedQueriesResponse { queries: vec![], total: 0 });
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
                let extension = sq.get("extension").and_then(|v| v.as_str()).map(str::to_string);
                let filter = build_find_filter(state, extension, None, None, limit)?;
                let graph = self.open_graph()?;
                let items: Vec<serde_json::Value> = graph
                    .find(&filter)?
                    .into_iter()
                    .filter_map(|e| serde_json::to_value(e).ok())
                    .collect();
                let total = items.len();
                Ok(RunSavedQueryResponse { kind: "find".to_string(), items, total })
            }
            "search" => {
                let query_str = sq.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let mode_str = sq.get("mode").and_then(|v| v.as_str()).unwrap_or("vector");
                let mode = match mode_str {
                    "fts" => SearchMode::Fts,
                    "hybrid" => SearchMode::Hybrid,
                    _ => SearchMode::Vector,
                };
                let state = sq.get("state").and_then(|v| v.as_str()).map(str::to_string);
                let extension = sq.get("extension").and_then(|v| v.as_str()).map(str::to_string);
                let filter = build_find_filter(state, extension, None, None, limit)?;
                let items: Vec<serde_json::Value> = self
                    .search_files(&query_str, limit, None, mode, &filter, false)?
                    .into_iter()
                    .filter_map(|h| serde_json::to_value(h).ok())
                    .collect();
                let total = items.len();
                Ok(RunSavedQueryResponse { kind: "search".to_string(), items, total })
            }
            other => bail!("unknown query kind '{other}'"),
        }
    }

    pub fn find_duplicates(
        &self,
        near: bool,
        threshold: f64,
        limit: usize,
    ) -> Result<FindDuplicatesResponse> {
        let graph = self.open_graph()?;
        let exact = graph
            .exact_duplicates()?
            .into_iter()
            .map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null))
            .collect();
        let near_pairs = if near {
            let output = self.python_run(&[
                "-c",
                &format!(
                    "from ai.embeddings.store import find_near_duplicates; import json; \
                     print(json.dumps(find_near_duplicates(threshold={threshold}, limit={limit}, db_path={:?})))",
                    self.config.indexer.vectors_path,
                ),
            ])?;
            Some(serde_json::from_str::<Vec<serde_json::Value>>(&output)?)
        } else {
            None
        };
        Ok(FindDuplicatesResponse { exact, near: near_pairs })
    }

    pub fn search_similar(
        &self,
        path: &str,
        limit: usize,
        path_prefix: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.embeddings.store import search_by_path; import json; \
                 print(json.dumps(search_by_path({path:?}, limit={limit}, db_path={:?}, path_prefix={})))",
                self.config.indexer.vectors_path,
                path_prefix
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
        let graph = self.open_graph()?;
        let depth = depth.min(3);
        let mut visited = BTreeSet::new();
        let mut edges = Vec::new();
        let mut seen_edges = BTreeSet::new();
        let mut queue = VecDeque::from([(path.to_string(), 0u8)]);

        while let Some((current, level)) = queue.pop_front() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if level >= depth {
                continue;
            }

            for (from, to, kind) in graph.get_relations(&current)? {
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
        let mut merged: BTreeMap<String, MergeEntry> = BTreeMap::new();
        let candidate_limit = if metadata_filter_requested(metadata_filter) {
            limit.saturating_mul(12).max(12)
        } else {
            limit.saturating_mul(4).max(4)
        };

        if matches!(mode, SearchMode::Vector | SearchMode::Hybrid) {
            let results = self.vector_search(query, candidate_limit, path_prefix)?;
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
            if let Some(prefix) = path_prefix {
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

    pub fn get_file_content(&self, path: &str) -> Result<FileContent> {
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

    pub fn summarize_file(&self, path: &str, model: Option<&str>) -> Result<SummaryResponse> {
        let model_expr = model
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|| "None".to_string());
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.indexer import summarize_file; from pathlib import Path; import json; \
                 print(json.dumps(summarize_file(Path({:?}), {:?}, model={})))",
                self.db_path.to_string_lossy(),
                path,
                model_expr,
            ),
        ])?;
        let summary: Option<String> = serde_json::from_str(&output)?;
        Ok(SummaryResponse {
            path: path.to_string(),
            summary,
        })
    }

    pub fn query_graph(&self, nl_query: &str) -> Result<QueryGraphResult> {
        let output = self.python_run(&[
            "-c",
            &format!(
                "from ai.query.nl_query import run_nl_query; import json; \
                 print(json.dumps(run_nl_query({nl_query:?}, db_path={:?})))",
                self.db_path.to_string_lossy()
            ),
        ])?;
        Ok(serde_json::from_str(&output)?)
    }

    pub fn entities_resource(&self) -> Result<String> {
        let mut rows: Vec<_> = self
            .open_graph()?
            .all()?
            .into_iter()
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

    fn saved_queries_path(&self) -> PathBuf {
        std::env::var("ORGANON_QUERIES")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(format!("{home}/.organon/saved_queries.json"))
            })
    }

    fn python_run(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(self.python_bin());
        cmd.args(args)
            .env("ORGANON_VECTORS_DB", &self.config.indexer.vectors_path)
            .env("ORGANON_EMBED_MODEL", &self.config.indexer.embed_model)
            .env("ORGANON_OLLAMA_MODEL", &self.config.ollama.model)
            .env(
                "ORGANON_SUMMARIZE",
                if self.config.indexer.summarize {
                    "1"
                } else {
                    "0"
                },
            );
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

    #[tool(description = "Recompute and store summary for one file.")]
    async fn summarize_file(
        &self,
        Parameters(req): Parameters<SummarizeFileRequest>,
    ) -> Result<Json<SummaryResponse>, String> {
        self.service
            .summarize_file(&req.path, req.model.as_deref())
            .map(Json)
            .map_err(|e| e.to_string())
    }

    #[tool(description = "Natural-language query over graph.")]
    async fn query_graph(
        &self,
        Parameters(req): Parameters<QueryGraphRequest>,
    ) -> Result<Json<QueryGraphResult>, String> {
        self.service
            .query_graph(&req.nl_query)
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

    #[tool(
        description = "Find exact duplicates (same content hash) and optionally near-duplicates (embedding similarity). Useful for cleanup."
    )]
    async fn find_duplicates(
        &self,
        Parameters(req): Parameters<FindDuplicatesRequest>,
    ) -> Result<Json<FindDuplicatesResponse>, String> {
        self.service
            .find_duplicates(
                req.near.unwrap_or(false),
                req.threshold.unwrap_or(0.95),
                req.limit.unwrap_or(50),
            )
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
            .search_similar(&req.path, req.limit.unwrap_or(10), req.path_prefix.as_deref())
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
            "Local semantic filesystem graph. Tools return structured JSON objects. Resources expose entity summaries."
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
            resources: vec![RawResource::new("organon://entities", "entities").no_annotation()],
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
            "get_entity",
            "list_by_lifecycle",
            "graph_stats",
            "get_graph",
            "get_file_content",
            "summarize_file",
            "query_graph",
        ] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                tool.output_schema.is_some(),
                "missing output schema for {name}"
            );
        }
    }
}
