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
import sqlite3
import time
from pathlib import Path

from ai.common.ignore import is_ignored
from ai.embeddings.store import get_indexed_hashes, index_file
from ai.extractor.extract import extract_text
from ai.relations.extract import extract_relations
from ai.relations.store import upsert_relations

logger = logging.getLogger(__name__)

DEFAULT_DB = Path("~/.organon/entities.db").expanduser()


def get_entities(db_path: Path) -> list[dict]:
    """Fetch all non-dead entities with a content_hash from SQLite."""
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        rows = conn.execute(
            "SELECT path, content_hash, lifecycle FROM entities "
            "WHERE content_hash IS NOT NULL AND lifecycle != 'dead'"
        ).fetchall()
        return [dict(r) for r in rows]
    finally:
        conn.close()


def run_once(db_path: Path) -> dict:
    """Index all entities not yet in vector store. Returns stats dict."""
    entities = get_entities(db_path)
    if not entities:
        logger.info("no entities in graph")
        return {"total": 0, "indexed": 0, "skipped": 0, "errors": 0}

    logger.info("indexing %d entities from graph", len(entities))
    indexed_hashes = get_indexed_hashes()
    stats = {"total": len(entities), "indexed": 0, "skipped": 0, "errors": 0}

    for entity in entities:
        path = entity["path"]
        content_hash = entity["content_hash"]

        if is_ignored(path):
            logger.debug("skipped (ignored path): %s", path)
            stats["skipped"] += 1
            continue

        if content_hash in indexed_hashes:
            logger.debug("skipped (already indexed): %s", path)
            stats["skipped"] += 1
            continue

        text = extract_text(path)
        if not text or not text.strip():
            logger.debug("skipped (no text): %s", path)
            stats["skipped"] += 1
            continue

        try:
            index_file(path, text, content_hash)
            indexed_hashes.add(content_hash)
            stats["indexed"] += 1
            logger.info("indexed: %s", path)
        except Exception as e:
            logger.warning("error indexing %s: %s", path, e)
            stats["errors"] += 1
            continue

        # Extract and store explicit import/reference relations
        try:
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
