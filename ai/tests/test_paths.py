"""Tests for ai.common.paths — ORGANON_HOME isolation."""

from pathlib import Path

from ai.common.paths import default_db, default_vectors, organon_home


def test_defaults_live_under_dot_organon(monkeypatch, tmp_path):
    for var in ("ORGANON_HOME", "ORGANON_DB", "ORGANON_VECTORS_DB"):
        monkeypatch.delenv(var, raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))

    assert organon_home() == tmp_path / ".organon"
    assert default_db() == tmp_path / ".organon" / "entities.db"
    assert default_vectors() == str(tmp_path / ".organon" / "vectors")


def test_organon_home_moves_db_and_vectors(monkeypatch, tmp_path):
    monkeypatch.delenv("ORGANON_DB", raising=False)
    monkeypatch.delenv("ORGANON_VECTORS_DB", raising=False)
    monkeypatch.setenv("ORGANON_HOME", str(tmp_path / "sandbox"))

    assert default_db() == tmp_path / "sandbox" / "entities.db"
    assert default_vectors() == str(tmp_path / "sandbox" / "vectors")


def test_explicit_paths_win(monkeypatch, tmp_path):
    monkeypatch.setenv("ORGANON_HOME", str(tmp_path / "sandbox"))
    monkeypatch.setenv("ORGANON_DB", "/x/e.db")
    monkeypatch.setenv("ORGANON_VECTORS_DB", "/x/vec")

    assert default_db() == Path("/x/e.db")
    assert default_vectors() == "/x/vec"


def test_relations_store_uses_organon_home(monkeypatch, tmp_path):
    from ai.relations import store

    monkeypatch.delenv("ORGANON_DB", raising=False)
    monkeypatch.setenv("ORGANON_HOME", str(tmp_path))
    assert store._resolve(None) == tmp_path / "entities.db"


def test_vector_store_uses_organon_home(monkeypatch, tmp_path):
    from ai.embeddings import store

    monkeypatch.delenv("ORGANON_VECTORS_DB", raising=False)
    monkeypatch.setenv("ORGANON_HOME", str(tmp_path))
    assert store._resolve_db_path() == str(tmp_path / "vectors")


def test_empty_values_are_ignored(monkeypatch, tmp_path):
    monkeypatch.setenv("HOME", str(tmp_path))
    for var in ("ORGANON_HOME", "ORGANON_DB", "ORGANON_VECTORS_DB"):
        monkeypatch.setenv(var, "")

    assert default_db() == tmp_path / ".organon" / "entities.db"
    assert default_vectors() == str(tmp_path / ".organon" / "vectors")


def test_tilde_and_relative_values(monkeypatch, tmp_path):
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv("ORGANON_HOME", "~/sandbox")
    monkeypatch.setenv("ORGANON_DB", "rel/e.db")
    monkeypatch.delenv("ORGANON_VECTORS_DB", raising=False)

    assert organon_home() == tmp_path / "sandbox"
    assert default_db() == tmp_path / "rel" / "e.db"
    assert default_vectors() == str(tmp_path / "sandbox" / "vectors")
