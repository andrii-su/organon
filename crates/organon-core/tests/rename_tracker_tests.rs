/// Unit tests for the rename-detection tracker logic inside the watcher.
///
/// We test `RenameTracker` via its `push` / `try_match` / `flush_expired`
/// methods which are exposed via `pub(crate)` access through the `watcher`
/// module's internal type.  Because the tests live in the crate's `tests/`
/// directory they must access the type through the library's public surface.
///
/// To keep the tracker testable without making everything `pub`, we expose a
/// thin test helper in the watcher module (gated behind `#[cfg(test)]`).
/// That helper is exercised here.
///
/// Scenarios covered:
///  1. Same hash within window → matched as rename.
///  2. Same hash after window expires → NOT matched (different file lifecycle).
///  3. Different hash → NOT matched.
///  4. Ambiguous: two removes with same hash, one create → NOT auto-matched.
///  5. Same path pushed and matched → NOT matched (path guard).
///  6. flush_expired actually deletes from the graph.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use organon_core::{
    entity::{Entity, LifecycleState},
    graph::Graph,
    watcher::RenameTrackerHandle,
};
use tempfile::NamedTempFile;

fn temp_graph() -> (Arc<Mutex<Graph>>, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let g = Graph::open(f.path().to_str().unwrap()).unwrap();
    (Arc::new(Mutex::new(g)), f)
}

fn entity(path: &str, hash: &str) -> Entity {
    Entity {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.to_string(),
        name: std::path::Path::new(path)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string(),
        extension: std::path::Path::new(path)
            .extension()
            .map(|e| e.to_string_lossy().to_string()),
        size_bytes: 10,
        created_at: 1_000_000,
        modified_at: 1_000_000,
        accessed_at: 1_000_000,
        lifecycle: LifecycleState::Active,
        content_hash: Some(hash.to_string()),
        summary: None,
        git_author: None,
    }
}

#[test]
fn same_hash_within_window_matches_as_rename() {
    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(5));
    tracker.push("/tmp/old.rs".to_string(), "hash1".to_string());

    let matched = tracker.try_match("/tmp/new.rs", "hash1");
    assert_eq!(matched, Some("/tmp/old.rs".to_string()));
}

#[test]
fn same_hash_after_window_does_not_match() {
    // Use a zero-duration window so every entry is immediately "expired".
    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(0));
    tracker.push("/tmp/old.rs".to_string(), "hash_exp".to_string());

    // A tiny sleep to ensure the instant is past the 0s window.
    std::thread::sleep(Duration::from_millis(1));

    let matched = tracker.try_match("/tmp/new.rs", "hash_exp");
    assert_eq!(matched, None, "expired entry must not match");
}

#[test]
fn different_hash_does_not_match() {
    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(5));
    tracker.push("/tmp/old.rs".to_string(), "hash_a".to_string());

    let matched = tracker.try_match("/tmp/new.rs", "hash_b");
    assert_eq!(matched, None);
}

#[test]
fn ambiguous_duplicate_hash_does_not_match() {
    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(5));
    // Two different files deleted with identical content — ambiguous
    tracker.push("/tmp/dup1.rs".to_string(), "same_hash".to_string());
    tracker.push("/tmp/dup2.rs".to_string(), "same_hash".to_string());

    let matched = tracker.try_match("/tmp/new.rs", "same_hash");
    assert_eq!(
        matched, None,
        "ambiguous (2 candidates) must not auto-match"
    );
}

#[test]
fn same_path_push_and_match_is_guarded() {
    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(5));
    tracker.push("/tmp/same.rs".to_string(), "hashX".to_string());

    // Create arrives at same path (e.g., quick delete+recreate in place)
    let matched = tracker.try_match("/tmp/same.rs", "hashX");
    assert_eq!(matched, None, "same-path match must be guarded");
}

#[test]
fn flush_expired_deletes_from_graph() {
    let (graph, _f) = temp_graph();

    // Insert the entity so delete_by_path has something to remove
    graph
        .lock()
        .unwrap()
        .upsert(&entity("/tmp/expire.rs", "hash_flush"))
        .unwrap();
    assert!(graph
        .lock()
        .unwrap()
        .get_by_path("/tmp/expire.rs")
        .unwrap()
        .is_some());

    let mut tracker = RenameTrackerHandle::new(Duration::from_secs(0));
    tracker.push("/tmp/expire.rs".to_string(), "hash_flush".to_string());
    std::thread::sleep(Duration::from_millis(1));
    tracker.flush_expired(&graph);

    assert!(
        graph
            .lock()
            .unwrap()
            .get_by_path("/tmp/expire.rs")
            .unwrap()
            .is_none(),
        "expired pending delete must be committed to the graph"
    );
}
