use std::sync::{Arc, Mutex};

use anyhow::Result;
use organon_core::{graph::Graph, scanner, watcher};

fn main() -> Result<()> {
    env_logger::init();

    let db_path = std::env::var("ORGANON_DB")
        .unwrap_or_else(|_| format!("{}/.organon/entities.db",
            std::env::var("HOME").unwrap_or_else(|_| ".".to_string())));

    let watch_path = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());

    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let graph = Arc::new(Mutex::new(Graph::open(&db_path)?));
    log::info!("organon-core started. db={} watch={}", db_path, watch_path);

    // Phase 1a: index existing files
    let stats = scanner::scan(&watch_path, Arc::clone(&graph))?;
    println!(
        "indexed {} files ({} skipped, {} errors)",
        stats.indexed, stats.skipped, stats.errors
    );

    // Phase 1b: refresh lifecycle states
    scanner::refresh_lifecycle(Arc::clone(&graph))?;

    // Phase 1c: periodic lifecycle refresh every 6 hours
    let _refresh_handle = scanner::schedule_lifecycle_refresh(Arc::clone(&graph), 6);

    // Phase 1d: watch for changes
    watcher::watch(&watch_path, graph)?;

    Ok(())
}
