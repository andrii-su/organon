//! `organon search` and `organon search --like` drop deleted files returned by
//! the Python bridge and print the refresh backlog.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use organon_core::{entity::Entity, graph::Graph};

#[test]
fn cli_search_drops_deleted_bridge_hits_and_reports_backlog() {
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
    drop(graph);
    fs::remove_file(&gone).unwrap();

    let uv = root.join("uv");
    fs::write(
        &uv,
        format!(
            "#!/bin/sh\necho '{{\"items\":[{{\"path\":\"{}\",\"score\":0.9}},{{\"path\":\"{}\",\"score\":0.8}}],\"refresh\":{{\"refreshed\":0,\"removed\":0,\"pending\":2}}}}'\n",
            live.display(),
            gone.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
    let project = root.join("organon");
    fs::create_dir_all(project.join("ai")).unwrap();
    fs::write(project.join("pyproject.toml"), "").unwrap();
    fs::write(project.join("ai/indexer.py"), "").unwrap();
    let config = root.join("config.toml");
    fs::write(
        &config,
        format!(
            "[indexer]\ndb_path = \"{}\"\nvectors_path = \"{}\"\n",
            db.display(),
            root.join("vec").display()
        ),
    )
    .unwrap();

    for args in [
        vec!["search", "anything"],
        vec!["search", "--like", live.to_str().unwrap()],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_organon"))
            .arg("--db")
            .arg(&db)
            .args(&args)
            .current_dir(&root)
            .env("ORGANON_HOME", root.join("home"))
            .env("ORGANON_CONFIG", &config)
            .env("ORGANON_UV", &uv)
            .env("ORGANON_PYTHON_PROJECT", &project)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{args:?}: {stderr}");
        assert!(stdout.contains("live.py"), "{args:?}: {stdout}");
        assert!(!stdout.contains("gone.py"), "{args:?}: {stdout}");
        assert!(
            stderr.contains("2 changed file(s) not yet re-indexed"),
            "{args:?}: {stderr}"
        );
    }
}
