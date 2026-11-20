//! MCP search and similar-file search against a fake Python bridge: the graph
//! DB is passed, deleted files are dropped, stale hits are flagged and the
//! refresh backlog is reported. One test: it sets process env.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use organon_core::{config::OrgConfig, entity::Entity, graph::Graph};
use organon_mcp::{McpService, SearchMode};

#[test]
fn bridge_search_paths_are_guarded() {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let db = root.join("entities.db");
    let graph = Graph::open(db.to_str().unwrap()).unwrap();
    let live = root.join("live.py");
    let gone = root.join("gone.py");
    for p in [&live, &gone] {
        fs::write(p, "x = 1\n").unwrap();
        graph
            .upsert(&Entity::from_path(p.to_str().unwrap()).unwrap())
            .unwrap();
    }
    fs::remove_file(&gone).unwrap();
    let (live_s, gone_s) = (live.display().to_string(), gone.display().to_string());

    let args_log = root.join("bridge-args.json");
    let uv = root.join("uv");
    fs::write(
        &uv,
        format!(
            r#"#!/bin/sh
printf '%s' "$ORGANON_BRIDGE_ARGS" > '{log}'
case "$ORGANON_BRIDGE_ARGS" in
  *search_by_path*) echo '{{"items":[{{"path":"{live}","score":0.9}},{{"path":"{gone}","score":0.8}}],"refresh":{{"refreshed":0,"removed":0,"pending":2}}}}' ;;
  *oldshape*) echo '[{{"path":"{live}","score":0.9}}]' ;;
  *) echo '{{"items":[{{"path":"{live}","score":0.9,"stale":true}},{{"path":"{gone}","score":0.8}}],"refresh":{{"refreshed":0,"removed":0,"pending":4}}}}' ;;
esac
"#,
            log = args_log.display(),
            live = live_s,
            gone = gone_s,
        ),
    )
    .unwrap();
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
    let project = root.join("organon");
    fs::create_dir_all(project.join("ai")).unwrap();
    fs::write(project.join("pyproject.toml"), "").unwrap();
    fs::write(project.join("ai/indexer.py"), "").unwrap();
    std::env::set_var("ORGANON_UV", &uv);
    std::env::set_var("ORGANON_PYTHON_PROJECT", &project);

    let mut config = OrgConfig::default();
    config.indexer.db_path = db.display().to_string();
    config.indexer.vectors_path = root.join("vec").display().to_string();
    let svc = McpService::new(PathBuf::from(&db), config);

    // New bridge shape: deleted file dropped, stale hit flagged, backlog reported.
    let (hits, refresh) = svc
        .search_files_with_refresh("q", 5, None, SearchMode::Vector, &Default::default(), false)
        .unwrap();
    let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, [live_s.as_str()]);
    assert!(
        hits[0].changed_since_indexed,
        "stale vector hit must be flagged"
    );
    assert_eq!(refresh.unwrap().pending, 4);
    let sent: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&args_log).unwrap()).unwrap();
    assert_eq!(sent["graph_db"], db.display().to_string());

    // Older Python layer (plain list, no refresh): a file changed since
    // indexing is flagged even though it came from the vector side.
    fs::write(&live, "x = 1  # edited, longer now\n").unwrap();
    let (hits, refresh) = svc
        .search_files_with_refresh(
            "oldshape",
            5,
            None,
            SearchMode::Vector,
            &Default::default(),
            false,
        )
        .unwrap();
    assert!(refresh.is_none());
    assert!(hits[0].changed_since_indexed);

    // Similar-file search: deleted result dropped, backlog reported.
    let (hits, refresh) = svc.search_similar_with_refresh(&live_s, 5, None).unwrap();
    let paths: Vec<_> = hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, [live_s.as_str()]);
    assert_eq!(refresh.unwrap().pending, 2);
}
