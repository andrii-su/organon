"""Default storage paths, resolved at call time.

Same rules as the Rust side (`organon_core::config`): `ORGANON_HOME` moves
every default path, and `ORGANON_DB` / `ORGANON_VECTORS_DB` override the DB
and the vector store directly. Empty values are ignored, `~` expands to the
home directory, and relative paths are made absolute.
"""

import os
from pathlib import Path


def _env_path(key: str) -> Path | None:
    raw = os.environ.get(key)
    if not raw:
        return None
    return Path(os.path.abspath(Path(raw).expanduser()))


def organon_home() -> Path:
    """`$ORGANON_HOME`, else `~/.organon`."""
    return _env_path("ORGANON_HOME") or Path.home() / ".organon"


def default_db() -> Path:
    """SQLite graph DB: `$ORGANON_DB`, else `<organon home>/entities.db`."""
    return _env_path("ORGANON_DB") or organon_home() / "entities.db"


def default_vectors() -> str:
    """LanceDB directory: `$ORGANON_VECTORS_DB`, else `<organon home>/vectors`."""
    return str(_env_path("ORGANON_VECTORS_DB") or organon_home() / "vectors")
