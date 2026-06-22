//! `organon watch` and `organon daemon` — filesystem watcher and background daemon management.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use anyhow::{bail, Context, Result};
use log::info;
use organon_core::{config::OrgConfig, graph::Graph, ignore::IgnoreSet, scanner};

use crate::format::format_ts;
use crate::python;
use crate::search::python_env;
use crate::{organon_home, resolve_watch_roots, DaemonCmd};

/// Persisted metadata for a background watch daemon process.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub(crate) struct WatchDaemonMeta {
    pub id: String,
    pub pid: u32,
    pub roots: Vec<PathBuf>,
    pub db_path: PathBuf,
    pub log_path: PathBuf,
    pub pid_path: PathBuf,
    pub started_at: i64,
}

/// Start the filesystem watcher.
///
/// Performs an initial scan, optionally reconciles missed renames, kicks off the Python
/// indexer as a subprocess, and then enters the notify event loop.
pub(crate) fn cmd_watch(
    path: Option<PathBuf>,
    db_path: &Path,
    config: &OrgConfig,
    index_interval: Option<u64>,
    no_index: bool,
    detect_renames: bool,
    daemon: bool,
) -> Result<()> {
    let roots = resolve_watch_roots(path.as_deref(), config)?;
    if daemon {
        return spawn_watch_daemon(
            path.as_deref(),
            db_path,
            &roots,
            index_interval,
            no_index,
            detect_renames,
        );
    }

    let watch_roots: Vec<_> = roots
        .iter()
        .map(|root| {
            organon_core::watcher::WatchRoot::new(
                root.clone(),
                Arc::new(IgnoreSet::load(root, &config.watch.ignore_segments)),
            )
        })
        .collect();
    let index_interval = index_interval.unwrap_or(config.watch.index_interval_secs);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let joined_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        "organon watch: [{}] | db: {}",
        joined_roots,
        db_path.display()
    );
    let graph = Arc::new(Mutex::new(Graph::open(db_path.to_str().unwrap())?));

    let mut stats = organon_core::scanner::ScanStats {
        total: 0,
        indexed: 0,
        skipped: 0,
        errors: 0,
    };
    for root in &watch_roots {
        let root_stats = scanner::scan(
            root.path.to_string_lossy().as_ref(),
            Arc::clone(&graph),
            &root.ignore_set,
            config.watch.use_git_timestamps,
        )?;
        stats.total += root_stats.total;
        stats.indexed += root_stats.indexed;
        stats.skipped += root_stats.skipped;
        stats.errors += root_stats.errors;
    }
    eprintln!(
        "indexed {} files ({} skipped, {} errors)",
        stats.indexed, stats.skipped, stats.errors
    );

    if detect_renames {
        for root in &watch_roots {
            let root_str = root.path.to_string_lossy();
            match scanner::reconcile_renames(&root_str, Arc::clone(&graph)) {
                Ok(n) if n > 0 => eprintln!("reconciled {n} rename(s) in {root_str}"),
                Ok(_) => {}
                Err(e) => eprintln!("warning: rename reconciliation failed: {e}"),
            }
        }
    }

    scanner::refresh_lifecycle(Arc::clone(&graph), &config.lifecycle)?;

    let _refresh_handle =
        scanner::schedule_lifecycle_refresh(Arc::clone(&graph), 6, config.lifecycle.clone());

    let _indexer_child = if !no_index {
        let envs = python_env(config);
        match python::spawn_indexer_with_env(db_path, Some(index_interval), &roots, &envs) {
            Ok(child) => {
                eprintln!(
                    "indexer started (every {}s, pid {})",
                    index_interval,
                    child.id()
                );
                Some(child)
            }
            Err(e) => {
                eprintln!(
                    "warning: could not start indexer: {e} (run `organon index --watch {index_interval}` manually)"
                );
                None
            }
        }
    } else {
        eprintln!(
            "indexer disabled (--no-index). Run `organon index --watch {index_interval}` manually."
        );
        None
    };

    organon_core::watcher::watch_many(&watch_roots, graph, config.watch.use_git_timestamps)
}

/// Dispatch daemon subcommands.
pub(crate) fn cmd_daemon(action: DaemonCmd) -> Result<()> {
    match action {
        DaemonCmd::List => cmd_daemon_list(),
        DaemonCmd::Status { id } => cmd_daemon_status(id.as_deref()),
        DaemonCmd::Stop { id } => cmd_daemon_stop(&id),
        DaemonCmd::Logs { id, lines } => cmd_daemon_logs(&id, lines),
    }
}

fn cmd_daemon_list() -> Result<()> {
    let daemons = load_daemons()?;
    if daemons.is_empty() {
        println!("(no watch daemons)");
        return Ok(());
    }

    println!("{:<18}  {:>7}  {:<8}  ROOTS", "ID", "PID", "STATE");
    println!("{}", "-".repeat(96));
    for meta in daemons {
        let state = if process_alive(meta.pid) {
            "running"
        } else {
            "stale"
        };
        println!(
            "{:<18}  {:>7}  {:<8}  {}",
            meta.id,
            meta.pid,
            state,
            format_roots(&meta.roots)
        );
    }
    Ok(())
}

fn cmd_daemon_status(id: Option<&str>) -> Result<()> {
    let daemons = load_daemons()?;
    if daemons.is_empty() {
        println!("(no watch daemons)");
        return Ok(());
    }

    let selected = if let Some(id) = id {
        vec![resolve_daemon(&daemons, id)?]
    } else {
        daemons.iter().collect()
    };

    for (idx, meta) in selected.iter().enumerate() {
        if idx > 0 {
            println!();
        }
        let state = if process_alive(meta.pid) {
            "running"
        } else {
            "stale"
        };
        println!("id:         {}", meta.id);
        println!("state:      {state}");
        println!("pid:        {}", meta.pid);
        println!("started:    {}", format_ts(meta.started_at));
        println!("db:         {}", meta.db_path.display());
        println!("log:        {}", meta.log_path.display());
        println!("pidfile:    {}", meta.pid_path.display());
        println!("roots:");
        for root in &meta.roots {
            println!("  {}", root.display());
        }
    }
    Ok(())
}

fn cmd_daemon_stop(id: &str) -> Result<()> {
    let daemons = load_daemons()?;
    let meta = resolve_daemon(&daemons, id)?;

    if process_alive(meta.pid) {
        terminate_process(meta.pid)?;
        wait_for_process_exit(meta.pid, Duration::from_secs(2));
        println!("sent SIGTERM to daemon {} (pid {})", meta.id, meta.pid);
    } else {
        println!("daemon {} is not running; removing stale files", meta.id);
    }

    remove_daemon_files(meta)?;
    Ok(())
}

fn cmd_daemon_logs(id: &str, lines: usize) -> Result<()> {
    let daemons = load_daemons()?;
    let meta = resolve_daemon(&daemons, id)?;
    if !meta.log_path.exists() {
        bail!("log not found: {}", meta.log_path.display());
    }
    let content = fs::read_to_string(&meta.log_path)?;
    let tail: Vec<&str> = content.lines().rev().take(lines).collect();
    for line in tail.into_iter().rev() {
        println!("{line}");
    }
    Ok(())
}

/// Spawn `organon watch` as a detached background process.
fn spawn_watch_daemon(
    path: Option<&Path>,
    db_path: &Path,
    roots: &[PathBuf],
    index_interval: Option<u64>,
    no_index: bool,
    detect_renames: bool,
) -> Result<()> {
    if roots.is_empty() {
        bail!("no watch roots resolved");
    }

    let daemon_dir = organon_home().join("daemon");
    std::fs::create_dir_all(&daemon_dir)?;
    let id = watch_roots_id(roots);
    let pid_path = daemon_dir.join(format!("watch-{id}.pid"));
    let log_path = daemon_dir.join(format!("watch-{id}.log"));

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = std::process::Command::new(std::env::current_exe()?);
    cmd.arg("--db")
        .arg(db_path)
        .arg("watch")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    if let Some(path) = path {
        cmd.arg(path);
    }
    if let Some(secs) = index_interval {
        cmd.arg("--index-interval").arg(secs.to_string());
    }
    if no_index {
        cmd.arg("--no-index");
    }
    if detect_renames {
        cmd.arg("--detect-renames");
    }

    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            // Create a new session so the daemon outlives the terminal.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();
    std::fs::write(&pid_path, format!("{}\n", child.id()))?;
    let meta_path = daemon_meta_path(&daemon_dir, &id);
    let meta = WatchDaemonMeta {
        id: id.clone(),
        pid,
        roots: roots.to_vec(),
        db_path: db_path.to_path_buf(),
        log_path: log_path.clone(),
        pid_path: pid_path.clone(),
        started_at: unix_now(),
    };
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;

    let joined_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    println!("watch daemon started");
    println!("  id:   {id}");
    println!("  pid:  {pid}");
    println!("  roots: {joined_roots}");
    println!("  log:  {}", log_path.display());
    println!("  pidfile: {}", pid_path.display());
    println!("  meta: {}", meta_path.display());
    Ok(())
}

fn load_daemons() -> Result<Vec<WatchDaemonMeta>> {
    let daemon_dir = organon_home().join("daemon");
    if !daemon_dir.exists() {
        return Ok(Vec::new());
    }

    let mut daemons = Vec::new();
    for entry in fs::read_dir(&daemon_dir)? {
        let entry = entry?;
        let path = entry.path();
        let is_meta = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("watch-") && name.ends_with(".json"));
        if !is_meta {
            continue;
        }
        let content = fs::read_to_string(&path)?;
        match serde_json::from_str::<WatchDaemonMeta>(&content) {
            Ok(meta) => daemons.push(meta),
            Err(err) => eprintln!(
                "warning: could not read daemon metadata {}: {err}",
                path.display()
            ),
        }
    }
    daemons.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(daemons)
}

fn resolve_daemon<'a>(daemons: &'a [WatchDaemonMeta], id: &str) -> Result<&'a WatchDaemonMeta> {
    let normalized = id.strip_prefix("watch-").unwrap_or(id);
    let matches: Vec<_> = daemons
        .iter()
        .filter(|meta| meta.id == normalized || meta.id.starts_with(normalized))
        .collect();
    match matches.as_slice() {
        [meta] => Ok(meta),
        [] => bail!("daemon not found: {id}"),
        _ => bail!("daemon id is ambiguous: {id}"),
    }
}

fn remove_daemon_files(meta: &WatchDaemonMeta) -> Result<()> {
    let daemon_dir = organon_home().join("daemon");
    for path in [
        meta.pid_path.clone(),
        daemon_meta_path(&daemon_dir, &meta.id),
    ] {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn daemon_meta_path(daemon_dir: &Path, id: &str) -> PathBuf {
    daemon_dir.join(format!("watch-{id}.json"))
}

fn format_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Stable short ID derived from the sorted set of watch root paths.
fn watch_roots_id(roots: &[PathBuf]) -> String {
    let mut normalized: Vec<String> = roots
        .iter()
        .map(|root| root.to_string_lossy().to_string())
        .collect();
    normalized.sort();
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<()> {
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("failed to terminate daemon")?
    }
}

#[cfg(not(unix))]
fn terminate_process(_pid: u32) -> Result<()> {
    bail!("daemon stop is not supported on this platform")
}

fn wait_for_process_exit(pid: u32, timeout: Duration) {
    let started = SystemTime::now();
    while process_alive(pid) {
        if started.elapsed().unwrap_or_default() >= timeout {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
