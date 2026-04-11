use std::path::Path;
use std::time::UNIX_EPOCH;

use anyhow::Result;
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{git::git_file_metadata, lifecycle::compute_state_default};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub path: String,
    pub name: String,
    pub extension: Option<String>,
    pub size_bytes: u64,
    pub created_at: i64,
    pub modified_at: i64,
    pub accessed_at: i64,
    pub lifecycle: LifecycleState,
    pub content_hash: Option<String>,
    pub summary: Option<String>,
    pub git_author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LifecycleState {
    Born,
    Active,
    Dormant,
    Archived,
    Dead,
}

impl Entity {
    pub fn from_path(path: &str) -> Result<Self> {
        Self::from_path_with_options(path, false)
    }

    pub fn from_path_with_options(path: &str, use_git_timestamps: bool) -> Result<Self> {
        let canonical = std::fs::canonicalize(path)?;
        let p = canonical.as_path();
        let meta = std::fs::metadata(p)?;

        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)?
            .as_secs() as i64;

        let mut created_at = meta
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|| {
                warn!("created_at unavailable for {path}, using now");
                now
            });

        let mut modified_at = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|| {
                warn!("modified_at unavailable for {path}, using now");
                now
            });

        let accessed_at = meta
            .accessed()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|| {
                warn!("accessed_at unavailable for {path}, using now");
                now
            });

        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        let extension = p.extension().map(|e| e.to_string_lossy().to_string());

        let mut git_author = None;
        if use_git_timestamps {
            if let Some(metadata) = git_file_metadata(p) {
                created_at = metadata.created_at;
                modified_at = metadata.modified_at;
                git_author = metadata.top_author;
            }
        }

        let content_hash = match hash_file(p) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!("hash failed for {path}: {e}");
                None
            }
        };

        let lifecycle = compute_state_default(accessed_at, now);

        debug!(
            "entity: {} | lifecycle={} | size={} | hash={}",
            p.display(),
            lifecycle.as_str(),
            meta.len(),
            content_hash.as_deref().map(|h| &h[..8]).unwrap_or("none"),
        );

        Ok(Entity {
            id: uuid::Uuid::new_v4().to_string(),
            path: p.to_string_lossy().to_string(),
            name,
            extension,
            size_bytes: meta.len(),
            created_at,
            modified_at,
            accessed_at,
            lifecycle,
            content_hash,
            summary: None,
            git_author,
        })
    }
}

const MAX_HASH_SIZE: u64 = 100 * 1024 * 1024; // 100 MB

fn hash_file(path: &Path) -> Result<String> {
    use std::io::{BufReader, Read};

    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_HASH_SIZE {
        // Return a size-based pseudo-hash to avoid loading huge files into RAM
        return Ok(format!("size:{}", meta.len()));
    }

    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl LifecycleState {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Born => "born",
            Self::Active => "active",
            Self::Dormant => "dormant",
            Self::Archived => "archived",
            Self::Dead => "dead",
        }
    }
}
