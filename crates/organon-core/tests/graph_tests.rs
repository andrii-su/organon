use organon_core::{
    entity::{Entity, LifecycleState},
    graph::Graph,
};
use tempfile::NamedTempFile;

fn temp_graph() -> (Graph, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let g = Graph::open(f.path().to_str().unwrap()).unwrap();
    (g, f)
}

fn test_entity(path: &str) -> Entity {
    Entity {
        id:           uuid::Uuid::new_v4().to_string(),
        path:         path.to_string(),
        name:         "test.rs".to_string(),
        extension:    Some("rs".to_string()),
        size_bytes:   42,
        created_at:   1_000_000,
        modified_at:  1_000_000,
        accessed_at:  1_000_000,
        lifecycle:    LifecycleState::Active,
        content_hash: Some("abc123".to_string()),
        summary:      None,
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
        graph.upsert(&test_entity(&format!("/tmp/file_{}.rs", i))).unwrap();
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

    graph.upsert_relation("/main.rs", "/graph.rs", "mod").unwrap();
    graph.upsert_relation("/main.rs", "/scanner.rs", "mod").unwrap();
    graph.upsert_relation("/main.rs", "/entity.rs", "mod").unwrap();

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
