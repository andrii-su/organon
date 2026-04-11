use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use log::{info, warn};
use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::entity::Entity;
use crate::graph::Graph;
use crate::ignore::IgnoreSet;

// ── public types ──────────────────────────────────────────────────────────────

pub struct WatchRoot {
    pub path: PathBuf,
    pub ignore_set: Arc<IgnoreSet>,
}

impl WatchRoot {
    pub fn new(path: PathBuf, ignore_set: Arc<IgnoreSet>) -> Self {
        Self { path, ignore_set }
    }
}

// ── rename tracker ────────────────────────────────────────────────────────────

/// A pending remove that might be the "from" side of a rename.
struct PendingEntry {
    path: String,
    queued_at: Instant,
}

/// Buffers short-lived removes so that a subsequent create with the same
/// `content_hash` can be detected as a rename/move rather than
/// delete-then-create.
///
/// Strategy:
/// - On `Remove`: look up the entity's hash; push to `pending` keyed by hash.
/// - On `Create`: compute new file's hash; look up in `pending`.
///   - Exactly one pending entry for that hash within the window → `rename_entity`.
///   - Multiple entries (ambiguous duplicate content) → fall through to `upsert`.
///   - No entry → normal `upsert`.
/// - `flush_expired`: entries older than `window` are executed as real deletes.
///
/// `RenameTrackerHandle` is a public alias used in tests.
pub struct RenameTracker {
    /// content_hash → list of pending removes (usually 0-1 entries)
    pending: HashMap<String, Vec<PendingEntry>>,
    /// How long to wait for the paired create before committing a delete.
    window: Duration,
}

/// Public alias so integration tests can exercise the tracker without
/// reaching into private internals.
pub type RenameTrackerHandle = RenameTracker;

impl RenameTracker {
    pub fn new(window: Duration) -> Self {
        Self {
            pending: HashMap::new(),
            window,
        }
    }

    /// Queue a remove event for potential rename matching.
    /// `content_hash` is the hash of the entity that was removed.
    pub fn push(&mut self, path: String, content_hash: String) {
        self.pending
            .entry(content_hash)
            .or_default()
            .push(PendingEntry {
                path,
                queued_at: Instant::now(),
            });
    }

    /// Try to match a create at `new_path` with a pending remove.
    ///
    /// Returns `Some(old_path)` iff exactly one pending remove has `content_hash`
    /// and it is still within the time window.
    pub fn try_match(&mut self, new_path: &str, content_hash: &str) -> Option<String> {
        let entries = self.pending.get_mut(content_hash)?;
        let now = Instant::now();

        // Retain only entries still within window and not the same path
        entries.retain(|e| now.duration_since(e.queued_at) <= self.window && e.path != new_path);

        if entries.len() == 1 {
            // Exactly one candidate — safe to rename
            let old_path = entries.remove(0).path;
            if entries.is_empty() {
                self.pending.remove(content_hash);
            }
            Some(old_path)
        } else {
            // 0 (expired) or ≥2 (ambiguous) — don't auto-match
            None
        }
    }

    /// Commit all pending removes whose window has expired to the graph.
    /// Must be called periodically (before each event batch or on timeout).
    pub fn flush_expired(&mut self, graph: &Arc<Mutex<Graph>>) {
        let now = Instant::now();
        let mut to_delete: Vec<(String, String)> = Vec::new(); // (hash, path)

        for (hash, entries) in &mut self.pending {
            let expired: Vec<_> = entries
                .iter()
                .filter(|e| now.duration_since(e.queued_at) > self.window)
                .map(|e| (hash.clone(), e.path.clone()))
                .collect();
            to_delete.extend(expired);
        }

        for (hash, path) in &to_delete {
            match graph.lock().unwrap().delete_by_path(path) {
                Ok(_) => info!("removed entity (expired rename window): {}", path),
                Err(e) => warn!("delete error {}: {:?}", path, e),
            }
            if let Some(entries) = self.pending.get_mut(hash) {
                entries.retain(|e| e.path != *path);
                if entries.is_empty() {
                    self.pending.remove(hash);
                }
            }
        }
    }
}

// ── public API ────────────────────────────────────────────────────────────────

/// Watch one or more roots for filesystem events.
pub fn watch_many(
    roots: &[WatchRoot],
    graph: Arc<Mutex<Graph>>,
    use_git_timestamps: bool,
) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let config = Config::default().with_poll_interval(Duration::from_secs(2));
    let mut watcher: RecommendedWatcher = Watcher::new(tx, config)?;

    for root in roots {
        watcher.watch(root.path.as_path(), RecursiveMode::Recursive)?;
        info!("watching: {}", root.path.display());
    }

    let mut tracker = RenameTracker::new(Duration::from_secs(5));

    loop {
        // Use recv_timeout so expired pending deletes are flushed even if
        // the file system goes quiet after a lone Remove event.
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(Ok(ev)) => {
                tracker.flush_expired(&graph);
                handle_event(ev, roots, &graph, use_git_timestamps, &mut tracker);
            }
            Ok(Err(e)) => warn!("watch error: {:?}", e),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                tracker.flush_expired(&graph);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
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

// ── private helpers ───────────────────────────────────────────────────────────

fn handle_event(
    event: Event,
    roots: &[WatchRoot],
    graph: &Arc<Mutex<Graph>>,
    use_git_timestamps: bool,
    tracker: &mut RenameTracker,
) {
    match event.kind {
        // ── native rename (Linux inotify Both) ───────────────────────────────
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) => {
            if event.paths.len() == 2 {
                let old = &event.paths[0];
                let new = &event.paths[1];
                if !is_ignored_for_roots(roots, new) {
                    do_rename(old, new, graph, use_git_timestamps);
                }
            }
        }

        // ── rename From (Linux inotify) — treat as buffered remove ───────────
        EventKind::Modify(ModifyKind::Name(RenameMode::From)) => {
            for path in &event.paths {
                if is_ignored_for_roots(roots, path) {
                    continue;
                }
                buffer_remove(path, graph, tracker);
            }
        }

        // ── rename To (Linux inotify) — match against buffer or upsert ───────
        EventKind::Modify(ModifyKind::Name(RenameMode::To)) => {
            for path in &event.paths {
                if path.is_file() && !is_ignored_for_roots(roots, path) {
                    handle_create(path, graph, use_git_timestamps, tracker);
                }
            }
        }

        // ── create / modify ───────────────────────────────────────────────────
        EventKind::Create(CreateKind::File)
        | EventKind::Modify(ModifyKind::Data(_))
        | EventKind::Modify(ModifyKind::Metadata(_)) => {
            for path in &event.paths {
                if path.is_file() && !is_ignored_for_roots(roots, path) {
                    handle_create(path, graph, use_git_timestamps, tracker);
                }
            }
        }

        // ── remove ────────────────────────────────────────────────────────────
        EventKind::Remove(RemoveKind::File) => {
            for path in &event.paths {
                if is_ignored_for_roots(roots, path) {
                    continue;
                }
                buffer_remove(path, graph, tracker);
            }
        }

        // ── access ────────────────────────────────────────────────────────────
        EventKind::Access(AccessKind::Read) => {
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

/// Buffer a remove: look up current hash in graph and push to tracker.
/// If the entity has no hash, fall through to immediate delete.
fn buffer_remove(path: &Path, graph: &Arc<Mutex<Graph>>, tracker: &mut RenameTracker) {
    let path_str = path.to_string_lossy().to_string();

    let hash_opt = graph
        .lock()
        .unwrap()
        .get_by_path(&path_str)
        .ok()
        .flatten()
        .and_then(|e| e.content_hash);

    match hash_opt {
        Some(hash) => {
            info!("buffering remove for rename detection: {}", path_str);
            tracker.push(path_str, hash);
        }
        None => {
            // No hash or entity unknown — delete immediately
            match graph.lock().unwrap().delete_by_path(&path_str) {
                Ok(_) => info!("removed entity: {}", path_str),
                Err(e) => warn!("delete error {}: {:?}", path_str, e),
            }
        }
    }
}

/// Handle a create/modify event: check rename tracker first, then upsert.
fn handle_create(
    path: &Path,
    graph: &Arc<Mutex<Graph>>,
    use_git_timestamps: bool,
    tracker: &mut RenameTracker,
) {
    let path_str = path.to_string_lossy().to_string();

    // Compute hash of the new file to check for rename match.
    // We need the entity anyway, so build it now.
    let entity = match Entity::from_path_with_options(&path_str, use_git_timestamps) {
        Ok(e) => e,
        Err(e) => {
            warn!("entity from_path error {}: {:?}", path_str, e);
            return;
        }
    };

    // Try rename match
    if let Some(ref hash) = entity.content_hash {
        if let Some(old_path) = tracker.try_match(&path_str, hash) {
            info!("detected rename: {} → {}", old_path, path_str);
            match graph.lock().unwrap().rename_entity(&old_path, &path_str) {
                Ok(outcome) => {
                    info!("rename applied ({:?}): {} → {}", outcome, old_path, path_str);
                    // Also update mtime/size via upsert to reflect any metadata change.
                    // The id / summary / lifecycle are preserved by rename_entity.
                    if let Err(e) = graph.lock().unwrap().upsert(&entity) {
                        warn!("post-rename upsert error {}: {:?}", path_str, e);
                    }
                    return;
                }
                Err(e) => {
                    warn!("rename_entity error {} → {}: {:?}", old_path, path_str, e);
                    // Fall through to normal upsert
                }
            }
        }
    }

    // Normal upsert (new file or rename detection failed)
    upsert_entity(entity, graph);
}

fn do_rename(old: &Path, new: &Path, graph: &Arc<Mutex<Graph>>, use_git_timestamps: bool) {
    let old_str = old.to_string_lossy();
    let new_str = new.to_string_lossy();

    match graph.lock().unwrap().rename_entity(&old_str, &new_str) {
        Ok(outcome) => {
            info!(
                "native rename applied ({:?}): {} → {}",
                outcome,
                old.display(),
                new.display()
            );
            // Update metadata (mtime, size) at new path
            if new.is_file() {
                if let Ok(entity) = Entity::from_path_with_options(&new_str, use_git_timestamps) {
                    if let Err(e) = graph.lock().unwrap().upsert(&entity) {
                        warn!("post-rename upsert error {}: {:?}", new_str, e);
                    }
                }
            }
        }
        Err(e) => warn!(
            "native rename error {} → {}: {:?}",
            old.display(),
            new.display(),
            e
        ),
    }
}

fn upsert_entity(entity: Entity, graph: &Arc<Mutex<Graph>>) {
    let path = entity.path.clone();
    match graph.lock().unwrap().upsert(&entity) {
        Ok(_) => info!("upserted entity: {}", path),
        Err(e) => warn!("upsert error {}: {:?}", path, e),
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
