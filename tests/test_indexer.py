"""Tests for ai/indexer.py — run_once logic."""
import sqlite3
from pathlib import Path
from unittest.mock import MagicMock, call, patch

import pytest

from ai.indexer import get_entities, run_once


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

    assert stats["skipped"] >= 1
    # only utils.py should be indexed (main.py skipped)
    indexed_paths = [c.args[0] for c in mock_index.call_args_list]
    assert "/src/main.py" not in indexed_paths
    assert "/src/utils.py" in indexed_paths


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
