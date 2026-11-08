"""Verify published release assets and a runnable Linux archive."""

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
)
SEMVER_TAG = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+$")


def expected_asset_names() -> set[str]:
    names = {f"organon-{target}.tar.gz" for target in TARGETS}
    return names | {f"{name}.sha256" for name in names}


def check_assets(assets: list[dict]) -> None:
    found = {asset["name"]: asset for asset in assets}
    missing = expected_asset_names() - found.keys()
    if missing:
        raise ValueError(f"missing release assets: {sorted(missing)}")
    empty = sorted(name for name in expected_asset_names() if found[name].get("size", 0) <= 0)
    if empty:
        raise ValueError(f"empty release assets: {empty}")


def check_archive(archive: Path, tag: str, *, execute: bool = True) -> None:
    if not SEMVER_TAG.fullmatch(tag):
        raise ValueError(f"invalid release tag: {tag}")
    target = archive.name.removeprefix("organon-").removesuffix(".tar.gz")
    if target not in TARGETS or archive.name != f"organon-{target}.tar.gz":
        raise ValueError(f"unexpected release archive: {archive.name}")
    checksum = archive.with_name(f"{archive.name}.sha256")
    parts = checksum.read_text().strip().split()
    if len(parts) != 2 or parts[1].lstrip("*") != archive.name:
        raise ValueError("invalid checksum file")
    with archive.open("rb") as stream:
        actual = hashlib.file_digest(stream, "sha256").hexdigest()
    if actual != parts[0]:
        raise ValueError("archive checksum mismatch")

    member_name = f"organon-{target}/organon"
    with tarfile.open(archive, "r:gz") as tar, tempfile.TemporaryDirectory() as temporary:
        member = tar.getmember(member_name)
        if not member.isfile():
            raise ValueError("release binary is not a regular file")
        if member.mode & 0o111 == 0:
            raise ValueError("release binary is not executable in the archive")
        source = tar.extractfile(member)
        if source is None:
            raise ValueError("release binary missing from archive")
        binary = Path(temporary) / "organon"
        binary.write_bytes(source.read())
        binary.chmod(member.mode & 0o777)
        if execute:
            output = subprocess.run(
                [str(binary), "--version"], capture_output=True, text=True, check=True, timeout=10
            ).stdout.strip()
            if output != f"organon {tag[1:]}":
                raise ValueError(f"release binary version mismatch: {output!r} for {tag}")


def check_archives(directory: Path, tag: str) -> None:
    for target in TARGETS:
        check_archive(
            directory / f"organon-{target}.tar.gz",
            tag,
            execute=target == "x86_64-unknown-linux-gnu",
        )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("assets")
    archives = sub.add_parser("archives")
    archives.add_argument("directory", type=Path)
    archives.add_argument("tag")
    args = parser.parse_args()
    try:
        if args.command == "assets":
            check_assets(json.load(sys.stdin)["assets"])
        else:
            check_archives(args.directory, args.tag)
    except (
        KeyError,
        OSError,
        ValueError,
        tarfile.TarError,
        subprocess.CalledProcessError,
    ) as error:
        parser.exit(1, f"release verification failed: {error}\n")


if __name__ == "__main__":
    main()
