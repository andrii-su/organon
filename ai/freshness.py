"""Query-time freshness of the vector index.

A vector row is fresh when its ``content_hash`` equals the SHA-256 of the
file's current bytes (the same hash the Rust graph uses).
"""

import hashlib
import os
import stat
from typing import Literal

DiskState = Literal["fresh", "changed", "deleted", "unreadable"]

_CHUNK = 1 << 20


def _regular_file(path: str) -> bool:
    """A regular file at exactly ``path``, not reached through a symlink."""
    try:
        return stat.S_ISREG(os.lstat(path).st_mode)
    except OSError:
        return False


def file_hash(path: str) -> str | None:
    """SHA-256 hex of a regular file's bytes; None if missing, not a regular
    file (directories and symlinks included), or unreadable."""
    if not _regular_file(path):
        return None
    digest = hashlib.sha256()
    try:
        fd = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        with os.fdopen(fd, "rb") as f:
            while chunk := f.read(_CHUNK):
                digest.update(chunk)
    except OSError:
        return None
    return digest.hexdigest()


def disk_state(path: str, indexed_hash: str) -> DiskState:
    """Compare the file on disk with the hash its vector row was built from."""
    if not _regular_file(path):
        # Missing, a directory now, or a symlink. Symlinks are never indexed
        # (like the Rust scanner) and never read through: the target may be
        # outside the project.
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
# The query's own candidates (at most `limit` files) get a budget of their
# own, so a backlog cannot starve the edit the agent just made.
CANDIDATE_SECONDS = 0.5


@dataclass
class RefreshStats:
    refreshed: int = 0
    removed: int = 0
    # Files found changed or deleted but not re-indexed (budget or error).
    pending: int = 0
    # Suspects not even checked because time ran out.
    unchecked: int = 0
    # Set when the refresh failed as a whole; the search ran unrefreshed.
    error: str | None = None

    def to_dict(self) -> dict:
        return asdict(self)


def _in_scope(path: str, prefix: str | None) -> bool:
    return not prefix or path == prefix or path.startswith(prefix.rstrip("/") + "/")


# Files with no extractable text (empty, binary) never get a vector row;
# remember them per content hash so neither the search refresh nor the
# periodic indexer re-reads them while they are unchanged.
def ensure_no_text_table(graph_db: Path) -> None:
    with closing(sqlite3.connect(str(graph_db))) as conn, conn:
        conn.execute(
            "CREATE TABLE IF NOT EXISTS ai_no_text (path TEXT PRIMARY KEY, content_hash TEXT)"
        )


def no_text_map(graph_db: Path) -> dict[str, str]:
    ensure_no_text_table(graph_db)
    with closing(sqlite3.connect(str(graph_db))) as conn:
        return dict(conn.execute("SELECT path, content_hash FROM ai_no_text"))


def mark_no_text(graph_db: Path, path: str, content_hash: str) -> None:
    ensure_no_text_table(graph_db)
    with closing(sqlite3.connect(str(graph_db))) as conn, conn:
        conn.execute(
            "INSERT OR REPLACE INTO ai_no_text(path, content_hash) VALUES (?, ?)",
            (path, content_hash),
        )


class Refresher:
    """Brings vector and FTS rows up to date with the files on disk, on demand.

    Never writes the graph's ``entities`` table (the watcher owns it). Suspects
    share a budget of ``max_files`` re-indexed files or ``max_seconds``; the
    query's candidates get an extra ``candidate_seconds`` and enough files.
    Marking a file as having no text is not charged to the file budget.
    """

    def __init__(
        self,
        graph_db: Path,
        vectors_db: str | None,
        max_files: int = MAX_FILES,
        max_seconds: float = MAX_SECONDS,
        candidate_seconds: float = CANDIDATE_SECONDS,
    ) -> None:
        self.graph_db = Path(graph_db)
        self.vectors_db = vectors_db
        self.max_files = max_files
        self.deadline = time.monotonic() + max_seconds
        self.candidate_seconds = candidate_seconds
        self.stats = RefreshStats()
        self._applied = 0
        self._pending: set[str] = set()
        self._vectors: dict[str, str] | None = None
        ensure_no_text_table(self.graph_db)

    # ── public ────────────────────────────────────────────────────────────

    def refresh_suspects(self, path_prefix: str | None) -> None:
        """Graph ahead of vectors: entities whose hash has no vector row, and
        vector rows whose path is no longer an active entity."""
        from ai.indexer import get_entities  # local: ai.indexer imports this module

        entities = get_entities(self.graph_db, [path_prefix] if path_prefix else None)
        vectors = {p: h for p, h in self._vector_hashes().items() if _in_scope(p, path_prefix)}
        no_text = no_text_map(self.graph_db)
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
        self._apply_all(work, check_deadline=True)

    def refresh_candidates(self, rows: list[dict]) -> bool:
        """Disk ahead of graph: re-check the paths a query returned. Returns True
        if anything was applied (the caller should re-run the query)."""
        self.deadline = max(self.deadline, time.monotonic() + self.candidate_seconds)
        self.max_files = max(self.max_files, self._applied + len(rows))
        vectors = self._vector_hashes()
        work = [(r["path"], r.get("content_hash") or vectors.get(r["path"])) for r in rows]
        before = self.stats.refreshed + self.stats.removed
        self._apply_all(work, check_deadline=False)
        return self.stats.refreshed + self.stats.removed > before

    @property
    def stale_paths(self) -> set[str]:
        """Paths found changed or deleted but left unrefreshed."""
        return set(self._pending)

    # ── internals ─────────────────────────────────────────────────────────

    def _apply_all(self, work: list[tuple[str, str | None]], check_deadline: bool) -> None:
        todo = []
        for i, (path, vector_hash) in enumerate(work):
            if check_deadline and time.monotonic() >= self.deadline:
                self.stats.unchecked += len(work) - i
                break
            state = disk_state(path, vector_hash or "")
            if state in ("changed", "deleted"):
                todo.append((path, vector_hash, state))
        # Deleted first (cheap), then the newest edits: most likely the agent's own.
        todo.sort(key=lambda t: (t[2] != "deleted", -_mtime(t[0])))
        for path, vector_hash, state in todo:
            if not self._budget_left():
                self._pending.add(path)
                continue
            try:
                charged = self._apply(path, vector_hash, state)
            except Exception as e:  # one file must not fail the whole search
                logger.warning("refresh failed for %s: %s", path, e)
                self._pending.add(path)
                continue
            self._pending.discard(path)
            if charged:
                self._applied += 1
        self.stats.pending = len(self._pending)

    def _budget_left(self) -> bool:
        return self._applied < self.max_files and time.monotonic() < self.deadline

    def _apply(self, path: str, vector_hash: str | None, state: str) -> bool:
        """Re-index or remove one file. Returns False when nothing was written
        but a no-text mark (not charged to the file budget)."""
        from ai.indexer import delete_fts, update_fts

        had_row = vector_hash is not None
        if state == "changed" and not (is_sensitive(path) or is_ignored(path)):
            current = file_hash(path)
            text = extract_text(path) if current else None
            if current and text and text.strip():
                index_file(path, text, current, db_path=self.vectors_db)
                update_fts(self.graph_db, path, Path(path).name, text)
                self._vector_hashes()[path] = current
                self.stats.refreshed += 1
                return True
            if current:
                mark_no_text(self.graph_db, path, current)
                if not had_row:
                    return False
        delete_path_from_store(path, db_path=self.vectors_db)
        delete_fts(self.graph_db, path)
        self._vector_hashes().pop(path, None)
        if had_row:
            self.stats.removed += 1
        return had_row

    def _vector_hashes(self) -> dict[str, str]:
        if self._vectors is None:
            self._vectors = {
                e["path"]: e["content_hash"] for e in get_all_entries(db_path=self.vectors_db)
            }
        return self._vectors


def _mtime(path: str) -> float:
    try:
        return os.stat(path).st_mtime
    except OSError:
        return 0.0


def _finish(rows: list[dict], refresher: "Refresher | None", error: str | None) -> dict:
    stale = refresher.stale_paths if refresher else set()
    items = []
    for row in rows:
        item = {k: v for k, v in row.items() if k != "content_hash"}
        if item["path"] in stale:
            item["stale"] = True
        items.append(item)
    stats = refresher.stats if refresher else RefreshStats()
    stats.error = error
    return {"items": items, "refresh": stats.to_dict()}


def search_fresh(
    query: str,
    limit: int,
    vectors_db: str | None,
    path_prefix: str | None,
    graph_db: Path,
    max_files: int = MAX_FILES,
    max_seconds: float = MAX_SECONDS,
    candidate_seconds: float = CANDIDATE_SECONDS,
) -> dict:
    """Refresh stale rows, search, re-check the hits, and search once more if
    needed. A failing refresh degrades to a plain search with ``error`` set."""

    def run() -> list[dict]:
        return search(query, limit=limit, db_path=vectors_db, path_prefix=path_prefix)

    return _fresh(run, [], path_prefix, graph_db, vectors_db, max_files, max_seconds,
                  candidate_seconds)


def search_by_path_fresh(
    path: str,
    limit: int,
    vectors_db: str | None,
    path_prefix: str | None,
    graph_db: Path,
) -> dict:
    """Like :func:`search_fresh` for "files similar to ``path``": the seed and
    the results are re-checked against the disk."""
    from ai.embeddings.store import search_by_path

    def run() -> list[dict]:
        return search_by_path(path, limit=limit, db_path=vectors_db, path_prefix=path_prefix)

    return _fresh(run, [{"path": path}], path_prefix, graph_db, vectors_db, MAX_FILES,
                  MAX_SECONDS, CANDIDATE_SECONDS)


def _fresh(run, seeds, path_prefix, graph_db, vectors_db, max_files, max_seconds,
           candidate_seconds) -> dict:
    refresher = None
    error = None
    try:
        refresher = Refresher(graph_db, vectors_db, max_files=max_files,
                              max_seconds=max_seconds, candidate_seconds=candidate_seconds)
        refresher.refresh_suspects(path_prefix)
        if seeds:
            refresher.refresh_candidates(seeds)
    except Exception as e:
        logger.warning("index refresh failed, searching without it: %s", e)
        error = str(e)
    rows = run()
    if refresher is not None and error is None:
        try:
            if refresher.refresh_candidates(rows):
                rows = run()
        except Exception as e:
            logger.warning("index refresh of results failed: %s", e)
            error = str(e)
    return _finish(rows, refresher, error)
