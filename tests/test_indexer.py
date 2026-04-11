"""Tests for ai/indexer.py — run_once logic."""
import sqlite3
from pathlib import Path
from unittest.mock import MagicMock, call, patch

import pytest

from ai.indexer import get_entities, reconcile_lancedb_paths, run_once, summarize_file


# ── fixtures ──────────────────────────────────────────────────────────────────

@pytest.fixture
def tmp_db(tmp_path):
    """Minimal entities.db with a few rows."""
    db = tmp_path / "entities.db"
    conn = sqlite3.connect(str(db))
    conn.execute("""
        CREATE TABLE entities (
            id TEXT, path TEXT, name TEXT, extension TEXT,
            size_bytes INTEGER, created_at INTEGER, modified_at INTEGER,
            accessed_at INTEGER, lifecycle TEXT, content_hash TEXT, summary TEXT
        )
    """)
    conn.execute("""
        CREATE TABLE relationships (
            from_path TEXT, to_path TEXT, kind TEXT, created_at INTEGER,
            PRIMARY KEY (from_path, to_path, kind)
        )
    """)
    conn.executemany(
        "INSERT INTO entities VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        [
            ("1", "/src/main.py",    "main.py",    ".py", 100, 0, 0, 0, "active",   "hash_main",   None),
            ("2", "/src/utils.py",   "utils.py",   ".py", 200, 0, 0, 0, "active",   "hash_utils",  None),
            ("3", "/src/dead.py",    "dead.py",    ".py",  50, 0, 0, 0, "dead",     "hash_dead",   None),
            ("4", "/src/nohash.py",  "nohash.py",  ".py",  10, 0, 0, 0, "active",   None,          None),
        ],
    )
    conn.commit()
    conn.close()
    return db


# ── get_entities ──────────────────────────────────────────────────────────────

def test_get_entities_excludes_dead(tmp_db):
    entities = get_entities(tmp_db)
    paths = [e["path"] for e in entities]
    assert "/src/dead.py" not in paths


def test_get_entities_excludes_no_hash(tmp_db):
    entities = get_entities(tmp_db)
    paths = [e["path"] for e in entities]
    assert "/src/nohash.py" not in paths


def test_get_entities_returns_active(tmp_db):
    entities = get_entities(tmp_db)
    paths = [e["path"] for e in entities]
    assert "/src/main.py" in paths
    assert "/src/utils.py" in paths


# ── run_once ──────────────────────────────────────────────────────────────────

def test_run_once_indexes_new_files(tmp_db, tmp_path):
    vec_db = str(tmp_path / "vectors")
    with (
        patch("ai.indexer.extract_text", return_value="some content"),
        patch("ai.indexer.index_file") as mock_index,
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    assert stats["indexed"] == 2  # main.py + utils.py (dead and nohash excluded)
    assert mock_index.call_count == 2


def test_run_once_skips_already_indexed(tmp_db):
    already = {"hash_main"}  # main.py already in vector store
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file") as mock_index,
        patch("ai.indexer.get_indexed_hashes", return_value=already),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    # only utils.py should be indexed (main.py skipped)
    indexed_paths = [c.args[0] for c in mock_index.call_args_list]
    assert "/src/main.py" not in indexed_paths
    assert "/src/utils.py" in indexed_paths
    assert stats["indexed"] == 1


def test_run_once_skips_no_text(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value=None),
        patch("ai.indexer.index_file") as mock_index,
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    assert stats["indexed"] == 0
    assert mock_index.call_count == 0
    assert stats["skipped"] == 2


def test_run_once_skips_empty_text(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="   \n  "),
        patch("ai.indexer.index_file") as mock_index,
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    assert mock_index.call_count == 0


def test_run_once_counts_errors(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file", side_effect=RuntimeError("embed failed")),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    assert stats["errors"] == 2
    assert stats["indexed"] == 0


def test_run_once_extracts_relations(tmp_db):
    fake_rels = [("/src/main.py", "/src/utils.py", "imports")]
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=fake_rels),
        patch("ai.indexer.upsert_relations") as mock_upsert,
    ):
        run_once(tmp_db)

    assert mock_upsert.called
    # relations should be stored with the db_path
    call_args = mock_upsert.call_args
    assert call_args.kwargs["db_path"] == tmp_db


def test_run_once_replaces_stale_outgoing_relations(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[("/src/main.py", "/src/utils.py", "imports")]),
    ):
        run_once(tmp_db)

    conn = sqlite3.connect(str(tmp_db))
    conn.execute(
        "UPDATE entities SET content_hash = ? WHERE path = ?",
        ("hash_main_v2", "/src/main.py"),
    )
    conn.commit()
    conn.close()

    with (
        patch("ai.indexer.extract_text", return_value="content changed"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value={"hash_main", "hash_utils"}),
        patch("ai.indexer.extract_relations", return_value=[("/src/main.py", "/src/other.py", "imports")]),
    ):
        run_once(tmp_db)

    conn = sqlite3.connect(str(tmp_db))
    rows = conn.execute(
        "SELECT from_path, to_path, kind FROM relationships ORDER BY from_path, to_path"
    ).fetchall()
    conn.close()
    assert rows == [("/src/main.py", "/src/other.py", "imports")]


def test_run_once_returns_all_stat_keys(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="x"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    assert {"total", "indexed", "skipped", "errors"} == set(stats.keys())


def test_run_once_updates_fts(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="Rust graph and SQLite index"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        run_once(tmp_db)

    conn = sqlite3.connect(str(tmp_db))
    rows = conn.execute("SELECT path FROM entities_fts ORDER BY path").fetchall()
    conn.close()
    assert [r[0] for r in rows] == ["/src/main.py", "/src/utils.py"]


def test_run_once_backfills_fts_when_vectors_already_exist(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file") as mock_index,
        patch("ai.indexer.get_indexed_hashes", return_value={"hash_main", "hash_utils"}),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        stats = run_once(tmp_db)

    conn = sqlite3.connect(str(tmp_db))
    count = conn.execute("SELECT COUNT(*) FROM entities_fts").fetchone()[0]
    conn.close()
    assert count == 2
    assert mock_index.call_count == 0
    assert stats["indexed"] == 0


def test_run_once_stores_summary_when_enabled(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="content"),
        patch("ai.indexer.index_file"),
        patch("ai.indexer.get_indexed_hashes", return_value=set()),
        patch("ai.indexer.summarize_text", return_value="Short summary"),
        patch("ai.indexer.extract_relations", return_value=[]),
        patch("ai.indexer.upsert_relations"),
    ):
        run_once(tmp_db, summarize=True, ollama_model="test-model")

    conn = sqlite3.connect(str(tmp_db))
    summaries = conn.execute(
        "SELECT DISTINCT summary FROM entities "
        "WHERE lifecycle = 'active' AND content_hash IS NOT NULL"
    ).fetchall()
    conn.close()
    assert summaries == [("Short summary",)]


def test_summarize_file_recomputes_and_stores_summary(tmp_db):
    with (
        patch("ai.indexer.extract_text", return_value="fresh content"),
        patch("ai.indexer.summarize_text", return_value="Fresh summary"),
    ):
        summary = summarize_file(tmp_db, "/src/main.py", model="test-model")

    conn = sqlite3.connect(str(tmp_db))
    stored = conn.execute(
        "SELECT summary FROM entities WHERE path = ?",
        ("/src/main.py",),
    ).fetchone()[0]
    conn.close()

    assert summary == "Fresh summary"
    assert stored == "Fresh summary"


# ── reconcile_lancedb_paths ───────────────────────────────────────────────────

def test_reconcile_updates_stale_path(tmp_db):
    """Lancedb entry whose path no longer exists in SQLite but hash matches a
    new SQLite path should have its path updated."""
    # main.py was renamed to main_v2.py in SQLite (simulate by changing DB)
    conn = sqlite3.connect(str(tmp_db))
    conn.execute(
        "UPDATE entities SET path = ?, name = ? WHERE path = ?",
        ("/src/main_v2.py", "main_v2.py", "/src/main.py"),
    )
    conn.commit()
    conn.close()

    # lancedb still has the old path
    stale_entries = [{"path": "/src/main.py", "content_hash": "hash_main"}]

    with (
        patch("ai.indexer.get_all_entries", return_value=stale_entries),
        patch("ai.indexer.update_path_in_store", return_value=True) as mock_update,
    ):
        count = reconcile_lancedb_paths(tmp_db)

    assert count == 1
    mock_update.assert_called_once_with("/src/main.py", "/src/main_v2.py", db_path=None)


def test_reconcile_skips_valid_paths(tmp_db):
    """Lancedb entries whose path is still present in SQLite are not touched."""
    ldb_entries = [
        {"path": "/src/main.py",  "content_hash": "hash_main"},
        {"path": "/src/utils.py", "content_hash": "hash_utils"},
    ]
    with (
        patch("ai.indexer.get_all_entries", return_value=ldb_entries),
        patch("ai.indexer.update_path_in_store") as mock_update,
    ):
        count = reconcile_lancedb_paths(tmp_db)

    assert count == 0
    mock_update.assert_not_called()


def test_reconcile_skips_ambiguous_hash(tmp_db):
    """If multiple SQLite entities share the same hash as a stale lancedb entry,
    the entry is NOT updated (ambiguous)."""
    # Give main.py and utils.py the same hash
    conn = sqlite3.connect(str(tmp_db))
    conn.execute("UPDATE entities SET content_hash = ? WHERE path = ?", ("same", "/src/utils.py"))
    conn.execute("UPDATE entities SET content_hash = ? WHERE path = ?", ("same", "/src/main.py"))
    conn.commit()
    conn.close()

    ldb_entries = [{"path": "/src/old.py", "content_hash": "same"}]
    with (
        patch("ai.indexer.get_all_entries", return_value=ldb_entries),
        patch("ai.indexer.update_path_in_store") as mock_update,
    ):
        count = reconcile_lancedb_paths(tmp_db)

    assert count == 0
    mock_update.assert_not_called()


def test_reconcile_skips_when_no_sqlite_match(tmp_db):
    """Stale lancedb path with no hash match in SQLite → not updated (orphan)."""
    ldb_entries = [{"path": "/old/orphan.py", "content_hash": "unknown_hash"}]
    with (
        patch("ai.indexer.get_all_entries", return_value=ldb_entries),
        patch("ai.indexer.update_path_in_store") as mock_update,
    ):
        count = reconcile_lancedb_paths(tmp_db)

    assert count == 0
    mock_update.assert_not_called()


def test_reconcile_is_nonfatal_on_error(tmp_db):
    """If reconciliation fails for a single entry, the rest still proceed."""
    conn = sqlite3.connect(str(tmp_db))
    conn.execute("UPDATE entities SET path = ? WHERE path = ?", ("/src/a_v2.py", "/src/main.py"))
    conn.execute("UPDATE entities SET path = ? WHERE path = ?", ("/src/b_v2.py", "/src/utils.py"))
    conn.commit()
    conn.close()

    ldb_entries = [
        {"path": "/src/main.py",  "content_hash": "hash_main"},
        {"path": "/src/utils.py", "content_hash": "hash_utils"},
    ]

    results = iter([False, True])
    with (
        patch("ai.indexer.get_all_entries", return_value=ldb_entries),
        patch("ai.indexer.update_path_in_store", side_effect=results),
    ):
        count = reconcile_lancedb_paths(tmp_db)

    # Only 1 succeeded (False=fail, True=success)
    assert count == 1


def test_run_once_empty_db(tmp_path):
    db = tmp_path / "empty.db"
    conn = sqlite3.connect(str(db))
    conn.execute("""
        CREATE TABLE entities (
            id TEXT, path TEXT, name TEXT, extension TEXT,
            size_bytes INTEGER, created_at INTEGER, modified_at INTEGER,
            accessed_at INTEGER, lifecycle TEXT, content_hash TEXT, summary TEXT
        )
    """)
    conn.commit()
    conn.close()

    stats = run_once(db)
    assert stats["total"] == 0
    assert stats["indexed"] == 0
