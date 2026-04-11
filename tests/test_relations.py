"""Tests for ai/relations/ — extraction and graph store."""
import sqlite3
import tempfile
from pathlib import Path

import pytest

from ai.relations.extract import extract_relations, _extract_python, _extract_rust, _extract_ts
from ai.relations.store import get_graph, get_relations, upsert_relations


# ── helpers ───────────────────────────────────────────────────────────────────

def _make_db(tmp_path: Path) -> Path:
    """Create a minimal entities.db with relationships table."""
    db = tmp_path / "entities.db"
    conn = sqlite3.connect(str(db))
    conn.execute("""
        CREATE TABLE relationships (
            from_path TEXT NOT NULL,
            to_path   TEXT NOT NULL,
            kind      TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (from_path, to_path, kind)
        )
    """)
    conn.commit()
    conn.close()
    return db


# ── extraction tests ──────────────────────────────────────────────────────────

def test_extract_python_import(tmp_path):
    # Create a real sibling module
    target = tmp_path / "utils.py"
    target.write_text("# utils")

    src = tmp_path / "main.py"
    src.write_text("from utils import helper\n")

    rels = extract_relations(str(src))
    to_paths = [r[1] for r in rels]
    assert str(target.resolve()) in to_paths
    assert all(r[2] == "imports" for r in rels)


def test_extract_rs_mod(tmp_path):
    # Create sibling scanner.rs
    scanner = tmp_path / "scanner.rs"
    scanner.write_text("// scanner")

    src = tmp_path / "lib.rs"
    src.write_text("mod scanner;\n")

    rels = extract_relations(str(src))
    to_paths = [r[1] for r in rels]
    assert str(scanner.resolve()) in to_paths
    assert all(r[2] == "mod" for r in rels)


def test_extract_rs_mod_subdir(tmp_path):
    # mod foo; → foo/mod.rs
    subdir = tmp_path / "foo"
    subdir.mkdir()
    mod_rs = subdir / "mod.rs"
    mod_rs.write_text("// foo mod")

    src = tmp_path / "lib.rs"
    src.write_text("mod foo;\n")

    rels = extract_relations(str(src))
    to_paths = [r[1] for r in rels]
    assert str(mod_rs.resolve()) in to_paths


def test_extract_ts_relative_import(tmp_path):
    target = tmp_path / "utils.ts"
    target.write_text("export const x = 1;")

    src = tmp_path / "index.ts"
    src.write_text('import { x } from "./utils";\n')

    rels = extract_relations(str(src))
    to_paths = [r[1] for r in rels]
    assert str(target.resolve()) in to_paths


def test_extract_python_relative_same_package(tmp_path):
    # from . import utils  →  sibling utils.py
    utils = tmp_path / "utils.py"
    utils.write_text("# utils")

    src = tmp_path / "main.py"
    src.write_text("from . import utils\n")

    rels = extract_relations(str(src))
    # relative import "from . import utils" — utils.py in same dir
    to_paths = [r[1] for r in rels]
    assert str(utils.resolve()) in to_paths


def test_extract_python_relative_parent(tmp_path):
    # from ..common import foo  →  ../common/foo.py
    common = tmp_path / "common"
    common.mkdir()
    foo = common / "foo.py"
    foo.write_text("# foo")

    subpkg = tmp_path / "sub"
    subpkg.mkdir()
    src = subpkg / "main.py"
    src.write_text("from ..common import foo\n")

    rels = extract_relations(str(src))
    to_paths = [r[1] for r in rels]
    assert str(foo.resolve()) in to_paths


def test_extract_unresolvable_skipped(tmp_path):
    src = tmp_path / "main.py"
    src.write_text("from nonexistent_package import something\n")

    rels = extract_relations(str(src))
    # nonexistent_package can't be resolved → should be empty
    assert rels == []


def test_extract_ts_skips_node_modules(tmp_path):
    src = tmp_path / "app.ts"
    src.write_text('import React from "react";\n')

    rels = extract_relations(str(src))
    # "react" is not relative → skipped
    assert rels == []


def test_extract_unsupported_extension(tmp_path):
    src = tmp_path / "config.yaml"
    src.write_text("key: value\n")
    rels = extract_relations(str(src))
    assert rels == []


# ── store tests ───────────────────────────────────────────────────────────────

def test_upsert_and_get_relations(tmp_path):
    db = _make_db(tmp_path)
    rels = [("/a.py", "/b.py", "imports"), ("/a.py", "/c.py", "imports")]
    upsert_relations(rels, db_path=db)

    result = get_relations("/a.py", db_path=db)
    assert len(result) == 2
    froms = {r["from"] for r in result}
    tos   = {r["to"]   for r in result}
    assert froms == {"/a.py"}
    assert tos   == {"/b.py", "/c.py"}


def test_upsert_idempotent(tmp_path):
    db = _make_db(tmp_path)
    rel = [("/x.rs", "/y.rs", "mod")]
    upsert_relations(rel, db_path=db)
    upsert_relations(rel, db_path=db)  # duplicate — should be ignored

    result = get_relations("/x.rs", db_path=db)
    assert len(result) == 1


def test_get_graph_single_node(tmp_path):
    db = _make_db(tmp_path)
    result = get_graph("/isolated.py", depth=1, db_path=db)
    assert result["nodes"] == ["/isolated.py"]
    assert result["edges"] == []


def test_get_graph_depth_bfs(tmp_path):
    db = _make_db(tmp_path)
    # a → b → c chain
    upsert_relations([
        ("/a.py", "/b.py", "imports"),
        ("/b.py", "/c.py", "imports"),
    ], db_path=db)

    # depth=1: only a + b
    g1 = get_graph("/a.py", depth=1, db_path=db)
    assert "/a.py" in g1["nodes"]
    assert "/b.py" in g1["nodes"]
    assert "/c.py" not in g1["nodes"]

    # depth=2: all three
    g2 = get_graph("/a.py", depth=2, db_path=db)
    assert "/c.py" in g2["nodes"]


def test_get_relations_target(tmp_path):
    """get_relations also returns edges where path is the *target*."""
    db = _make_db(tmp_path)
    upsert_relations([("/x.py", "/y.py", "imports")], db_path=db)

    result = get_relations("/y.py", db_path=db)
    assert len(result) == 1
    assert result[0]["from"] == "/x.py"
    assert result[0]["to"]   == "/y.py"
