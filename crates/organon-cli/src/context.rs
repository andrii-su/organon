use std::path::{Path, PathBuf};

use anyhow::Result;
use organon_core::{
    config::OrgConfig,
    graph::{FindFilter, Graph},
};
use serde::Serialize;

use crate::search::{search_entities, SearchHit, SearchMode, SearchParams};

#[derive(Debug, Serialize)]
pub struct ContextPack {
    pub query: Option<String>,
    pub seed_path: Option<String>,
    pub scope: String,
    pub budget_chars: usize,
    pub items: Vec<ContextItem>,
    pub total_candidates: usize,
}

#[derive(Debug, Serialize)]
pub struct ContextItem {
    pub path: String,
    pub score: f64,
    pub source: String,
    pub lifecycle: Option<String>,
    pub summary: Option<String>,
    pub snippet: Option<String>,
    pub relations: Vec<ContextRelation>,
    pub recent_history: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct ContextRelation {
    pub from: String,
    pub to: String,
    pub kind: String,
}

pub struct ContextParams<'a> {
    pub query: Option<&'a str>,
    pub seed_path: Option<&'a Path>,
    pub scope: Option<&'a Path>,
    pub budget_chars: usize,
    pub limit: usize,
    pub mode: SearchMode,
    pub metadata_filter: &'a FindFilter,
    pub config: &'a OrgConfig,
    pub db_path: &'a Path,
}

pub fn build_context_pack(params: ContextParams) -> Result<ContextPack> {
    let graph = Graph::open(params.db_path.to_string_lossy().as_ref())?;
    let seed_path = params.seed_path.map(canonical_path_string);
    let scope = resolve_scope(params.scope, seed_path.as_deref());
    let query = params
        .query
        .filter(|q| !q.trim().is_empty())
        .map(str::to_string)
        .or_else(|| seed_path.as_ref().map(|p| path_query_hint(p)));

    let mut hits = Vec::new();
    let mut total_candidates = 0;
    if let Some(query) = &query {
        let page = search_entities(SearchParams {
            query,
            limit: params.limit,
            offset: 0,
            dir: Some(Path::new(&scope)),
            mode: params.mode,
            metadata_filter: params.metadata_filter,
            config: params.config,
            db_path: params.db_path,
            explain: true,
        })?;
        total_candidates = page.total;
        hits.extend(page.items);
    }

    if let Some(seed) = &seed_path {
        if seed.starts_with(&scope) && !hits.iter().any(|hit| hit.path == *seed) {
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

    let mut remaining = params.budget_chars.max(512);
    let mut items = Vec::new();
    for hit in hits {
        if remaining == 0 {
            break;
        }
        if !hit.path.starts_with(&scope) {
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
            .filter(|(from, to, _)| from.starts_with(&scope) && to.starts_with(&scope))
            .take(8)
            .map(|(from, to, kind)| ContextRelation { from, to, kind })
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
        scope,
        budget_chars: params.budget_chars,
        items,
        total_candidates,
    })
}

fn resolve_scope(scope: Option<&Path>, seed_path: Option<&str>) -> String {
    if let Some(scope) = scope {
        return canonical_path_string(scope);
    }
    if let Some(seed) = seed_path {
        let path = Path::new(seed);
        if path.is_dir() {
            return canonical_path_string(path);
        }
        if let Some(parent) = path.parent() {
            return parent.to_string_lossy().to_string();
        }
    }
    canonical_path_string(Path::new("."))
}

fn canonical_path_string(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| PathBuf::from(path))
        .to_string_lossy()
        .to_string()
}

fn path_query_hint(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().replace(['_', '-', '.'], " "))
        .unwrap_or_else(|| path.to_string())
}

fn file_excerpt(path: &str, max_chars: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.chars().take(max_chars).collect())
}
