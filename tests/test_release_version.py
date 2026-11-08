"""Release numbering must agree with both package manifests and lock files."""

import subprocess
from pathlib import Path

import pytest

from tools.release_version import (
    apply_release_version,
    ci_state_for_commit,
    find_release_tag_for_merge,
    merged_pr_for_commit,
    next_release_version,
)


def test_next_release_uses_manifest_baseline_then_increments_patch():
    assert next_release_version("1.4.0", ["v1.3.0"]) == "1.4.0"
    assert next_release_version("1.4.0", ["v1.3.0", "v1.4.0"]) == "1.4.1"
    assert next_release_version("1.5.0", ["v1.4.9"]) == "1.5.0"
    assert next_release_version("1.4.0", ["v1.4.9", "v1.4.10"]) == "1.4.11"


def test_next_release_ignores_preview_tags_and_rejects_invalid_manifest():
    assert next_release_version("1.4.0", ["v1.4.0-rc.1", "other-tag"]) == "1.4.0"
    with pytest.raises(ValueError, match="SemVer"):
        next_release_version("1.4", [])


def test_apply_release_updates_only_local_packages(tmp_path: Path):
    (tmp_path / "Cargo.toml").write_text('[workspace.package]\nversion = "1.4.0"\n')
    (tmp_path / "pyproject.toml").write_text('[project]\nname = "organon-ai"\nversion = "1.4.0"\n')
    (tmp_path / "Cargo.lock").write_text(
        '[[package]]\nname = "other"\nversion = "1.4.0"\n'
        + "".join(
            f'[[package]]\nname = "{name}"\nversion = "1.4.0"\n'
            for name in ("organon-core", "organon-mcp", "organon-cli")
        )
    )
    (tmp_path / "uv.lock").write_text(
        '[[package]]\nname = "other"\nversion = "1.4.0"\n'
        '[[package]]\nname = "organon-ai"\nversion = "1.4.0"\n'
    )

    apply_release_version(tmp_path, "1.4.1")

    for name in ("Cargo.toml", "Cargo.lock", "pyproject.toml", "uv.lock"):
        assert 'version = "1.4.1"' in (tmp_path / name).read_text()
    assert 'name = "other"\nversion = "1.4.0"' in (tmp_path / "Cargo.lock").read_text()
    assert 'name = "other"\nversion = "1.4.0"' in (tmp_path / "uv.lock").read_text()


def test_apply_release_rejects_inconsistent_lock_without_partial_writes(tmp_path: Path):
    (tmp_path / "Cargo.toml").write_text('[workspace.package]\nversion = "1.4.0"\n')
    (tmp_path / "pyproject.toml").write_text('[project]\nversion = "1.4.0"\n')
    (tmp_path / "Cargo.lock").write_text(
        "".join(
            f'[[package]]\nname = "{name}"\nversion = "1.4.0"\n'
            for name in ("organon-core", "organon-mcp", "organon-cli")
        )
    )
    (tmp_path / "uv.lock").write_text('[[package]]\nname = "organon-ai"\nversion = "1.4.2"\n')

    with pytest.raises(ValueError, match="mismatch"):
        apply_release_version(tmp_path, "1.4.1")
    assert 'version = "1.4.0"' in (tmp_path / "Cargo.toml").read_text()


def test_merged_pr_requires_matching_merge_sha_and_main_base():
    sha = "a" * 40
    pulls = [
        {
            "number": 4,
            "merged_at": "2026-11-08T00:00:00Z",
            "merge_commit_sha": sha,
            "base": {"ref": "main"},
        },
        {"number": 5, "merged_at": None, "merge_commit_sha": sha, "base": {"ref": "main"}},
    ]
    assert merged_pr_for_commit(pulls, sha) == 4
    assert merged_pr_for_commit(pulls, "b" * 40) is None
    assert merged_pr_for_commit([{**pulls[0], "base": {"ref": "dev"}}], sha) is None


def test_ci_state_requires_successful_push_run_for_exact_merge():
    sha = "a" * 40
    runs = [
        {"head_sha": "b" * 40, "event": "push", "status": "completed", "conclusion": "success"},
        {"head_sha": sha, "event": "push", "status": "completed", "conclusion": "success"},
    ]
    assert ci_state_for_commit(runs, sha) == "success"
    assert ci_state_for_commit(runs[:1], sha) == "pending"
    assert ci_state_for_commit([{**runs[1], "conclusion": "failure"}], sha) == "failure"
    assert (
        ci_state_for_commit([{**runs[1], "status": "in_progress", "conclusion": None}], sha)
        == "pending"
    )


def test_existing_release_tag_is_found_for_merge_rerun(tmp_path: Path):
    def git(*args: str) -> str:
        result = subprocess.run(
            ["git", *args], cwd=tmp_path, capture_output=True, text=True, check=True
        )
        return result.stdout.strip()

    git("init", "-q")
    git("config", "user.name", "Test")
    git("config", "user.email", "test@example.com")
    (tmp_path / "file").write_text("merge")
    git("add", "file")
    git("commit", "-qm", "merge")
    merge_sha = git("rev-parse", "HEAD")
    git("commit", "--allow-empty", "-qm", "chore(release): v1.4.0")
    git("tag", "v1.4.0")

    assert find_release_tag_for_merge(tmp_path, merge_sha) == "v1.4.0"
    assert find_release_tag_for_merge(tmp_path, "b" * 40) is None
