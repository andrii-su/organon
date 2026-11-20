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
