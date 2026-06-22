//! Graph analysis commands: graph, history, impact, duplicates.

use std::path::{Path, PathBuf};

use anyhow::Result;
use log::info;
use organon_core::graph::DuplicateGroup;

use crate::format::format_ts;
use crate::graph_view::{
    build_relation_graph, render_graph_dot, render_graph_mermaid, render_graph_text,
};
use crate::{open_graph, GraphFormat};

/// Warn (to stderr) when the relationship table is empty so users don't mistake an
/// un-indexed DB for "no dependencies". Relations are populated by the Python indexer
/// (`organon index`), not by `organon watch` alone. Printed to stderr to keep stdout
/// (text/DOT/Mermaid/JSON) clean for piping.
fn warn_if_no_relations(graph: &organon_core::graph::Graph) {
    if graph.relation_count().unwrap_or(0) == 0 {
        eprintln!(
            "note: graph DB has 0 relations — run `organon index` to extract imports/references"
        );
    }
}

/// Show the import/reference graph for a file.
pub(crate) fn cmd_graph(
    path: &PathBuf,
    depth: u8,
    format: GraphFormat,
    db_path: &Path,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    warn_if_no_relations(&graph);
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();
    let depth_clamped = depth.min(3);
    info!("graph: {path_str} depth={depth_clamped} format={format:?}");

    let view = build_relation_graph(&graph, path_str.as_ref(), depth_clamped)?;
    let rendered = match format {
        GraphFormat::Text => render_graph_text(&view),
        GraphFormat::Dot => render_graph_dot(&view),
        GraphFormat::Mermaid => render_graph_mermaid(&view),
    };
    print!("{rendered}");
    Ok(())
}

/// Show lifecycle and content-change history for a file.
pub(crate) fn cmd_history(path: &PathBuf, limit: usize, json: bool, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();
    log::debug!("history: {path_str} limit={limit}");

    let entries = graph.get_history(&path_str, limit)?;

    if entries.is_empty() {
        println!("no history found for: {path_str}");
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("history: {path_str}");
    println!("{:<20}  {:<12}  detail", "time", "event");
    println!("{}", "-".repeat(72));
    for e in &entries {
        let ts = format_ts(e.recorded_at);
        let detail = match e.event.as_str() {
            "created" => format!(
                "lifecycle={}{}",
                e.new_lifecycle.as_deref().unwrap_or("?"),
                e.size_bytes
                    .map(|b| format!("  size={b}"))
                    .unwrap_or_default()
            ),
            "modified" => format!(
                "hash={}",
                e.content_hash
                    .as_deref()
                    .map(|h| &h[..16.min(h.len())])
                    .unwrap_or("?")
            ),
            "lifecycle" => format!(
                "{} → {}",
                e.old_lifecycle.as_deref().unwrap_or("?"),
                e.new_lifecycle.as_deref().unwrap_or("?")
            ),
            "renamed" => format!("from {}", e.old_path.as_deref().unwrap_or("?")),
            "deleted" => format!("was {}", e.old_lifecycle.as_deref().unwrap_or("?")),
            other => other.to_string(),
        };
        println!("{ts:<20}  {:<12}  {detail}", e.event);
    }
    Ok(())
}

/// Show reverse dependencies — files that import/reference this file.
pub(crate) fn cmd_impact(path: &PathBuf, depth: u8, json: bool, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    warn_if_no_relations(&graph);
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();

    let entries = graph.reverse_deps(&path_str, depth)?;

    if json {
        #[derive(serde::Serialize)]
        struct ImpactReport<'a> {
            path: &'a str,
            depth: u8,
            total: usize,
            direct_dependents: usize,
            risk_level: &'static str,
            entries: &'a [organon_core::graph::ImpactEntry],
        }
        let direct = entries.iter().filter(|e| e.depth == 1).count();
        println!(
            "{}",
            serde_json::to_string_pretty(&ImpactReport {
                path: &path_str,
                depth,
                total: entries.len(),
                direct_dependents: direct,
                risk_level: impact_risk_level(entries.len()),
                entries: &entries,
            })?
        );
        return Ok(());
    }

    let direct = entries.iter().filter(|e| e.depth == 1).count();
    let risk = impact_risk_level(entries.len());

    println!("impact: {path_str}");
    if entries.is_empty() {
        println!("  risk: {risk} — no dependents found up to depth {depth}");
        return Ok(());
    }
    println!(
        "  risk: {risk} — {} total dependent(s), {} direct (depth 1), max depth {depth}\n",
        entries.len(),
        direct,
    );
    println!("{:<5}  {:<10}  PATH", "DEPTH", "KIND");
    println!("{}", "-".repeat(80));
    for e in &entries {
        println!("  {:<3}  {:<10}  {}", e.depth, e.kind, e.path);
    }
    Ok(())
}

/// Find files with identical content hashes.
pub(crate) fn cmd_duplicates(json: bool, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let exact = graph.exact_duplicates()?;

    if json {
        println!("{}", serde_json::to_string_pretty(&exact)?);
        return Ok(());
    }
    print_exact_dups(&exact);
    Ok(())
}

/// Classify change impact by number of dependents.
pub(crate) fn impact_risk_level(total: usize) -> &'static str {
    match total {
        0 => "none",
        1..=3 => "low",
        4..=10 => "medium",
        _ => "high",
    }
}

fn print_exact_dups(groups: &[DuplicateGroup]) {
    println!("exact duplicates (by content hash):");
    if groups.is_empty() {
        println!("  (none)");
        return;
    }
    for g in groups {
        println!("  {}:", &g.content_hash[..16.min(g.content_hash.len())]);
        for p in &g.paths {
            println!("    {p}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impact_risk_level_thresholds() {
        assert_eq!(impact_risk_level(0), "none");
        assert_eq!(impact_risk_level(1), "low");
        assert_eq!(impact_risk_level(3), "low");
        assert_eq!(impact_risk_level(4), "medium");
        assert_eq!(impact_risk_level(10), "medium");
        assert_eq!(impact_risk_level(11), "high");
        assert_eq!(impact_risk_level(100), "high");
    }
}
