"""Tests for ai.relations.store — relationship CRUD + graph BFS.

Also guards against the connection-leak regression: `_db` must close the
connection it opens (sqlite3's own context manager commits but never closes).
"""

import sqlite3
from pathlib import Path

import pytest

from ai.relations import store


@pytest.fixture
def db(tmp_path: Path) -> Path:
    """A fresh entities.db with the relationships table organon-core defines."""
    p = tmp_path / "entities.db"
    conn = sqlite3.connect(str(p))
    conn.execute(
        "CREATE TABLE relationships ("
        " from_path TEXT NOT NULL,"
        " to_path   TEXT NOT NULL,"
        " kind      TEXT NOT NULL,"
        " created_at INTEGER NOT NULL,"
        " PRIMARY KEY (from_path, to_path, kind))"
    )
    conn.commit()
    conn.close()
    return p


def test_upsert_and_get_relations(db: Path):
    store.upsert_relations([("a.py", "b.py", "imports")], db_path=db)
    edges = store.get_relations("a.py", db_path=db)
    assert edges == [{"from": "a.py", "to": "b.py", "kind": "imports"}]
    # target side is also matched
    assert store.get_relations("b.py", db_path=db) == edges


def test_upsert_is_idempotent(db: Path):
    store.upsert_relations([("a.py", "b.py", "imports")], db_path=db)
    store.upsert_relations([("a.py", "b.py", "imports")], db_path=db)
    assert len(store.get_relations("a.py", db_path=db)) == 1


def test_delete_relations_from(db: Path):
    store.upsert_relations([("a.py", "b.py", "imports")], db_path=db)
    removed = store.delete_relations_from("a.py", db_path=db)
    assert removed == 1
    assert store.get_relations("a.py", db_path=db) == []


def test_get_graph_bfs(db: Path):
    store.upsert_relations(
        [("a.py", "b.py", "imports"), ("b.py", "c.py", "imports")],
        db_path=db,
    )
    g1 = store.get_graph("a.py", depth=1, db_path=db)
    assert "a.py" in g1["nodes"] and "b.py" in g1["nodes"]
    assert "c.py" not in g1["nodes"]  # depth 1 stops before c

    g2 = store.get_graph("a.py", depth=2, db_path=db)
    assert "c.py" in g2["nodes"]  # depth 2 reaches c


def test_db_context_manager_closes_connection(db: Path):
    with store._db(db) as conn:
        conn.execute("SELECT 1")
    # After the with-block the connection must be closed: using it raises.
    with pytest.raises(sqlite3.ProgrammingError):
        conn.execute("SELECT 1")


def test_db_commits_writes(db: Path):
    with store._db(db) as conn:
        conn.execute(
            "INSERT INTO relationships VALUES (?, ?, ?, ?)",
            ("x.py", "y.py", "imports", 0),
        )
    # A fresh read sees the committed row.
    assert store.get_relations("x.py", db_path=db)
