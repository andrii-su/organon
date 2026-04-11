"""
Single source of truth for ignored path segments.
Mirrors IGNORED_SEGMENTS in crates/organon-core/src/scanner.rs — keep in sync.
"""
from pathlib import Path

IGNORED_SEGMENTS: frozenset[str] = frozenset({
    ".git", ".hg", ".svn",
    "node_modules", "target", ".venv", "__pycache__",
    ".pytest_cache", ".mypy_cache", ".ruff_cache",
    ".DS_Store", "dist", "build", ".next", ".nuxt",
})


def is_ignored(path: str | Path) -> bool:
    """Return True if any path component is in IGNORED_SEGMENTS."""
    return any(part in IGNORED_SEGMENTS for part in Path(path).parts)
