"""Published release checks must reject missing or inconsistent artifacts."""

import hashlib
import io
import tarfile
from pathlib import Path

import pytest

from tools.verify_release import check_archive, check_archives, check_assets, expected_asset_names


def test_asset_check_requires_all_platform_archives_and_checksums():
    assets = [{"name": name, "size": 1} for name in expected_asset_names()]
    check_assets(assets)
    with pytest.raises(ValueError, match="missing"):
        check_assets(assets[:-1])
    with pytest.raises(ValueError, match="empty"):
        check_assets([{**assets[0], "size": 0}, *assets[1:]])


def test_archive_check_verifies_checksum_and_binary_version(tmp_path: Path):
    archive = tmp_path / "organon-x86_64-unknown-linux-gnu.tar.gz"
    binary = b"#!/bin/sh\necho 'organon 1.4.1'\n"
    with tarfile.open(archive, "w:gz") as tar:
        member = tarfile.TarInfo("organon-x86_64-unknown-linux-gnu/organon")
        member.mode = 0o755
        member.size = len(binary)
        tar.addfile(member, io.BytesIO(binary))
    checksum = tmp_path / f"{archive.name}.sha256"
    checksum.write_text(f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")

    check_archive(archive, "v1.4.1")
    with pytest.raises(ValueError, match="version"):
        check_archive(archive, "v1.4.2")
    checksum.write_text(f"{'0' * 64}  {archive.name}\n")
    with pytest.raises(ValueError, match="checksum"):
        check_archive(archive, "v1.4.1")


def test_all_archives_are_required_and_cross_platform_binary_is_inspected(tmp_path: Path):
    archive = tmp_path / "organon-aarch64-apple-darwin.tar.gz"
    binary = b"macOS binary contents"
    with tarfile.open(archive, "w:gz") as tar:
        member = tarfile.TarInfo("organon-aarch64-apple-darwin/organon")
        member.size = len(binary)
        tar.addfile(member, io.BytesIO(binary))
    (tmp_path / f"{archive.name}.sha256").write_text(
        f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
    )
    check_archive(archive, "v1.4.1", execute=False)
    with pytest.raises(FileNotFoundError):
        check_archives(tmp_path, "v1.4.1")
