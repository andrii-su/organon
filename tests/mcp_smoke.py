"""Protocol-level smoke test for the built MCP binary."""

import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path


def _json_responses(stdout: str) -> dict:
    responses = [json.loads(line) for line in stdout.splitlines() if line.startswith("{")]
    return {response.get("id"): response for response in responses if "id" in response}


def _build_graph(binary: Path, workspace: Path, db: Path) -> None:
    proc = subprocess.Popen(
        [str(binary), "--db", str(db), "watch", str(workspace), "--no-index"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=os.environ.copy(),
    )
    assert proc.stderr is not None
    deadline = time.monotonic() + 10
    indexed = False
    while time.monotonic() < deadline:
        line = proc.stderr.readline()
        if "indexed " in line:
            indexed = True
            break
        if proc.poll() is not None:
            break
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)
    assert indexed, proc.stderr.read()


def _run_stdio(binary: Path, db: Path, workspace: Path) -> dict:
    file_path = str(workspace / "main.py")
    outside_path = str(workspace.parent / "outside.py")
    messages = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "smoke-test", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}},
        {"jsonrpc": "2.0", "id": 3, "method": "resources/list", "params": {}},
        {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "graph_stats", "arguments": {}}},
        {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "get_entity", "arguments": {"path": file_path}}},
        {"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "get_file_content", "arguments": {"path": file_path}}},
        {"jsonrpc": "2.0", "id": 7, "method": "resources/read", "params": {"uri": "organon://entities"}},
        {"jsonrpc": "2.0", "id": 8, "method": "tools/call", "params": {"name": "get_entity", "arguments": {"path": outside_path}}},
    ]
    proc = subprocess.run(
        [str(binary), "--db", str(db), "mcp", "--scope", str(workspace)],
        input="\n".join(json.dumps(message) for message in messages) + "\n",
        text=True,
        capture_output=True,
        env={**os.environ, "ORGANON_DB": str(db)},
        timeout=20,
        check=True,
    )
    responses = _json_responses(proc.stdout)
    assert responses[1]["result"]["serverInfo"]["name"]
    assert responses[2]["result"]["tools"]
    assert responses[3]["result"]["resources"] == [{"uri": "organon://entities", "name": "entities"}]
    assert responses[4]["result"]["structuredContent"]["total_entities"] == 2
    assert responses[5]["result"]["structuredContent"]["entity"]["path"] == file_path
    assert responses[6]["result"]["structuredContent"]["content"].startswith("print")
    assert file_path in responses[7]["result"]["contents"][0]["text"]
    assert "error" in responses[8] or responses[8]["result"].get("isError") is True
    return responses


def _run_http(binary: Path, db: Path, workspace: Path) -> None:
    proc = subprocess.Popen(
        [str(binary), "--db", str(db), "mcp", "--sse", "--scope", str(workspace)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        env={**os.environ, "ORGANON_DB": str(db)},
    )
    try:
        request = urllib.request.Request(
            "http://127.0.0.1:7474/mcp",
            data=json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": 20,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": {"name": "http-smoke", "version": "1"},
                    },
                }
            ).encode(),
            headers={"content-type": "application/json", "accept": "application/json, text/event-stream"},
            method="POST",
        )
        deadline = time.monotonic() + 10
        body = ""
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(request, timeout=2) as response:
                    body = response.read().decode()
                    break
            except (ConnectionError, OSError, urllib.error.URLError):
                time.sleep(0.2)
        assert '"id":20' in body
        assert '"protocolVersion":"2025-06-18"' in body
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)


def main() -> int:
    binary = Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix="organon-mcp-smoke-") as tmp:
        root = Path(tmp).resolve()
        workspace = (root / "workspace").resolve()
        workspace.mkdir()
        (workspace / "main.py").write_text('print("hello")\n', encoding="utf-8")
        (workspace / "README.md").write_text("MCP smoke fixture\n", encoding="utf-8")
        (root / "outside.py").write_text("outside\n", encoding="utf-8")
        db = root / "entities.db"
        _build_graph(binary, workspace, db)
        _run_stdio(binary, db, workspace)
        _run_http(binary, db, workspace)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
