//! Disk ↔ graph drift: detect it cheaply and repair it.
//!
//! The watcher keeps the graph in sync event by event, but events get lost:
//! the daemon was down, the OS coalesced or dropped them, a directory moved.
//! `quick_drift` finds the gap from file metadata alone (no hashing), cheap
//! enough to run on MCP requests behind a short cache. `reconcile` applies
//! it, so a periodic call heals whatever the watcher missed.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use log::{info, warn};
use walkdir::WalkDir;

use crate::entity::{is_pseudo_hash, Entity};
use crate::graph::Graph;
use crate::ignore::IgnoreSet;
use crate::scanner::MAX_FILE_SIZE;
use crate::LockRecover;

/// Below this many deletions the mass-delete guard never trips.
const MASS_DELETE_MIN: usize = 50;

/// Paths under one root where the graph and the disk disagree.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Drift {
    /// On disk, not in the graph.
    pub new: Vec<String>,
    /// In the graph, gone from disk.
    pub deleted: Vec<String>,
    /// In both, but size (or mtime) differs.
    pub changed: Vec<String>,
}

impl Drift {
    pub fn is_fresh(&self) -> bool {
        self.new.is_empty() && self.deleted.is_empty() && self.changed.is_empty()
    }

    pub fn total(&self) -> usize {
        self.new.len() + self.deleted.len() + self.changed.len()
    }

    pub fn extend(&mut self, other: Drift) {
        self.new.extend(other.new);
        self.deleted.extend(other.deleted);
        self.changed.extend(other.changed);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    pub added: usize,
    pub updated: usize,
    pub renamed: usize,
    pub removed: usize,
    /// Deletions held back by the mass-delete guard.
    pub deletes_skipped: usize,
    pub errors: usize,
}

/// Size and mtime of a file on disk, as the scanner would index it.
struct DiskFile {
    size: u64,
    mtime: Option<i64>,
}

/// Compare the graph with the disk under `root` using metadata only.
///
/// "Changed" means the size differs, or the mtime differs when the graph
/// stores filesystem mtimes. With `use_git_timestamps` the stored mtime is a
/// commit time, so only size is compared; same-size edits are then missed
/// here and caught by the watcher or a full `organon diff`.
///
/// `max_files` bounds the walk; exceeding it is an error, so callers on a
/// request path can report "unknown" instead of stalling.
pub fn quick_drift(
    graph: &Graph,
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
    max_files: Option<usize>,
) -> Result<Drift> {
    let root = canonical_root(root)?;
    let disk = walk_disk(&root, ignore_set, max_files)?;
    let indexed = indexed_under(graph, &root)?;
    Ok(compare(&disk, &indexed, use_git_timestamps))
}

/// Bring the graph under `root` in line with the disk.
///
/// New and changed files are re-read and upserted; a deleted file whose
/// content reappears at exactly one new path is renamed, keeping its id,
/// history and relationships; other deleted files are removed. The disk walk
/// and hashing run without the graph lock. Errors on single files are counted
/// and logged, not fatal.
pub fn reconcile(
    graph: &Arc<Mutex<Graph>>,
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<ReconcileStats> {
    let root = canonical_root(root)?;
    let disk = walk_disk(&root, ignore_set, None)?;
    let indexed = indexed_under(&graph.lock_recover(), &root)?;
    let drift = compare(&disk, &indexed, use_git_timestamps);
    let mut stats = ReconcileStats::default();
    if drift.is_fresh() {
        return Ok(stats);
    }

    // An unmounted volume or evicted cloud folder looks like an empty
    // directory; deleting everything under it would destroy history and
    // relationships for files that will come back. Hold deletions when the
    // root is empty or they would remove most of the index.
    let deletes_allowed = drift.deleted.is_empty()
        || !(disk.is_empty()
            || (drift.deleted.len() >= MASS_DELETE_MIN && drift.deleted.len() * 2 > indexed.len()));
    if !deletes_allowed {
        warn!(
            "reconcile {}: holding {} deletion(s) of {} indexed file(s) — root looks empty or \
             unmounted; run `organon clean` if the files are really gone",
            root.display(),
            drift.deleted.len(),
            indexed.len()
        );
        stats.deletes_skipped = drift.deleted.len();
    }

    let read = |path: &str, stats: &mut ReconcileStats| match Entity::from_path_with_options(
        path,
        use_git_timestamps,
    ) {
        Ok(e) => Some(e),
        Err(e) => {
            warn!("reconcile read error {path}: {e:?}");
            stats.errors += 1;
            None
        }
    };
    let new_entities: Vec<Entity> = drift
        .new
        .iter()
        .filter_map(|p| read(p, &mut stats))
        .collect();

    // Rename pairing: unique content hash on both sides.
    let mut renames: Vec<(String, Entity)> = Vec::new();
    let mut unpaired_new: Vec<Entity> = Vec::new();
    let mut deleted_left: Vec<String> = drift.deleted.clone();
    if deletes_allowed {
        let mut gone_by_hash: HashMap<&str, Vec<&str>> = HashMap::new();
        for path in &drift.deleted {
            if let Some(h) = indexed[path].content_hash.as_deref() {
                if !is_pseudo_hash(h) {
                    gone_by_hash.entry(h).or_default().push(path);
                }
            }
        }
        let mut new_by_hash: HashMap<String, usize> = HashMap::new();
        for e in &new_entities {
            if let Some(h) = &e.content_hash {
                *new_by_hash.entry(h.clone()).or_default() += 1;
            }
        }
        for e in new_entities {
            let pair = e.content_hash.as_deref().and_then(|h| {
                let olds = gone_by_hash.get(h)?;
                (olds.len() == 1 && new_by_hash[h] == 1).then(|| olds[0].to_string())
            });
            match pair {
                Some(old) => {
                    deleted_left.retain(|p| p != &old);
                    renames.push((old, e));
                }
                None => unpaired_new.push(e),
            }
        }
    } else {
        unpaired_new = new_entities;
    }

    for (old, entity) in renames {
        let g = graph.lock_recover();
        if !is_gone(Path::new(&old)) {
            // Came back since the walk: index the new path as a new file.
            if let Err(e) = g.upsert_scanned(&entity) {
                warn!("reconcile upsert error {}: {e:?}", entity.path);
                stats.errors += 1;
            } else {
                stats.added += 1;
            }
            continue;
        }
        match g.rename_entity(&old, &entity.path) {
            Ok(_) => match g.upsert_scanned(&entity) {
                Ok(()) => stats.renamed += 1,
                Err(e) => {
                    warn!("reconcile post-rename upsert error {}: {e:?}", entity.path);
                    stats.errors += 1;
                }
            },
            Err(e) => {
                warn!("reconcile rename error {old} → {}: {e:?}", entity.path);
                stats.errors += 1;
            }
        }
    }
    for entity in unpaired_new {
        match graph.lock_recover().upsert_scanned(&entity) {
            Ok(()) => stats.added += 1,
            Err(e) => {
                warn!("reconcile upsert error {}: {e:?}", entity.path);
                stats.errors += 1;
            }
        }
    }
    for path in &drift.changed {
        let Some(entity) = read(path, &mut stats) else {
            continue;
        };
        match graph.lock_recover().upsert_scanned(&entity) {
            Ok(()) => stats.updated += 1,
            Err(e) => {
                warn!("reconcile upsert error {path}: {e:?}");
                stats.errors += 1;
            }
        }
    }
    if deletes_allowed {
        for path in &deleted_left {
            let g = graph.lock_recover();
            // Re-check under the lock: the walk may be minutes old and the
            // file re-created since (branch switch, stash pop).
            if !is_gone(Path::new(path)) {
                continue;
            }
            match g.delete_by_path(path) {
                Ok(()) => stats.removed += 1,
                Err(e) => {
                    warn!("reconcile delete error {path}: {e:?}");
                    stats.errors += 1;
                }
            }
        }
    }
    info!(
        "reconciled {}: {} added, {} updated, {} renamed, {} removed, {} deletion(s) held, {} errors",
        root.display(),
        stats.added,
        stats.updated,
        stats.renamed,
        stats.removed,
        stats.deletes_skipped,
        stats.errors
    );
    Ok(stats)
}

/// Run `reconcile` for every root every `interval`, in a background thread.
pub fn schedule_reconcile(
    graph: Arc<Mutex<Graph>>,
    roots: Vec<(PathBuf, Arc<IgnoreSet>)>,
    interval: std::time::Duration,
    use_git_timestamps: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        for (root, ignore_set) in &roots {
            if let Err(e) = reconcile(&graph, root, ignore_set, use_git_timestamps) {
                warn!("scheduled reconcile error {}: {e:?}", root.display());
            }
        }
    })
}

/// Roots that some watcher keeps indexed: `[watch].roots` plus the roots of
/// daemons recorded under `$ORGANON_HOME/daemon`. Canonicalized, deduplicated.
pub fn known_watch_roots(config_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = config_roots.to_vec();
    let daemon_dir = crate::workspace::organon_home().join("daemon");
    if let Ok(entries) = std::fs::read_dir(daemon_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(meta) = serde_json::from_str::<serde_json::Value>(&text) else {
                continue;
            };
            if let Some(list) = meta.get("roots").and_then(|r| r.as_array()) {
                roots.extend(list.iter().filter_map(|r| r.as_str()).map(PathBuf::from));
            }
        }
    }
    let mut roots: Vec<PathBuf> = roots
        .into_iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// True when nothing exists at exactly `path` any more.
///
/// Symlinks are not followed. On case-insensitive volumes `Foo.py` still
/// "exists" after a rename to `foo.py`, so a name that only survives as a case
/// variant in the directory listing counts as gone. If the listing has no
/// exact or case-variant match (e.g. a Unicode normalization difference) or
/// cannot be read, the file is treated as present: wrongly deleting a live
/// entity is the worse failure.
pub fn is_gone(path: &Path) -> bool {
    match std::fs::symlink_metadata(path) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => {
            let (Some(name), Some(parent)) = (path.file_name(), path.parent()) else {
                return false;
            };
            let Ok(entries) = std::fs::read_dir(parent) else {
                return false;
            };
            let wanted = name.to_string_lossy().to_lowercase();
            let mut case_variant = false;
            for entry in entries.flatten() {
                let entry_name = entry.file_name();
                if entry_name == name {
                    return false;
                }
                if entry_name.to_string_lossy().to_lowercase() == wanted {
                    case_variant = true;
                }
            }
            case_variant
        }
    }
}

fn compare(
    disk: &BTreeMap<String, DiskFile>,
    indexed: &BTreeMap<String, Entity>,
    use_git_timestamps: bool,
) -> Drift {
    let mut drift = Drift::default();
    for (path, file) in disk {
        match indexed.get(path) {
            None => drift.new.push(path.clone()),
            Some(e) => {
                let size_differs = e.size_bytes != file.size;
                let mtime_differs = !use_git_timestamps && file.mtime != Some(e.modified_at);
                if size_differs || mtime_differs {
                    drift.changed.push(path.clone());
                }
            }
        }
    }
    for path in indexed.keys() {
        // Paths missing from the walk but still on disk (ignored, walk error
        // such as permission denied) are not deletions.
        if !disk.contains_key(path) && is_gone(Path::new(path)) {
            drift.deleted.push(path.clone());
        }
    }
    drift
}

fn indexed_under(graph: &Graph, root: &Path) -> Result<BTreeMap<String, Entity>> {
    let prefix = dir_prefix(root);
    Ok(graph
        .all()?
        .into_iter()
        .filter(|e| e.path.starts_with(&prefix))
        .map(|e| (e.path.clone(), e))
        .collect())
}

fn canonical_root(root: &Path) -> Result<PathBuf> {
    // A missing root must fail loudly: walking it would yield nothing and
    // every indexed file under it would look deleted.
    match std::fs::canonicalize(root) {
        Ok(p) if p.is_dir() => Ok(p),
        Ok(p) => bail!("not a directory: {}", p.display()),
        Err(e) => bail!("cannot read root {}: {e}", root.display()),
    }
}

/// `root` as a string prefix that only matches paths inside it.
fn dir_prefix(root: &Path) -> String {
    let mut s = root.to_string_lossy().to_string();
    if !s.ends_with(std::path::MAIN_SEPARATOR) {
        s.push(std::path::MAIN_SEPARATOR);
    }
    s
}

fn walk_disk(
    root: &Path,
    ignore_set: &IgnoreSet,
    max_files: Option<usize>,
) -> Result<BTreeMap<String, DiskFile>> {
    let mut files = BTreeMap::new();
    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !ignore_set.is_ignored(e.path()))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.len() > MAX_FILE_SIZE {
            continue;
        }
        if max_files.is_some_and(|max| files.len() >= max) {
            bail!("more than {} files under {}", files.len(), root.display());
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        files.insert(
            entry.path().to_string_lossy().to_string(),
            DiskFile {
                size: meta.len(),
                mtime,
            },
        );
    }
    Ok(files)
}
