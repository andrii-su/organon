use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use log::{info, warn};
use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::entity::Entity;
use crate::graph::Graph;
use crate::ignore::IgnoreSet;

pub struct WatchRoot {
    pub path: PathBuf,
    pub ignore_set: Arc<IgnoreSet>,
}

impl WatchRoot {
    pub fn new(path: PathBuf, ignore_set: Arc<IgnoreSet>) -> Self {
        Self { path, ignore_set }
    }
}

/// Watch one or more roots for filesystem events.
/// Uses a single watcher instance for efficiency.
pub fn watch_many(
    roots: &[WatchRoot],
    graph: Arc<Mutex<Graph>>,
    use_git_timestamps: bool,
) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let config = Config::default().with_poll_interval(std::time::Duration::from_secs(2));
    let mut watcher: RecommendedWatcher = Watcher::new(tx, config)?;

    for root in roots {
        watcher.watch(root.path.as_path(), RecursiveMode::Recursive)?;
        info!("watching: {}", root.path.display());
    }

    for event in rx {
        match event {
            Ok(ev) => handle_event(ev, roots, &graph, use_git_timestamps),
            Err(e) => warn!("watch error: {:?}", e),
        }
    }

    Ok(())
}

/// Watch a single root (backward-compatible entry point).
pub fn watch(
    path: &str,
    graph: Arc<Mutex<Graph>>,
    ignore_set: Arc<IgnoreSet>,
    use_git_timestamps: bool,
) -> Result<()> {
    let root = WatchRoot::new(PathBuf::from(path), ignore_set);
    watch_many(&[root], graph, use_git_timestamps)
}

fn handle_event(
    event: Event,
    roots: &[WatchRoot],
    graph: &Arc<Mutex<Graph>>,
    use_git_timestamps: bool,
) {
    match event.kind {
        EventKind::Create(CreateKind::File)
        | EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_)) => {
            for path in &event.paths {
                if path.is_file() && !is_ignored_for_roots(roots, path) {
                    upsert(path, graph, use_git_timestamps);
                }
            }
        }

        EventKind::Remove(RemoveKind::File) => {
            for path in &event.paths {
                if is_ignored_for_roots(roots, path) {
                    continue;
                }
                let path_str = path.to_string_lossy();
                match graph.lock().unwrap().delete_by_path(&path_str) {
                    Ok(_) => info!("removed entity: {}", path_str),
                    Err(e) => warn!("delete error {}: {:?}", path_str, e),
                }
            }
        }

        EventKind::Access(AccessKind::Read) => {
            // Update accessed_at when a file is read (best-effort — depends on FS/OS atime).
            use std::time::{SystemTime, UNIX_EPOCH};
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            for path in &event.paths {
                if path.is_file() && !is_ignored_for_roots(roots, path) {
                    let path_str = path.to_string_lossy();
                    if let Err(e) = graph.lock().unwrap().touch_accessed(&path_str, now) {
                        warn!("touch_accessed error {}: {:?}", path_str, e);
                    }
                }
            }
        }

        _ => {}
    }
}

fn is_ignored_for_roots(roots: &[WatchRoot], path: &Path) -> bool {
    match matching_root(roots, path) {
        Some(root) => root.ignore_set.is_ignored(path),
        None => true,
    }
}

fn matching_root<'a>(roots: &'a [WatchRoot], path: &Path) -> Option<&'a WatchRoot> {
    roots
        .iter()
        .filter(|root| path.starts_with(&root.path))
        .max_by_key(|root| root.path.components().count())
}

fn upsert(path: &Path, graph: &Arc<Mutex<Graph>>, use_git_timestamps: bool) {
    let path_str = path.to_string_lossy();
    match Entity::from_path_with_options(&path_str, use_git_timestamps) {
        Ok(entity) => match graph.lock().unwrap().upsert(&entity) {
            Ok(_) => info!("upserted entity: {}", entity.path),
            Err(e) => warn!("upsert error {}: {:?}", entity.path, e),
        },
        Err(e) => warn!("entity from_path error {}: {:?}", path_str, e),
    }
}
