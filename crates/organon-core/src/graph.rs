use std::path::Path;

use anyhow::Result;
use log::{debug, info, warn};
use rusqlite::{params, params_from_iter, types::Value, Connection};

use crate::entity::{Entity, LifecycleState};

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
        debug!("opening graph db: {}", db_path);
        let conn = Connection::open(db_path)?;
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
        ",
        )?;
        ensure_column(&self.conn, "entities", "git_author", "TEXT")?;
        debug!("schema migration ok");
        Ok(())
    }

    // ── entities CRUD ─────────────────────────────────────────────────────────

    pub fn upsert(&self, entity: &Entity) -> Result<()> {
        debug!("upsert: {} [{}]", entity.path, entity.lifecycle.as_str());
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
        Ok(())
    }

    pub fn get_by_path(&self, path: &str) -> Result<Option<Entity>> {
        debug!("get_by_path: {}", path);
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author
             FROM entities WHERE path = ?1",
        )?;
        let mut rows = stmt.query(params![path])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_entity(row)?))
        } else {
            debug!("get_by_path: not found — {}", path);
            Ok(None)
        }
    }

    pub fn delete_by_path(&self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM relationships WHERE from_path = ?1 OR to_path = ?1",
            params![path],
        )?;
        let n = self
            .conn
            .execute("DELETE FROM entities WHERE path = ?1", params![path])?;
        if n > 0 {
            info!("deleted entity: {}", path);
        } else {
            warn!("delete_by_path: no entity at {}", path);
        }
        Ok(())
    }

    pub fn all(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary, git_author FROM entities",
        )?;
        let rows = stmt.query_map([], |row| row_to_entity(row))?;
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
        debug!("find: {}", sql);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| row_to_entity(row))?;
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
        let rows = stmt.query_map([], |row| row_to_entity(row))?;
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

    /// Store an LLM-generated summary for a file.
    pub fn update_summary(&self, path: &str, summary: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE entities SET summary = ?2 WHERE path = ?1",
            params![path, summary],
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
        let rows = stmt.query_map(params![content_hash], |row| row_to_entity(row))?;
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
        debug!("rename_entity: {} → {}", old_path, new_path);

        if self.get_by_path(old_path)?.is_none() {
            warn!("rename_entity: old path not found: {}", old_path);
            return Ok(RenameOutcome::OldNotFound);
        }

        // ── handle conflict at new_path ───────────────────────────────────────
        let conflict_resolved = if self.get_by_path(new_path)?.is_some() {
            info!(
                "rename_entity: new_path exists (overwritten by OS rename), removing: {}",
                new_path
            );
            self.conn.execute(
                "DELETE FROM relationships WHERE from_path = ?1 OR to_path = ?1",
                params![new_path],
            )?;
            self.conn
                .execute("DELETE FROM entities WHERE path = ?1", params![new_path])?;
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

        // ── drop stale FTS entry (indexer re-adds under new path) ─────────────
        self.conn
            .execute("DELETE FROM entities_fts WHERE path = ?1", params![old_path])?;

        info!("renamed entity: {} → {}", old_path, new_path);

        if conflict_resolved {
            Ok(RenameOutcome::ConflictResolved)
        } else {
            Ok(RenameOutcome::Renamed)
        }
    }

    // ── relationships ─────────────────────────────────────────────────────────

    pub fn upsert_relation(&self, from_path: &str, to_path: &str, kind: &str) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        debug!("upsert_relation: {} --[{}]--> {}", from_path, kind, to_path);
        self.conn.execute(
            "INSERT INTO relationships (from_path, to_path, kind, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(from_path, to_path, kind) DO NOTHING",
            params![from_path, to_path, kind, now],
        )?;
        Ok(())
    }

    pub fn get_relations(&self, path: &str) -> Result<Vec<(String, String, String)>> {
        debug!("get_relations: {}", path);
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
        debug!(
            "delete_relations_from: {} edges removed for {}",
            n, from_path
        );
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
        info!("cleaned {} stale relations", n);
        Ok(n)
    }

    // ── FTS5 ──────────────────────────────────────────────────────────────────

    /// Update FTS index for a file after text extraction.
    pub fn update_fts(&self, path: &str, name: &str, content: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM entities_fts WHERE path = ?1", params![path])?;
        self.conn.execute(
            "INSERT INTO entities_fts(path, name, content) VALUES (?1, ?2, ?3)",
            params![path, name, &content[..content.len().min(4000)]],
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
}

// ── helpers ───────────────────────────────────────────────────────────────────

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
            warn!("unknown lifecycle value '{}', defaulting to Born", other);
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
