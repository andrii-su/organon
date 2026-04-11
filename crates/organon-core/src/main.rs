use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use organon_core::{config::OrgConfig, graph::Graph, ignore::IgnoreSet, scanner, watcher};

fn main() -> Result<()> {
    env_logger::init();
    let config = OrgConfig::load();

    let db_path = std::env::var("ORGANON_DB").unwrap_or_else(|_| config.indexer.db_path.clone());

    let cli_root = std::env::args().nth(1).map(PathBuf::from);
    let roots = resolve_watch_roots(cli_root.as_deref(), &config);
    let watch_roots: Vec<_> = roots
        .iter()
        .map(|root| {
            watcher::WatchRoot::new(
                root.clone(),
                Arc::new(IgnoreSet::load(root, &config.watch.ignore_segments)),
            )
        })
        .collect();

    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let graph = Arc::new(Mutex::new(Graph::open(&db_path)?));
    let joined_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    log::info!(
        "organon-core started. db={db_path} watch=[{joined_roots}]"
    );

    // Phase 1a: index existing files
    let mut stats = scanner::ScanStats {
        total: 0,
        indexed: 0,
        skipped: 0,
        errors: 0,
    };
    for root in &watch_roots {
        let root_stats = scanner::scan(
            root.path.to_string_lossy().as_ref(),
            Arc::clone(&graph),
            &root.ignore_set,
            config.watch.use_git_timestamps,
        )?;
        stats.total += root_stats.total;
        stats.indexed += root_stats.indexed;
        stats.skipped += root_stats.skipped;
        stats.errors += root_stats.errors;
    }
    println!(
        "indexed {} files ({} skipped, {} errors)",
        stats.indexed, stats.skipped, stats.errors
    );

    // Phase 1b: refresh lifecycle states
    scanner::refresh_lifecycle(Arc::clone(&graph), &config.lifecycle)?;

    // Phase 1c: periodic lifecycle refresh every 6 hours
    let _refresh_handle =
        scanner::schedule_lifecycle_refresh(Arc::clone(&graph), 6, config.lifecycle.clone());

    // Phase 1d: watch for changes
    watcher::watch_many(&watch_roots, graph, config.watch.use_git_timestamps)?;

    Ok(())
}

fn resolve_watch_roots(path: Option<&Path>, config: &OrgConfig) -> Vec<PathBuf> {
    let mut raw_roots = Vec::new();
    if let Some(path) = path {
        raw_roots.push(path.to_path_buf());
    }
    raw_roots.extend(config.watch.roots.iter().cloned());
    if raw_roots.is_empty() {
        raw_roots.push(PathBuf::from("."));
    }

    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for root in raw_roots {
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        if seen.insert(canonical.clone()) {
            roots.push(canonical);
        }
    }
    roots
}
