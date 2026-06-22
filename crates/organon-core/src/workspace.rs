//! Workspace registry and per-workspace storage paths.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceEntry {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WorkspaceRegistry {
    pub default: Option<String>,
    pub workspaces: Vec<WorkspaceEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspacePaths {
    pub root: PathBuf,
    pub db_path: PathBuf,
    pub vectors_path: PathBuf,
}

impl WorkspaceRegistry {
    pub fn load() -> Result<Self> {
        Self::load_from(&registry_path())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&registry_path())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{text}\n"))?;
        Ok(())
    }

    pub fn add(
        &mut self,
        path: &Path,
        name: Option<String>,
        make_default: bool,
    ) -> Result<WorkspaceEntry> {
        let canonical = std::fs::canonicalize(path)?;
        if !canonical.is_dir() {
            bail!("workspace path is not a directory: {}", canonical.display());
        }

        if let Some(existing) = self.by_path(&canonical) {
            let existing = existing.clone();
            if make_default {
                self.default = Some(existing.id.clone());
            }
            return Ok(existing);
        }

        let name = name.unwrap_or_else(|| {
            canonical
                .file_name()
                .and_then(|part| part.to_str())
                .unwrap_or("workspace")
                .to_string()
        });
        let id = unique_id(&self.workspaces, &name, &canonical);
        let now = now_ts();
        let entry = WorkspaceEntry {
            id: id.clone(),
            name,
            path: canonical,
            created_at: now,
            updated_at: now,
        };
        self.workspaces.push(entry.clone());
        self.workspaces
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.path.cmp(&b.path)));
        if make_default || self.default.is_none() {
            self.default = Some(id);
        }
        Ok(entry)
    }

    pub fn remove(&mut self, selector: &str) -> Result<WorkspaceEntry> {
        let Some(index) = self.find_index(selector) else {
            bail!("workspace not found: {selector}");
        };
        let removed = self.workspaces.remove(index);
        if self.default.as_deref() == Some(removed.id.as_str()) {
            self.default = None;
        }
        Ok(removed)
    }

    pub fn set_default(&mut self, selector: &str) -> Result<WorkspaceEntry> {
        let Some(entry) = self.find(selector).cloned() else {
            bail!("workspace not found: {selector}");
        };
        self.default = Some(entry.id.clone());
        Ok(entry)
    }

    pub fn default_workspace(&self) -> Option<&WorkspaceEntry> {
        self.default.as_deref().and_then(|id| self.by_id(id))
    }

    pub fn find(&self, selector: &str) -> Option<&WorkspaceEntry> {
        self.by_id(selector)
            .or_else(|| self.workspaces.iter().find(|entry| entry.name == selector))
            .or_else(|| {
                let path = PathBuf::from(selector);
                let canonical = std::fs::canonicalize(&path).unwrap_or(path);
                self.by_path(&canonical)
            })
    }

    pub fn match_path(&self, path: &Path) -> Option<&WorkspaceEntry> {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.workspaces
            .iter()
            .filter(|entry| canonical.starts_with(&entry.path))
            .max_by_key(|entry| entry.path.components().count())
    }

    pub fn paths_for(&self, id: &str) -> WorkspacePaths {
        workspace_paths(id)
    }

    fn by_id(&self, id: &str) -> Option<&WorkspaceEntry> {
        self.workspaces.iter().find(|entry| entry.id == id)
    }

    fn by_path(&self, path: &Path) -> Option<&WorkspaceEntry> {
        self.workspaces.iter().find(|entry| entry.path == path)
    }

    fn find_index(&self, selector: &str) -> Option<usize> {
        self.workspaces
            .iter()
            .position(|entry| entry.id == selector || entry.name == selector)
            .or_else(|| {
                let path = PathBuf::from(selector);
                let canonical = std::fs::canonicalize(&path).unwrap_or(path);
                self.workspaces
                    .iter()
                    .position(|entry| entry.path == canonical)
            })
    }
}

pub fn registry_path() -> PathBuf {
    organon_home().join("workspaces").join("registry.json")
}

pub fn workspace_paths(id: &str) -> WorkspacePaths {
    let root = organon_home().join("workspaces").join(id);
    WorkspacePaths {
        db_path: root.join("entities.db"),
        vectors_path: root.join("vectors"),
        root,
    }
}

fn organon_home() -> PathBuf {
    std::env::var("ORGANON_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".organon")
        })
}

fn unique_id(existing: &[WorkspaceEntry], name: &str, path: &Path) -> String {
    let base = slugify(name);
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    let mut id = format!("{base}-{}", &hash[..12]);
    let mut suffix = 2;
    while existing.iter().any(|entry| entry.id == id) {
        id = format!("{base}-{}-{suffix}", &hash[..12]);
        suffix += 1;
    }
    id
}

fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "workspace".to_string()
    } else {
        slug
    }
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registry_adds_and_matches_workspace_paths() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("project");
        let nested = root.join("src");
        std::fs::create_dir_all(&nested).unwrap();

        let mut registry = WorkspaceRegistry::default();
        let entry = registry.add(&root, None, true).unwrap();

        assert!(entry.id.starts_with("project-"));
        assert_eq!(registry.default_workspace().unwrap().id, entry.id);
        assert_eq!(registry.match_path(&nested).unwrap().id, entry.id);

        let paths = registry.paths_for(&entry.id);
        assert!(paths.db_path.ends_with("entities.db"));
        assert!(paths.vectors_path.ends_with("vectors"));
    }

    #[test]
    fn registry_round_trips_json() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).unwrap();
        let registry_file = dir.path().join("registry.json");

        let mut registry = WorkspaceRegistry::default();
        let entry = registry
            .add(&root, Some("Project".to_string()), true)
            .unwrap();
        registry.save_to(&registry_file).unwrap();

        let loaded = WorkspaceRegistry::load_from(&registry_file).unwrap();
        assert_eq!(loaded.default.as_deref(), Some(entry.id.as_str()));
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].name, "Project");
    }
}
