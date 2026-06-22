"""
Organon indexer daemon.

Reads entity graph from SQLite (maintained by organon-core),
extracts text, embeds, and stores vectors in lancedb.

Usage:
    uv run python -m ai.indexer               # run once
    uv run python -m ai.indexer --watch 30    # run every 30s
    uv run python -m ai.indexer --health
"""

import argparse
import importlib.metadata
import logging
import sqlite3
import sys
import time
from pathlib import Path

from ai.common.ignore import is_ignored
from ai.common.sensitive import is_sensitive, sensitive_reason
from ai.embeddings.store import (
    get_all_entries,
    get_indexed_hashes,
    index_file,
    update_path_in_store,
)
from ai.extractor.extract import extract_text
from ai.relations.extract import extract_relations
from ai.relations.store import delete_relations_from, upsert_relations

logger = logging.getLogger(__name__)

DEFAULT_DB = Path("~/.organon/entities.db").expanduser()
PACKAGE_NAME = "organon-ai"


def indexer_version() -> str:
    try:
        return importlib.metadata.version(PACKAGE_NAME)
    except importlib.metadata.PackageNotFoundError:
        return "0.0.0+local"


def health_line() -> str:
    return f"organon-indexer {indexer_version()} ok (python {sys.version.split()[0]})"


def _normalize_prefixes(path_prefixes: list[str] | None) -> list[str]:
    return [str(Path(prefix).expanduser().resolve()) for prefix in path_prefixes or []]


def _path_in_scope(path: str, path_prefixes: list[str]) -> bool:
    if not path_prefixes:
        return True
    return any(path == prefix or path.startswith(f"{prefix}/") for prefix in path_prefixes)


def _scoped_where(path_prefixes: list[str]) -> tuple[str, list[str]]:
    if not path_prefixes:
        return "", []
    clauses = []
    params = []
    for prefix in path_prefixes:
        clauses.append("(path = ? OR path LIKE ?)")
        params.extend([prefix, f"{prefix}/%"])
    return f" AND ({' OR '.join(clauses)})", params


def get_entities(db_path: Path, path_prefixes: list[str] | None = None) -> list[dict]:
    """Fetch all non-dead entities with a content_hash from SQLite."""
    prefixes = _normalize_prefixes(path_prefixes)
    scoped_where, scoped_params = _scoped_where(prefixes)
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT path, name, content_hash, lifecycle FROM entities "
            "WHERE content_hash IS NOT NULL AND lifecycle != 'dead'"
            f"{scoped_where}",
            scoped_params,
        ).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


def get_fts_paths(db_path: Path, path_prefixes: list[str] | None = None) -> set[str]:
    prefixes = _normalize_prefixes(path_prefixes)
    scoped_where, scoped_params = _scoped_where(prefixes)
    with sqlite3.connect(str(db_path)) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5("
            "path UNINDEXED, name, content)"
        )
        rows = conn.execute(
            f"SELECT path FROM entities_fts WHERE 1=1{scoped_where}",
            scoped_params,
        ).fetchall()
    return {r["path"] for r in rows}


def update_fts(db_path: Path, path: str, name: str, text: str) -> None:
    with sqlite3.connect(str(db_path)) as conn:
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5("
            "path UNINDEXED, name, content)"
        )
        conn.execute("DELETE FROM entities_fts WHERE path = ?", (path,))
        conn.execute(
            "INSERT INTO entities_fts(path, name, content) VALUES (?, ?, ?)",
            (path, name, text[:4000]),
        )


def reconcile_lancedb_paths(
    db_path: Path,
    vectors_db_path: str | None = None,
    path_prefixes: list[str] | None = None,
) -> int:
    """Fix stale paths in lancedb after file renames.

    For each lancedb entry whose `path` is no longer present in SQLite,
    look for a SQLite entity with the same `content_hash`.  If exactly one
    such entity exists the lancedb row's path is updated in-place — no
    re-embedding needed.

    Returns the number of entries reconciled.
    """
    prefixes = _normalize_prefixes(path_prefixes)
    sqlite_entities = get_entities(db_path, prefixes)
    # hash → list of paths that currently hold that hash
    hash_to_paths: dict[str, list[str]] = {}
    sqlite_path_set: set[str] = set()
    for e in sqlite_entities:
        sqlite_path_set.add(e["path"])
        h = e.get("content_hash")
        if h:
            hash_to_paths.setdefault(h, []).append(e["path"])

    ldb_entries = get_all_entries(db_path=vectors_db_path)
    reconciled = 0
    for entry in ldb_entries:
        ldb_path = entry["path"]
        ldb_hash = entry["content_hash"]
        if not _path_in_scope(ldb_path, prefixes):
            continue
        if ldb_path in sqlite_path_set:
            continue  # still valid
        candidates = hash_to_paths.get(ldb_hash, [])
        if len(candidates) == 1:
            new_path = candidates[0]
            logger.info("reconcile lancedb path: %s → %s", ldb_path, new_path)
            if update_path_in_store(ldb_path, new_path, db_path=vectors_db_path):
                reconciled += 1
        elif len(candidates) == 0:
            logger.debug(
                "reconcile: no sqlite entity for lancedb entry %s — leaving stale", ldb_path
            )
        else:
            logger.debug(
                "reconcile: ambiguous (%d candidates) for lancedb entry %s — skipping",
                len(candidates),
                ldb_path,
            )
    if reconciled:
        logger.info("reconcile_lancedb_paths: %d path(s) updated", reconciled)
    return reconciled


def run_once(
    db_path: Path,
    *,
    vectors_db_path: str | None = None,
    path_prefixes: list[str] | None = None,
) -> dict:
    """Index all entities not yet in vector store. Returns stats dict."""
    prefixes = _normalize_prefixes(path_prefixes)
    # Reconcile stale paths in lancedb (e.g. from rename/move operations).
    try:
        reconcile_lancedb_paths(db_path, vectors_db_path=vectors_db_path, path_prefixes=prefixes)
    except Exception as e:
        logger.warning("path reconciliation failed (non-fatal): %s", e)

    entities = get_entities(db_path, prefixes)
    if not entities:
        logger.info("no entities in graph")
        return {"total": 0, "indexed": 0, "skipped": 0, "errors": 0, "sensitive_skipped": 0}

    logger.info("indexing %d entities from graph", len(entities))
    indexed_hashes = get_indexed_hashes(db_path=vectors_db_path)
    fts_paths = get_fts_paths(db_path, prefixes)
    stats = {
        "total": len(entities),
        "indexed": 0,
        "skipped": 0,
        "errors": 0,
        "sensitive_skipped": 0,
    }

    for entity in entities:
        path = entity["path"]
        name = entity["name"]
        content_hash = entity["content_hash"]
        needs_vector = content_hash not in indexed_hashes
        needs_fts = path not in fts_paths

        if is_ignored(path):
            logger.debug("skipped (ignored path): %s", path)
            stats["skipped"] += 1
            continue

        if is_sensitive(path):
            reason = sensitive_reason(path)
            logger.info("skipped (sensitive): %s — %s", path, reason)
            stats["sensitive_skipped"] += 1
            continue

        if not (needs_vector or needs_fts):
            logger.debug("skipped (already indexed and enriched): %s", path)
            stats["skipped"] += 1
            continue

        text = extract_text(path)
        if not text or not text.strip():
            logger.debug("skipped (no text): %s", path)
            stats["skipped"] += 1
            continue

        try:
            update_fts(db_path, path, name, text)
            fts_paths.add(path)
        except Exception as e:
            logger.warning("error updating FTS for %s: %s", path, e)
            stats["errors"] += 1
            continue

        try:
            if needs_vector:
                index_file(path, text, content_hash, db_path=vectors_db_path)
                indexed_hashes.add(content_hash)
                stats["indexed"] += 1
                logger.info("indexed: %s", path)
            else:
                logger.debug("vector up-to-date: %s", path)
        except Exception as e:
            logger.warning("error indexing %s: %s", path, e)
            stats["errors"] += 1
            continue

        # Extract and store explicit import/reference relations
        try:
            delete_relations_from(path, db_path=db_path)
            rels = extract_relations(path)
            if rels:
                upsert_relations(rels, db_path=db_path)
                logger.debug("relations: %d edges from %s", len(rels), path)
        except Exception as e:
            logger.debug("relation extraction failed for %s: %s", path, e)

    logger.info(
        "done: %d indexed, %d skipped, %d sensitive skipped, %d errors (total %d)",
        stats["indexed"],
        stats["skipped"],
        stats["sensitive_skipped"],
        stats["errors"],
        stats["total"],
    )
    return stats


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    parser = argparse.ArgumentParser(description="Organon indexer")
    parser.add_argument(
        "--version",
        action="version",
        version=f"organon-indexer {indexer_version()}",
    )
    parser.add_argument(
        "--health",
        action="store_true",
        help="Validate that the indexer module and dependencies can be imported.",
    )
    parser.add_argument("--db", default=str(DEFAULT_DB), help="SQLite DB path")
    parser.add_argument(
        "--watch", type=int, metavar="SECONDS", help="Run continuously every N seconds"
    )
    parser.add_argument(
        "--path-prefix",
        action="append",
        dest="path_prefixes",
        help="Only index and reconcile files under this absolute workspace path. May be repeated.",
    )
    args = parser.parse_args()

    if args.health:
        print(health_line())
        return

    db_path = Path(args.db).expanduser()
    if not db_path.exists():
        logger.error("DB not found: %s (run organon-core first)", db_path)
        return

    if args.watch:
        logger.info("watch mode: every %ds. Ctrl+C to stop.", args.watch)
        while True:
            run_once(db_path, path_prefixes=args.path_prefixes)
            time.sleep(args.watch)
    else:
        run_once(db_path, path_prefixes=args.path_prefixes)


if __name__ == "__main__":
    main()
