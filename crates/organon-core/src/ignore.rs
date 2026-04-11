//! Layered ignore logic.
//!
//! Layer 1 — built-in segment list (same as before, matches any component).
//! Layer 2 — extra segments from config `[watch] ignore_segments`.
//! Layer 3 — `.organonignore` file in the watch root (gitignore syntax).

use std::path::Path;

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use log::debug;

/// Built-in path segments that are always ignored.
pub const BUILT_IN_SEGMENTS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".DS_Store",
    "dist",
    "build",
    ".next",
    ".nuxt",
];

pub struct IgnoreSet {
    extra_segments: Vec<String>,
    gitignore: Option<Gitignore>,
}

impl IgnoreSet {
    /// Build an IgnoreSet for a given watch root.
    /// - `extra_segments`: from config `[watch] ignore_segments`
    /// - Looks for `.organonignore` in `watch_root`
    pub fn load(watch_root: &Path, extra_segments: &[String]) -> Self {
        let gitignore = Self::load_organonignore(watch_root);
        Self {
            extra_segments: extra_segments.to_vec(),
            gitignore,
        }
    }

    fn load_organonignore(root: &Path) -> Option<Gitignore> {
        let file = root.join(".organonignore");
        if !file.exists() {
            return None;
        }
        let mut builder = GitignoreBuilder::new(root);
        if let Some(e) = builder.add(file.clone()) {
            debug!(".organonignore add error: {e:?}");
            return None;
        }
        match builder.build() {
            Ok(gi) => {
                debug!("loaded .organonignore from {}", file.display());
                Some(gi)
            }
            Err(e) => {
                debug!(".organonignore build error: {e:?}");
                None
            }
        }
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        // Layer 1: built-in segments
        if path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            BUILT_IN_SEGMENTS.contains(&s.as_ref())
        }) {
            return true;
        }

        // Layer 2: extra segments from config
        if !self.extra_segments.is_empty()
            && path.components().any(|c| {
                let s = c.as_os_str().to_string_lossy();
                self.extra_segments.iter().any(|seg| seg == s.as_ref())
            })
        {
            return true;
        }

        // Layer 3: .organonignore gitignore rules
        if let Some(gi) = &self.gitignore {
            let is_dir = path.is_dir();
            if gi.matched(path, is_dir).is_ignore() {
                return true;
            }
        }

        false
    }
}

/// Convenience function for callers that don't have an IgnoreSet (e.g. tests).
pub fn is_ignored_default(path: &Path) -> bool {
    path.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        BUILT_IN_SEGMENTS.contains(&s.as_ref())
    })
}
