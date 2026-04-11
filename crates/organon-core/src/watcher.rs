use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use log::{info, warn};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};

use crate::entity::Entity;
use crate::graph::Graph;
use crate::ignore::IgnoreSet;

/// Watch one or more roots for filesystem events.
/// Uses a single watcher instance for efficiency.
pub fn watch_many(roots: &[&str], graph: Arc<Mutex<Graph>>, ignore_set: Arc<IgnoreSet>) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let config = Config::default().with_poll_interval(std::time::Duration::from_secs(2));
    let mut watcher: RecommendedWatcher = Watcher::new(tx, config)?;

    for root in roots {
        watcher.watch(PathBuf::from(root).as_path(), RecursiveMode::Recursive)?;
        info!("watching: {}", root);
    }

    for event in rx {
        match event {
            Ok(ev) => handle_event(ev, &graph, &ignore_set),
            Err(e) => warn!("watch error: {:?}", e),
        }
    }

    Ok(())
}

/// Watch a single root (backward-compatible entry point).
pub fn watch(path: &str, graph: Arc<Mutex<Graph>>, ignore_set: Arc<IgnoreSet>) -> Result<()> {
    watch_many(&[path], graph, ignore_set)
}

fn handle_event(event: Event, graph: &Arc<Mutex<Graph>>, ignore_set: &IgnoreSet) {
    match event.kind {
        EventKind::Create(CreateKind::File)
        | EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_)) => {
            for path in &event.paths {
                if path.is_file() && !ignore_set.is_ignored(path) {
                    upsert(path, graph);
                }
            }
        }

        EventKind::Remove(RemoveKind::File) => {
            for path in &event.paths {
                if ignore_set.is_ignored(path) { continue; }
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
                if path.is_file() && !ignore_set.is_ignored(path) {
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

fn upsert(path: &Path, graph: &Arc<Mutex<Graph>>) {
    let path_str = path.to_string_lossy();
    match Entity::from_path(&path_str) {
        Ok(entity) => match graph.lock().unwrap().upsert(&entity) {
            Ok(_) => info!("upserted entity: {}", entity.path),
            Err(e) => warn!("upsert error {}: {:?}", entity.path, e),
        },
        Err(e) => warn!("entity from_path error {}: {:?}", path_str, e),
    }
}
