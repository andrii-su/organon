use organon_core::{
    entity::{Entity, LifecycleState},
    graph::{FindFilter, Graph},
};
use tempfile::NamedTempFile;

fn temp_graph() -> (Graph, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let g = Graph::open(f.path().to_str().unwrap()).unwrap();
    (g, f)
}

fn test_entity(path: &str) -> Entity {
    Entity {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.to_string(),
        name: "test.rs".to_string(),
        extension: Some("rs".to_string()),
        size_bytes: 42,
        created_at: 1_000_000,
        modified_at: 1_000_000,
        accessed_at: 1_000_000,
        lifecycle: LifecycleState::Active,
        content_hash: Some("abc123".to_string()),
        summary: None,
        git_author: None,
    }
}

#[test]
fn upsert_and_get() {
    let (graph, _f) = temp_graph();
    let entity = test_entity("/tmp/test.rs");

    graph.upsert(&entity).unwrap();
    let got = graph.get_by_path("/tmp/test.rs").unwrap().unwrap();

    assert_eq!(got.path, "/tmp/test.rs");
    assert_eq!(got.lifecycle, LifecycleState::Active);
    assert_eq!(got.size_bytes, 42);
    assert_eq!(got.content_hash.as_deref(), Some("abc123"));
}

#[test]
fn get_missing_returns_none() {
    let (graph, _f) = temp_graph();
    let result = graph.get_by_path("/nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn upsert_is_idempotent() {
    let (graph, _f) = temp_graph();
    let entity = test_entity("/tmp/dup.rs");

    graph.upsert(&entity).unwrap();
    graph.upsert(&entity).unwrap();

    let all = graph.all().unwrap();
    assert_eq!(all.len(), 1);
}

#[test]
fn upsert_updates_existing() {
    let (graph, _f) = temp_graph();
    let mut entity = test_entity("/tmp/update.rs");
    graph.upsert(&entity).unwrap();

    entity.lifecycle = LifecycleState::Dormant;
    entity.size_bytes = 999;
    graph.upsert(&entity).unwrap();

    let got = graph.get_by_path("/tmp/update.rs").unwrap().unwrap();
    assert_eq!(got.lifecycle, LifecycleState::Dormant);
    assert_eq!(got.size_bytes, 999);
}

#[test]
fn delete_removes_entity() {
    let (graph, _f) = temp_graph();
    let entity = test_entity("/tmp/delete_me.rs");

    graph.upsert(&entity).unwrap();
    assert!(graph.get_by_path("/tmp/delete_me.rs").unwrap().is_some());

    graph.delete_by_path("/tmp/delete_me.rs").unwrap();
    assert!(graph.get_by_path("/tmp/delete_me.rs").unwrap().is_none());
}

#[test]
fn all_returns_all_entities() {
    let (graph, _f) = temp_graph();

    for i in 0..5 {
        graph
            .upsert(&test_entity(&format!("/tmp/file_{}.rs", i)))
            .unwrap();
    }

    let all = graph.all().unwrap();
    assert_eq!(all.len(), 5);
}

#[test]
fn upsert_relation_and_get() {
    let (graph, _f) = temp_graph();

    graph.upsert_relation("/a.rs", "/b.rs", "mod").unwrap();

    let rels = graph.get_relations("/a.rs").unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].0, "/a.rs");
    assert_eq!(rels[0].1, "/b.rs");
    assert_eq!(rels[0].2, "mod");
}

#[test]
fn get_relations_by_target() {
    let (graph, _f) = temp_graph();

    graph.upsert_relation("/x.py", "/y.py", "imports").unwrap();

    // query by target — should still return the edge
    let rels = graph.get_relations("/y.py").unwrap();
    assert_eq!(rels.len(), 1);
    assert_eq!(rels[0].0, "/x.py");
}

#[test]
fn upsert_relation_idempotent() {
    let (graph, _f) = temp_graph();

    graph.upsert_relation("/a.rs", "/b.rs", "mod").unwrap();
    graph.upsert_relation("/a.rs", "/b.rs", "mod").unwrap(); // duplicate

    let rels = graph.get_relations("/a.rs").unwrap();
    assert_eq!(rels.len(), 1, "duplicate relation should be ignored");
}

#[test]
fn upsert_multiple_relations() {
    let (graph, _f) = temp_graph();

    graph
        .upsert_relation("/main.rs", "/graph.rs", "mod")
        .unwrap();
    graph
        .upsert_relation("/main.rs", "/scanner.rs", "mod")
        .unwrap();
    graph
        .upsert_relation("/main.rs", "/entity.rs", "mod")
        .unwrap();

    let rels = graph.get_relations("/main.rs").unwrap();
    assert_eq!(rels.len(), 3);
}

#[test]
fn get_relations_empty() {
    let (graph, _f) = temp_graph();
    let rels = graph.get_relations("/isolated.rs").unwrap();
    assert!(rels.is_empty());
}

#[test]
fn lifecycle_roundtrip_all_states() {
    let (graph, _f) = temp_graph();
    let states = [
        LifecycleState::Born,
        LifecycleState::Active,
        LifecycleState::Dormant,
        LifecycleState::Archived,
        LifecycleState::Dead,
    ];

    for state in &states {
        let mut e = test_entity("/tmp/lifecycle.rs");
        e.lifecycle = state.clone();
        graph.upsert(&e).unwrap();

        let got = graph.get_by_path("/tmp/lifecycle.rs").unwrap().unwrap();
        assert_eq!(&got.lifecycle, state);
    }
}

#[test]
fn find_filters_by_extension_and_size() {
    let (graph, _f) = temp_graph();

    let mut small_rs = test_entity("/tmp/small.rs");
    small_rs.modified_at = 100;
    small_rs.size_bytes = 10;

    let mut large_rs = test_entity("/tmp/large.rs");
    large_rs.modified_at = 200;
    large_rs.size_bytes = 5 * 1024 * 1024;

    let mut py = test_entity("/tmp/tool.py");
    py.name = "tool.py".to_string();
    py.extension = Some("py".to_string());
    py.modified_at = 300;

    graph.upsert(&small_rs).unwrap();
    graph.upsert(&large_rs).unwrap();
    graph.upsert(&py).unwrap();

    let results = graph
        .find(&FindFilter {
            extension: Some("rs".to_string()),
            larger_than: Some(1024),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "/tmp/large.rs");
}

#[test]
fn find_filters_by_created_and_modified_after() {
    let (graph, _f) = temp_graph();

    let mut old = test_entity("/tmp/old.rs");
    old.created_at = 100;
    old.modified_at = 200;

    let mut fresh = test_entity("/tmp/fresh.rs");
    fresh.created_at = 500;
    fresh.modified_at = 600;

    graph.upsert(&old).unwrap();
    graph.upsert(&fresh).unwrap();

    let results = graph
        .find(&FindFilter {
            created_after: Some(300),
            modified_after: Some(400),
            limit: 10,
            ..Default::default()
        })
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].path, "/tmp/fresh.rs");
}

#[test]
fn delete_dead_entities_removes_related_edges() {
    let (graph, _f) = temp_graph();

    let mut dead = test_entity("/tmp/dead.rs");
    dead.lifecycle = LifecycleState::Dead;
    let live = test_entity("/tmp/live.rs");

    graph.upsert(&dead).unwrap();
    graph.upsert(&live).unwrap();
    graph
        .upsert_relation("/tmp/dead.rs", "/tmp/live.rs", "imports")
        .unwrap();

    assert_eq!(graph.get_relations("/tmp/live.rs").unwrap().len(), 1);

    let deleted = graph.delete_dead_entities().unwrap();
    assert_eq!(deleted, 1);
    assert!(graph.get_by_path("/tmp/dead.rs").unwrap().is_none());
    assert!(graph.get_relations("/tmp/live.rs").unwrap().is_empty());
}

#[test]
fn delete_stale_relations_removes_orphans() {
    let (graph, _f) = temp_graph();

    graph
        .upsert_relation("/tmp/missing-a.rs", "/tmp/missing-b.rs", "imports")
        .unwrap();

    let stale = graph.stale_relations().unwrap();
    assert_eq!(stale.len(), 1);

    let deleted = graph.delete_stale_relations().unwrap();
    assert_eq!(deleted, 1);
    assert!(graph.stale_relations().unwrap().is_empty());
}

#[test]
fn find_supports_offset_and_count() {
    let (graph, _f) = temp_graph();

    for i in 0..3 {
        let mut entity = test_entity(&format!("/tmp/file-{i}.rs"));
        entity.modified_at = 100 + i;
        graph.upsert(&entity).unwrap();
    }

    let filter = FindFilter {
        limit: 1,
        offset: 1,
        ..Default::default()
    };
    let page = graph.find(&filter).unwrap();
    let total = graph.count_find(&FindFilter::default()).unwrap();

    assert_eq!(page.len(), 1);
    assert_eq!(total, 3);
}
