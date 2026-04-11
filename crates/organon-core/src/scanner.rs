use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use log::{info, warn};
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::config::LifecycleConfig;
use crate::entity::Entity;
use crate::graph::Graph;
use crate::ignore::{is_ignored_default, IgnoreSet};

const MAX_FILE_SIZE: u64 = 100 * 1024 * 1024; // 100 MB

pub struct ScanStats {
    pub total: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub errors: usize,
}

/// Walk `root`, upsert all files into graph. Parallel via rayon.
/// Uses `ignore_set` for layered ignore logic (built-in + extra + .organonignore).
pub fn scan(
    root: &str,
    graph: Arc<Mutex<Graph>>,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<ScanStats> {
    info!("scanning: {root}");

    let paths: Vec<_> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| !ignore_set.is_ignored(e.path()))
        .map(|e| e.path().to_path_buf())
        .collect();

    let total = paths.len();
    info!("found {total} files, indexing...");

    let indexed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let skipped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let errors = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    paths.par_iter().for_each(|path| {
        let path_str = path.to_string_lossy();

        if let Ok(meta) = std::fs::metadata(path) {
            if meta.len() > MAX_FILE_SIZE {
                skipped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        }

        match Entity::from_path_with_options(&path_str, use_git_timestamps) {
            Ok(entity) => match graph.lock().unwrap().upsert(&entity) {
                Ok(_) => {
                    indexed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                Err(e) => {
                    warn!("upsert error {path_str}: {e:?}");
                    errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            },
            Err(e) => {
                warn!("entity error {path_str}: {e:?}");
                errors.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    });

    let stats = ScanStats {
        total,
        indexed: indexed.load(std::sync::atomic::Ordering::Relaxed),
        skipped: skipped.load(std::sync::atomic::Ordering::Relaxed),
        errors: errors.load(std::sync::atomic::Ordering::Relaxed),
    };

    info!(
        "scan done: {} indexed, {} skipped, {} errors (total {})",
        stats.indexed, stats.skipped, stats.errors, stats.total
    );

    Ok(stats)
}

/// Recompute lifecycle for all entities already in DB, using configurable thresholds.
pub fn refresh_lifecycle(graph: Arc<Mutex<Graph>>, lc: &LifecycleConfig) -> Result<usize> {
    use crate::lifecycle::compute_state;
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;

    let entities = graph.lock().unwrap().all()?;
    let mut updated = 0;

    for mut entity in entities {
        let new_state = if Path::new(&entity.path).exists() {
            compute_state(entity.accessed_at, now, lc.dormant_days, lc.archive_days)
        } else {
            crate::entity::LifecycleState::Dead
        };
        if new_state != entity.lifecycle {
            entity.lifecycle = new_state;
            graph.lock().unwrap().upsert(&entity)?;
            updated += 1;
        }
    }

    info!("lifecycle refresh: {updated} entities updated");
    Ok(updated)
}

/// Spawn a background thread that calls `refresh_lifecycle` every `interval_hours`.
pub fn schedule_lifecycle_refresh(
    graph: Arc<Mutex<Graph>>,
    interval_hours: u64,
    lc: LifecycleConfig,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let interval = std::time::Duration::from_secs(interval_hours * 3600);
        loop {
            std::thread::sleep(interval);
            match refresh_lifecycle(Arc::clone(&graph), &lc) {
                Ok(n) => info!("scheduled lifecycle refresh: {n} entities updated"),
                Err(e) => warn!("scheduled lifecycle refresh error: {e:?}"),
            }
        }
    })
}

/// Legacy convenience — uses default ignore logic (no .organonignore, no config extras).
pub fn is_ignored(path: &std::path::Path) -> bool {
    is_ignored_default(path)
}
