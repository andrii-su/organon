//! Search and indexing commands: search, index, context.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use log::info;
use organon_core::config::OrgConfig;

use crate::context::{build_context_pack, ContextParams};
use crate::python;
use crate::search::{
    default_search_mode, parse_query_expr, python_env, search_by_example, search_entities,
    SearchMode, SearchParams,
};
use crate::{build_find_filter, inspect::cmd_find, FindFilterParams};

/// Run a semantic, FTS, or hybrid search.
///
/// Inline field tokens (`state:dormant ext:rs`) are parsed from the query and merged
/// with explicit flags. If the entire query consists of field tokens, falls back to
/// `cmd_find`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_search(
    query: &str,
    limit: Option<usize>,
    dir: Option<&Path>,
    mode: Option<SearchMode>,
    state: Option<String>,
    extension: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    explain: bool,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<()> {
    let pq = parse_query_expr(query);
    if pq.has_filters() {
        log::debug!(
            "query parse: free_text={:?} state={:?} ext={:?} modified_after={:?} created_after={:?} size>{:?}MB",
            pq.free_text, pq.state, pq.extension, pq.modified_after, pq.created_after, pq.larger_than_mb
        );
    }
    let effective_query = if pq.free_text.is_empty() && pq.has_filters() {
        return cmd_find(
            db_path,
            pq.state.or(state),
            pq.extension.or(extension),
            created_after.or(pq.created_after),
            modified_after.or(pq.modified_after),
            None,
            pq.larger_than_mb,
            limit.unwrap_or(config.search.default_limit),
        );
    } else {
        pq.free_text.clone()
    };
    let effective_query = if effective_query.is_empty() {
        query
    } else {
        &effective_query
    };

    let merged_state = state.or(pq.state);
    let merged_ext = extension.or(pq.extension);
    let merged_created = created_after.or(pq.created_after);
    let merged_modified = modified_after.or(pq.modified_after);
    let merged_size = pq.larger_than_mb;

    let limit = limit.unwrap_or(config.search.default_limit);
    let mode = mode.unwrap_or_else(|| default_search_mode(config));
    let metadata_filter = build_find_filter(FindFilterParams {
        state: merged_state,
        extension: merged_ext,
        created_after: merged_created,
        modified_after: merged_modified,
        modified_within_days: None,
        larger_than_mb: merged_size,
        limit,
        offset: 0,
    })?;
    info!("search: {effective_query:?} limit={limit} dir={dir:?} mode={mode:?} explain={explain}");
    let results = search_entities(SearchParams {
        query,
        limit,
        offset: 0,
        dir,
        mode,
        metadata_filter: &metadata_filter,
        config,
        db_path,
        explain,
    })?;

    if results.items.is_empty() {
        println!("(no results — run `organon index` first)");
        return Ok(());
    }

    if explain {
        for hit in &results.items {
            println!("{:.3}  {}  {}", hit.score, hit.source, hit.path);
            if let Some(exp) = &hit.explanation {
                for reason in &exp.reasons {
                    println!("  → {reason}");
                }
                if let Some(preview) = &exp.text_preview {
                    let snippet: String = preview.chars().take(120).collect();
                    println!("  preview: {snippet}");
                }
            }
            println!();
        }
    } else {
        println!("{:<6}  {:<7}  PATH", "SCORE", "SOURCE");
        println!("{}", "-".repeat(96));
        for hit in results.items {
            println!("{:.3}   {:<7}  {}", hit.score, hit.source, hit.path);
        }
    }
    Ok(())
}

/// Find files semantically similar to a given file using vector embeddings.
pub(crate) fn cmd_search_like(
    like_path: &PathBuf,
    limit: Option<usize>,
    dir: Option<&Path>,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<()> {
    if !db_path.exists() {
        bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }
    let canonical = std::fs::canonicalize(like_path).unwrap_or_else(|_| like_path.clone());
    let path_str = canonical.to_string_lossy().to_string();
    let limit = limit.unwrap_or(config.search.default_limit);

    info!("search --like: {path_str} limit={limit}");
    let results = search_by_example(&path_str, limit, 0, dir, config)?;

    if results.items.is_empty() {
        println!("(no results — run `organon index` first or file not indexed)");
        return Ok(());
    }

    println!("{:<6}  PATH", "SCORE");
    println!("{}", "-".repeat(80));
    for hit in results.items {
        println!("{:.3}   {}", hit.score, hit.path);
    }
    Ok(())
}

/// Run the Python indexer (extracts text, builds embeddings, updates FTS).
pub(crate) fn cmd_index(
    path: Option<&Path>,
    watch: Option<u64>,
    db_path: &Path,
    config: &OrgConfig,
) -> Result<()> {
    let mut path_prefixes = Vec::new();
    if let Some(path) = path {
        path_prefixes.push(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
    }
    if let Some(secs) = watch {
        info!("index watch mode: {secs}s");
    }
    let invocation = python::indexer_invocation(db_path, watch, &path_prefixes);
    python::python_exec_invocation_with_env(&invocation, &python_env(config))
}

/// Build a compact agent context pack from a query and/or seed path.
///
/// The pack includes ranked file snippets, lifecycle metadata, relations, and recent
/// history — all trimmed to the token budget.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_context(
    query: Option<&str>,
    path: Option<&Path>,
    scope: Option<&Path>,
    budget: usize,
    limit: Option<usize>,
    mode: Option<SearchMode>,
    state: Option<String>,
    extension: Option<String>,
    json: bool,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<()> {
    if query.is_none() && path.is_none() {
        bail!("provide a context query or --path <path>");
    }
    if !db_path.exists() {
        bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }

    let limit = limit.unwrap_or(config.search.default_limit);
    let metadata_filter = build_find_filter(FindFilterParams {
        state,
        extension,
        created_after: None,
        modified_after: None,
        modified_within_days: None,
        larger_than_mb: None,
        limit,
        offset: 0,
    })?;
    let pack = build_context_pack(ContextParams {
        query,
        seed_path: path,
        scope,
        budget_chars: budget,
        limit,
        mode: mode.unwrap_or_else(|| default_search_mode(config)),
        metadata_filter: &metadata_filter,
        config,
        db_path,
    })?;

    if json {
        println!("{}", serde_json::to_string_pretty(&pack)?);
        return Ok(());
    }

    println!("context scope: {}", pack.scope);
    if let Some(query) = &pack.query {
        println!("query: {query}");
    }
    if let Some(path) = &pack.seed_path {
        println!("seed: {path}");
    }
    println!("items: {} / {}", pack.items.len(), pack.total_candidates);
    println!();
    for item in pack.items {
        println!("{:.3}  {:<7}  {}", item.score, item.source, item.path);
        if let Some(lifecycle) = item.lifecycle {
            println!("  lifecycle: {lifecycle}");
        }
        if let Some(summary) = item.summary {
            println!("  summary: {summary}");
        }
        if let Some(snippet) = item.snippet {
            let compact = snippet.replace('\n', " ");
            println!(
                "  snippet: {}",
                compact.chars().take(220).collect::<String>()
            );
        }
        if !item.relations.is_empty() {
            println!("  relations: {}", item.relations.len());
        }
        if !item.recent_history.is_empty() {
            println!("  history: {} recent event(s)", item.recent_history.len());
        }
        println!();
    }
    Ok(())
}
