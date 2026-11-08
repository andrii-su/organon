"""Deterministic end-to-end contract for Organon's filesystem query tools."""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

from mcp_smoke import _build_graph, _json_responses


def call_tool(request_id: int, name: str, arguments: dict) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }


def run(binary: Path) -> None:
    with tempfile.TemporaryDirectory(prefix="organon-contract-") as temporary:
        root = Path(temporary).resolve()
        workspace = root / "workspace"
        workspace.mkdir()
        route = workspace / "routes.py"
        model = workspace / "models.py"
        route.write_text(
            "from models import Order\n@app.get('/x/sds/sd')\ndef get_order(): return Order()\n"
        )
        model.write_text("class Order: pass\n")
        db = root / "graph.db"
        _build_graph(binary, workspace, db)
        route.write_text(
            "from models import Order\n@app.get('/x/sds/sd')\ndef get_order(): return Order(1)\n"
        )
        _build_graph(binary, workspace, db)

        with sqlite3.connect(db) as conn:
            conn.execute(
                "INSERT INTO entities_fts(path, name, content) VALUES (?, ?, ?)",
                (str(route), route.name, route.read_text()),
            )
            conn.execute(
                "INSERT INTO relationships(from_path, to_path, kind, created_at) VALUES (?, ?, ?, 1)",
                (str(route), str(model), "imports"),
            )
            conn.execute(
                "INSERT INTO indexed_routes(route, handler_path, provenance) VALUES (?, ?, ?)",
                ("/x/sds/sd", str(route), "[]"),
            )

        messages = [
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "contract", "version": "1"},
                },
            },
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            call_tool(2, "search_files", {"query": "get_order", "mode": "fts"}),
            call_tool(3, "query_route_files", {"query": "/x/sds/sd"}),
            call_tool(4, "build_context", {"path": str(route), "mode": "fts"}),
            call_tool(5, "get_impact", {"path": str(model)}),
            call_tool(6, "get_history", {"path": str(route)}),
        ]
        output = subprocess.run(
            [str(binary), "--db", str(db), "mcp", "--scope", str(workspace)],
            input="\n".join(json.dumps(message) for message in messages) + "\n",
            check=True,
            capture_output=True,
            text=True,
            timeout=20,
            env={**os.environ, "ORGANON_DB": str(db)},
        ).stdout
        responses = _json_responses(output)
        for request_id in range(2, 7):
            assert "error" not in responses[request_id], responses[request_id]
        assert any(
            item["path"] == str(route)
            for item in responses[2]["result"]["structuredContent"]["items"]
        )
        candidates = responses[3]["result"]["structuredContent"]["items"]
        assert any(
            item["path"] == str(route) and item["match_percent"] == 100 for item in candidates
        )
        assert any(
            item["path"] == str(route)
            for item in responses[4]["result"]["structuredContent"]["items"]
        )
        assert any(
            item["path"] == str(route)
            for item in responses[5]["result"]["structuredContent"]["entries"]
        )
        assert any(
            item["event"] == "modified"
            for item in responses[6]["result"]["structuredContent"]["entries"]
        )


if __name__ == "__main__":
    run(Path(sys.argv[1]).resolve())
