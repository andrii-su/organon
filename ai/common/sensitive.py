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
    "*.asc",        # GPG armored
    "*.gpg",        # GPG binary
    "id_rsa", "id_rsa.*",
    "id_ed25519", "id_ed25519.*",
    "id_ecdsa", "id_ecdsa.*",
    "id_dsa", "id_dsa.*",
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
SENSITIVE_DIRS: frozenset[str] = frozenset({
    ".ssh",
    ".gnupg",
    ".aws",
    ".config/gcloud",
    "secrets",       # common convention
})


def _extra_patterns() -> tuple[str, ...]:
    """Return additional patterns from ORGANON_SENSITIVE_EXTRA (colon-separated)."""
    raw = os.environ.get("ORGANON_SENSITIVE_EXTRA", "")
    return tuple(p.strip() for p in raw.split(":") if p.strip()) if raw.strip() else ()


def is_sensitive(path: str | Path) -> bool:
    """Return True if the file looks like it contains secrets or key material.

    Checks:
      1. Any parent directory component matches SENSITIVE_DIRS.
      2. The filename matches DEFAULT_SENSITIVE_PATTERNS or ORGANON_SENSITIVE_EXTRA.
    """
    p = Path(path)
    name = p.name
    parts = {part for part in p.parts[:-1]}  # directory components only

    # Check directory components
    for sd in SENSITIVE_DIRS:
        if sd in parts:
            return True

    # Check filename against all patterns
    all_patterns = DEFAULT_SENSITIVE_PATTERNS + _extra_patterns()
    return any(fnmatch.fnmatch(name, pattern) for pattern in all_patterns)


def sensitive_reason(path: str | Path) -> str | None:
    """Return a human-readable reason why the file is sensitive, or None."""
    p = Path(path)
    name = p.name
    parts = {part for part in p.parts[:-1]}

    for sd in SENSITIVE_DIRS:
        if sd in parts:
            return f"in sensitive directory: {sd}"

    all_patterns = DEFAULT_SENSITIVE_PATTERNS + _extra_patterns()
    for pattern in all_patterns:
        if fnmatch.fnmatch(name, pattern):
            return f"matches sensitive pattern: {pattern}"

    return None
