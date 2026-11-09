"""Exercise real per-user autostart, restart, recovery, logs, and removal."""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def run(args: list[str], env: dict[str, str], *, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(args, env=env, text=True, capture_output=True, timeout=30, check=check)


def wait_for_event(db: Path, path: Path, event: str, timeout: float = 20) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if db.exists():
            with sqlite3.connect(db) as connection:
                try:
                    count = connection.execute(
                        "SELECT COUNT(*) FROM entity_history WHERE path = ? AND event = ?",
                        (str(path), event),
                    ).fetchone()[0]
                    if count:
                        return True
                except sqlite3.OperationalError:
                    pass
        time.sleep(0.25)
    return False


def check_autostart(binary: Path) -> None:
    mac = sys.platform == "darwin"
    assert mac or sys.platform.startswith("linux"), sys.platform
    with tempfile.TemporaryDirectory(prefix="organon-autostart-smoke-") as temporary:
        root = Path(temporary).resolve()
        home = root / "home" if mac else Path.home()
        if mac:
            home.mkdir()
        workspace = root / "workspace"
        workspace.mkdir()
        sample = workspace / "sample.py"
        sample.write_text("value = 1\n")
        db = root / "entities.db"
        organon_home = root / "organon-home"
        config = root / "config.toml"
        config.write_text(
            "[watch]\nroots = ["
            + json.dumps(str(workspace))
            + "]\n[indexer]\nvectors_path = "
            + json.dumps(str(root / "vectors"))
            + "\n"
        )
        env = {
            **os.environ,
            "HOME": str(home),
            "ORGANON_HOME": str(organon_home),
            "ORGANON_CONFIG": str(config),
            "ORGANON_DB": str(db),
            "ORGANON_PYTHON_PROJECT": str(Path(__file__).resolve().parents[1]),
        }
        if mac:
            target = f"gui/{os.getuid()}/dev.organon.watch"
            service = home / "Library/LaunchAgents/dev.organon.watch.plist"
            assert run(["launchctl", "print", target], env, check=False).returncode != 0, (
                "Organon autostart already installed; refusing to replace it"
            )
            stop = ["launchctl", "bootout", target]
            start = ["launchctl", "bootstrap", f"gui/{os.getuid()}", str(service)]
        else:
            service = home / ".config/systemd/user/organon.service"
            assert not service.exists(), "Organon autostart already installed"
            run(["systemctl", "--user", "show-environment"], env)
            stop = ["systemctl", "--user", "stop", "organon.service"]
            start = ["systemctl", "--user", "start", "organon.service"]
        installed = False
        try:
            run([str(binary), "daemon", "autostart", "install"], env)
            installed = True
            assert service.is_file(), service
            assert wait_for_event(db, sample, "created"), "watcher did not scan the workspace"
            run([str(binary), "daemon", "autostart", "install"], env)
            run(stop, env)
            sample.write_text("value = 2\n")
            run(start, env)
            assert wait_for_event(db, sample, "modified"), "watcher did not recover missed edit"
            log = organon_home / "daemon/watch-autostart.log"
            assert log.is_file() and log.stat().st_size > 0, "watcher log is missing"
            print("autostart install, reinstall, restart, recovery, and logs passed")
        finally:
            if installed:
                run([str(binary), "daemon", "autostart", "uninstall"], env)
                assert not service.exists(), "autostart service was not removed"


if __name__ == "__main__":
    check_autostart(Path(sys.argv[1]).resolve())
