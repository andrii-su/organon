"""Exercise the release coordinator against a local remote and stubbed services."""

import os
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]


def git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    ).stdout.strip()


@pytest.fixture
def release_repo(tmp_path: Path) -> tuple[Path, Path, str]:
    remote = tmp_path / "remote.git"
    root = tmp_path / "checkout"
    subprocess.run(["git", "init", "--bare", "-q", str(remote)], check=True)
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "user.name", "Test")
    git(root, "config", "user.email", "test@example.com")
    git(root, "remote", "add", "origin", str(remote))
    (root / "tools").mkdir()
    shutil.copy(ROOT / "tools/release_version.py", root / "tools/release_version.py")
    shutil.copy(ROOT / "tools/release_merge.sh", root / "tools/release_merge.sh")
    (root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.4.0"\n')
    (root / "pyproject.toml").write_text('[project]\nversion = "1.4.0"\n')
    (root / "Cargo.lock").write_text(
        "".join(
            f'[[package]]\nname = "{name}"\nversion = "1.4.0"\n'
            for name in ("organon-core", "organon-mcp", "organon-cli")
        )
    )
    (root / "uv.lock").write_text('[[package]]\nname = "organon-ai"\nversion = "1.4.0"\n')
    git(root, "add", ".")
    git(root, "commit", "-qm", "baseline")
    git(root, "tag", "v1.3.0")
    (root / "merged.txt").write_text("merged PR")
    git(root, "add", "merged.txt")
    git(root, "commit", "-qm", "merge PR")
    sha = git(root, "rev-parse", "HEAD")
    git(root, "push", "-q", "origin", "HEAD:main", "v1.3.0")
    return root, tmp_path, sha


def stub_tools(tmp_path: Path) -> Path:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    gh = bin_dir / "gh"
    gh.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, sys\n"
        "args = sys.argv[1:]\n"
        "if args[0] == 'api' and '/pulls' in args[1]:\n"
        "  print(json.dumps([{'number': 7, 'merged_at': '2026-11-08T00:00:00Z', "
        "'merge_commit_sha': os.environ['GITHUB_SHA'], 'base': {'ref': 'main'}}] "
        "if os.environ.get('FAKE_PR', 'yes') == 'yes' else []))\n"
        "elif args[0] == 'api' and '/runs?' in args[1]:\n"
        "  print(json.dumps({'workflow_runs': [{'head_sha': os.environ['GITHUB_SHA'], "
        "'event': 'push', 'status': 'completed', "
        "'conclusion': os.environ.get('FAKE_SECURITY' if '/security.yml/' in args[1] "
        "else 'FAKE_CI', 'success')}] }))\n"
        "elif args[:2] == ['workflow', 'run']:\n"
        "  with open(os.environ['FAKE_DISPATCH_LOG'], 'a') as f: "
        "f.write(' '.join(args) + '\\n')\n"
        "else: sys.exit(2)\n"
    )
    for name in ("cargo", "uv"):
        target = bin_dir / name
        target.write_text("#!/bin/sh\nexit 0\n")
        target.chmod(0o755)
    gh.chmod(0o755)
    return bin_dir


def run_release(
    root: Path, tmp_path: Path, sha: str, **overrides: str
) -> subprocess.CompletedProcess[str]:
    bin_dir = tmp_path / "bin"
    if not bin_dir.exists():
        stub_tools(tmp_path)
    env = {
        **os.environ,
        "PATH": f"{bin_dir}:{os.environ['PATH']}",
        "GITHUB_SHA": sha,
        "GITHUB_REPOSITORY": "example/organon",
        "GH_TOKEN": "test-token",
        "FAKE_DISPATCH_LOG": str(tmp_path / "dispatch.log"),
        **overrides,
    }
    return subprocess.run(
        ["bash", "tools/release_merge.sh"],
        check=False,
        cwd=root,
        env=env,
        capture_output=True,
        text=True,
    )


def test_release_merge_creates_one_tag_and_rerun_reuses_it(release_repo):
    root, tmp_path, sha = release_repo
    first = run_release(root, tmp_path, sha)
    assert first.returncode == 0, first.stderr
    assert git(root, "tag", "--list", "v*").splitlines() == ["v1.3.0", "v1.4.0"]
    assert git(root, "rev-parse", "v1.4.0^{commit}^") == sha
    second = run_release(root, tmp_path, sha)
    assert second.returncode == 0, second.stderr
    assert git(root, "tag", "--list", "v*").splitlines() == ["v1.3.0", "v1.4.0"]
    assert (tmp_path / "dispatch.log").read_text().splitlines() == [
        "workflow run release.yml --ref v1.4.0",
        "workflow run docker.yml --ref v1.4.0",
        "workflow run release.yml --ref v1.4.0",
        "workflow run docker.yml --ref v1.4.0",
    ]


def test_release_merge_skips_non_pr_and_failed_ci(release_repo):
    root, tmp_path, sha = release_repo
    skipped = run_release(root, tmp_path, sha, FAKE_PR="no")
    assert skipped.returncode == 0, skipped.stderr
    failed = run_release(root, tmp_path, sha, FAKE_CI="failure")
    assert failed.returncode != 0
    assert git(root, "tag", "--list", "v*") == "v1.3.0"
    assert not (tmp_path / "dispatch.log").exists()


def test_release_merge_blocks_on_failed_security_scan(release_repo):
    root, tmp_path, sha = release_repo
    failed = run_release(root, tmp_path, sha, FAKE_SECURITY="failure")
    assert failed.returncode != 0
    assert git(root, "tag", "--list", "v*") == "v1.3.0"


def test_next_merge_increments_patch_from_previous_tag(release_repo):
    root, tmp_path, first_sha = release_repo
    first = run_release(root, tmp_path, first_sha)
    assert first.returncode == 0, first.stderr

    git(root, "checkout", "--detach", first_sha)
    (root / "next.txt").write_text("second merged PR")
    git(root, "add", "next.txt")
    git(root, "commit", "-qm", "second merge")
    second_sha = git(root, "rev-parse", "HEAD")
    git(root, "push", "-q", "origin", "HEAD:main")

    second = run_release(root, tmp_path, second_sha)
    assert second.returncode == 0, second.stderr
    assert git(root, "tag", "--list", "v*").splitlines() == [
        "v1.3.0",
        "v1.4.0",
        "v1.4.1",
    ]
    assert git(root, "rev-parse", "v1.4.1^{commit}^") == second_sha
    for name in ("Cargo.toml", "Cargo.lock", "pyproject.toml", "uv.lock"):
        assert 'version = "1.4.1"' in git(root, "show", f"v1.4.1:{name}")
