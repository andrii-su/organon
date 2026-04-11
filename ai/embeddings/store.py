"""Local vector store using fastembed + lancedb."""
import logging
import os
import time
from pathlib import Path
from typing import Any

import pyarrow as pa

logger = logging.getLogger(__name__)

DEFAULT_DB_PATH = "~/.organon/vectors"
TABLE_NAME = "entities"

# BAAI/bge-small-en-v1.5 — 384 dims, ~130MB, fast on CPU
DEFAULT_EMBED_MODEL = "BAAI/bge-small-en-v1.5"
EMBED_DIM = 384

_model = None  # lazy singleton
_model_name = None


def _get_model():
    global _model, _model_name
    model_name = os.environ.get("ORGANON_EMBED_MODEL", DEFAULT_EMBED_MODEL)
    if _model is None or _model_name != model_name:
        logger.info("loading embedding model: %s", model_name)
        from fastembed import TextEmbedding
        _model = TextEmbedding(model_name=model_name)
        _model_name = model_name
        logger.info("embedding model loaded")
    return _model


def _resolve_db_path(db_path: str | None = None) -> str:
    return db_path or os.environ.get("ORGANON_VECTORS_DB", DEFAULT_DB_PATH)


def _get_table(db_path: str | None = None):
    import lancedb

    path = Path(_resolve_db_path(db_path)).expanduser()
    path.mkdir(parents=True, exist_ok=True)
    db = lancedb.connect(str(path))

    try:
        return db.open_table(TABLE_NAME)
    except Exception:
        logger.info("creating lancedb table '%s' at %s", TABLE_NAME, path)
        schema = pa.schema([
            pa.field("path",         pa.utf8()),
            pa.field("content_hash", pa.utf8()),
            pa.field("text_preview", pa.utf8()),
            pa.field("vector",       pa.list_(pa.float32(), EMBED_DIM)),
            pa.field("indexed_at",   pa.int64()),
        ])
        return db.create_table(TABLE_NAME, schema=schema)


def embed_text(text: str) -> list[float]:
    model = _get_model()
    embeddings = list(model.embed([text]))
    return embeddings[0].tolist()


def index_file(path: str, text: str, content_hash: str, db_path: str | None = None) -> None:
    """Embed and store a file. Skips if content_hash already indexed."""
    table = _get_table(db_path)

    # Skip if already indexed with same hash
    try:
        existing = table.search().where(
            f"content_hash = '{content_hash}'"
        ).limit(1).to_list()
        if existing:
            logger.debug("index_file: already indexed [%s]: %s", content_hash[:8], path)
            return
    except Exception as e:
        logger.debug("index_file: hash check failed: %s", e)

    # Remove old entry for this path (hash changed)
    try:
        table.delete(f"path = '{path}'")
        logger.debug("index_file: removed stale entry: %s", path)
    except Exception as e:
        logger.debug("index_file: delete failed (ok if new): %s", e)

    vector = embed_text(text)
    preview = text[:500].replace("\n", " ")

    table.add([{
        "path":         path,
        "content_hash": content_hash,
        "text_preview": preview,
        "vector":       vector,
        "indexed_at":   int(time.time()),
    }])
    logger.debug("index_file: indexed [%s]: %s", content_hash[:8], path)


def search(
    query: str,
    limit: int = 10,
    db_path: str | None = None,
    path_prefix: str | None = None,
) -> list[dict[str, Any]]:
    """Semantic search. Returns list of {path, score, text_preview}.

    Args:
        query: Natural language query.
        limit: Max results returned.
        db_path: Vector store path.
        path_prefix: If set, only return files whose path starts with this prefix.
    """
    logger.debug("search: %r limit=%d prefix=%r", query, limit, path_prefix)
    table = _get_table(db_path)
    vector = embed_text(query)

    fetch_limit = limit * 4 if path_prefix else limit
    results = table.search(vector).limit(fetch_limit).to_list()

    if path_prefix:
        results = [r for r in results if r["path"].startswith(path_prefix)]
        logger.debug("search: %d results after prefix filter", len(results))

    results = results[:limit]
    logger.debug("search: %d results", len(results))

    return [
        {
            "path":         r["path"],
            "score":        float(1 - r.get("_distance", 0)),
            "text_preview": r["text_preview"],
        }
        for r in results
    ]


def get_indexed_hashes(db_path: str | None = None) -> set[str]:
    """Return all content_hashes currently in the vector store."""
    try:
        table = _get_table(db_path)
        rows = table.search().select(["content_hash"]).limit(100_000).to_list()
        hashes = {r["content_hash"] for r in rows}
        logger.debug("get_indexed_hashes: %d hashes", len(hashes))
        return hashes
    except Exception as e:
        logger.warning("get_indexed_hashes failed: %s", e)
        return set()


def get_all_entries(db_path: str | None = None) -> list[dict]:
    """Return all (path, content_hash) pairs stored in the vector store."""
    try:
        table = _get_table(db_path)
        rows = table.search().select(["path", "content_hash"]).limit(100_000).to_list()
        return [{"path": r["path"], "content_hash": r["content_hash"]} for r in rows]
    except Exception as e:
        logger.warning("get_all_entries failed: %s", e)
        return []


def update_path_in_store(old_path: str, new_path: str, db_path: str | None = None) -> bool:
    """Update the path field for a lancedb row whose path == old_path.

    Returns True if the update succeeded, False otherwise.
    """
    try:
        table = _get_table(db_path)
        # Escape single quotes in path strings to avoid injection in the SQL fragment.
        old_escaped = old_path.replace("'", "''")
        table.update(where=f"path = '{old_escaped}'", values={"path": new_path})
        logger.debug("update_path_in_store: %s → %s", old_path, new_path)
        return True
    except Exception as e:
        logger.warning("update_path_in_store failed %s → %s: %s", old_path, new_path, e)
        return False
