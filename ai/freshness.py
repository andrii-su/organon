"""Query-time freshness of the vector index.

A vector row is fresh when its ``content_hash`` equals the SHA-256 of the
file's current bytes (the same hash the Rust graph uses).
"""

import hashlib
import os
from typing import Literal

DiskState = Literal["fresh", "changed", "deleted", "unreadable"]

_CHUNK = 1 << 20


def file_hash(path: str) -> str | None:
    """SHA-256 hex of a regular file's bytes; None if missing, not a file, or unreadable."""
    if not os.path.isfile(path):
        return None
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            while chunk := f.read(_CHUNK):
                digest.update(chunk)
    except OSError:
        return None
    return digest.hexdigest()


def disk_state(path: str, indexed_hash: str) -> DiskState:
    """Compare the file on disk with the hash its vector row was built from."""
    if not os.path.isfile(path):
        # Missing, a directory now, or a symlink whose target is gone.
        return "deleted"
    current = file_hash(path)
    if current is None:
        return "unreadable"
    return "fresh" if current == indexed_hash else "changed"


# ── refresh-then-search ───────────────────────────────────────────────────────

import logging
import sqlite3
import time
from contextlib import closing
from dataclasses import asdict, dataclass
from pathlib import Path

from ai.common.ignore import is_ignored
from ai.common.sensitive import is_sensitive
from ai.embeddings.store import (
    delete_path_from_store,
    get_all_entries,
    index_file,
    search,
)
from ai.extractor.extract import extract_text

logger = logging.getLogger(__name__)

MAX_FILES = 50
MAX_SECONDS = 1.5


@dataclass
class RefreshStats:
    refreshed: int = 0
    removed: int = 0
    pending: int = 0

    def to_dict(self) -> dict:
        return asdict(self)


def _in_scope(path: str, prefix: str | None) -> bool:
    return not prefix or path == prefix or path.startswith(prefix.rstrip("/") + "/")


class Refresher:
    """Brings vector and FTS rows up to date with the files on disk, on demand.

    Never writes the graph's ``entities`` table (the watcher owns it). Works
    within a shared budget of ``max_files`` applied changes or ``max_seconds``.
    """

    def __init__(
        self,
        graph_db: Path,
        vectors_db: str | None,
        max_files: int = MAX_FILES,
        max_seconds: float = MAX_SECONDS,
    ) -> None:
        self.graph_db = Path(graph_db)
        self.vectors_db = vectors_db
        self.max_files = max_files
        self.deadline = time.monotonic() + max_seconds
        self.stats = RefreshStats()
        self._applied = 0
        self._pending: set[str] = set()
        self._vectors: dict[str, str] | None = None
        self._ensure_no_text_table()

    # ── public ────────────────────────────────────────────────────────────

    def refresh_suspects(self, path_prefix: str | None) -> None:
        """Graph ahead of vectors: entities whose hash has no vector row, and
        vector rows whose path is no longer an active entity."""
        from ai.indexer import get_entities  # local: ai.indexer imports this module

        entities = get_entities(self.graph_db, [path_prefix] if path_prefix else None)
        vectors = {p: h for p, h in self._vector_hashes().items() if _in_scope(p, path_prefix)}
        no_text = self._no_text()
        active = {e["path"] for e in entities}
        work: list[tuple[str, str | None]] = []
        for entity in entities:
            path, graph_hash = entity["path"], entity["content_hash"]
            if graph_hash.startswith("size:"):
                continue
            vector_hash = vectors.get(path)
            if vector_hash == graph_hash or no_text.get(path) == graph_hash:
                continue
            if vector_hash is None and (is_sensitive(path) or is_ignored(path)):
                continue
            work.append((path, vector_hash))
        work.extend((path, h) for path, h in vectors.items() if path not in active)
        self._apply_all(work)

    def refresh_candidates(self, rows: list[dict]) -> bool:
        """Disk ahead of graph: re-check the paths a query returned. Returns True
        if anything was applied (the caller should re-run the query)."""
        vectors = self._vector_hashes()
        work = [(r["path"], r.get("content_hash") or vectors.get(r["path"])) for r in rows]
        before = self.stats.refreshed + self.stats.removed
        self._apply_all(work)
        return self.stats.refreshed + self.stats.removed > before

    # ── internals ─────────────────────────────────────────────────────────

    def _apply_all(self, work: list[tuple[str, str | None]]) -> None:
        todo = []
        for path, vector_hash in work:
            state = disk_state(path, vector_hash or "")
            if state in ("changed", "deleted"):
                todo.append((path, vector_hash, state))
        # Deleted first, then oldest edits first.
        todo.sort(key=lambda t: _mtime(t[0]))
        for path, vector_hash, state in todo:
            if not self._budget_left():
                self._pending.add(path)
                continue
            self._pending.discard(path)
            self._apply(path, vector_hash, state)
        self.stats.pending = len(self._pending)

    def _budget_left(self) -> bool:
        return self._applied < self.max_files and time.monotonic() < self.deadline

    def _apply(self, path: str, vector_hash: str | None, state: str) -> None:
        from ai.indexer import delete_fts, update_fts

        self._applied += 1
        had_row = vector_hash is not None
        if state == "changed" and not (is_sensitive(path) or is_ignored(path)):
            current = file_hash(path)
            text = extract_text(path) if current else None
            if current and text and text.strip():
                index_file(path, text, current, db_path=self.vectors_db)
                update_fts(self.graph_db, path, Path(path).name, text)
                self._vector_hashes()[path] = current
                self.stats.refreshed += 1
                return
            if current:
                self._mark_no_text(path, current)
        delete_path_from_store(path, db_path=self.vectors_db)
        delete_fts(self.graph_db, path)
        self._vector_hashes().pop(path, None)
        if had_row:
            self.stats.removed += 1

    def _vector_hashes(self) -> dict[str, str]:
        if self._vectors is None:
            self._vectors = {
                e["path"]: e["content_hash"] for e in get_all_entries(db_path=self.vectors_db)
            }
        return self._vectors

    # Files with no extractable text (empty, binary) never get a vector row;
    # remember them per content hash so they are not re-read on every search.
    def _ensure_no_text_table(self) -> None:
        with closing(sqlite3.connect(str(self.graph_db))) as conn, conn:
            conn.execute(
                "CREATE TABLE IF NOT EXISTS ai_no_text (path TEXT PRIMARY KEY, content_hash TEXT)"
            )

    def _no_text(self) -> dict[str, str]:
        with closing(sqlite3.connect(str(self.graph_db))) as conn:
            return dict(conn.execute("SELECT path, content_hash FROM ai_no_text"))

    def _mark_no_text(self, path: str, content_hash: str) -> None:
        with closing(sqlite3.connect(str(self.graph_db))) as conn, conn:
            conn.execute(
                "INSERT OR REPLACE INTO ai_no_text(path, content_hash) VALUES (?, ?)",
                (path, content_hash),
            )


def _mtime(path: str) -> float:
    try:
        return os.stat(path).st_mtime
    except OSError:
        return 0.0  # deleted files first


def search_fresh(
    query: str,
    limit: int,
    vectors_db: str | None,
    path_prefix: str | None,
    graph_db: Path,
    max_files: int = MAX_FILES,
    max_seconds: float = MAX_SECONDS,
) -> dict:
    """Refresh stale rows, search, re-check the hits, and search once more if needed."""
    refresher = Refresher(graph_db, vectors_db, max_files=max_files, max_seconds=max_seconds)
    refresher.refresh_suspects(path_prefix)
    rows = search(query, limit=limit, db_path=vectors_db, path_prefix=path_prefix)
    if refresher.refresh_candidates(rows):
        rows = search(query, limit=limit, db_path=vectors_db, path_prefix=path_prefix)
    items = [{k: v for k, v in row.items() if k != "content_hash"} for row in rows]
    return {"items": items, "refresh": refresher.stats.to_dict()}
