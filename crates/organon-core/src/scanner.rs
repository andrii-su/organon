use std::collections::BTreeMap;
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

/// After a full scan, detect renames the file-watcher may have missed.
///
/// Matches entities present in the DB (within `root`) whose paths are no
/// longer on disk against newly-scanned paths that share the same
/// `content_hash`.  Only performs an unambiguous rename (exactly one
/// on-disk candidate for a given hash that has no entity yet).
///
/// Should be called *after* `scan()` so the DB already holds fresh hashes.
/// Returns the number of renames applied.
pub fn reconcile_renames(root: &str, graph: Arc<Mutex<Graph>>) -> Result<usize> {
    let root_prefix = std::fs::canonicalize(root)
        .unwrap_or_else(|_| std::path::PathBuf::from(root))
        .to_string_lossy()
        .to_string();

    let all = graph.lock().unwrap().all()?;

    // Group on-disk entities by content_hash (hash → paths that exist on FS).
    let mut hash_to_disk_paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for e in all.iter().filter(|e| e.path.starts_with(&root_prefix)) {
        if std::path::Path::new(&e.path).exists() {
            if let Some(h) = &e.content_hash {
                hash_to_disk_paths
                    .entry(h.clone())
                    .or_default()
                    .push(e.path.clone());
            }
        }
    }

    // Entities within root that are missing from disk and not already dead.
    let missing: Vec<_> = all
        .iter()
        .filter(|e| e.path.starts_with(&root_prefix))
        .filter(|e| !Path::new(&e.path).exists())
        .filter(|e| !matches!(e.lifecycle, crate::entity::LifecycleState::Dead))
        .filter(|e| e.content_hash.is_some())
        .collect();

    let mut renames = 0;
    for entity in missing {
        let hash = entity.content_hash.as_deref().unwrap();
        let Some(disk_paths) = hash_to_disk_paths.get(hash) else {
            continue;
        };
        // Only rename if the hash maps to exactly one on-disk path that
        // differs from the entity's current (missing) path.
        let candidates: Vec<_> = disk_paths
            .iter()
            .filter(|p| p.as_str() != entity.path)
            .collect();
        if candidates.len() != 1 {
            continue; // ambiguous or none
        }
        let new_path = candidates[0];
        // Skip if the target already has its own entity (would be a conflict).
        if graph
            .lock()
            .unwrap()
            .get_by_path(new_path)?
            .map(|e| e.path != entity.path)
            .unwrap_or(false)
        {
            continue;
        }
        match graph.lock().unwrap().rename_entity(&entity.path, new_path) {
            Ok(_) => {
                info!("scan: rename continuity {} → {}", entity.path, new_path);
                renames += 1;
            }
            Err(e) => warn!("scan: rename continuity error: {e}"),
        }
    }

    if renames > 0 {
        info!("reconcile_renames: {renames} rename(s) applied");
    }
    Ok(renames)
}

/// Legacy convenience — uses default ignore logic (no .organonignore, no config extras).
pub fn is_ignored(path: &std::path::Path) -> bool {
    is_ignored_default(path)
}
