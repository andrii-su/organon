#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::TempDir;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_organon")
}

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .args(args)
        .env("ORGANON_HOME", home)
        .output()
        .expect("organon command should start")
}

fn wait_for<F>(description: &str, mut condition: F)
where
    F: FnMut() -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {description}");
}

fn daemon_id(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("  id:   "))
        .expect("daemon start should print an id")
        .to_string()
}

fn meta_path(home: &Path, id: &str) -> PathBuf {
    home.join("daemon").join(format!("watch-{id}.json"))
}

fn read_pid(meta_path: &Path) -> u32 {
    serde_json::from_str::<serde_json::Value>(&fs::read_to_string(meta_path).unwrap()).unwrap()
        ["pid"]
        .as_u64()
        .unwrap() as u32
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

fn stop_daemon(home: &Path, id: &str) {
    let output = run(home, &["daemon", "stop", id]);
    assert!(
        output.status.success(),
        "daemon stop failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn daemon_start_stop_cleans_metadata_and_process() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
    let db = temp.path().join("entities.db");

    let started = run(
        &home,
        &[
            "--db",
            db.to_str().unwrap(),
            "watch",
            workspace.to_str().unwrap(),
            "--daemon",
            "--no-index",
        ],
    );
    assert!(started.status.success(), "{started:?}");
    let id = daemon_id(&started);
    let metadata = meta_path(&home, &id);
    wait_for("daemon metadata", || metadata.exists());
    let pid = read_pid(&metadata);
    assert!(process_alive(pid), "daemon should be running");

    let status = run(&home, &["daemon", "status", &id]);
    let status_stdout = String::from_utf8_lossy(&status.stdout);
    assert!(status.status.success());
    assert!(
        status_stdout.contains("state:      running"),
        "{status_stdout}"
    );

    stop_daemon(&home, &id);
    wait_for("daemon process exit", || !process_alive(pid));
    assert!(!metadata.exists(), "daemon metadata should be removed");
    assert!(!home.join("daemon").join(format!("watch-{id}.pid")).exists());
}

#[test]
fn parallel_daemons_have_independent_metadata() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let mut ids = Vec::new();

    for name in ["one", "two"] {
        let workspace = temp.path().join(name);
        fs::create_dir_all(&workspace).unwrap();
        fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
        let db = temp.path().join(format!("{name}.db"));
        let started = run(
            &home,
            &[
                "--db",
                db.to_str().unwrap(),
                "watch",
                workspace.to_str().unwrap(),
                "--daemon",
                "--no-index",
            ],
        );
        assert!(started.status.success(), "{started:?}");
        let id = daemon_id(&started);
        wait_for("parallel daemon metadata", || {
            meta_path(&home, &id).exists()
        });
        ids.push(id);
    }

    assert_ne!(ids[0], ids[1]);
    let listed = run(&home, &["daemon", "list"]);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success());
    assert!(stdout.contains(&ids[0]));
    assert!(stdout.contains(&ids[1]));

    for id in &ids {
        stop_daemon(&home, id);
    }
    for id in ids {
        wait_for("parallel daemon cleanup", || {
            !meta_path(&home, &id).exists()
        });
    }
}

#[test]
fn stale_metadata_is_reported_and_removed() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let daemon_dir = home.join("daemon");
    fs::create_dir_all(&daemon_dir).unwrap();

    let mut exited = Command::new("sh").arg("-c").arg("exit 0").spawn().unwrap();
    let stale_pid = exited.id();
    exited.wait().unwrap();

    let id = "stale-daemon";
    let pid_path = daemon_dir.join(format!("watch-{id}.pid"));
    let log_path = daemon_dir.join(format!("watch-{id}.log"));
    let metadata = meta_path(&home, id);
    fs::write(&pid_path, format!("{stale_pid}\n")).unwrap();
    fs::write(&log_path, "stale daemon\n").unwrap();
    fs::write(
        &metadata,
        serde_json::to_vec_pretty(&json!({
            "id": id,
            "pid": stale_pid,
            "roots": [temp.path().join("workspace")],
            "db_path": temp.path().join("entities.db"),
            "log_path": log_path,
            "pid_path": pid_path,
            "started_at": 1,
        }))
        .unwrap(),
    )
    .unwrap();

    let listed = run(&home, &["daemon", "list"]);
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success());
    assert!(stdout.contains("stale"), "{stdout}");

    stop_daemon(&home, id);
    assert!(!metadata.exists());
    assert!(!pid_path.exists());
}

#[test]
fn stopping_daemon_terminates_child_indexer() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("main.rs"), "fn main() {}\n").unwrap();
    let db = temp.path().join("entities.db");
    let fake_uv = temp.path().join("fake-uv");
    let indexer_pid = temp.path().join("indexer.pid");
    fs::write(
        &fake_uv,
        "#!/bin/sh\nprintf '%s\\n' \"$$\" > \"$FAKE_INDEXER_PID_FILE\"\ntrap 'exit 0' TERM INT\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(&fake_uv, fs::Permissions::from_mode(0o755)).unwrap();

    let started = Command::new(binary())
        .args([
            "--db",
            db.to_str().unwrap(),
            "watch",
            workspace.to_str().unwrap(),
            "--daemon",
            "--index-interval",
            "60",
        ])
        .env("ORGANON_HOME", &home)
        .env("ORGANON_UV", &fake_uv)
        .env("FAKE_INDEXER_PID_FILE", &indexer_pid)
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let id = daemon_id(&started);
    let metadata = meta_path(&home, &id);
    wait_for("daemon metadata", || metadata.exists());
    let daemon_pid = read_pid(&metadata);
    wait_for("fake indexer startup", || indexer_pid.exists());
    let child_pid = fs::read_to_string(&indexer_pid)
        .unwrap()
        .trim()
        .parse::<u32>()
        .unwrap();
    assert!(process_alive(child_pid));

    stop_daemon(&home, &id);
    wait_for("daemon process exit", || !process_alive(daemon_pid));
    wait_for("child indexer exit", || !process_alive(child_pid));
}
