use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use log::{debug, info, warn};
use rusqlite::{params, params_from_iter, types::Value, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::entity::{Entity, LifecycleState, PSEUDO_HASH_PREFIX};

/// A single audit entry in the entity history log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub entity_id: String,
    pub path: String,
    /// One of: "created", "modified", "lifecycle", "renamed", "deleted"
    pub event: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_lifecycle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_lifecycle: Option<String>,
    /// Previous path — set on "renamed" events only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_change: Option<TextChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail_status: Option<String>,
    pub observation_gap: bool,
    /// True when this entry was retrieved from cold history storage.
    #[serde(default)]
    pub archived: bool,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryStorageStats {
    pub active_entries: u64,
    pub archive_entries: u64,
    pub active_bytes: u64,
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextChange {
    pub start_line: usize,
    pub before: String,
    pub after: String,
}

pub struct Graph {
    conn: Connection,
}

/// Outcome of a `rename_entity` call.
#[derive(Debug, PartialEq)]
pub enum RenameOutcome {
    /// Entity successfully renamed; no conflict at new path.
    Renamed,
    /// `old_path` was not found in the graph; nothing changed.
    OldNotFound,
    /// `new_path` already existed (OS rename overwrote it); the entity at
    /// `new_path` was removed and `old_path`'s entity now lives at `new_path`.
    ConflictResolved,
}

/// One reverse-dependency entry from `Graph::reverse_deps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImpactEntry {
    /// File that directly or transitively depends on the queried path.
    pub path: String,
    /// Relationship kind (e.g. "imports").
    pub kind: String,
    /// BFS depth from the queried file (1 = direct importer).
    pub depth: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteMatch {
    pub path: String,
    pub match_percent: u8,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexedRoute {
    pub route: String,
    pub handler_path: String,
    pub provenance: Vec<String>,
}

/// A group of files that share the same `content_hash`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub content_hash: String,
    pub paths: Vec<String>,
}

/// Filter for `Graph::find()`.
#[derive(Debug, Default)]
pub struct FindFilter {
    pub state: Option<String>,
    pub extension: Option<String>,
    pub created_after: Option<i64>,  // unix timestamp
    pub modified_after: Option<i64>, // unix timestamp
    pub larger_than: Option<u64>,    // bytes
    pub offset: usize,
    pub limit: usize,
}

impl Graph {
    pub fn open(db_path: &str) -> Result<Self> {
        debug!("opening graph db: {db_path}");
        let conn = Connection::open(db_path)?;
        // The watch daemon writes continuously while the MCP server and CLI open
        // their own connections to the same file. Without WAL + a busy timeout,
        // concurrent access fails immediately with SQLITE_BUSY ("database is
        // locked"). WAL lets readers and a writer coexist; the busy timeout makes
        // contending writers block-and-retry instead of erroring out.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let graph = Self { conn };
        graph.migrate()?;
        Ok(graph)
    }

    fn migrate(&self) -> Result<()> {
        info!("running schema migration");
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS entities (
                id           TEXT PRIMARY KEY,
                path         TEXT NOT NULL UNIQUE,
                name         TEXT NOT NULL,
                extension    TEXT,
                size_bytes   INTEGER NOT NULL,
                created_at   INTEGER NOT NULL,
                modified_at  INTEGER NOT NULL,
                accessed_at  INTEGER NOT NULL,
                lifecycle    TEXT NOT NULL DEFAULT 'born',
                content_hash TEXT,
                summary      TEXT,
                git_author   TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_entities_path      ON entities(path);
            CREATE INDEX IF NOT EXISTS idx_entities_lifecycle ON entities(lifecycle);
            CREATE INDEX IF NOT EXISTS idx_entities_modified  ON entities(modified_at);

            CREATE TABLE IF NOT EXISTS relationships (
                from_path  TEXT NOT NULL,
                to_path    TEXT NOT NULL,
                kind       TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (from_path, to_path, kind)
            );
            CREATE INDEX IF NOT EXISTS idx_rel_from ON relationships(from_path);
            CREATE INDEX IF NOT EXISTS idx_rel_to   ON relationships(to_path);
            CREATE INDEX IF NOT EXISTS idx_entities_hash ON entities(content_hash);

            CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
                path UNINDEXED,
                name,
                content
            );

            CREATE TABLE IF NOT EXISTS indexed_routes (
                route TEXT NOT NULL,
                handler_path TEXT NOT NULL,
                provenance TEXT NOT NULL,
                PRIMARY KEY (route, handler_path)
            );
            CREATE INDEX IF NOT EXISTS idx_indexed_routes_route ON indexed_routes(route);

            CREATE TABLE IF NOT EXISTS entity_history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                entity_id    TEXT    NOT NULL,
                path         TEXT    NOT NULL,
                event        TEXT    NOT NULL,
                old_lifecycle TEXT,
                new_lifecycle TEXT,
                old_path     TEXT,
                size_bytes   INTEGER,
                content_hash TEXT,
                recorded_at  INTEGER NOT NULL,
                text_change TEXT,
                detail_status TEXT,
                observation_gap INTEGER NOT NULL DEFAULT 0,
                before_text TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_history_path     ON entity_history(path);
            CREATE INDEX IF NOT EXISTS idx_history_entity   ON entity_history(entity_id);
            CREATE INDEX IF NOT EXISTS idx_history_recorded ON entity_history(recorded_at DESC);

            CREATE TABLE IF NOT EXISTS entity_history_archive (
                id INTEGER PRIMARY KEY,
                entity_id TEXT NOT NULL,
                path TEXT NOT NULL,
                event TEXT NOT NULL,
                old_lifecycle TEXT,
                new_lifecycle TEXT,
                old_path TEXT,
                size_bytes INTEGER,
                content_hash TEXT,
                recorded_at INTEGER NOT NULL,
                text_change TEXT,
                detail_status TEXT,
                observation_gap INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_history_archive_path ON entity_history_archive(path, recorded_at DESC);

            CREATE TABLE IF NOT EXISTS file_snapshots (
                entity_id TEXT PRIMARY KEY,
                content_hash TEXT NOT NULL,
                content TEXT NOT NULL
            );
        ",
        )?;
        ensure_column(&self.conn, "entities", "git_author", "TEXT")?;
        ensure_column(&self.conn, "entity_history", "text_change", "TEXT")?;
        ensure_column(&self.conn, "entity_history", "detail_status", "TEXT")?;
        ensure_column(
            &self.conn,
            "entity_history",
            "observation_gap",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&self.conn, "entity_history", "before_text", "TEXT")?;
        debug!("schema migration ok");
        Ok(())
    }

    // ── history ───────────────────────────────────────────────────────────────

    fn now_ts() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn record_history(&self, entry: &HistoryEntry) -> Result<()> {
        self.conn.execute(
            "INSERT INTO entity_history
                (entity_id, path, event, old_lifecycle, new_lifecycle,
                 old_path, size_bytes, content_hash, recorded_at,
                 text_change, detail_status, observation_gap)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                entry.entity_id,
                entry.path,
                entry.event,
                entry.old_lifecycle,
                entry.new_lifecycle,
                entry.old_path,
                entry.size_bytes,
                entry.content_hash,
                entry.recorded_at,
                entry
                    .text_change
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                entry.detail_status,
                entry.observation_gap,
            ],
        )?;
        debug!(
            "history: {} [{}] at {}",
            entry.event, entry.path, entry.recorded_at
        );
        Ok(())
    }

    /// Return history entries for a given path, newest first.
    pub fn get_history(&self, path: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.get_history_with_archive(path, limit, false)
    }

    /// Return recent history, optionally including the local cold archive.
    pub fn get_history_with_archive(
        &self,
        path: &str,
        limit: usize,
        include_archive: bool,
    ) -> Result<Vec<HistoryEntry>> {
        let limit = if limit == 0 { 50 } else { limit };
        let cols = "id, entity_id, path, event, old_lifecycle, new_lifecycle, old_path, size_bytes, content_hash, recorded_at, text_change, detail_status, observation_gap";
        let query = if include_archive {
            format!("SELECT {cols}, 0 AS archived FROM entity_history WHERE path = ?1 UNION ALL SELECT {cols}, 1 AS archived FROM entity_history_archive WHERE path = ?1 ORDER BY recorded_at DESC, id DESC LIMIT ?2")
        } else {
            format!("SELECT {cols}, 0 AS archived FROM entity_history WHERE path = ?1 ORDER BY recorded_at DESC, id DESC LIMIT ?2")
        };
        let mut stmt = self.conn.prepare(&query)?;
        let rows = stmt.query_map(params![path, limit as i64], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                entity_id: row.get(1)?,
                path: row.get(2)?,
                event: row.get(3)?,
                old_lifecycle: row.get(4)?,
                new_lifecycle: row.get(5)?,
                old_path: row.get(6)?,
                size_bytes: row.get(7)?,
                content_hash: row.get(8)?,
                recorded_at: row.get(9)?,
                text_change: row
                    .get::<_, Option<String>>(10)?
                    .and_then(|value| serde_json::from_str(&value).ok()),
                detail_status: row.get(11)?,
                observation_gap: row.get(12)?,
                archived: row.get(13)?,
            })
        })?;
        let entries: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(entries?)
    }

    /// Move history older than `cutoff` into cold storage in one transaction.
    pub fn archive_history_before(&self, cutoff: i64) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let moved = tx.execute(
            "INSERT INTO entity_history_archive
             (id, entity_id, path, event, old_lifecycle, new_lifecycle, old_path,
              size_bytes, content_hash, recorded_at, text_change, detail_status, observation_gap)
             SELECT id, entity_id, path, event, old_lifecycle, new_lifecycle, old_path,
                    size_bytes, content_hash, recorded_at, text_change, detail_status, observation_gap
             FROM entity_history WHERE recorded_at < ?1",
            [cutoff],
        )?;
        tx.execute(
            "DELETE FROM entity_history WHERE recorded_at < ?1",
            [cutoff],
        )?;
        tx.commit()?;
        Ok(moved)
    }

    /// Approximate history payload size, separately for active and cold rows.
    pub fn history_storage_stats(&self) -> Result<HistoryStorageStats> {
        fn size(conn: &Connection, table: &str) -> Result<(u64, u64)> {
            let query = format!("SELECT COUNT(*), COALESCE(SUM(128 + LENGTH(path) + LENGTH(entity_id) + LENGTH(event) + COALESCE(LENGTH(text_change), 0) + COALESCE(LENGTH(content_hash), 0)), 0) FROM {table}");
            let (count, bytes): (i64, i64) =
                conn.query_row(&query, [], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok((count as u64, bytes as u64))
        }
        let (active_entries, active_bytes) = size(&self.conn, "entity_history")?;
        let (archive_entries, archive_bytes) = size(&self.conn, "entity_history_archive")?;
        Ok(HistoryStorageStats {
            active_entries,
            archive_entries,
            active_bytes,
            archive_bytes,
        })
    }

    // ── entities CRUD ─────────────────────────────────────────────────────────

    pub fn upsert(&self, entity: &Entity) -> Result<()> {
        self.upsert_with_origin(entity, false)
    }

    pub fn upsert_scanned(&self, entity: &Entity) -> Result<()> {
        self.upsert_with_origin(entity, true)
    }

    fn upsert_with_origin(&self, entity: &Entity, scanned: bool) -> Result<()> {
        debug!("upsert: {} [{}]", entity.path, entity.lifecycle.as_str());

        // Read prior state to derive history events (one indexed read per upsert).
        let prior = self.get_by_path(&entity.path)?;
        let now = Self::now_ts();

        self.conn.execute(
            "INSERT INTO entities
                (id, path, name, extension, size_bytes, created_at, modified_at, accessed_at,
                 lifecycle, content_hash, summary, git_author)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(path) DO UPDATE SET
                name         = excluded.name,
                extension    = excluded.extension,
                size_bytes   = excluded.size_bytes,
                modified_at  = excluded.modified_at,
                accessed_at  = excluded.accessed_at,
                lifecycle    = excluded.lifecycle,
                content_hash = excluded.content_hash,
                git_author   = excluded.git_author",
            params![
                entity.id,
                entity.path,
                entity.name,
                entity.extension,
                entity.size_bytes as i64,
                entity.created_at,
                entity.modified_at,
                entity.accessed_at,
                entity.lifecycle.as_str(),
                entity.content_hash,
                entity.summary,
                entity.git_author,
            ],
        )?;

        // ── history ────────────────────────────────────────────────────────────
        match prior {
            None => {
                // New entity
                self.record_history(&HistoryEntry {
                    id: 0,
                    entity_id: entity.id.clone(),
                    path: entity.path.clone(),
                    event: "created".into(),
                    old_lifecycle: None,
                    new_lifecycle: Some(entity.lifecycle.as_str().into()),
                    old_path: None,
                    size_bytes: Some(entity.size_bytes as i64),
                    content_hash: entity.content_hash.clone(),
                    text_change: None,
                    detail_status: None,
                    observation_gap: false,
                    archived: false,
                    recorded_at: now,
                })?;
                self.store_snapshot(&entity.id, entity, capture_text(entity))?;
            }
            Some(prev) => {
                let lifecycle_changed = prev.lifecycle.as_str() != entity.lifecycle.as_str();
                let hash_changed =
                    entity.content_hash.is_some() && prev.content_hash != entity.content_hash;

                if lifecycle_changed {
                    self.record_history(&HistoryEntry {
                        id: 0,
                        entity_id: prev.id.clone(),
                        path: entity.path.clone(),
                        event: "lifecycle".into(),
                        old_lifecycle: Some(prev.lifecycle.as_str().into()),
                        new_lifecycle: Some(entity.lifecycle.as_str().into()),
                        old_path: None,
                        size_bytes: Some(entity.size_bytes as i64),
                        content_hash: entity.content_hash.clone(),
                        text_change: None,
                        detail_status: None,
                        observation_gap: false,
                        archived: false,
                        recorded_at: now,
                    })?;
                }
                if hash_changed {
                    self.record_content_change(&prev.id, entity, now, scanned)?;
                } else if scanned {
                    let has_snapshot: bool = self.conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM file_snapshots WHERE entity_id = ?1)",
                        [&prev.id],
                        |row| row.get(0),
                    )?;
                    if !has_snapshot {
                        self.store_snapshot(&prev.id, entity, capture_text(entity))?;
                    }
                }
            }
        }

        Ok(())
    }

    fn store_snapshot(
        &self,
        entity_id: &str,
        entity: &Entity,
        content: Option<String>,
    ) -> Result<()> {
        if let (Some(content), Some(hash)) = (content, entity.content_hash.as_deref()) {
            self.conn.execute(
                "INSERT INTO file_snapshots(entity_id, content_hash, content) VALUES (?1, ?2, ?3)
                 ON CONFLICT(entity_id) DO UPDATE SET content_hash = excluded.content_hash, content = excluded.content",
                params![entity_id, hash, content],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM file_snapshots WHERE entity_id = ?1",
                [entity_id],
            )?;
        }
        Ok(())
    }

    fn record_content_change(
        &self,
        entity_id: &str,
        entity: &Entity,
        now: i64,
        scanned: bool,
    ) -> Result<()> {
        let previous: Option<String> = self
            .conn
            .query_row(
                "SELECT content FROM file_snapshots WHERE entity_id = ?1",
                [entity_id],
                |row| row.get(0),
            )
            .optional()?;
        let current = capture_text(entity);
        let recent: Option<(i64, i64, Option<String>, bool)> = self
            .conn
            .query_row(
                "SELECT id, recorded_at, before_text, observation_gap FROM entity_history
             WHERE entity_id = ?1 AND event = 'modified' ORDER BY id DESC LIMIT 1",
                [entity_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        let grouped = recent
            .as_ref()
            .filter(|(_, last_at, _, _)| now >= *last_at && now - *last_at <= 5);
        let baseline = if let Some((_, _, before, _)) = grouped {
            before.clone()
        } else {
            previous.clone()
        };
        let text_change = baseline
            .as_deref()
            .zip(current.as_deref())
            .map(|(before, after)| text_change(before, after));
        let detail_status = if text_change.is_some() {
            "text"
        } else {
            "binary_or_unavailable"
        };
        let observation_gap = scanned || grouped.is_some_and(|(_, _, _, gap)| *gap);
        if let Some((id, _, _, _)) = grouped {
            self.conn.execute(
                "UPDATE entity_history SET size_bytes = ?2, content_hash = ?3, recorded_at = ?4,
                 text_change = ?5, detail_status = ?6, observation_gap = ?7 WHERE id = ?1",
                params![
                    id,
                    entity.size_bytes as i64,
                    entity.content_hash,
                    now,
                    text_change
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    detail_status,
                    observation_gap
                ],
            )?;
        } else {
            self.record_history(&HistoryEntry {
                id: 0,
                entity_id: entity_id.to_string(),
                path: entity.path.clone(),
                event: "modified".into(),
                old_lifecycle: None,
                new_lifecycle: Some(entity.lifecycle.as_str().into()),
                old_path: None,
                size_bytes: Some(entity.size_bytes as i64),
                content_hash: entity.content_hash.clone(),
                text_change,
                detail_status: Some(detail_status.to_string()),
                observation_gap,
                archived: false,
                recorded_at: now,
            })?;
            self.conn.execute(
                "UPDATE entity_history SET before_text = ?1 WHERE id = last_insert_rowid()",
                [previous],
            )?;
        }
        self.store_snapshot(entity_id, entity, current)?;
        Ok(())
    }

    pub fn get_by_path(&self, path: &str) -> Result<Option<Entity>> {
        debug!("get_by_path: {path}");
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author
             FROM entities WHERE path = ?1",
        )?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_entity(row)?))
        } else {
            debug!("get_by_path: not found — {path}");
            Ok(None)
        }
    }

    pub fn delete_by_path(&self, path: &str) -> Result<()> {
        // Capture entity before deletion so we can record history.
        let prior = self.get_by_path(path)?;

        self.conn.execute(
            "DELETE FROM relationships WHERE from_path = ?1 OR to_path = ?1",
            params![path],
        )?;
        let n = self
            .conn
            .execute("DELETE FROM entities WHERE path = ?1", params![path])?;
        if n > 0 {
            info!("deleted entity: {path}");
            if let Some(prev) = prior {
                self.conn.execute(
                    "DELETE FROM file_snapshots WHERE entity_id = ?1",
                    [&prev.id],
                )?;
                self.record_history(&HistoryEntry {
                    id: 0,
                    entity_id: prev.id,
                    path: path.to_string(),
                    event: "deleted".into(),
                    old_lifecycle: Some(prev.lifecycle.as_str().into()),
                    new_lifecycle: None,
                    old_path: None,
                    size_bytes: Some(prev.size_bytes as i64),
                    content_hash: prev.content_hash,
                    text_change: None,
                    detail_status: None,
                    observation_gap: false,
                    archived: false,
                    recorded_at: Self::now_ts(),
                })?;
            }
        } else {
            warn!("delete_by_path: no entity at {path}");
        }
        Ok(())
    }

    pub fn all(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author FROM entities",
        )?;
        let rows = stmt.query_map([], row_to_entity)?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        let entities = entities?;
        debug!("all: {} entities", entities.len());
        Ok(entities)
    }

    /// Filtered query — for `organon find`.
    pub fn find(&self, filter: &FindFilter) -> Result<Vec<Entity>> {
        let (where_parts, mut params) = build_find_where(filter);
        let limit = if filter.limit == 0 { 50 } else { filter.limit };
        let offset = filter.offset as i64;
        params.push(Value::Integer(limit as i64));
        params.push(Value::Integer(offset));

        let sql = format!(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author
             FROM entities WHERE {} ORDER BY modified_at DESC LIMIT ? OFFSET ?",
            where_parts.join(" AND ")
        );
        debug!("find: {sql}");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), row_to_entity)?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(entities?)
    }

    pub fn count_find(&self, filter: &FindFilter) -> Result<usize> {
        let (where_parts, params) = build_find_where(filter);
        let sql = format!(
            "SELECT COUNT(*) FROM entities WHERE {}",
            where_parts.join(" AND ")
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let total: i64 = stmt.query_row(params_from_iter(params.iter()), |row| row.get(0))?;
        Ok(total as usize)
    }

    /// Delete all dead entities. Returns count removed.
    pub fn delete_dead_entities(&self) -> Result<usize> {
        let paths: Vec<String> = self
            .dead_entities()?
            .into_iter()
            .map(|entity| entity.path)
            .collect();
        for path in &paths {
            self.delete_by_path(path)?;
        }
        info!("cleaned {} dead entities", paths.len());
        Ok(paths.len())
    }

    /// List dead entities (for --dry-run preview).
    pub fn dead_entities(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author
             FROM entities WHERE lifecycle = 'dead'",
        )?;
        let rows = stmt.query_map([], row_to_entity)?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(entities?)
    }

    /// Update accessed_at only if the new timestamp is later (avoids retrograde).
    pub fn touch_accessed(&self, path: &str, ts: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE entities SET accessed_at = ?2 WHERE path = ?1 AND accessed_at < ?2",
            params![path, ts],
        )?;
        Ok(())
    }

    /// Return all entities whose `content_hash` matches. Used for rename detection.
    pub fn get_by_hash(&self, content_hash: &str) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author
             FROM entities WHERE content_hash = ?1",
        )?;
        let rows = stmt.query_map(params![content_hash], row_to_entity)?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(entities?)
    }

    /// Rename an entity from `old_path` to `new_path` in-place, preserving id,
    /// summary, lifecycle, created_at, and all relationships.
    ///
    /// If `new_path` already exists in the graph the entity there is removed
    /// first (it was overwritten by the OS rename).
    ///
    /// Relationships are updated with INSERT-OR-IGNORE + DELETE to handle
    /// duplicate-path edge cases without violating the PRIMARY KEY constraint.
    pub fn rename_entity(&self, old_path: &str, new_path: &str) -> Result<RenameOutcome> {
        debug!("rename_entity: {old_path} → {new_path}");

        if self.get_by_path(old_path)?.is_none() {
            warn!("rename_entity: old path not found: {old_path}");
            return Ok(RenameOutcome::OldNotFound);
        }

        // ── handle conflict at new_path ───────────────────────────────────────
        let conflict_resolved = if let Some(conflict) = self.get_by_path(new_path)? {
            info!(
                "rename_entity: new_path exists (overwritten by OS rename), removing: {new_path}"
            );
            self.conn.execute(
                "DELETE FROM relationships WHERE from_path = ?1 OR to_path = ?1",
                params![new_path],
            )?;
            self.conn
                .execute("DELETE FROM entities WHERE path = ?1", params![new_path])?;
            self.conn.execute(
                "DELETE FROM file_snapshots WHERE entity_id = ?1",
                [&conflict.id],
            )?;
            self.conn.execute(
                "DELETE FROM entities_fts WHERE path = ?1",
                params![new_path],
            )?;
            true
        } else {
            false
        };

        // ── derive new name / extension from new_path ─────────────────────────
        let new_name = Path::new(new_path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let new_ext: Option<String> = Path::new(new_path)
            .extension()
            .map(|e| e.to_string_lossy().to_string());

        // ── update entity row ─────────────────────────────────────────────────
        self.conn.execute(
            "UPDATE entities SET path = ?2, name = ?3, extension = ?4 WHERE path = ?1",
            params![old_path, new_path, new_name, new_ext],
        )?;

        // ── cascade rename to relationships ───────────────────────────────────
        // Use INSERT OR IGNORE + DELETE so that if (new_path, Y, K) already
        // existed the old (old_path, Y, K) edge is simply dropped without
        // violating the PRIMARY KEY constraint.

        // from_path side
        self.conn.execute(
            "INSERT OR IGNORE INTO relationships (from_path, to_path, kind, created_at)
             SELECT ?2, to_path, kind, created_at FROM relationships WHERE from_path = ?1",
            params![old_path, new_path],
        )?;
        // to_path side
        self.conn.execute(
            "INSERT OR IGNORE INTO relationships (from_path, to_path, kind, created_at)
             SELECT from_path, ?2, kind, created_at FROM relationships WHERE to_path = ?1",
            params![old_path, new_path],
        )?;
        // remove old edges
        self.conn.execute(
            "DELETE FROM relationships WHERE from_path = ?1 OR to_path = ?1",
            params![old_path],
        )?;

        // ── cascade rename to history entries ─────────────────────────────────
        // Update all prior history rows so get_history(new_path) returns the
        // full timeline, not just post-rename entries.
        self.conn.execute(
            "UPDATE entity_history SET path = ?2 WHERE path = ?1",
            params![old_path, new_path],
        )?;
        self.conn.execute(
            "UPDATE entity_history_archive SET path = ?2 WHERE path = ?1",
            params![old_path, new_path],
        )?;

        // ── drop stale FTS entry (indexer re-adds under new path) ─────────────
        self.conn.execute(
            "DELETE FROM entities_fts WHERE path = ?1",
            params![old_path],
        )?;

        info!("renamed entity: {old_path} → {new_path}");

        // Record rename in history (look up the entity at its new path).
        if let Some(entity) = self.get_by_path(new_path)? {
            self.record_history(&HistoryEntry {
                id: 0,
                entity_id: entity.id,
                path: new_path.to_string(),
                event: "renamed".into(),
                old_lifecycle: None,
                new_lifecycle: Some(entity.lifecycle.as_str().into()),
                old_path: Some(old_path.to_string()),
                size_bytes: Some(entity.size_bytes as i64),
                content_hash: entity.content_hash,
                text_change: None,
                detail_status: None,
                observation_gap: false,
                archived: false,
                recorded_at: Self::now_ts(),
            })?;
        }

        if conflict_resolved {
            Ok(RenameOutcome::ConflictResolved)
        } else {
            Ok(RenameOutcome::Renamed)
        }
    }

    // ── relationships ─────────────────────────────────────────────────────────

    pub fn upsert_relation(&self, from_path: &str, to_path: &str, kind: &str) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        debug!("upsert_relation: {from_path} --[{kind}]--> {to_path}");
        self.conn.execute(
            "INSERT INTO relationships (from_path, to_path, kind, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(from_path, to_path, kind) DO NOTHING",
            params![from_path, to_path, kind, now],
        )?;
        Ok(())
    }

    pub fn get_relations(&self, path: &str) -> Result<Vec<(String, String, String)>> {
        debug!("get_relations: {path}");
        let mut stmt = self.conn.prepare(
            "SELECT from_path, to_path, kind FROM relationships
             WHERE from_path = ?1 OR to_path = ?1",
        )?;
        let rows = stmt.query_map(params![path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let rels: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(rels?)
    }

    pub fn all_relations(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_path, to_path, kind FROM relationships")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let rels: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(rels?)
    }

    /// Delete all edges where from_path matches — called before re-extracting relations.
    pub fn delete_relations_from(&self, from_path: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM relationships WHERE from_path = ?1",
            params![from_path],
        )?;
        debug!("delete_relations_from: {n} edges removed for {from_path}");
        Ok(n)
    }

    pub fn stale_relations(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.from_path, r.to_path, r.kind
             FROM relationships r
             LEFT JOIN entities src ON src.path = r.from_path
             LEFT JOIN entities dst ON dst.path = r.to_path
             WHERE src.path IS NULL OR dst.path IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let rels: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(rels?)
    }

    pub fn delete_stale_relations(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM relationships
             WHERE NOT EXISTS (SELECT 1 FROM entities e WHERE e.path = relationships.from_path)
                OR NOT EXISTS (SELECT 1 FROM entities e WHERE e.path = relationships.to_path)",
            [],
        )?;
        info!("cleaned {n} stale relations");
        Ok(n)
    }

    // ── FTS5 ──────────────────────────────────────────────────────────────────

    /// Update FTS index for a file after text extraction.
    pub fn update_fts(&self, path: &str, name: &str, content: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM entities_fts WHERE path = ?1", params![path])?;
        self.conn.execute(
            "INSERT INTO entities_fts(path, name, content) VALUES (?1, ?2, ?3)",
            params![path, name, truncate_on_char_boundary(content, 4000)],
        )?;
        Ok(())
    }

    /// Full-text search. Returns (path, rank) pairs sorted by relevance.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f64)>> {
        let sanitized = query
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || matches!(c, '_' | '-' | '/' | '.') {
                    c
                } else {
                    ' '
                }
            })
            .collect::<String>();
        let sanitized = sanitized.split_whitespace().collect::<Vec<_>>().join(" ");
        if sanitized.is_empty() {
            return Ok(vec![]);
        }

        let mut stmt = self.conn.prepare(
            "SELECT path, bm25(entities_fts) AS score
             FROM entities_fts
             WHERE entities_fts MATCH ?1
             ORDER BY score
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![sanitized, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;
        let results: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(results?)
    }

    pub fn find_route_matches(
        &self,
        route: &str,
        path_prefix: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<RouteMatch>> {
        let segments: Vec<String> = route
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_lowercase)
            .collect();
        if segments.is_empty() {
            return Ok(Vec::new());
        }
        let fts_query = segments
            .iter()
            .map(|segment| format!("\"{}\"", segment.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut stmt = self.conn.prepare(
            "SELECT path, content FROM entities_fts WHERE entities_fts MATCH ?1
             UNION
             SELECT path, content FROM entities_fts WHERE instr(lower(path), lower(?2)) > 0",
        )?;
        let rows = stmt.query_map(params![fts_query, route], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let route_lower = route.to_lowercase();
        let mut matches = Vec::new();
        for row in rows {
            let (path, content) = row?;
            if let Some(prefix) = path_prefix {
                if path != prefix
                    && !path
                        .strip_prefix(prefix)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                {
                    continue;
                }
            }
            let searchable = format!("{}\n{}", path.to_lowercase(), content.to_lowercase());
            let exact = contains_route_fragment(&searchable, &route_lower, true);
            let mut matched = 0;
            if !exact {
                for start in 0..segments.len() {
                    for end in start + 1..=segments.len() {
                        let fragment = format!("/{}", segments[start..end].join("/"));
                        if contains_route_fragment(&searchable, &fragment, false) {
                            matched = matched.max(end - start);
                        }
                    }
                }
            }
            if !exact && matched == 0 {
                continue;
            }
            let percent = if exact {
                100
            } else {
                ((matched * 100) / segments.len()).min(99) as u8
            };
            matches.push(RouteMatch {
                path,
                match_percent: percent,
                reason: if exact {
                    "exact route occurrence in indexed text or path".to_string()
                } else {
                    format!(
                        "{matched}/{} contiguous route segments in indexed text or path",
                        segments.len()
                    )
                },
            });
        }
        matches.sort_by(|left, right| {
            right
                .match_percent
                .cmp(&left.match_percent)
                .then(left.path.cmp(&right.path))
        });
        Ok(matches.into_iter().skip(offset).take(limit).collect())
    }

    pub fn get_indexed_routes(&self, route: &str) -> Result<Vec<IndexedRoute>> {
        let mut stmt = self.conn.prepare(
            "SELECT handler_path, provenance FROM indexed_routes WHERE route = ?1 ORDER BY handler_path",
        )?;
        let rows = stmt.query_map([route], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut records = Vec::new();
        for row in rows {
            let (handler_path, provenance) = row?;
            records.push(IndexedRoute {
                route: route.to_string(),
                handler_path,
                provenance: serde_json::from_str(&provenance)?,
            });
        }
        Ok(records)
    }

    // ── impact analysis ───────────────────────────────────────────────────────

    /// BFS over reverse edges: who imports/references `path`, up to `depth` hops.
    /// Returns entries sorted by (depth, path).
    pub fn reverse_deps(&self, path: &str, depth: u8) -> Result<Vec<ImpactEntry>> {
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut entries: Vec<ImpactEntry> = Vec::new();
        let mut queue: VecDeque<(String, u8)> = VecDeque::new();

        visited.insert(path.to_string());
        queue.push_back((path.to_string(), 0));

        while let Some((current, current_depth)) = queue.pop_front() {
            if current_depth >= depth {
                continue;
            }
            let mut stmt = self
                .conn
                .prepare("SELECT from_path, kind FROM relationships WHERE to_path = ?1")?;
            let deps: Vec<(String, String)> = stmt
                .query_map(params![current], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .filter_map(|r| r.ok())
                .collect();

            for (from_path, kind) in deps {
                if !visited.contains(&from_path) {
                    visited.insert(from_path.clone());
                    entries.push(ImpactEntry {
                        path: from_path.clone(),
                        kind,
                        depth: current_depth + 1,
                    });
                    queue.push_back((from_path, current_depth + 1));
                }
            }
        }

        entries.sort_by(|a, b| a.depth.cmp(&b.depth).then(a.path.cmp(&b.path)));
        Ok(entries)
    }

    // ── duplicate detection ───────────────────────────────────────────────────

    /// Return groups of entities that share the same non-null `content_hash`.
    pub fn exact_duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        // Exclude size-based pseudo-hashes: distinct large files of equal size
        // collide on them and would be reported as false duplicates.
        let pseudo_hash_like = format!("{PSEUDO_HASH_PREFIX}%");
        let mut stmt = self.conn.prepare(
            "SELECT content_hash FROM entities
             WHERE content_hash IS NOT NULL AND content_hash NOT LIKE ?1
             GROUP BY content_hash HAVING COUNT(*) > 1
             ORDER BY COUNT(*) DESC",
        )?;
        let hashes: Vec<String> = stmt
            .query_map([pseudo_hash_like], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();

        let mut groups = Vec::new();
        for hash in hashes {
            let paths = self
                .get_by_hash(&hash)?
                .into_iter()
                .map(|e| e.path)
                .collect();
            groups.push(DuplicateGroup {
                content_hash: hash,
                paths,
            });
        }
        Ok(groups)
    }

    // ── diagnostics ───────────────────────────────────────────────────────────

    /// Names of all tables in the database, sorted.
    pub fn table_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?;
        let names: rusqlite::Result<Vec<String>> = stmt.query_map([], |row| row.get(0))?.collect();
        Ok(names?)
    }

    /// Efficient total entity count.
    pub fn entity_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))?;
        Ok(n as usize)
    }

    /// Efficient total relation count.
    pub fn relation_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM relationships", [], |row| row.get(0))?;
        Ok(n as usize)
    }
}

fn contains_route_fragment(text: &str, fragment: &str, exact: bool) -> bool {
    text.match_indices(fragment).any(|(start, _)| {
        let before = text[..start].chars().next_back();
        let after = text[start + fragment.len()..].chars().next();
        let left_boundary = before.is_none_or(|character| {
            !character.is_alphanumeric() && character != '_' && character != '/'
        });
        let right_boundary = after.is_none_or(|character| {
            !character.is_alphanumeric() && character != '_' && (!exact || character != '/')
        });
        left_boundary && right_boundary
    })
}

fn capture_text(entity: &Entity) -> Option<String> {
    if entity.size_bytes > 256 * 1024 || entity.content_hash.as_deref()?.starts_with("size:") {
        return None;
    }
    let path = Path::new(&entity.path);
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_lowercase();
    if [
        "png", "jpg", "jpeg", "gif", "webp", "heic", "pdf", "zip", "gz", "tar", "7z", "mp3", "mp4",
        "mov", "wav", "db", "sqlite", "exe", "dylib", "so", "woff", "woff2", "ttf",
    ]
    .contains(&extension.as_str())
    {
        return None;
    }
    if name == ".env"
        || name.starts_with(".env.")
        || name == ".netrc"
        || name == ".htpasswd"
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || name.contains("secret")
        || name.contains("credential")
        || [
            "pem", "key", "p12", "pfx", "jks", "keystore", "asc", "gpg", "token",
        ]
        .iter()
        .any(|extension| name.ends_with(&format!(".{extension}")))
        || path.components().any(|component| {
            [".ssh", ".aws", ".gnupg", "secrets"]
                .contains(&component.as_os_str().to_str().unwrap_or(""))
        })
        || std::env::var("ORGANON_SENSITIVE_EXTRA")
            .ok()
            .is_some_and(|patterns| extra_sensitive_match(&name, &patterns))
    {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 256 * 1024 || bytes.contains(&0) {
        return None;
    }
    if hex::encode(Sha256::digest(&bytes)) != entity.content_hash.as_deref()? {
        return None;
    }
    String::from_utf8(bytes).ok()
}

fn extra_sensitive_match(name: &str, patterns: &str) -> bool {
    patterns.split(':').any(|pattern| {
        globset::Glob::new(&pattern.trim().to_lowercase())
            .ok()
            .is_some_and(|glob| glob.compile_matcher().is_match(name))
    })
}

fn text_change(before: &str, after: &str) -> TextChange {
    let old_lines: Vec<&str> = before.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = after.split_inclusive('\n').collect();
    let mut start = 0;
    while start < old_lines.len() && start < new_lines.len() && old_lines[start] == new_lines[start]
    {
        start += 1;
    }
    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > start && new_end > start && old_lines[old_end - 1] == new_lines[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }
    TextChange {
        start_line: start + 1,
        before: old_lines[start..old_end].concat(),
        after: new_lines[start..new_end].concat(),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Truncate `s` to at most `max_bytes` bytes without splitting a UTF-8
/// character. Returns the largest prefix whose byte length is `<= max_bytes`.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk back from max_bytes until we land on a char boundary.
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    Ok(Entity {
        id: row.get(0)?,
        path: row.get(1)?,
        name: row.get(2)?,
        extension: row.get(3)?,
        size_bytes: row.get::<_, i64>(4)? as u64,
        created_at: row.get(5)?,
        modified_at: row.get(6)?,
        accessed_at: row.get(7)?,
        lifecycle: lifecycle_from_str(&row.get::<_, String>(8)?),
        content_hash: row.get(9)?,
        summary: row.get(10)?,
        git_author: row.get(11)?,
    })
}

fn lifecycle_from_str(s: &str) -> LifecycleState {
    match s {
        "born" => LifecycleState::Born,
        "active" => LifecycleState::Active,
        "dormant" => LifecycleState::Dormant,
        "archived" => LifecycleState::Archived,
        "dead" => LifecycleState::Dead,
        other => {
            warn!("unknown lifecycle value '{other}', defaulting to Born");
            LifecycleState::Born
        }
    }
}

pub fn entity_matches_filter(entity: &Entity, filter: &FindFilter) -> bool {
    if filter
        .state
        .as_ref()
        .is_some_and(|state| entity.lifecycle.as_str() != state)
    {
        return false;
    }
    if filter
        .extension
        .as_ref()
        .is_some_and(|ext| entity.extension.as_deref() != Some(ext.trim_start_matches('.')))
    {
        return false;
    }
    if filter
        .created_after
        .is_some_and(|created_after| entity.created_at <= created_after)
    {
        return false;
    }
    if filter
        .modified_after
        .is_some_and(|modified_after| entity.modified_at <= modified_after)
    {
        return false;
    }
    if filter
        .larger_than
        .is_some_and(|larger_than| entity.size_bytes <= larger_than)
    {
        return false;
    }
    true
}

fn build_find_where(filter: &FindFilter) -> (Vec<String>, Vec<Value>) {
    let mut where_parts: Vec<String> = vec!["1=1".into()];
    let mut params: Vec<Value> = Vec::new();

    if let Some(s) = &filter.state {
        where_parts.push("lifecycle = ?".into());
        params.push(Value::Text(s.clone()));
    }
    if let Some(e) = &filter.extension {
        where_parts.push("extension = ?".into());
        params.push(Value::Text(e.trim_start_matches('.').to_string()));
    }
    if let Some(t) = filter.created_after {
        where_parts.push("created_at > ?".into());
        params.push(Value::Integer(t));
    }
    if let Some(t) = filter.modified_after {
        where_parts.push("modified_at > ?".into());
        params.push(Value::Integer(t));
    }
    if let Some(b) = filter.larger_than {
        where_parts.push("size_bytes > ?".into());
        params.push(Value::Integer(b as i64));
    }

    (where_parts, params)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, definition: &str) -> Result<()> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let columns = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let existing: rusqlite::Result<Vec<_>> = columns.collect();
    if existing?.iter().any(|name| name == column) {
        return Ok(());
    }

    let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {definition}");
    conn.execute(&sql, [])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Entity, LifecycleState};
    use tempfile::NamedTempFile;

    #[test]
    fn extra_sensitive_patterns_match_filename() {
        assert!(extra_sensitive_match("vault.json", "vault.json:*.secret"));
        assert!(extra_sensitive_match("token.secret", "vault.json:*.secret"));
        assert!(!extra_sensitive_match("readme.md", "vault.json:*.secret"));
    }

    fn tmp_graph() -> (Graph, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let g = Graph::open(f.path().to_str().unwrap()).unwrap();
        (g, f)
    }

    fn mk_entity(path: &str) -> Entity {
        Entity {
            id: path.to_string(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap().to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 64,
            created_at: 1000,
            modified_at: 1000,
            accessed_at: 1000,
            lifecycle: LifecycleState::Active,
            content_hash: Some(format!("hash-{path}")),
            summary: None,
            git_author: None,
        }
    }

    #[test]
    fn route_matches_exact_partial_and_scope() {
        let (graph, _file) = tmp_graph();
        for (path, text) in [
            ("/work/route.py", "@app.get('/x/sds/sd')"),
            ("/work/partial.py", "@app.get('/x/sds')"),
            ("/work/other.py", "@app.get('/x/sds/sd2')"),
            ("/work/scattered.py", "x and sds and sd are unrelated words"),
            ("/else/route.py", "@app.get('/x/sds/sd')"),
        ] {
            graph.update_fts(path, "route.py", text).unwrap();
        }
        let matches = graph
            .find_route_matches("/x/sds/sd", Some("/work"), 10, 0)
            .unwrap();
        assert_eq!(matches[0].path, "/work/route.py");
        assert_eq!(matches[0].match_percent, 100);
        assert!(matches
            .iter()
            .any(|item| item.path == "/work/partial.py" && item.match_percent < 100));
        assert!(matches.iter().all(|item| item.path != "/else/route.py"));
        assert!(matches
            .iter()
            .all(|item| item.path != "/work/other.py" || item.match_percent < 100));
        assert!(matches.iter().all(|item| item.path != "/work/scattered.py"));
        assert_eq!(
            graph
                .find_route_matches("/x/sds/sd", Some("/work"), 1, 1)
                .unwrap()
                .len(),
            1
        );
    }

    // ── concurrency ──────────────────────────────────────────────────────────

    #[test]
    fn wal_mode_is_enabled() {
        let (g, _f) = tmp_graph();
        let mode: String = g
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn two_connections_can_write_and_read() {
        // Open a second Graph on the same file; a committed write on one
        // connection must be visible to the other.
        let f = NamedTempFile::new().unwrap();
        let path = f.path().to_str().unwrap();
        let writer = Graph::open(path).unwrap();
        let reader = Graph::open(path).unwrap();

        writer.upsert(&mk_entity("/concurrent.rs")).unwrap();
        let got = reader.get_by_path("/concurrent.rs").unwrap();
        assert!(
            got.is_some(),
            "second connection must see the committed write"
        );
    }

    // ── truncate_on_char_boundary / update_fts ──────────────────────────────

    #[test]
    fn truncate_never_splits_multibyte_char() {
        // "é" is 2 bytes (U+00E9). A 4001-char string of "é" is 8002 bytes;
        // cutting at 4000 bytes lands mid-char with a naive byte slice.
        let s = "é".repeat(4001);
        let out = truncate_on_char_boundary(&s, 4000);
        assert!(out.len() <= 4000);
        assert!(s.is_char_boundary(out.len()));
        // Result is valid UTF-8 by construction (it's a &str), so just re-derive.
        assert_eq!(out.len() % 2, 0); // whole "é" chars only
    }

    #[test]
    fn truncate_shorter_than_limit_is_identity() {
        assert_eq!(truncate_on_char_boundary("hello", 4000), "hello");
    }

    #[test]
    fn update_fts_with_multibyte_content_does_not_panic() {
        let (g, _f) = tmp_graph();
        let content = "日本語テスト".repeat(1000); // multi-byte, > 4000 bytes
        g.update_fts("a/b.rs", "b.rs", &content).unwrap();
    }

    // ── reverse_deps ─────────────────────────────────────────────────────────

    #[test]
    fn reverse_deps_direct_importer() {
        let (g, _f) = tmp_graph();
        g.upsert(&mk_entity("/a.rs")).unwrap();
        g.upsert(&mk_entity("/b.rs")).unwrap();
        // b imports a
        g.upsert_relation("/b.rs", "/a.rs", "imports").unwrap();

        let entries = g.reverse_deps("/a.rs", 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/b.rs");
        assert_eq!(entries[0].depth, 1);
    }

    #[test]
    fn reverse_deps_transitive() {
        let (g, _f) = tmp_graph();
        for p in ["/a.rs", "/b.rs", "/c.rs"] {
            g.upsert(&mk_entity(p)).unwrap();
        }
        g.upsert_relation("/b.rs", "/a.rs", "imports").unwrap();
        g.upsert_relation("/c.rs", "/b.rs", "imports").unwrap();

        let entries = g.reverse_deps("/a.rs", 5).unwrap();
        assert_eq!(entries.len(), 2);
        let paths: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"/b.rs"));
        assert!(paths.contains(&"/c.rs"));
        let b = entries.iter().find(|e| e.path == "/b.rs").unwrap();
        let c = entries.iter().find(|e| e.path == "/c.rs").unwrap();
        assert_eq!(b.depth, 1);
        assert_eq!(c.depth, 2);
    }

    #[test]
    fn reverse_deps_depth_limit_respected() {
        let (g, _f) = tmp_graph();
        for p in ["/a.rs", "/b.rs", "/c.rs"] {
            g.upsert(&mk_entity(p)).unwrap();
        }
        g.upsert_relation("/b.rs", "/a.rs", "imports").unwrap();
        g.upsert_relation("/c.rs", "/b.rs", "imports").unwrap();

        // depth=1 → only direct importers
        let entries = g.reverse_deps("/a.rs", 1).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "/b.rs");
    }

    #[test]
    fn reverse_deps_no_cycle_panic() {
        let (g, _f) = tmp_graph();
        for p in ["/a.rs", "/b.rs"] {
            g.upsert(&mk_entity(p)).unwrap();
        }
        g.upsert_relation("/a.rs", "/b.rs", "imports").unwrap();
        g.upsert_relation("/b.rs", "/a.rs", "imports").unwrap();

        // Must not infinite-loop
        let entries = g.reverse_deps("/a.rs", 5).unwrap();
        assert_eq!(entries.len(), 1);
    }

    // ── exact_duplicates ──────────────────────────────────────────────────────

    #[test]
    fn exact_duplicates_finds_matching_hashes() {
        let (g, _f) = tmp_graph();
        let mut a = mk_entity("/copy1.rs");
        let mut b = mk_entity("/copy2.rs");
        a.content_hash = Some("deadbeef".to_string());
        b.content_hash = Some("deadbeef".to_string());
        g.upsert(&a).unwrap();
        g.upsert(&b).unwrap();
        g.upsert(&mk_entity("/unique.rs")).unwrap();

        let groups = g.exact_duplicates().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].content_hash, "deadbeef");
        assert_eq!(groups[0].paths.len(), 2);
    }

    #[test]
    fn exact_duplicates_empty_when_none() {
        let (g, _f) = tmp_graph();
        g.upsert(&mk_entity("/solo.rs")).unwrap();
        assert!(g.exact_duplicates().unwrap().is_empty());
    }

    #[test]
    fn exact_duplicates_ignores_size_pseudo_hashes() {
        let (g, _f) = tmp_graph();
        // Two distinct large files of equal size share a `size:N` pseudo-hash;
        // they must NOT be reported as duplicates.
        let mut a = mk_entity("/big_a.bin");
        let mut b = mk_entity("/big_b.bin");
        a.content_hash = Some("size:104857600".to_string());
        b.content_hash = Some("size:104857600".to_string());
        g.upsert(&a).unwrap();
        g.upsert(&b).unwrap();
        assert!(
            g.exact_duplicates().unwrap().is_empty(),
            "size-based pseudo-hashes must not count as duplicates"
        );
    }

    // ── diagnostics ──────────────────────────────────────────────────────────

    #[test]
    fn table_names_includes_required_tables() {
        let (g, _f) = tmp_graph();
        let tables = g.table_names().unwrap();
        for required in &["entities", "entity_history", "relationships"] {
            assert!(tables.iter().any(|t| t == *required), "missing: {required}");
        }
    }

    #[test]
    fn entity_count_and_relation_count() {
        let (g, _f) = tmp_graph();
        assert_eq!(g.entity_count().unwrap(), 0);
        assert_eq!(g.relation_count().unwrap(), 0);
        g.upsert(&mk_entity("/a.rs")).unwrap();
        g.upsert(&mk_entity("/b.rs")).unwrap();
        g.upsert_relation("/a.rs", "/b.rs", "imports").unwrap();
        assert_eq!(g.entity_count().unwrap(), 2);
        assert_eq!(g.relation_count().unwrap(), 1);
    }

    // ── rename history continuity ─────────────────────────────────────────────

    #[test]
    fn rename_preserves_history_under_new_path() {
        let (g, _f) = tmp_graph();
        // Insert entity and let upsert record an initial history entry.
        g.upsert(&mk_entity("/old.rs")).unwrap();

        // Confirm history exists under old path.
        let before = g.get_history("/old.rs", 50).unwrap();
        assert!(
            !before.is_empty(),
            "upsert should record at least one history entry"
        );

        // Rename.
        g.rename_entity("/old.rs", "/new.rs").unwrap();

        // History under old path should be empty.
        let old_hist = g.get_history("/old.rs", 50).unwrap();
        assert!(
            old_hist.is_empty(),
            "old path should have no history after rename"
        );

        // History under new path should include pre-rename entries + the rename event.
        let new_hist = g.get_history("/new.rs", 50).unwrap();
        assert!(
            new_hist.len() > before.len(),
            "new path should have original entries plus the rename event; got {}",
            new_hist.len()
        );

        // The most recent entry should be the rename event.
        let rename_entry = new_hist.iter().find(|e| e.event == "renamed");
        assert!(
            rename_entry.is_some(),
            "rename event should appear in history"
        );
        assert_eq!(
            rename_entry.unwrap().old_path.as_deref(),
            Some("/old.rs"),
            "rename event should record old_path"
        );
    }

    #[test]
    fn rename_history_continuity_across_multiple_renames() {
        let (g, _f) = tmp_graph();
        g.upsert(&mk_entity("/a.rs")).unwrap();

        g.rename_entity("/a.rs", "/b.rs").unwrap();
        g.rename_entity("/b.rs", "/c.rs").unwrap();

        // All history should accumulate under the final path.
        let hist = g.get_history("/c.rs", 50).unwrap();
        let rename_events: Vec<_> = hist.iter().filter(|e| e.event == "renamed").collect();
        assert_eq!(rename_events.len(), 2, "should have two rename events");

        // No history under old paths.
        assert!(g.get_history("/a.rs", 50).unwrap().is_empty());
        assert!(g.get_history("/b.rs", 50).unwrap().is_empty());
    }
}
