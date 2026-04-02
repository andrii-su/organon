use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LifecycleState {
    Born,
    Active,
    Dormant,
    Archived,
    Dead,
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
