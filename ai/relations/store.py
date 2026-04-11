"""
SQLite CRUD for file relationships.
Uses the same entities.db as organon-core (relationships table added in migration).
"""
import logging
import sqlite3
import time
from collections import deque
from pathlib import Path

logger = logging.getLogger(__name__)

DB_PATH = Path("~/.organon/entities.db").expanduser()
_MAX_GRAPH_NODES = 50


def _db(db_path=DB_PATH):
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    return conn


# ── write ─────────────────────────────────────────────────────────────────────

def upsert_relations(
    relations: list[tuple[str, str, str]],
    db_path=DB_PATH,
) -> None:
    """Batch-upsert (from_path, to_path, kind) triples."""
    if not relations:
        return
    now = int(time.time())
    with _db(db_path) as conn:
        conn.executemany(
            "INSERT INTO relationships (from_path, to_path, kind, created_at) "
            "VALUES (?, ?, ?, ?) "
            "ON CONFLICT(from_path, to_path, kind) DO NOTHING",
            [(f, t, k, now) for f, t, k in relations],
        )
    logger.debug("upsert_relations: %d relations stored", len(relations))


def delete_relations_from(path: str, db_path=DB_PATH) -> int:
    """Delete all outgoing edges from path before re-extracting relations."""
    with _db(db_path) as conn:
        cursor = conn.execute(
            "DELETE FROM relationships WHERE from_path = ?",
            (path,),
        )
        deleted = cursor.rowcount or 0
    logger.debug("delete_relations_from: %d relations removed for %s", deleted, path)
    return deleted


# ── read ──────────────────────────────────────────────────────────────────────

def get_relations(path: str, db_path=DB_PATH) -> list[dict]:
    """Return all edges where path is source or target."""
    with _db(db_path) as conn:
        rows = conn.execute(
            "SELECT from_path, to_path, kind FROM relationships "
            "WHERE from_path = ? OR to_path = ?",
            (path, path),
        ).fetchall()
    result = [{"from": r["from_path"], "to": r["to_path"], "kind": r["kind"]} for r in rows]
    logger.debug("get_relations: %d edges for %s", len(result), path)
    return result


def get_graph(path: str, depth: int = 1, db_path=DB_PATH) -> dict:
    """
    BFS from `path` up to `depth` hops. Returns {nodes: [...], edges: [...]}.
    Caps at _MAX_GRAPH_NODES nodes.
    """
    depth = min(depth, 3)
    visited: set[str] = set()
    edges: list[dict] = []
    queue: deque[tuple[str, int]] = deque([(path, 0)])

    with _db(db_path) as conn:
        while queue and len(visited) < _MAX_GRAPH_NODES:
            current, level = queue.popleft()
            if current in visited:
                continue
            visited.add(current)
            if level >= depth:
                continue

            rows = conn.execute(
                "SELECT from_path, to_path, kind FROM relationships "
                "WHERE from_path = ? OR to_path = ?",
                (current, current),
            ).fetchall()

            for row in rows:
                frm, to, kind = row["from_path"], row["to_path"], row["kind"]
                edge = {"from": frm, "to": to, "kind": kind}
                if edge not in edges:
                    edges.append(edge)
                neighbor = to if frm == current else frm
                if neighbor not in visited and len(visited) < _MAX_GRAPH_NODES:
                    queue.append((neighbor, level + 1))

    logger.debug("get_graph: %d nodes %d edges for %s", len(visited), len(edges), path)
    return {"nodes": sorted(visited), "edges": edges}
