"""Tests for ai/query/nl_query.py (mocks ollama)."""
import sqlite3
from pathlib import Path
from unittest.mock import patch

import pytest

from ai.query.nl_query import (
    _extract_sql,
    _validate_sql,
    generate_sql,
    run_nl_query,
)


# ── _validate_sql ─────────────────────────────────────────────────────────────

def test_validate_accepts_select():
    assert _validate_sql("SELECT * FROM entities") is True


def test_validate_accepts_select_with_joins():
    sql = "SELECT e.path, r.to_path FROM entities e JOIN relationships r ON e.path = r.from_path"
    assert _validate_sql(sql) is True


def test_validate_rejects_drop():
    assert _validate_sql("DROP TABLE entities") is False


def test_validate_rejects_delete():
    assert _validate_sql("DELETE FROM entities") is False


def test_validate_rejects_update():
    assert _validate_sql("UPDATE entities SET lifecycle = 'dead'") is False


def test_validate_rejects_insert():
    assert _validate_sql("INSERT INTO entities VALUES (1)") is False


def test_validate_rejects_non_select():
    assert _validate_sql("PRAGMA table_info(entities)") is False


# ── _extract_sql ──────────────────────────────────────────────────────────────

def test_extract_sql_from_fence():
    text = "Here is the query:\n```sql\nSELECT path FROM entities LIMIT 5\n```"
    result = _extract_sql(text)
    assert result == "SELECT path FROM entities LIMIT 5"


def test_extract_sql_from_plain_fence():
    text = "```\nSELECT * FROM entities WHERE lifecycle = 'dormant'\n```"
    result = _extract_sql(text)
    assert result is not None
    assert result.startswith("SELECT")


def test_extract_sql_bare_select():
    text = "The answer is: SELECT path FROM entities ORDER BY size_bytes DESC LIMIT 10"
    result = _extract_sql(text)
    assert result is not None
    assert "SELECT" in result


def test_extract_sql_rejects_fenced_drop():
    text = "```sql\nDROP TABLE entities\n```"
    result = _extract_sql(text)
    assert result is None


def test_extract_sql_none_when_no_match():
    result = _extract_sql("I cannot answer that question.")
    assert result is None


# ── run_nl_query ──────────────────────────────────────────────────────────────

@pytest.fixture
def tmp_db(tmp_path):
    db = tmp_path / "entities.db"
    conn = sqlite3.connect(str(db))
    conn.execute("""
        CREATE TABLE entities (
            id TEXT PRIMARY KEY, path TEXT, name TEXT, extension TEXT,
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
    conn.execute(
        "INSERT INTO entities VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        ("1", "/src/main.py", "main.py", ".py", 1024, 0, 0, 0, "active", None, None),
    )
    conn.commit()
    conn.close()
    return db


def test_run_nl_query_fallback_when_ollama_absent(tmp_db):
    """When generate_sql returns None, fall back to default SELECT."""
    with patch("ai.query.nl_query.generate_sql", return_value=None):
        result = run_nl_query("show everything", db_path=tmp_db)

    assert result["mode"] == "fallback"
    assert isinstance(result["results"], list)
    assert "sql" in result


def test_run_nl_query_with_generated_sql(tmp_db):
    """When generate_sql returns valid SQL, use it."""
    sql = "SELECT path, lifecycle FROM entities WHERE lifecycle = 'active'"
    with patch("ai.query.nl_query.generate_sql", return_value=sql):
        result = run_nl_query("active files", db_path=tmp_db)

    assert result["mode"] == "generated"
    assert result["sql"] == sql
    assert len(result["results"]) == 1
    assert result["results"][0]["path"] == "/src/main.py"


def test_run_nl_query_handles_bad_sql(tmp_db):
    """When generated SQL causes an error, return empty results with error key."""
    bad_sql = "SELECT nonexistent_column FROM entities"
    with patch("ai.query.nl_query.generate_sql", return_value=bad_sql):
        result = run_nl_query("bad query", db_path=tmp_db)

    assert "error" in result
    assert result["results"] == []


def test_run_nl_query_returns_all_keys(tmp_db):
    with patch("ai.query.nl_query.generate_sql", return_value=None):
        result = run_nl_query("list files", db_path=tmp_db)

    assert "results" in result
    assert "sql"     in result
    assert "mode"    in result
