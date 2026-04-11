use anyhow::Result;
use log::{debug, info, warn};
use rusqlite::{Connection, params};

use crate::entity::{Entity, LifecycleState};

pub struct Graph {
    conn: Connection,
}

/// Filter for `Graph::find()`.
#[derive(Debug, Default)]
pub struct FindFilter {
    pub state:          Option<String>,
    pub extension:      Option<String>,
    pub modified_after: Option<i64>,   // unix timestamp
    pub larger_than:    Option<u64>,   // bytes
    pub limit:          usize,
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
        self.conn.execute_batch("
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
                summary      TEXT
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

            CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
                path UNINDEXED,
                name,
                content,
                content='entities',
                content_rowid='rowid'
            );
        ")?;
        debug!("schema migration ok");
        Ok(())
    }

    // ── entities CRUD ─────────────────────────────────────────────────────────

    pub fn upsert(&self, entity: &Entity) -> Result<()> {
        debug!("upsert: {} [{}]", entity.path, entity.lifecycle.as_str());
        self.conn.execute(
            "INSERT INTO entities
                (id, path, name, extension, size_bytes, created_at, modified_at, accessed_at,
                 lifecycle, content_hash, summary)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(path) DO UPDATE SET
                name         = excluded.name,
                extension    = excluded.extension,
                size_bytes   = excluded.size_bytes,
                modified_at  = excluded.modified_at,
                accessed_at  = excluded.accessed_at,
                lifecycle    = excluded.lifecycle,
                content_hash = excluded.content_hash",
            params![
                entity.id, entity.path, entity.name, entity.extension,
                entity.size_bytes as i64, entity.created_at, entity.modified_at,
                entity.accessed_at, entity.lifecycle.as_str(),
                entity.content_hash, entity.summary,
            ],
        )?;
        Ok(())
    }

    pub fn get_by_path(&self, path: &str) -> Result<Option<Entity>> {
        debug!("get_by_path: {}", path);
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary
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
        let n = self.conn.execute("DELETE FROM entities WHERE path = ?1", params![path])?;
        if n > 0 { info!("deleted entity: {}", path); }
        else { warn!("delete_by_path: no entity at {}", path); }
        Ok(())
    }

    pub fn all(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary FROM entities",
        )?;
        let rows = stmt.query_map([], |row| row_to_entity(row))?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        let entities = entities?;
        debug!("all: {} entities", entities.len());
        Ok(entities)
    }

    /// Filtered query — for `organon find`.
    pub fn find(&self, filter: &FindFilter) -> Result<Vec<Entity>> {
        let mut where_parts: Vec<String> = vec!["1=1".into()];
        if let Some(s) = &filter.state          { where_parts.push(format!("lifecycle = '{}'", s.replace('\'', ""))); }
        if let Some(e) = &filter.extension      { where_parts.push(format!("extension = '{}'", e.replace('\'', ""))); }
        if let Some(t) = filter.modified_after  { where_parts.push(format!("modified_at > {}", t)); }
        if let Some(b) = filter.larger_than     { where_parts.push(format!("size_bytes > {}", b)); }

        let limit = if filter.limit == 0 { 50 } else { filter.limit };
        let sql = format!(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary
             FROM entities WHERE {} ORDER BY modified_at DESC LIMIT {}",
            where_parts.join(" AND "), limit
        );
        debug!("find: {}", sql);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| row_to_entity(row))?;
        let entities: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(entities?)
    }

    /// Delete all dead entities. Returns count removed.
    pub fn delete_dead_entities(&self) -> Result<usize> {
        let n = self.conn.execute("DELETE FROM entities WHERE lifecycle = 'dead'", [])?;
        info!("cleaned {} dead entities", n);
        Ok(n)
    }

    /// List dead entities (for --dry-run preview).
    pub fn dead_entities(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, name, extension, size_bytes, created_at, modified_at,
                    accessed_at, lifecycle, content_hash, summary
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
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        let rels: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(rels?)
    }

    pub fn all_relations(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_path, to_path, kind FROM relationships",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        let rels: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(rels?)
    }

    /// Delete all edges where from_path matches — called before re-extracting relations.
    pub fn delete_relations_from(&self, from_path: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM relationships WHERE from_path = ?1", params![from_path],
        )?;
        debug!("delete_relations_from: {} edges removed for {}", n, from_path);
        Ok(n)
    }

    // ── FTS5 ──────────────────────────────────────────────────────────────────

    /// Update FTS index for a file after text extraction.
    pub fn update_fts(&self, path: &str, name: &str, content: &str) -> Result<()> {
        // FTS content table — use INSERT OR REPLACE keyed on path
        self.conn.execute(
            "INSERT INTO entities_fts(path, name, content) VALUES (?1, ?2, ?3)
             ON CONFLICT DO UPDATE SET name=excluded.name, content=excluded.content",
            params![path, name, &content[..content.len().min(4000)]],
        )?;
        Ok(())
    }

    /// Full-text search. Returns (path, rank) pairs sorted by relevance.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f64)>> {
        let safe_query = query.replace('"', " ");
        let sql = format!(
            "SELECT path, rank FROM entities_fts WHERE entities_fts MATCH '\"{}\"' ORDER BY rank LIMIT {}",
            safe_query, limit
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?;
        let results: rusqlite::Result<Vec<_>> = rows.collect();
        Ok(results?)
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn row_to_entity(row: &rusqlite::Row<'_>) -> rusqlite::Result<Entity> {
    Ok(Entity {
        id:           row.get(0)?,
        path:         row.get(1)?,
        name:         row.get(2)?,
        extension:    row.get(3)?,
        size_bytes:   row.get::<_, i64>(4)? as u64,
        created_at:   row.get(5)?,
        modified_at:  row.get(6)?,
        accessed_at:  row.get(7)?,
        lifecycle:    lifecycle_from_str(&row.get::<_, String>(8)?),
        content_hash: row.get(9)?,
        summary:      row.get(10)?,
    })
}

fn lifecycle_from_str(s: &str) -> LifecycleState {
    match s {
        "born"     => LifecycleState::Born,
        "active"   => LifecycleState::Active,
        "dormant"  => LifecycleState::Dormant,
        "archived" => LifecycleState::Archived,
        "dead"     => LifecycleState::Dead,
        other      => {
            warn!("unknown lifecycle value '{}', defaulting to Born", other);
            LifecycleState::Born
        }
    }
}
