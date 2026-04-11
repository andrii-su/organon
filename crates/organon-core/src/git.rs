//! Git integration — extract file timestamps from git history.
//!
//! Returns (created_at, modified_at) as Unix timestamps.
//! Returns None if not in a git repo or file is untracked.

use std::path::Path;
use std::process::Command;

use log::debug;

/// Get (created_at, modified_at) from git for a file.
/// - created_at  = timestamp of the first commit that added this file
/// - modified_at = timestamp of the most recent commit touching this file
///
/// Returns None if git is not available, file is not tracked, or any error.
pub fn git_timestamps(path: &Path) -> Option<(i64, i64)> {
    // `git log --follow --format=%at -- <file>` outputs timestamps newest-first
    let out = Command::new("git")
        .args(["log", "--follow", "--format=%at", "--", &path.to_string_lossy()])
        .current_dir(path.parent().unwrap_or(Path::new(".")))
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let timestamps: Vec<i64> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| l.trim().parse::<i64>().ok())
        .collect();

    if timestamps.is_empty() {
        debug!("git: no commits for {}", path.display());
        return None;
    }

    let modified_at = timestamps[0];                      // newest commit
    let created_at  = timestamps[timestamps.len() - 1];  // oldest commit

    debug!("git timestamps for {}: created={} modified={}", path.display(), created_at, modified_at);
    Some((created_at, modified_at))
}

/// Check if a path is inside a git repository.
pub fn is_git_repo(path: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
