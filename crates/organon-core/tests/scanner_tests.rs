use std::fs;
use std::sync::{Arc, Mutex};

use organon_core::{graph::Graph, ignore::IgnoreSet, scanner};
use tempfile::{NamedTempFile, TempDir};

fn temp_graph() -> (Arc<Mutex<Graph>>, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let g = Graph::open(f.path().to_str().unwrap()).unwrap();
    (Arc::new(Mutex::new(g)), f)
}

// ── is_ignored ────────────────────────────────────────────────────────────────

#[test]
fn ignores_git_dir() {
    assert!(scanner::is_ignored(std::path::Path::new(
        "/repo/.git/config"
    )));
}

#[test]
fn ignores_node_modules() {
    assert!(scanner::is_ignored(std::path::Path::new(
        "/project/node_modules/lodash/index.js"
    )));
}

#[test]
fn ignores_target_dir() {
    assert!(scanner::is_ignored(std::path::Path::new(
        "/project/target/debug/binary"
    )));
}

#[test]
fn ignores_venv() {
    assert!(scanner::is_ignored(std::path::Path::new(
        "/project/.venv/lib/python3.12/site.py"
    )));
}

#[test]
fn does_not_ignore_src() {
    assert!(!scanner::is_ignored(std::path::Path::new(
        "/project/src/main.rs"
    )));
}

#[test]
fn does_not_ignore_ai_dir() {
    assert!(!scanner::is_ignored(std::path::Path::new(
        "/project/ai/embeddings/store.py"
    )));
}

// ── scan ──────────────────────────────────────────────────────────────────────

#[test]
fn scan_indexes_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.txt"), "hello").unwrap();
    fs::write(dir.path().join("b.rs"), "fn main() {}").unwrap();

    let (graph, _f) = temp_graph();
    let ignore_set = IgnoreSet::load(dir.path(), &[]);
    let stats = scanner::scan(
        dir.path().to_str().unwrap(),
        Arc::clone(&graph),
        &ignore_set,
        false,
    )
    .unwrap();

    assert_eq!(stats.indexed, 2);
    assert_eq!(stats.errors, 0);

    let all = graph.lock().unwrap().all().unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn scan_skips_ignored_dirs() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();

    let git_dir = dir.path().join(".git");
    fs::create_dir(&git_dir).unwrap();
    fs::write(git_dir.join("config"), "[core]").unwrap();

    let (graph, _f) = temp_graph();
    let ignore_set = IgnoreSet::load(dir.path(), &[]);
    let stats = scanner::scan(
        dir.path().to_str().unwrap(),
        Arc::clone(&graph),
        &ignore_set,
        false,
    )
    .unwrap();

    // only src.rs, not .git/config
    assert_eq!(stats.indexed, 1);
}

#[test]
fn scan_second_run_upserts_not_duplicates() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("x.txt"), "content").unwrap();

    let (graph, _f) = temp_graph();
    let ignore_set = IgnoreSet::load(dir.path(), &[]);
    scanner::scan(
        dir.path().to_str().unwrap(),
        Arc::clone(&graph),
        &ignore_set,
        false,
    )
    .unwrap();
    scanner::scan(
        dir.path().to_str().unwrap(),
        Arc::clone(&graph),
        &ignore_set,
        false,
    )
    .unwrap();

    let all = graph.lock().unwrap().all().unwrap();
    assert_eq!(all.len(), 1);
}
