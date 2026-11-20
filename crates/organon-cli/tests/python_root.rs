//! The CLI must never run Python from the current directory: a workspace with
//! its own `pyproject.toml` + `ai/indexer.py` must not supply the code that
//! runs, and uv must not create a venv or lockfile in it.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn fake_uv(dir: &Path) -> std::path::PathBuf {
    let uv = dir.join("uv");
    let log = dir.join("uv.log");
    fs::write(
        &uv,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" >> '{}'\necho 'organon-indexer test ok'\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&uv, fs::Permissions::from_mode(0o755)).unwrap();
    uv
}

#[test]
fn cli_runs_python_in_organon_root_not_in_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&tools).unwrap();
    fs::create_dir_all(workspace.join("ai")).unwrap();
    fs::write(
        workspace.join("pyproject.toml"),
        "[project]\nname = \"victim\"\n",
    )
    .unwrap();
    fs::write(workspace.join("ai/__init__.py"), "").unwrap();
    fs::write(
        workspace.join("ai/indexer.py"),
        "open('PLANTED_RAN', 'w')\n",
    )
    .unwrap();
    let uv = fake_uv(&tools);

    let out = Command::new(env!("CARGO_BIN_EXE_organon"))
        .args(["--db", tmp.path().join("e.db").to_str().unwrap(), "doctor"])
        .current_dir(&workspace)
        .env_remove("ORGANON_PYTHON_PROJECT")
        .env("ORGANON_HOME", tmp.path().join("home"))
        .env("ORGANON_UV", &uv)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let log = fs::read_to_string(tools.join("uv.log")).expect("uv was invoked");
    let repo = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let mut lines = log.lines();
    let cwd = fs::canonicalize(lines.next().unwrap()).unwrap();
    let args: Vec<&str> = lines.collect();
    assert_eq!(cwd, repo, "python must run inside the Organon root");
    assert!(args.contains(&"--no-dev"), "{args:?}");
    let project = args[args.iter().position(|a| *a == "--project").unwrap() + 1];
    assert_eq!(fs::canonicalize(project).unwrap(), repo);
    assert!(!log.contains(workspace.to_str().unwrap()), "{log}");
    for leftover in ["PLANTED_RAN", ".venv", "uv.lock"] {
        assert!(
            !workspace.join(leftover).exists(),
            "{leftover} created in workspace"
        );
    }
}

#[test]
fn installed_binary_without_root_refuses_instead_of_using_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let tools = tmp.path().join("tools");
    let workspace = tmp.path().join("workspace");
    fs::create_dir_all(&tools).unwrap();
    fs::create_dir_all(workspace.join("ai")).unwrap();
    fs::write(
        workspace.join("pyproject.toml"),
        "[project]\nname = \"victim\"\n",
    )
    .unwrap();
    fs::write(
        workspace.join("ai/indexer.py"),
        "open('PLANTED_RAN', 'w')\n",
    )
    .unwrap();
    let uv = fake_uv(&tools);
    // A copy outside the repository, like a release-tarball install.
    let organon = tools.join("organon");
    fs::copy(env!("CARGO_BIN_EXE_organon"), &organon).unwrap();

    let out = Command::new(&organon)
        .args(["--db", tmp.path().join("e.db").to_str().unwrap(), "doctor"])
        .current_dir(&workspace)
        .env_remove("ORGANON_PYTHON_PROJECT")
        .env("ORGANON_HOME", tmp.path().join("home"))
        .env("ORGANON_UV", &uv)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ORGANON_PYTHON_PROJECT"), "{stdout}");
    let log = fs::read_to_string(tools.join("uv.log")).unwrap_or_default();
    assert!(
        !log.contains(workspace.to_str().unwrap()),
        "uv ran in workspace:\n{log}"
    );
    for leftover in ["PLANTED_RAN", ".venv", "uv.lock"] {
        assert!(
            !workspace.join(leftover).exists(),
            "{leftover} created in workspace"
        );
    }
}
