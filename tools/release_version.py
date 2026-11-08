"""Choose a SemVer patch release and synchronize tagged source versions."""

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

SEMVER = re.compile(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
LOCAL_CARGO_PACKAGES = ("organon-core", "organon-mcp", "organon-cli")


def _parse_version(value: str) -> tuple[int, int, int]:
    match = SEMVER.fullmatch(value)
    if match is None:
        raise ValueError(f"expected a stable SemVer version, got {value!r}")
    return tuple(map(int, match.groups()))


def next_release_version(manifest_version: str, tags: list[str]) -> str:
    """Respect an intentional manifest minor bump, otherwise increment the latest patch."""
    baseline = _parse_version(manifest_version)
    stable = [
        _parse_version(tag[1:]) for tag in tags if tag.startswith("v") and SEMVER.fullmatch(tag[1:])
    ]
    latest = max(stable, default=None)
    if latest is None or baseline > latest:
        chosen = baseline
    else:
        chosen = (latest[0], latest[1], latest[2] + 1)
    return ".".join(map(str, chosen))


def _package_versions(data: dict, names: tuple[str, ...]) -> dict[str, str]:
    found = {}
    for package in data.get("package", []):
        name = package.get("name")
        if name in names:
            if name in found:
                raise ValueError(f"duplicate package {name}")
            found[name] = package["version"]
    if set(found) != set(names):
        raise ValueError(f"missing local packages: {sorted(set(names) - set(found))}")
    return found


def _read_versions(contents: dict[str, str]) -> str:
    cargo = tomllib.loads(contents["Cargo.toml"])
    python = tomllib.loads(contents["pyproject.toml"])
    cargo_lock = tomllib.loads(contents["Cargo.lock"])
    uv_lock = tomllib.loads(contents["uv.lock"])
    versions = {
        "Cargo.toml": cargo["workspace"]["package"]["version"],
        "pyproject.toml": python["project"]["version"],
        **_package_versions(cargo_lock, LOCAL_CARGO_PACKAGES),
        **_package_versions(uv_lock, ("organon-ai",)),
    }
    if len(set(versions.values())) != 1:
        raise ValueError(f"local version mismatch: {versions}")
    version = next(iter(versions.values()))
    _parse_version(version)
    return version


def _replace_section_version(text: str, section: str, old: str, new: str) -> str:
    pattern = re.compile(
        rf'(^\[{re.escape(section)}\]\n(?:(?!^\[).)*?^version = "){re.escape(old)}(")',
        re.MULTILINE | re.DOTALL,
    )
    result, count = pattern.subn(lambda match: f"{match.group(1)}{new}{match.group(2)}", text)
    if count != 1:
        raise ValueError(f"cannot update [{section}] version")
    return result


def _replace_package_version(text: str, name: str, old: str, new: str) -> str:
    pattern = re.compile(
        rf'(^name = "{re.escape(name)}"\nversion = "){re.escape(old)}(")', re.MULTILINE
    )
    result, count = pattern.subn(lambda match: f"{match.group(1)}{new}{match.group(2)}", text)
    if count != 1:
        raise ValueError(f"cannot update {name} lock version")
    return result


def apply_release_version(root: Path, version: str) -> None:
    """Validate all current versions before writing any release file."""
    target = _parse_version(version)
    names = ("Cargo.toml", "Cargo.lock", "pyproject.toml", "uv.lock")
    contents = {name: (root / name).read_text() for name in names}
    current = _read_versions(contents)
    if target < _parse_version(current):
        raise ValueError(f"release version {version} is older than manifest {current}")
    updated = {
        "Cargo.toml": _replace_section_version(
            contents["Cargo.toml"], "workspace.package", current, version
        ),
        "pyproject.toml": _replace_section_version(
            contents["pyproject.toml"], "project", current, version
        ),
        "Cargo.lock": contents["Cargo.lock"],
        "uv.lock": _replace_package_version(contents["uv.lock"], "organon-ai", current, version),
    }
    for name in LOCAL_CARGO_PACKAGES:
        updated["Cargo.lock"] = _replace_package_version(
            updated["Cargo.lock"], name, current, version
        )
    if _read_versions(updated) != version:
        raise ValueError("release files do not agree after update")
    for name in names:
        (root / name).write_text(updated[name])


def merged_pr_for_commit(pulls: list[dict], sha: str) -> int | None:
    for pull in pulls:
        if (
            pull.get("merged_at")
            and pull.get("merge_commit_sha") == sha
            and pull.get("base", {}).get("ref") == "main"
        ):
            return pull["number"]
    return None


def ci_state_for_commit(runs: list[dict], sha: str) -> str:
    """Return pending, success, or failure for the main-push CI run."""
    for run in runs:
        if run.get("head_sha") != sha or run.get("event") != "push":
            continue
        if run.get("status") != "completed":
            return "pending"
        return "success" if run.get("conclusion") == "success" else "failure"
    return "pending"


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    ).stdout.strip()


def find_release_tag_for_merge(root: Path, merge_sha: str) -> str | None:
    """Reuse a stable release tag pointing at the merge or its release child."""
    for tag in _git(root, "tag", "--list", "v*").splitlines():
        if not SEMVER.fullmatch(tag[1:]):
            continue
        target = _git(root, "rev-parse", f"{tag}^{{commit}}")
        if target == merge_sha:
            return tag
        parents = _git(root, "rev-list", "--parents", "-n", "1", target).split()
        if len(parents) > 1 and parents[1] == merge_sha:
            return tag
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("next")
    apply = sub.add_parser("apply")
    apply.add_argument("version")
    merged = sub.add_parser("merged-pr")
    merged.add_argument("--sha", required=True)
    ci = sub.add_parser("ci-state")
    ci.add_argument("--sha", required=True)
    existing = sub.add_parser("find-tag")
    existing.add_argument("--merge-sha", required=True)
    args = parser.parse_args()
    root = Path.cwd()
    try:
        if args.command == "next":
            contents = {
                name: (root / name).read_text()
                for name in ("Cargo.toml", "Cargo.lock", "pyproject.toml", "uv.lock")
            }
            current = _read_versions(contents)
            print(next_release_version(current, _git(root, "tag", "--list", "v*").splitlines()))
        elif args.command == "apply":
            apply_release_version(root, args.version)
        elif args.command == "merged-pr":
            number = merged_pr_for_commit(json.load(sys.stdin), args.sha)
            print(number if number is not None else "")
        elif args.command == "ci-state":
            print(ci_state_for_commit(json.load(sys.stdin)["workflow_runs"], args.sha))
        elif args.command == "find-tag":
            print(find_release_tag_for_merge(root, args.merge_sha) or "")
    except (KeyError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"release version error: {error}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
