"""Index Organon's own source with the production CLI, then query it through MCP."""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest.mock import patch

from mcp_smoke import _build_graph, _json_responses

from ai.embeddings.store import get_all_entries


def call_tool(request_id: int, name: str, arguments: dict) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }


def run(binary: Path) -> None:
    repo = Path(__file__).resolve().parents[1]
    source = repo / "ai" / "routes"
    target = source / "fastapi.py"
    assert target.is_file(), target

    with tempfile.TemporaryDirectory(prefix="organon-self-dogfood-") as temporary:
        root = Path(temporary)
        db = root / "entities.db"
        vectors = root / "vectors"
        config = root / "config.toml"
        config.write_text(f"[indexer]\nvectors_path = {json.dumps(str(vectors))}\n")
        env = {
            **os.environ,
            "ORGANON_HOME": str(root),
            "ORGANON_DB": str(db),
            "ORGANON_CONFIG": str(config),
            "ORGANON_VECTORS_DB": str(vectors),
            "ORGANON_PYTHON_PROJECT": str(repo),
        }

        with patch.dict(os.environ, env):
            _build_graph(binary, source, db)
        index = subprocess.run(
            [str(binary), "--db", str(db), "index", str(source)],
            check=True,
            capture_output=True,
            text=True,
            timeout=300,
            env=env,
        )
        assert "0 errors" in index.stderr, index.stderr
        with sqlite3.connect(db) as conn:
            row = conn.execute(
                "SELECT content FROM entities_fts WHERE path = ?", (str(target),)
            ).fetchone()
        assert row and "discover_routes" in row[0], "Organon source missing from real FTS index"
        assert any(entry["path"] == str(target) for entry in get_all_entries(str(vectors))), (
            "Organon source missing from real vector index"
        )

        messages = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "self-dogfood", "version": "1"},
                },
            },
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            call_tool(2, "search_files", {"query": "discover_routes", "mode": "fts"}),
            call_tool(3, "build_context", {"path": str(target), "mode": "fts"}),
            call_tool(4, "get_history", {"path": str(target)}),
        ]
        output = subprocess.run(
            [str(binary), "--db", str(db), "mcp", "--scope", str(source)],
            input="\n".join(json.dumps(message) for message in messages) + "\n",
            check=True,
            capture_output=True,
            text=True,
            timeout=30,
            env=env,
        ).stdout
        responses = _json_responses(output)
        for request_id in range(2, 5):
            assert "error" not in responses[request_id], responses[request_id]
        assert any(
            item["path"] == str(target)
            for item in responses[2]["result"]["structuredContent"]["items"]
        ), responses[2]
        assert any(
            item["path"] == str(target)
            for item in responses[3]["result"]["structuredContent"]["items"]
        ), responses[3]
        assert responses[4]["result"]["structuredContent"]["entries"], responses[4]
        print("Organon indexed and queried its own source successfully")


if __name__ == "__main__":
    run(Path(sys.argv[1]).resolve())
