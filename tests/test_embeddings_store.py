"""Unit tests for LanceDB predicate construction and model initialization."""

import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from types import SimpleNamespace

import pytest

from ai.embeddings import store


class FakeQuery:
    def __init__(self, table):
        self.table = table

    def where(self, expression):
        self.table.filters.append(expression)
        return self

    def limit(self, count):
        self.table.limit_count = count
        return self

    def to_list(self):
        return self.table.rows


class FakeTable:
    def __init__(self, rows=None):
        self.rows = rows or []
        self.filters = []
        self.deletes = []
        self.updates = []
        self.added = []
        self.limit_count = None

    def search(self, *_args, **_kwargs):
        return FakeQuery(self)

    def delete(self, expression):
        self.deletes.append(expression)

    def update(self, *, where, values):
        self.updates.append((where, values))

    def add(self, rows):
        self.added.extend(rows)


def test_where_equals_escapes_quotes_and_rejects_unknown_column():
    assert store._where_equals("path", "/tmp/a' OR path != '' --") == (
        "path = '/tmp/a'' OR path != '''' --'"
    )
    with pytest.raises(ValueError, match="unsupported filter column"):
        store._where_equals("name", "x")


def test_index_file_builds_escaped_predicates(monkeypatch):
    table = FakeTable()
    monkeypatch.setattr(store, "_get_table", lambda _db_path: table)
    monkeypatch.setattr(store, "embed_text", lambda _text: [0.0] * store.EMBED_DIM)

    path = "/tmp/a' OR path != '' --.rs"
    content_hash = "hash' OR content_hash != '' --"
    store.index_file(path, "content", content_hash, db_path="unused")

    assert table.filters == [
        f"{store._where_equals('path', path)} AND "
        f"{store._where_equals('content_hash', content_hash)}"
    ]
    assert table.deletes == [store._where_equals("path", path)]
    assert table.added[0]["path"] == path
    assert table.added[0]["content_hash"] == content_hash


def test_index_file_skips_exact_existing_path_and_hash(monkeypatch):
    table = FakeTable(rows=[{"path": "/tmp/repeat.rs", "content_hash": "same-hash"}])
    monkeypatch.setattr(store, "_get_table", lambda _db_path: table)
    monkeypatch.setattr(
        store,
        "embed_text",
        lambda _text: pytest.fail("existing index entry should not be re-embedded"),
    )

    store.index_file("/tmp/repeat.rs", "content", "same-hash", db_path="unused")

    assert table.deletes == []
    assert table.added == []


def test_search_by_path_and_update_escape_quotes(monkeypatch):
    table = FakeTable(
        rows=[
            {
                "path": "/tmp/it's indexed.rs",
                "vector": [0.0] * store.EMBED_DIM,
                "text_preview": "source",
                "_distance": 0.0,
            }
        ]
    )
    monkeypatch.setattr(store, "_get_table", lambda _db_path: table)
    path = "/tmp/it's indexed.rs"

    store.search_by_path(path, db_path="unused")
    assert table.filters == [store._where_equals("path", path)]

    assert store.update_path_in_store(path, "/tmp/new.rs", db_path="unused")
    assert table.updates == [(store._where_equals("path", path), {"path": "/tmp/new.rs"})]


def test_model_is_initialized_once_under_concurrent_calls(monkeypatch):
    constructor_calls = 0
    calls_lock = threading.Lock()
    model = object()

    def create_model(*, model_name):
        nonlocal constructor_calls
        with calls_lock:
            constructor_calls += 1
        time.sleep(0.02)
        assert model_name == "test-model"
        return model

    monkeypatch.setitem(sys.modules, "fastembed", SimpleNamespace(TextEmbedding=create_model))
    monkeypatch.setenv("ORGANON_EMBED_MODEL", "test-model")
    monkeypatch.setattr(store, "_model", None)
    monkeypatch.setattr(store, "_model_name", None)

    with ThreadPoolExecutor(max_workers=8) as executor:
        models = list(executor.map(lambda _index: store._get_model(), range(8)))

    assert constructor_calls == 1
    assert all(result is model for result in models)
