"""
Organon indexer daemon.

Reads entity graph from SQLite (maintained by organon-core),
extracts text, embeds, and stores vectors in lancedb.

Usage:
    python -m ai.indexer               # run once
    python -m ai.indexer --watch 30    # run every 30s
"""
import argparse
import logging
import os
import sqlite3
import time
from pathlib import Path

from ai.common.ignore import is_ignored
from ai.embeddings.store import get_all_entries, get_indexed_hashes, index_file, update_path_in_store
from ai.extractor.extract import extract_text
from ai.relations.extract import extract_relations
from ai.relations.store import delete_relations_from, upsert_relations

logger = logging.getLogger(__name__)

DEFAULT_DB = Path("~/.organon/entities.db").expanduser()
DEFAULT_OLLAMA_MODEL = os.environ.get("ORGANON_OLLAMA_MODEL", "llama3.2")


def _truthy(value: str | None) -> bool:
    return (value or "").strip().lower() in {"1", "true", "yes", "on"}


def get_entities(db_path: Path) -> list[dict]:
    """Fetch all non-dead entities with a content_hash from SQLite."""
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT path, name, content_hash, lifecycle, summary FROM entities "
            "WHERE content_hash IS NOT NULL AND lifecycle != 'dead'"
        ).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


def get_fts_paths(db_path: Path) -> set[str]:
    with sqlite3.connect(str(db_path)) as conn:
        conn.row_factory = sqlite3.Row
        conn.execute(
            "CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5("
            "path UNINDEXED, name, content)"
        )
        rows = conn.execute("SELECT path FROM entities_fts").fetchall()
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


def update_summary(db_path: Path, path: str, summary: str) -> None:
    with sqlite3.connect(str(db_path)) as conn:
        conn.execute(
            "UPDATE entities SET summary = ? WHERE path = ?",
            (summary[:1000], path),
        )


def summarize_text(text: str, model: str = DEFAULT_OLLAMA_MODEL) -> str | None:
    try:
        import ollama
    except ImportError:
        logger.warning("ollama package not installed — file summaries unavailable")
        return None

    prompt = (
        "Summarize this file in 1-2 concise sentences. Focus on what it does and "
        "what kind of content it contains. Do not use bullet points.\n\n"
        f"{text[:6000]}"
    )
    try:
        response = ollama.chat(
            model=model,
            messages=[{"role": "user", "content": prompt}],
            options={"temperature": 0},
        )
        summary = response["message"]["content"].strip()
        return summary or None
    except Exception as e:
        logger.warning("summary generation failed: %s", e)
        return None


def summarize_file(db_path: Path, path: str, model: str | None = None) -> str | None:
    """Recompute summary for one file and persist it in SQLite."""
    text = extract_text(path)
    if not text or not text.strip():
        logger.debug("summary skipped (no text): %s", path)
        return None

    summary = summarize_text(text, model=model or DEFAULT_OLLAMA_MODEL)
    if not summary:
        return None

    update_summary(db_path, path, summary)
    return summary


def reconcile_lancedb_paths(
    db_path: Path,
    vectors_db_path: str | None = None,
) -> int:
    """Fix stale paths in lancedb after file renames.

    For each lancedb entry whose `path` is no longer present in SQLite,
    look for a SQLite entity with the same `content_hash`.  If exactly one
    such entity exists the lancedb row's path is updated in-place — no
    re-embedding needed.

    Returns the number of entries reconciled.
    """
    sqlite_entities = get_entities(db_path)
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
        if ldb_path in sqlite_path_set:
            continue  # still valid
        candidates = hash_to_paths.get(ldb_hash, [])
        if len(candidates) == 1:
            new_path = candidates[0]
            logger.info("reconcile lancedb path: %s → %s", ldb_path, new_path)
            if update_path_in_store(ldb_path, new_path, db_path=vectors_db_path):
                reconciled += 1
        elif len(candidates) == 0:
            logger.debug("reconcile: no sqlite entity for lancedb entry %s — leaving stale", ldb_path)
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
    summarize: bool | None = None,
    ollama_model: str | None = None,
    vectors_db_path: str | None = None,
) -> dict:
    """Index all entities not yet in vector store. Returns stats dict."""
    summarize = _truthy(os.environ.get("ORGANON_SUMMARIZE")) if summarize is None else summarize
    ollama_model = ollama_model or DEFAULT_OLLAMA_MODEL

    # Reconcile stale paths in lancedb (e.g. from rename/move operations).
    try:
        reconcile_lancedb_paths(db_path, vectors_db_path=vectors_db_path)
    except Exception as e:
        logger.warning("path reconciliation failed (non-fatal): %s", e)

    entities = get_entities(db_path)
    if not entities:
        logger.info("no entities in graph")
        return {"total": 0, "indexed": 0, "skipped": 0, "errors": 0}

    logger.info("indexing %d entities from graph", len(entities))
    indexed_hashes = get_indexed_hashes(db_path=vectors_db_path)
    fts_paths = get_fts_paths(db_path)
    stats = {"total": len(entities), "indexed": 0, "skipped": 0, "errors": 0}

    for entity in entities:
        path = entity["path"]
        name = entity["name"]
        content_hash = entity["content_hash"]
        summary = entity["summary"]
        needs_vector = content_hash not in indexed_hashes
        needs_summary = summarize and (needs_vector or not summary)
        needs_fts = path not in fts_paths

        if is_ignored(path):
            logger.debug("skipped (ignored path): %s", path)
            stats["skipped"] += 1
            continue

        if not (needs_vector or needs_summary or needs_fts):
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

        if needs_summary:
            generated_summary = summarize_text(text, model=ollama_model)
            if generated_summary:
                try:
                    update_summary(db_path, path, generated_summary)
                except Exception as e:
                    logger.warning("error storing summary for %s: %s", path, e)

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
        "done: %d indexed, %d skipped, %d errors (total %d)",
        stats["indexed"], stats["skipped"], stats["errors"], stats["total"],
    )
    return stats


def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    parser = argparse.ArgumentParser(description="Organon indexer")
    parser.add_argument("--db", default=str(DEFAULT_DB), help="SQLite DB path")
    parser.add_argument("--watch", type=int, metavar="SECONDS",
                        help="Run continuously every N seconds")
    args = parser.parse_args()

    db_path = Path(args.db).expanduser()
    if not db_path.exists():
        logger.error("DB not found: %s (run organon-core first)", db_path)
        return

    if args.watch:
        logger.info("watch mode: every %ds. Ctrl+C to stop.", args.watch)
        while True:
            run_once(db_path)
            time.sleep(args.watch)
    else:
        run_once(db_path)


if __name__ == "__main__":
    main()
