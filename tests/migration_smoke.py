"""Check that the current binary opens and extends a previous release's graph."""

import json
import sqlite3
import sys
import tempfile
from pathlib import Path

from mcp_smoke import _build_graph


def run(old_binary: Path, current_binary: Path) -> None:
    import subprocess

    with tempfile.TemporaryDirectory(prefix="organon-migration-") as temporary:
        root = Path(temporary).resolve()
        workspace = root / "workspace"
        workspace.mkdir()
        source = workspace / "main.py"
        source.write_text('print("old release")\n')
        db = root / "graph.db"

        _build_graph(old_binary, workspace, db)
        with sqlite3.connect(db) as conn:
            before = conn.execute(
                "SELECT id, event FROM entity_history WHERE path = ? ORDER BY id", (str(source),)
            ).fetchall()
            assert any(event == "created" for _, event in before), before

        stats = subprocess.run(
            [str(current_binary), "--db", str(db), "stats"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        assert "total:       1" in stats, stats
        with sqlite3.connect(db) as conn:
            assert (
                conn.execute(
                    "SELECT id, event FROM entity_history WHERE path = ? ORDER BY id",
                    (str(source),),
                ).fetchall()
                == before
            )
            assert "text_change" in {
                row[1] for row in conn.execute("PRAGMA table_info(entity_history)")
            }
            assert conn.execute(
                "SELECT name FROM sqlite_master WHERE name = 'entity_history_archive'"
            ).fetchone()

        source.write_text('print("current release")\n')
        _build_graph(current_binary, workspace, db)
        entries = json.loads(
            subprocess.run(
                [str(current_binary), "--db", str(db), "history", str(source), "--json"],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
        )
        assert any(entry["id"] == before[0][0] and entry["event"] == "created" for entry in entries)
        assert any(entry["event"] == "modified" for entry in entries)


if __name__ == "__main__":
    run(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
