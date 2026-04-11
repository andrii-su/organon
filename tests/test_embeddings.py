"""Tests for embeddings store."""
import tempfile
import time

import pytest

from ai.embeddings.store import embed_text, index_file, search, get_indexed_hashes, EMBED_DIM


# ── embed_text ────────────────────────────────────────────────────────────────

def test_embed_returns_vector():
    vec = embed_text("hello world")
    assert isinstance(vec, list)
    assert len(vec) == EMBED_DIM


def test_embed_returns_floats():
    vec = embed_text("test content")
    assert all(isinstance(v, float) for v in vec)


def test_different_texts_produce_different_vectors():
    v1 = embed_text("Rust programming language systems")
    v2 = embed_text("Python data science machine learning")
    assert v1 != v2


def test_same_text_produces_same_vector():
    text = "organon semantic filesystem"
    v1 = embed_text(text)
    v2 = embed_text(text)
    assert v1 == v2


# ── index_file + search ────────────────────────────────────────────────────────

@pytest.fixture
def tmp_db(tmp_path):
    return str(tmp_path / "vectors")


def test_index_and_search(tmp_db):
    index_file("/tmp/rust.rs",   "Rust entity graph SQLite",  "hash_rust",   db_path=tmp_db)
    index_file("/tmp/python.py", "Python embeddings lancedb", "hash_python", db_path=tmp_db)

    results = search("Rust SQLite database", limit=2, db_path=tmp_db)
    assert len(results) > 0
    paths = [r["path"] for r in results]
    assert "/tmp/rust.rs" in paths


def test_search_ranks_by_relevance(tmp_db):
    index_file("/tmp/lifecycle.rs", "lifecycle born active dormant archived dead state machine", "h1", db_path=tmp_db)
    index_file("/tmp/watcher.rs",   "file system watcher notify events create modify delete",   "h2", db_path=tmp_db)

    results = search("dormant archived lifecycle transitions", limit=2, db_path=tmp_db)
    assert results[0]["path"] == "/tmp/lifecycle.rs"


def test_index_skips_duplicate_hash(tmp_db):
    index_file("/tmp/a.txt", "hello world", "same_hash", db_path=tmp_db)
    index_file("/tmp/b.txt", "hello world", "same_hash", db_path=tmp_db)

    hashes = get_indexed_hashes(db_path=tmp_db)
    # same hash → only one entry
    assert hashes == {"same_hash"}


def test_index_replaces_on_path_change(tmp_db):
    index_file("/tmp/old_path.rs", "content here", "hash_v1", db_path=tmp_db)
    index_file("/tmp/old_path.rs", "updated content", "hash_v2", db_path=tmp_db)

    hashes = get_indexed_hashes(db_path=tmp_db)
    assert "hash_v2" in hashes
    assert "hash_v1" not in hashes


def test_search_result_schema(tmp_db):
    index_file("/tmp/schema_test.py", "test content for schema", "hash_schema", db_path=tmp_db)
    results = search("test content", limit=1, db_path=tmp_db)

    assert len(results) == 1
    r = results[0]
    assert "path" in r
    assert "score" in r
    assert "text_preview" in r
    assert 0.0 <= r["score"] <= 1.0


def test_get_indexed_hashes_empty(tmp_db):
    hashes = get_indexed_hashes(db_path=tmp_db)
    assert hashes == set()


# ── path_prefix filter ────────────────────────────────────────────────────────

def test_search_with_prefix_filters(tmp_db):
    index_file("/src/core/graph.rs",   "SQLite graph entity upsert",    "h_core",   db_path=tmp_db)
    index_file("/src/ai/indexer.py",   "Python indexer embeddings",     "h_ai",     db_path=tmp_db)
    index_file("/src/core/scanner.rs", "file scanner walkdir parallel", "h_scanner",db_path=tmp_db)

    results = search("graph entity SQLite", limit=10, db_path=tmp_db, path_prefix="/src/core/")
    paths = [r["path"] for r in results]
    assert all(p.startswith("/src/core/") for p in paths)
    assert "/src/ai/indexer.py" not in paths


def test_search_with_nonmatching_prefix_returns_empty(tmp_db):
    index_file("/src/core/graph.rs", "SQLite graph entity", "h_graph", db_path=tmp_db)

    results = search("graph entity", limit=10, db_path=tmp_db, path_prefix="/nonexistent/dir/")
    assert results == []
