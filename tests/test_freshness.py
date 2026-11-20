"""Tests for ai.freshness: query-time freshness of the vector index."""

import hashlib
import os

import pytest

from ai.freshness import disk_state, file_hash


def test_file_hash_matches_sha256(tmp_path):
    p = tmp_path / "a.py"
    p.write_bytes(b"abc")
    assert file_hash(str(p)) == hashlib.sha256(b"abc").hexdigest()


def test_disk_state_fresh_changed_deleted(tmp_path):
    p = tmp_path / "a.py"
    p.write_bytes(b"one")
    indexed = hashlib.sha256(b"one").hexdigest()

    assert disk_state(str(p), indexed) == "fresh"
    p.write_bytes(b"two")
    assert disk_state(str(p), indexed) == "changed"
    p.unlink()
    assert disk_state(str(p), indexed) == "deleted"


def test_disk_state_directory_and_dangling_symlink_are_deleted(tmp_path):
    as_dir = tmp_path / "now_a_dir.py"
    as_dir.mkdir()
    dangling = tmp_path / "link.py"
    dangling.symlink_to(tmp_path / "missing.py")

    assert disk_state(str(as_dir), "h") == "deleted"
    assert disk_state(str(dangling), "h") == "deleted"
    assert file_hash(str(as_dir)) is None
    assert file_hash(str(dangling)) is None


def test_disk_state_unreadable(tmp_path):
    p = tmp_path / "locked.py"
    p.write_bytes(b"x")
    p.chmod(0)
    try:
        if os.access(p, os.R_OK):
            pytest.skip("running as a user that ignores file permissions")
        assert disk_state(str(p), "h") == "unreadable"
        assert file_hash(str(p)) is None
    finally:
        p.chmod(0o644)


# ── refresh-then-search ───────────────────────────────────────────────────────

import sqlite3
from pathlib import Path

from ai.embeddings import store
from ai.freshness import search_fresh

_WORDS = ("payment", "refund", "shipping")


def fake_embed(text: str) -> list[float]:
    vec = [0.0] * store.EMBED_DIM
    lowered = text.lower()
    for i, word in enumerate(_WORDS):
        if word in lowered:
            vec[i] = 1.0
    vec[len(_WORDS)] = 0.05  # keeps every vector non-zero
    return vec


@pytest.fixture
def env(tmp_path, monkeypatch):
    """A graph DB, a vector store and a project dir, with a fake embedding."""
    monkeypatch.setattr(store, "embed_text", fake_embed)
    graph = tmp_path / "entities.db"
    conn = sqlite3.connect(str(graph))
    conn.execute(
        "CREATE TABLE entities (id TEXT, path TEXT, name TEXT, extension TEXT, "
        "size_bytes INTEGER, created_at INTEGER, modified_at INTEGER, accessed_at INTEGER, "
        "lifecycle TEXT, content_hash TEXT, summary TEXT)"
    )
    conn.execute("CREATE VIRTUAL TABLE entities_fts USING fts5(path UNINDEXED, name, content)")
    conn.commit()
    conn.close()
    proj = tmp_path / "proj"
    proj.mkdir()
    return {"graph": graph, "vec": str(tmp_path / "vec"), "proj": proj}


def _graph_upsert(graph: Path, path: str, content_hash: str) -> None:
    conn = sqlite3.connect(str(graph))
    conn.execute("DELETE FROM entities WHERE path = ?", (path,))
    conn.execute(
        "INSERT INTO entities VALUES (?, ?, ?, '.py', 0, 0, 0, 0, 'active', ?, NULL)",
        (path, path, Path(path).name, content_hash),
    )
    conn.commit()
    conn.close()


def _index(env, name: str, text: str) -> str:
    """Write a file and index it in graph, vectors and FTS, like the daemon would."""
    from ai.indexer import update_fts

    p = env["proj"] / name
    p.write_text(text)
    h = file_hash(str(p))
    _graph_upsert(env["graph"], str(p), h)
    store.index_file(str(p), text, h, db_path=env["vec"])
    update_fts(env["graph"], str(p), name, text)
    return str(p)


def _search(env, query: str, **kw) -> dict:
    return search_fresh(query, 5, env["vec"], str(env["proj"]), env["graph"], **kw)


def _fts_paths(env) -> set[str]:
    conn = sqlite3.connect(str(env["graph"]))
    paths = {r[0] for r in conn.execute("SELECT path FROM entities_fts")}
    conn.close()
    return paths


def _vector_rows(env) -> dict[str, str]:
    return {e["path"]: e["content_hash"] for e in store.get_all_entries(db_path=env["vec"])}


def test_edited_file_ranks_by_new_content(env):
    a = _index(env, "a.py", "def pay(): # payment gateway")
    Path(a).write_text("def back(): # refund flow")  # graph not updated yet

    result = _search(env, "refund")

    assert result["items"][0]["path"] == a
    assert result["refresh"]["refreshed"] == 1
    payment = _search(env, "payment")["items"]
    assert not payment or payment[0]["path"] != a or payment[0]["score"] < 0.5


def test_deleted_file_never_returned(env):
    _index(env, "a.py", "payment")
    b = _index(env, "b.py", "shipping label")
    Path(b).unlink()  # graph still lists b.py

    result = _search(env, "shipping")

    assert b not in [i["path"] for i in result["items"]]
    assert result["refresh"]["removed"] == 1
    assert b not in _fts_paths(env)
    assert b not in _vector_rows(env)


def test_graph_behind_disk_does_not_reembed_twice(env, monkeypatch):
    a = _index(env, "a.py", "payment")
    Path(a).write_text("refund")
    _search(env, "refund")  # refreshes a.py; graph still has the old hash
    calls = []
    monkeypatch.setattr(store, "embed_text", lambda t: calls.append(t) or fake_embed(t))

    result = _search(env, "refund")

    assert result["refresh"] == {"refreshed": 0, "removed": 0, "pending": 0}
    assert calls == ["refund"]


def test_budget_stops_at_50_and_reports_pending(env):
    for i in range(60):
        p = _index(env, f"f{i:02d}.py", "payment")
        Path(p).write_text(f"refund {i}")
        # graph learns the new hash, vectors still hold the old one
        _graph_upsert(env["graph"], p, file_hash(p))

    result = _search(env, "refund")

    assert result["refresh"]["refreshed"] == 50
    assert result["refresh"]["pending"] == 10


def test_zero_time_budget_refreshes_nothing_but_answers(env):
    for i in range(3):
        p = _index(env, f"f{i}.py", "payment")
        Path(p).write_text("refund")
        _graph_upsert(env["graph"], p, file_hash(p))

    result = _search(env, "payment", max_seconds=0.0)

    assert result["refresh"]["refreshed"] == 0
    assert result["refresh"]["pending"] == 3
    assert result["items"]


def test_sensitive_file_never_embedded(env):
    secret = env["proj"] / ".env"  # `*.env` patterns arrive with PR #204
    secret.write_text("payment API_KEY=zzz")
    _graph_upsert(env["graph"], str(secret), "stale-hash")

    _search(env, "payment")

    assert str(secret) not in _vector_rows(env)
    assert str(secret) not in _fts_paths(env)


def test_unreadable_file_keeps_old_vector(env):
    a = _index(env, "a.py", "payment")
    old = _vector_rows(env)[a]
    Path(a).write_text("refund")
    Path(a).chmod(0)
    try:
        if os.access(a, os.R_OK):
            pytest.skip("running as a user that ignores file permissions")
        result = _search(env, "payment")
    finally:
        Path(a).chmod(0o644)

    assert _vector_rows(env)[a] == old
    assert result["refresh"]["refreshed"] == 0


def test_search_by_path_with_graph_db_refreshes_seed(env):
    from ai.bridge import _run

    a = _index(env, "a.py", "payment")
    Path(a).write_text("refund")

    out = _run(
        {"op": "search_by_path", "path": a, "limit": 3, "db_path": env["vec"],
         "graph_db": str(env["graph"])}
    )

    assert isinstance(out, list)
    assert _vector_rows(env)[a] == file_hash(a)


def test_bridge_without_graph_db_returns_list(env):
    from ai.bridge import _run

    _index(env, "a.py", "payment")
    base = {"op": "search", "query": "payment", "limit": 3, "db_path": env["vec"]}

    assert isinstance(_run(base), list)
    out = _run({**base, "graph_db": str(env["graph"])})
    assert set(out) == {"items", "refresh"}


def test_file_without_text_is_read_once_per_version(env, monkeypatch):
    import ai.freshness as fr

    empty = env["proj"] / "__init__.py"
    empty.write_text("")
    _graph_upsert(env["graph"], str(empty), file_hash(str(empty)))
    reads = []
    real = fr.extract_text
    monkeypatch.setattr(fr, "extract_text", lambda p: reads.append(p) or real(p))

    _search(env, "payment")
    _search(env, "payment")

    assert reads == [str(empty)]


def test_file_replaced_by_symlink_is_never_read(env, tmp_path):
    outside = tmp_path / "outside.txt"
    outside.write_text("refund secrets from outside the project")
    a = _index(env, "a.py", "payment")
    Path(a).unlink()
    Path(a).symlink_to(outside)

    result = _search(env, "refund")

    assert disk_state(a, "h") == "deleted"
    assert file_hash(a) is None
    assert a not in [i["path"] for i in result["items"]]
    assert a not in _vector_rows(env)
    assert a not in _fts_paths(env)
