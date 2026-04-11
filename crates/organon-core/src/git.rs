//! Git integration — extract file timestamps from git history.
//!
//! Returns (created_at, modified_at) as Unix timestamps.
//! Returns None if not in a git repo or file is untracked.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use log::debug;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileMetadata {
    pub created_at: i64,
    pub modified_at: i64,
    pub top_author: Option<String>,
}

/// Get (created_at, modified_at) from git for a file.
/// - created_at  = timestamp of the first commit that added this file
/// - modified_at = timestamp of the most recent commit touching this file
///
/// Returns None if git is not available, file is not tracked, or any error.
pub fn git_timestamps(path: &Path) -> Option<(i64, i64)> {
    git_file_metadata(path).map(|meta| (meta.created_at, meta.modified_at))
}

/// Get created/modified timestamps and top author from git history.
pub fn git_file_metadata(path: &Path) -> Option<GitFileMetadata> {
    // `git log --follow --format=%at -- <file>` outputs timestamps newest-first
    let out = Command::new("git")
        .args([
            "log",
            "--follow",
            "--format=%at%x1f%an",
            "--",
            &path.to_string_lossy(),
        ])
        .current_dir(path.parent().unwrap_or(Path::new(".")))
        .output()
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let metadata = parse_git_log(stdout.as_ref())?;
    debug!(
        "git metadata for {}: created={} modified={} author={:?}",
        path.display(),
        metadata.created_at,
        metadata.modified_at,
        metadata.top_author
    );
    Some(metadata)
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

fn parse_git_log(stdout: &str) -> Option<GitFileMetadata> {
    let mut timestamps = Vec::new();
    let mut authors: HashMap<String, usize> = HashMap::new();

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (timestamp, author) = line.split_once('\u{1f}')?;
        let timestamp = timestamp.trim().parse::<i64>().ok()?;
        timestamps.push(timestamp);
        let author = author.trim();
        if !author.is_empty() {
            *authors.entry(author.to_string()).or_insert(0) += 1;
        }
    }

    if timestamps.is_empty() {
        return None;
    }

    let top_author = authors
        .into_iter()
        .max_by(|(author_a, count_a), (author_b, count_b)| {
            count_a.cmp(count_b).then_with(|| author_b.cmp(author_a))
        })
        .map(|(author, _)| author);

    Some(GitFileMetadata {
        modified_at: timestamps[0],
        created_at: timestamps[timestamps.len() - 1],
        top_author,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_log_extracts_timestamps_and_top_author() {
        let metadata = parse_git_log("300\x1fAlice\n200\x1fBob\n100\x1fAlice\n").unwrap();
        assert_eq!(metadata.created_at, 100);
        assert_eq!(metadata.modified_at, 300);
        assert_eq!(metadata.top_author.as_deref(), Some("Alice"));
    }

    #[test]
    fn parse_git_log_returns_none_for_empty_output() {
        assert!(parse_git_log("").is_none());
    }
}
