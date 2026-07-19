"""Sensitive file detection — skips secrets and key material before indexing.

Patterns are checked against the filename and directory components.
Extra patterns can be added via the ORGANON_SENSITIVE_EXTRA env var
(colon-separated glob patterns, e.g. "vault.json:*.secret").
"""

import fnmatch
import os
from pathlib import Path

# Filename globs that indicate secret / key material.
DEFAULT_SENSITIVE_PATTERNS: tuple[str, ...] = (
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "*.jks",
    "*.keystore",
    "*.pkcs12",
    "*.cer",
    "*.asc",  # GPG armored
    "*.gpg",  # GPG binary
    "id_rsa",
    "id_rsa.*",
    "id_ed25519",
    "id_ed25519.*",
    "id_ecdsa",
    "id_ecdsa.*",
    "id_dsa",
    "id_dsa.*",
    ".netrc",
    "credentials",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secrets.toml",
    "secret.json",
    "secret.yaml",
    "secret.yml",
    "secret.toml",
    "*.token",
    "*.secret",
    "htpasswd",
    ".htpasswd",
)

# Directory components that indicate sensitive areas.
SENSITIVE_DIRS: frozenset[str] = frozenset(
    {
        ".ssh",
        ".gnupg",
        ".aws",
        ".config/gcloud",
        "secrets",  # common convention
    }
)


def _extra_patterns() -> tuple[str, ...]:
    """Return additional patterns from ORGANON_SENSITIVE_EXTRA (colon-separated)."""
    raw = os.environ.get("ORGANON_SENSITIVE_EXTRA", "")
    return tuple(p.strip() for p in raw.split(":") if p.strip()) if raw.strip() else ()


def _dir_components(p: Path) -> list[str]:
    """Return the lowercased directory components of ``p`` (excluding filename)."""
    return [part.lower() for part in p.parts[:-1]]


def _matched_sensitive_dir(dir_parts: list[str]) -> str | None:
    """Return the SENSITIVE_DIRS entry that matches, or None.

    Handles both single-component entries (e.g. ``.ssh``) and multi-component
    entries (e.g. ``.config/gcloud``), matching the latter as a consecutive
    subsequence of path components. All comparison is case-insensitive so that
    e.g. ``.SSH`` on a case-insensitive filesystem is still caught.
    """
    for sd in SENSITIVE_DIRS:
        needle = [c.lower() for c in Path(sd).parts]
        if len(needle) == 1:
            if needle[0] in dir_parts:
                return sd
        else:
            # consecutive subsequence match
            n = len(needle)
            for i in range(len(dir_parts) - n + 1):
                if dir_parts[i : i + n] == needle:
                    return sd
    return None


def _matched_pattern(name: str) -> str | None:
    """Return the first sensitive filename glob that matches ``name`` (case-insensitive)."""
    lname = name.lower()
    all_patterns = DEFAULT_SENSITIVE_PATTERNS + _extra_patterns()
    for pattern in all_patterns:
        if fnmatch.fnmatch(lname, pattern.lower()):
            return pattern
    return None


def _canonical(path: str | Path) -> Path:
    """Resolve symlinks so a benign-named link into e.g. ``.ssh`` cannot bypass
    the directory check. Falls back to the raw path if resolution fails."""
    p = Path(path)
    try:
        return p.resolve()
    except OSError:
        return p


def is_sensitive(path: str | Path) -> bool:
    """Return True if the file looks like it contains secrets or key material.

    Checks (case-insensitive, symlinks resolved):
      1. Any parent directory component matches SENSITIVE_DIRS.
      2. The filename matches DEFAULT_SENSITIVE_PATTERNS or ORGANON_SENSITIVE_EXTRA.
    """
    return sensitive_reason(path) is not None


def sensitive_reason(path: str | Path) -> str | None:
    """Return a human-readable reason why the file is sensitive, or None."""
    # Check both the literal path and its symlink-resolved form: the literal
    # path catches sensitive names, the resolved path catches links into
    # sensitive directories.
    for candidate in (Path(path), _canonical(path)):
        dir_parts = _dir_components(candidate)
        if (sd := _matched_sensitive_dir(dir_parts)) is not None:
            return f"in sensitive directory: {sd}"
        if (pattern := _matched_pattern(candidate.name)) is not None:
            return f"matches sensitive pattern: {pattern}"
    return None
