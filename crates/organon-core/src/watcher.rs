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
use crate::LockRecover;

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

    /// Drop pending removes for `path`: the file is back on disk, so a
    /// buffered remove for it is stale. Removes are keyed by the *old*
    /// content hash, so a re-created file with new content is never found by
    /// `try_match` and would otherwise be deleted when the window expires.
    pub fn cancel_path(&mut self, path: &str) {
        self.pending.retain(|_, entries| {
            entries.retain(|e| e.path != path);
            !entries.is_empty()
        });
    }

    /// Try to match a create at `new_path` with a pending remove.
    ///
    /// Returns `Some(old_path)` iff exactly one pending remove has `content_hash`
    /// and it is still within the time window.
    pub fn try_match(&mut self, new_path: &str, content_hash: &str) -> Option<String> {
        // Size-based pseudo-hashes only encode byte length; two unrelated large
        // files of equal size collide, so matching on them would treat a
        // delete+create of distinct files as a rename and destroy identity.
        if crate::entity::is_pseudo_hash(content_hash) {
            return None;
        }
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
            if on_disk(Path::new(path)).is_some() {
                info!("kept entity, file still on disk after remove event: {path}");
            } else {
                match graph.lock_recover().delete_by_path(path) {
                    Ok(_) => info!("removed entity (expired rename window): {path}"),
                    Err(e) => warn!("delete error {path}: {e:?}"),
                }
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
            Ok(Err(e)) => warn!("watch error: {e:?}"),
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
        EventKind::Modify(ModifyKind::Name(RenameMode::Both)) if event.paths.len() == 2 => {
            let old = &event.paths[0];
            let new = &event.paths[1];
            if !is_ignored_for_roots(roots, new) {
                do_rename(old, new, graph, use_git_timestamps);
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

        // ── rename Any (macOS FSEvents) — one event per side, unpaired ───────
        // The side that exists is the destination; the missing side is the
        // source. Buffering the source lets `handle_create` pair them by hash.
        EventKind::Modify(ModifyKind::Name(RenameMode::Any)) => {
            for path in &event.paths {
                if is_ignored_for_roots(roots, path) {
                    continue;
                }
                match on_disk(path) {
                    Some(ft) if ft.is_file() => {
                        handle_create(path, graph, use_git_timestamps, tracker)
                    }
                    None => buffer_remove(path, graph, tracker),
                    // Directories and symlinks are not indexed as files.
                    Some(_) => {}
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
                    if let Err(e) = graph.lock_recover().touch_accessed(&path_str, now) {
                        warn!("touch_accessed error {path_str}: {e:?}");
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

    // Events are handled after the fact. On macOS FSEvents coalesces
    // Created|Removed for one path (e.g. `git checkout` replacing a file) and
    // notify emits the Remove after the Create, so a remove for a file that
    // exists now is stale; acting on it would drop a live file from the index.
    if on_disk(path).is_some() {
        info!("ignored remove event, file exists: {path_str}");
        return;
    }

    let hash_opt = graph
        .lock_recover()
        .get_by_path(&path_str)
        .ok()
        .flatten()
        .and_then(|e| e.content_hash);

    match hash_opt {
        Some(hash) => {
            if crate::entity::is_pseudo_hash(&hash) {
                // Size-based pseudo-hashes are intentionally excluded from
                // rename matching; delete immediately instead of buffering.
                match graph.lock_recover().delete_by_path(&path_str) {
                    Ok(_) => info!("removed entity: {path_str}"),
                    Err(e) => warn!("delete error {path_str}: {e:?}"),
                }
            } else {
                info!("buffering remove for rename detection: {path_str}");
                tracker.push(path_str, hash);
            }
        }
        None => {
            // No hash or entity unknown — delete immediately
            match graph.lock_recover().delete_by_path(&path_str) {
                Ok(_) => info!("removed entity: {path_str}"),
                Err(e) => warn!("delete error {path_str}: {e:?}"),
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
    tracker.cancel_path(&path_str);

    // Compute hash of the new file to check for rename match.
    // We need the entity anyway, so build it now.
    let entity = match Entity::from_path_with_options(&path_str, use_git_timestamps) {
        Ok(e) => e,
        Err(e) => {
            warn!("entity from_path error {path_str}: {e:?}");
            return;
        }
    };

    // Try rename match
    if let Some(ref hash) = entity.content_hash {
        if let Some(old_path) = tracker.try_match(&path_str, hash) {
            info!("detected rename: {old_path} → {path_str}");
            // Bind first: a guard in the match scrutinee lives for the whole
            // match, and the Ok arm locks again (std Mutex is not reentrant).
            let renamed = graph.lock_recover().rename_entity(&old_path, &path_str);
            match renamed {
                Ok(outcome) => {
                    info!("rename applied ({outcome:?}): {old_path} → {path_str}");
                    // Also update mtime/size via upsert to reflect any metadata change.
                    // The id / summary / lifecycle are preserved by rename_entity.
                    if let Err(e) = graph.lock_recover().upsert(&entity) {
                        warn!("post-rename upsert error {path_str}: {e:?}");
                    }
                    return;
                }
                Err(e) => {
                    warn!("rename_entity error {old_path} → {path_str}: {e:?}");
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

    // Bind first: see handle_create — the Ok arm locks the graph again.
    let renamed = graph.lock_recover().rename_entity(&old_str, &new_str);
    match renamed {
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
                    if let Err(e) = graph.lock_recover().upsert(&entity) {
                        warn!("post-rename upsert error {new_str}: {e:?}");
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

/// File type of whatever sits at exactly `path` now, or `None` if it is gone.
///
/// Symlinks are not followed. On case-insensitive volumes (the macOS default)
/// `Path::exists` reports `Foo.py` present after a rename to `foo.py`, which
/// would leave the old entity in the graph forever, so the name is also
/// checked against the directory listing. Only a case-variant entry counts
/// as "gone": if the listing has no exact or case-variant match (e.g. a
/// Unicode normalization difference) or cannot be read, trust the metadata
/// and keep the file, because wrongly deleting a live entity is the worse
/// failure.
fn on_disk(path: &Path) -> Option<std::fs::FileType> {
    let file_type = std::fs::symlink_metadata(path).ok()?.file_type();
    if crate::freshness::only_case_variant(path) {
        None
    } else {
        Some(file_type)
    }
}

fn upsert_entity(entity: Entity, graph: &Arc<Mutex<Graph>>) {
    let path = entity.path.clone();
    match graph.lock_recover().upsert(&entity) {
        Ok(_) => info!("upserted entity: {path}"),
        Err(e) => warn!("upsert error {path}: {e:?}"),
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

#[cfg(test)]
mod tests {
    //! Event-sequence tests for `handle_event`.
    //!
    //! On macOS, FSEvents coalesces flags for one path, and notify splits a
    //! coalesced `Created|Removed|Modified` record into Create → Remove →
    //! Modify(Data), in that order. `git checkout` (unlink + create) produces
    //! exactly this. These tests replay such sequences against real files.
    use super::*;
    use tempfile::TempDir;

    struct Fixture {
        _dir: TempDir,
        roots: Vec<WatchRoot>,
        graph: Arc<Mutex<Graph>>,
        _db: tempfile::NamedTempFile,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let ignore = Arc::new(IgnoreSet::load(&root, &[]));
            let db = tempfile::NamedTempFile::new().unwrap();
            let graph = Arc::new(Mutex::new(
                Graph::open(db.path().to_str().unwrap()).unwrap(),
            ));
            Self {
                _dir: dir,
                roots: vec![WatchRoot::new(root, ignore)],
                graph,
                _db: db,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.roots[0].path.join(name)
        }

        fn send(&self, kind: EventKind, path: &Path, tracker: &mut RenameTracker) {
            let ev = Event::new(kind).add_path(path.to_path_buf());
            handle_event(ev, &self.roots, &self.graph, false, tracker);
        }

        fn indexed(&self, path: &Path) -> Option<Entity> {
            self.graph
                .lock_recover()
                .get_by_path(&path.to_string_lossy())
                .unwrap()
        }
    }

    const CREATE: EventKind = EventKind::Create(CreateKind::File);
    const REMOVE: EventKind = EventKind::Remove(RemoveKind::File);
    const RENAME_ANY: EventKind = EventKind::Modify(ModifyKind::Name(RenameMode::Any));
    const SHORT_WINDOW: Duration = Duration::from_millis(20);

    /// Expire every pending remove and flush it.
    fn flush_now(tracker: &mut RenameTracker, graph: &Arc<Mutex<Graph>>) {
        std::thread::sleep(SHORT_WINDOW * 3);
        tracker.flush_expired(graph);
    }

    #[test]
    fn coalesced_create_remove_modify_keeps_existing_file() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(SHORT_WINDOW);
        let p = fx.path("runner.py");
        std::fs::write(&p, "v1").unwrap();
        fx.send(CREATE, &p, &mut tracker);
        assert!(fx.indexed(&p).is_some());

        // git checkout replaces the file; FSEvents reports one coalesced record.
        std::fs::remove_file(&p).unwrap();
        std::fs::write(&p, "v2").unwrap();
        // No trailing Modify: try_match would prune the same-path entry and
        // hide the bug. Create|Removed without Modified is enough to lose it.
        fx.send(CREATE, &p, &mut tracker);
        fx.send(REMOVE, &p, &mut tracker);
        flush_now(&mut tracker, &fx.graph);

        assert!(fx.indexed(&p).is_some(), "file on disk must stay indexed");
    }

    #[test]
    fn remove_then_recreate_same_path_keeps_entity() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(SHORT_WINDOW);
        let p = fx.path("urls.py");
        std::fs::write(&p, "v1").unwrap();
        fx.send(CREATE, &p, &mut tracker);

        // Delete and re-create delivered as separate events.
        std::fs::remove_file(&p).unwrap();
        fx.send(REMOVE, &p, &mut tracker);
        std::fs::write(&p, "v2").unwrap();
        fx.send(CREATE, &p, &mut tracker);
        flush_now(&mut tracker, &fx.graph);

        assert!(
            fx.indexed(&p).is_some(),
            "re-created file must stay indexed"
        );
    }

    #[test]
    fn flush_skips_pending_remove_when_file_exists() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(SHORT_WINDOW);
        let p = fx.path("tasks.py");
        std::fs::write(&p, "v1").unwrap();
        fx.send(CREATE, &p, &mut tracker);
        let hash = fx.indexed(&p).unwrap().content_hash.unwrap();

        // A stale remove that slipped into the buffer.
        tracker.push(p.to_string_lossy().to_string(), hash);
        flush_now(&mut tracker, &fx.graph);

        assert!(fx.indexed(&p).is_some());
    }

    #[test]
    fn real_delete_still_removes_entity() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(SHORT_WINDOW);
        let p = fx.path("gone.py");
        std::fs::write(&p, "v1").unwrap();
        fx.send(CREATE, &p, &mut tracker);

        std::fs::remove_file(&p).unwrap();
        fx.send(REMOVE, &p, &mut tracker);
        flush_now(&mut tracker, &fx.graph);

        assert!(
            fx.indexed(&p).is_none(),
            "deleted file must leave the index"
        );
    }

    #[test]
    fn macos_rename_any_moves_entity() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(Duration::from_secs(5));
        let old = fx.path("old.py");
        let new = fx.path("new.py");
        std::fs::write(&old, "same content").unwrap();
        fx.send(CREATE, &old, &mut tracker);
        let id = fx.indexed(&old).unwrap().id;

        // FSEvents emits Modify(Name(Any)) for both sides, no pairing info.
        std::fs::rename(&old, &new).unwrap();
        fx.send(RENAME_ANY, &old, &mut tracker);
        fx.send(RENAME_ANY, &new, &mut tracker);

        assert!(fx.indexed(&old).is_none());
        assert_eq!(fx.indexed(&new).map(|e| e.id), Some(id), "identity kept");
    }

    #[test]
    fn macos_rename_any_indexes_file_moved_in() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(Duration::from_secs(5));
        let p = fx.path("moved_in.py");
        std::fs::write(&p, "x").unwrap();
        fx.send(RENAME_ANY, &p, &mut tracker);
        assert!(fx.indexed(&p).is_some());
    }

    #[test]
    fn case_only_rename_drops_old_name() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(Duration::from_secs(5));
        let old = fx.path("Foo.py");
        let new = fx.path("foo.py");
        std::fs::write(&old, "case").unwrap();
        fx.send(CREATE, &old, &mut tracker);
        let id = fx.indexed(&old).unwrap().id;

        std::fs::rename(&old, &new).unwrap();
        fx.send(RENAME_ANY, &old, &mut tracker);
        fx.send(RENAME_ANY, &new, &mut tracker);

        assert!(
            fx.indexed(&old).is_none(),
            "old-case name must leave the index"
        );
        assert_eq!(fx.indexed(&new).map(|e| e.id), Some(id));
    }

    #[cfg(unix)]
    #[test]
    fn rename_any_ignores_symlinks() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(Duration::from_secs(5));
        let target = fx.path("target.py");
        let link = fx.path("link.py");
        std::fs::write(&target, "t").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        fx.send(RENAME_ANY, &link, &mut tracker);
        assert!(fx.indexed(&link).is_none(), "symlinks are not indexed");
    }

    #[test]
    fn native_rename_both_moves_entity_without_deadlock() {
        let fx = Fixture::new();
        let mut tracker = RenameTracker::new(Duration::from_secs(5));
        let old = fx.path("a.py");
        let new = fx.path("b.py");
        std::fs::write(&old, "x").unwrap();
        fx.send(CREATE, &old, &mut tracker);
        let id = fx.indexed(&old).unwrap().id;

        std::fs::rename(&old, &new).unwrap();
        let ev = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
            .add_path(old.clone())
            .add_path(new.clone());
        handle_event(ev, &fx.roots, &fx.graph, false, &mut tracker);

        assert!(fx.indexed(&old).is_none());
        assert_eq!(fx.indexed(&new).map(|e| e.id), Some(id));
    }
}
