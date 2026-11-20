//! Sensitive file detection: secrets and key material must never be stored
//! (snapshots, history, FTS) or returned to an agent.
//!
//! Mirrors `ai/common/sensitive.py`. Both are checked against the shared cases
//! in `tests/fixtures/sensitive_cases.txt`, so change the two together.
//! `ORGANON_SENSITIVE_EXTRA` globs are matched with `globset` here and
//! `fnmatch` in Python; stick to `*`, `?` and `[...]` there, since `{a,b}`
//! alternation only works on the Rust side.

use std::path::{Component, Path};
use std::sync::OnceLock;

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Filename globs, matched case-insensitively against the file name.
pub const SENSITIVE_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "*.env",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.jks",
    "*.keystore",
    "*.pkcs12",
    "*.cer",
    "*.asc",
    "*.gpg",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    "id_dsa*",
    ".netrc",
    ".envrc",
    ".pgpass",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    "credentials",
    "credentials.*",
    "*credentials*.json",
    "client_secret*.json",
    "service_account*.json",
    "service-account*.json",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secrets.toml",
    "secret.json",
    "secret.yaml",
    "secret.yml",
    "secret.toml",
    "*.token",
    "*.secret",
    "htpasswd",
    ".htpasswd",
    "*.tfstate",
    "*.tfstate.*",
    "*.tfvars",
];

/// Words that mark a config or text file (but not code) as sensitive:
/// `db_credentials.ini` is, `secret_manager.py` is not.
const SENSITIVE_WORDS: &[&str] = &["secret", "credential"];
const CONFIG_EXTENSIONS: &[&str] = &[
    "json",
    "yaml",
    "yml",
    "toml",
    "ini",
    "conf",
    "cfg",
    "txt",
    "properties",
    "xml",
];

/// Directory components (or consecutive component sequences) that mark a
/// sensitive area, matched case-insensitively.
pub const SENSITIVE_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".kube",
    ".docker",
    ".config/gcloud",
    "secrets",
];

/// Extra filename globs from this colon-separated environment variable.
pub const EXTRA_ENV: &str = "ORGANON_SENSITIVE_EXTRA";

/// True if `path` looks like it holds secrets or key material.
///
/// Checks the literal path and its symlink-resolved form, so a benign-named
/// link into `.ssh` is still caught.
pub fn is_sensitive(path: &Path) -> bool {
    let extra = std::env::var(EXTRA_ENV).ok();
    if matches_rules(path, extra.as_deref()) {
        return true;
    }
    std::fs::canonicalize(path)
        .is_ok_and(|resolved| resolved != path && matches_rules(&resolved, extra.as_deref()))
}

/// Like [`is_sensitive`] for many stored paths, without touching the
/// filesystem: stored paths are already canonical. `extra` is the value of
/// `ORGANON_SENSITIVE_EXTRA`, read once by the caller.
pub fn is_sensitive_stored_path(path: &Path, extra: Option<&str>) -> bool {
    matches_rules(path, extra)
}

fn matches_rules(path: &Path, extra: Option<&str>) -> bool {
    in_sensitive_dir(path) || name_matches(path, extra)
}

fn default_globs() -> &'static GlobSet {
    static GLOBS: OnceLock<GlobSet> = OnceLock::new();
    GLOBS.get_or_init(|| {
        let mut builder = GlobSetBuilder::new();
        let word_globs = SENSITIVE_WORDS.iter().flat_map(|word| {
            CONFIG_EXTENSIONS
                .iter()
                .map(move |ext| format!("*{word}*.{ext}"))
        });
        for pattern in SENSITIVE_PATTERNS
            .iter()
            .map(|p| p.to_string())
            .chain(word_globs)
        {
            builder.add(Glob::new(&pattern).expect("valid built-in sensitive glob"));
        }
        builder.build().expect("valid built-in sensitive globs")
    })
}

fn name_matches(path: &Path, extra: Option<&str>) -> bool {
    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.to_string_lossy().to_lowercase();
    if default_globs().is_match(&name) {
        return true;
    }
    extra.is_some_and(|patterns| {
        patterns
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .any(|pattern| {
                Glob::new(&pattern.to_lowercase())
                    .is_ok_and(|glob| glob.compile_matcher().is_match(&name))
            })
    })
}

fn in_sensitive_dir(path: &Path) -> bool {
    let dirs: Vec<String> = path
        .parent()
        .map(|parent| {
            parent
                .components()
                .filter_map(|c| match c {
                    Component::Normal(part) => Some(part.to_string_lossy().to_lowercase()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    SENSITIVE_DIRS.iter().any(|entry| {
        let needle: Vec<&str> = entry.split('/').collect();
        dirs.windows(needle.len())
            .any(|window| window.iter().zip(&needle).all(|(a, b)| a == b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARED_CASES: &str = include_str!("../../../tests/fixtures/sensitive_cases.txt");

    #[test]
    fn shared_cases_match_python() {
        let mut failures = Vec::new();
        for line in SHARED_CASES.lines() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let (verdict, path) = line.split_once('\t').expect("<verdict>\\t<path>");
            let expected = verdict == "sensitive";
            if is_sensitive(Path::new(path)) != expected {
                failures.push(format!("{verdict}\t{path}"));
            }
        }
        assert!(
            failures.is_empty(),
            "mismatched cases:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn extra_patterns_match_filename() {
        let extra = Some("vault.json: *.Secret2 ");
        assert!(name_matches(Path::new("/r/vault.json"), extra));
        assert!(name_matches(Path::new("/r/db.secret2"), extra));
        assert!(!name_matches(Path::new("/r/readme.md"), extra));
        assert!(!name_matches(Path::new("/r/readme.md"), Some("::")));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_into_sensitive_dir_is_sensitive() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join(".ssh");
        std::fs::create_dir(&ssh).unwrap();
        std::fs::write(ssh.join("config"), "Host x").unwrap();
        let link = dir.path().join("notes.txt");
        std::os::unix::fs::symlink(ssh.join("config"), &link).unwrap();

        assert!(is_sensitive(&link));
    }
}
