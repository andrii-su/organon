"""Tests for ai.common.sensitive — sensitive file detection."""
import os
import pytest

from ai.common.sensitive import is_sensitive, sensitive_reason


# ── filename patterns ─────────────────────────────────────────────────────────

@pytest.mark.parametrize("path", [
    "/home/user/project/.env",
    "/home/user/project/.env.production",
    "/home/user/project/.env.local",
    "/home/user/project/secrets.json",
    "/home/user/project/secrets.yaml",
    "/home/user/project/secrets.yml",
    "/home/user/project/secrets.toml",
    "/home/user/project/secret.json",
    "/home/user/project/server.key",
    "/home/user/project/cert.pem",
    "/home/user/project/keystore.jks",
    "/home/user/project/keystore.p12",
    "/home/user/project/keystore.pfx",
    "/home/user/project/keystore.pkcs12",
    "/home/user/project/cert.cer",
    "/home/user/project/backup.gpg",
    "/home/user/project/message.asc",
    "/home/user/project/id_rsa",
    "/home/user/project/id_rsa.pub",
    "/home/user/project/id_ed25519",
    "/home/user/project/id_ed25519.pub",
    "/home/user/project/id_ecdsa",
    "/home/user/project/id_dsa",
    "/home/user/project/.netrc",
    "/home/user/project/credentials",
    "/home/user/project/api.token",
    "/home/user/project/deploy.secret",
    "/home/user/project/htpasswd",
    "/home/user/project/.htpasswd",
])
def test_sensitive_filename_patterns(path):
    assert is_sensitive(path), f"expected {path!r} to be sensitive"


# ── directory patterns ────────────────────────────────────────────────────────

@pytest.mark.parametrize("path", [
    "/home/user/.ssh/known_hosts",
    "/home/user/.ssh/authorized_keys",
    "/home/user/.gnupg/pubring.kbx",
    "/home/user/.aws/credentials",
    "/home/user/.aws/config",
    "/home/user/project/secrets/db_password.txt",
])
def test_sensitive_directory_patterns(path):
    assert is_sensitive(path), f"expected {path!r} to be sensitive"


# ── non-sensitive paths ───────────────────────────────────────────────────────

@pytest.mark.parametrize("path", [
    "/home/user/project/src/main.rs",
    "/home/user/project/README.md",
    "/home/user/project/Cargo.toml",
    "/home/user/project/config.json",
    "/home/user/project/public.key.md",   # markdown, not a key file
    "/home/user/project/environment.py",  # contains "env" but isn't .env
    "/home/user/project/secrets_manager.py",  # code file, not a secrets file
])
def test_non_sensitive_paths(path):
    assert not is_sensitive(path), f"expected {path!r} to NOT be sensitive"


# ── sensitive_reason ─────────────────────────────────────────────────────────

def test_sensitive_reason_filename():
    reason = sensitive_reason("/home/user/project/.env")
    assert reason is not None
    assert "sensitive pattern" in reason
    assert ".env" in reason


def test_sensitive_reason_directory():
    reason = sensitive_reason("/home/user/.ssh/id_rsa")
    assert reason is not None
    assert "sensitive directory" in reason
    assert ".ssh" in reason


def test_sensitive_reason_non_sensitive_returns_none():
    assert sensitive_reason("/home/user/project/main.py") is None


# ── ORGANON_SENSITIVE_EXTRA env var ──────────────────────────────────────────

def test_extra_patterns_env_var(monkeypatch):
    monkeypatch.setenv("ORGANON_SENSITIVE_EXTRA", "vault.json:*.mysecret")
    assert is_sensitive("/home/user/project/vault.json")
    assert is_sensitive("/home/user/project/deploy.mysecret")
    # Unrelated file still not sensitive
    assert not is_sensitive("/home/user/project/main.py")


def test_extra_patterns_empty_env_var(monkeypatch):
    monkeypatch.setenv("ORGANON_SENSITIVE_EXTRA", "")
    # Should not crash, normal file should still not be sensitive
    assert not is_sensitive("/home/user/project/main.py")
