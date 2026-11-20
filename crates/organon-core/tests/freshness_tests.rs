//! Disk ↔ graph drift detection and reconciliation.
use std::path::Path;
use std::sync::{Arc, Mutex};

use organon_core::{entity::Entity, freshness, graph::Graph, ignore::IgnoreSet};
use tempfile::{NamedTempFile, TempDir};

struct Fx {
    dir: TempDir,
    graph: Arc<Mutex<Graph>>,
    ignore: IgnoreSet,
    _db: NamedTempFile,
}

impl Fx {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let db = NamedTempFile::new().unwrap();
        let graph = Arc::new(Mutex::new(
            Graph::open(db.path().to_str().unwrap()).unwrap(),
        ));
        let ignore = IgnoreSet::load(&dir.path().canonicalize().unwrap(), &[]);
        Self {
            dir,
            graph,
            ignore,
            _db: db,
        }
    }
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().canonicalize().unwrap()
    }
    fn write(&self, name: &str, body: &str) -> String {
        let p = self.root().join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        p.to_string_lossy().to_string()
    }
    fn index(&self, path: &str) {
        let e = Entity::from_path_with_options(path, false).unwrap();
        self.graph.lock().unwrap().upsert(&e).unwrap();
    }
    fn indexed(&self, path: &str) -> bool {
        self.graph
            .lock()
            .unwrap()
            .get_by_path(path)
            .unwrap()
            .is_some()
    }
    fn drift(&self) -> freshness::Drift {
        let g = self.graph.lock().unwrap();
        freshness::quick_drift(&g, &self.root(), &self.ignore, false, None).unwrap()
    }
}

#[test]
fn fresh_index_has_no_drift() {
    let fx = Fx::new();
    let a = fx.write("a.py", "a");
    fx.index(&a);
    let d = fx.drift();
    assert!(d.is_fresh(), "{d:?}");
}

#[test]
fn reports_new_deleted_and_changed() {
    let fx = Fx::new();
    let kept = fx.write("kept.py", "1");
    let changed = fx.write("changed.py", "1");
    let gone = fx.write("gone.py", "1");
    for p in [&kept, &changed, &gone] {
        fx.index(p);
    }
    let new = fx.write("pkg/new.py", "1");
    std::fs::write(&changed, "longer content").unwrap();
    std::fs::remove_file(&gone).unwrap();

    let d = fx.drift();
    assert_eq!(d.new, vec![new]);
    assert_eq!(d.deleted, vec![gone]);
    assert_eq!(d.changed, vec![changed]);
}

#[test]
fn ignores_ignored_paths_and_other_roots() {
    let fx = Fx::new();
    fx.write("node_modules/x/index.js", "x");
    // An entity outside the root must not count as deleted.
    fx.graph
        .lock()
        .unwrap()
        .upsert(
            &Entity::from_path_with_options(
                &NamedTempFile::new().unwrap().path().to_string_lossy(),
                false,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(fx.drift().is_fresh(), "{:?}", fx.drift());
}

#[test]
fn reconcile_applies_drift() {
    let fx = Fx::new();
    let changed = fx.write("changed.py", "1");
    let gone = fx.write("gone.py", "1");
    fx.index(&changed);
    fx.index(&gone);
    let new = fx.write("new.py", "different");
    std::fs::write(&changed, "longer content").unwrap();
    std::fs::remove_file(&gone).unwrap();

    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!(
        (stats.added, stats.updated, stats.removed, stats.renamed),
        (1, 1, 1, 0)
    );
    assert!(fx.indexed(&new));
    assert!(!fx.indexed(&gone));
    let hash_now = Entity::from_path_with_options(&changed, false)
        .unwrap()
        .content_hash;
    let stored = fx
        .graph
        .lock()
        .unwrap()
        .get_by_path(&changed)
        .unwrap()
        .unwrap()
        .content_hash;
    assert_eq!(stored, hash_now);
    assert!(fx.drift().is_fresh());
}

#[test]
fn missing_root_is_an_error_not_mass_delete() {
    let fx = Fx::new();
    let a = fx.write("a.py", "a");
    fx.index(&a);
    let missing = fx.root().join("does-not-exist");
    let r = freshness::reconcile(&fx.graph, Path::new(&missing), &fx.ignore, false);
    assert!(r.is_err());
    assert!(fx.indexed(&a));
}

fn id_of(fx: &Fx, path: &str) -> Option<String> {
    fx.graph
        .lock()
        .unwrap()
        .get_by_path(path)
        .unwrap()
        .map(|e| e.id)
}

#[test]
fn sibling_directory_with_common_prefix_is_not_drift() {
    // /x/proj must not see /x/proj2/* as its own deleted files.
    let fx = Fx::new();
    let sibling = TempDir::new().unwrap();
    let sib_root = fx.root().parent().unwrap().join(format!(
        "{}2",
        fx.root().file_name().unwrap().to_string_lossy()
    ));
    std::fs::create_dir_all(&sib_root).unwrap();
    let other = sib_root.join("other.py");
    std::fs::write(&other, "o").unwrap();
    fx.index(&other.to_string_lossy());
    std::fs::remove_file(&other).unwrap();
    assert!(fx.drift().is_fresh(), "{:?}", fx.drift());
    std::fs::remove_dir_all(&sib_root).unwrap();
    drop(sibling);
}

#[test]
fn mtime_only_change_is_detected() {
    let fx = Fx::new();
    let a = fx.write("a.py", "same");
    fx.index(&a);
    let f = std::fs::File::options().write(true).open(&a).unwrap();
    f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000))
        .unwrap();
    assert_eq!(fx.drift().changed, vec![a]);
}

#[test]
fn empty_root_holds_deletions() {
    // An unmounted volume looks like an empty directory.
    let fx = Fx::new();
    let paths: Vec<String> = (0..3).map(|i| fx.write(&format!("f{i}.py"), "x")).collect();
    for p in &paths {
        fx.index(p);
    }
    for p in &paths {
        std::fs::remove_file(p).unwrap();
    }
    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!((stats.removed, stats.deletes_skipped), (0, 3));
    assert!(paths.iter().all(|p| fx.indexed(p)));
}

#[test]
fn mass_deletion_is_held_but_small_deletions_go_through() {
    let fx = Fx::new();
    let keep = fx.write("keep.py", "k");
    fx.index(&keep);
    let many: Vec<String> = (0..60)
        .map(|i| fx.write(&format!("m{i}.py"), &format!("{i}")))
        .collect();
    for p in &many {
        fx.index(p);
    }
    for p in &many {
        std::fs::remove_file(p).unwrap();
    }
    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!(stats.deletes_skipped, 60, "60 of 61 gone: held");

    let fx = Fx::new();
    let few: Vec<String> = (0..60)
        .map(|i| fx.write(&format!("k{i}.py"), &format!("{i}")))
        .collect();
    for p in &few {
        fx.index(p);
    }
    for p in &few[..5] {
        std::fs::remove_file(p).unwrap();
    }
    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!((stats.removed, stats.deletes_skipped), (5, 0));
}

#[test]
fn missed_rename_keeps_identity() {
    let fx = Fx::new();
    let old = fx.write("old.py", "unique content");
    let other = fx.write("other.py", "o");
    fx.index(&old);
    fx.index(&other);
    let id = id_of(&fx, &old).unwrap();
    let new = fx.root().join("moved/new.py");
    std::fs::create_dir_all(new.parent().unwrap()).unwrap();
    std::fs::rename(&old, &new).unwrap();
    let new = new.to_string_lossy().to_string();

    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!((stats.renamed, stats.added, stats.removed), (1, 0, 0));
    assert!(!fx.indexed(&old));
    assert_eq!(id_of(&fx, &new), Some(id));
}

#[test]
fn duplicate_content_is_not_paired() {
    let fx = Fx::new();
    let old = fx.write("old.py", "dup");
    fx.index(&old);
    std::fs::remove_file(&old).unwrap();
    let a = fx.write("a.py", "dup");
    let b = fx.write("b.py", "dup");
    let stats = freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert_eq!((stats.renamed, stats.added, stats.removed), (0, 2, 1));
    assert!(fx.indexed(&a) && fx.indexed(&b) && !fx.indexed(&old));
}

#[test]
fn case_only_rename_does_not_leave_duplicate() {
    let fx = Fx::new();
    let old = fx.write("Foo.py", "case content");
    fx.index(&old);
    let id = id_of(&fx, &old).unwrap();
    let new = fx.root().join("foo.py");
    std::fs::rename(&old, &new).unwrap();
    let new = new.to_string_lossy().to_string();

    freshness::reconcile(&fx.graph, &fx.root(), &fx.ignore, false).unwrap();
    assert!(!fx.indexed(&old), "old-case entity must go");
    assert_eq!(id_of(&fx, &new), Some(id));
    assert!(fx.drift().is_fresh());
}

#[test]
fn walk_limit_is_an_error() {
    let fx = Fx::new();
    for i in 0..5 {
        fx.write(&format!("f{i}.py"), "x");
    }
    let g = fx.graph.lock().unwrap();
    assert!(freshness::quick_drift(&g, &fx.root(), &fx.ignore, false, Some(3)).is_err());
    assert!(freshness::quick_drift(&g, &fx.root(), &fx.ignore, false, Some(5)).is_ok());
}
