//! The MCP search passes the graph DB to the Python bridge and reports the
//! refresh backlog. Runs in its own test binary because it sets process env.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use organon_core::{config::OrgConfig, graph::Graph};
use organon_mcp::{McpService, SearchMode};

#[test]
fn vector_search_passes_graph_db_and_reports_pending() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("entities.db");
    Graph::open(db.to_str().unwrap()).unwrap();
    let args_log = tmp.path().join("bridge-args.json");
    let uv = tmp.path().join("uv");
    fs::write(
        &uv,
        format!(
            "#!/bin/sh\nprintf '%s' \"$ORGANON_BRIDGE_ARGS\" > '{}'\n\
             echo '{{\"items\":[],\"refresh\":{{\"refreshed\":0,\"removed\":0,\"pending\":4}}}}'\n",
            args_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
    // A directory that looks like the Organon Python project.
    let project = tmp.path().join("organon");
    fs::create_dir_all(project.join("ai")).unwrap();
    fs::write(project.join("pyproject.toml"), "").unwrap();
    fs::write(project.join("ai/indexer.py"), "").unwrap();
    std::env::set_var("ORGANON_UV", &uv);
    std::env::set_var("ORGANON_PYTHON_PROJECT", &project);

    let mut config = OrgConfig::default();
    config.indexer.db_path = db.display().to_string();
    config.indexer.vectors_path = tmp.path().join("vec").display().to_string();
    let svc = McpService::new(PathBuf::from(&db), config);

    let (hits, refresh) = svc
        .search_files_with_refresh(
            "anything",
            5,
            None,
            SearchMode::Vector,
            &Default::default(),
            false,
        )
        .unwrap();

    assert!(hits.is_empty());
    assert_eq!(refresh.unwrap().pending, 4);
    let sent: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&args_log).unwrap()).unwrap();
    assert_eq!(sent["graph_db"], db.display().to_string());
    assert_eq!(sent["op"], "search");
}
