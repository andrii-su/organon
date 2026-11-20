"""Query-time freshness of the vector index.

A vector row is fresh when its ``content_hash`` equals the SHA-256 of the
file's current bytes (the same hash the Rust graph uses).
"""

import hashlib
import os
from typing import Literal

DiskState = Literal["fresh", "changed", "deleted", "unreadable"]

_CHUNK = 1 << 20


def file_hash(path: str) -> str | None:
    """SHA-256 hex of a regular file's bytes; None if missing, not a file, or unreadable."""
    if not os.path.isfile(path):
        return None
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as f:
            while chunk := f.read(_CHUNK):
                digest.update(chunk)
    except OSError:
        return None
    return digest.hexdigest()


def disk_state(path: str, indexed_hash: str) -> DiskState:
    """Compare the file on disk with the hash its vector row was built from."""
    if not os.path.isfile(path):
        # Missing, a directory now, or a symlink whose target is gone.
        return "deleted"
    current = file_hash(path)
    if current is None:
        return "unreadable"
    return "fresh" if current == indexed_hash else "changed"
