"""Benchmark Organon file and history queries against real, indexed projects.

Usage: python3 tools/benchmark_queries.py target/debug/organon manifest.json
The manifest contains {"projects": [{"name": str, "root": absolute path,
"queries": [{"kind": "route"|"functionality"|"model"|"history",
"query": str, "expected": [relative paths], "known_negative": [relative paths],
"known_negative_suffixes": [str], "from_date": str, "to_date": str}]}]}.
Only the relevant query fields are required. Results are JSON on stdout.
"""

import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def request(request_id: int, name: str, arguments: dict) -> dict:
    return {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments},
    }


def scan(binary: Path, workspace: Path, db: Path, env: dict[str, str]) -> float:
    started = time.perf_counter()
    process = subprocess.Popen(
        [str(binary), "--db", str(db), "watch", str(workspace), "--no-index"],
        env=env,
        text=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    try:
        assert process.stderr is not None
        while True:
            line = process.stderr.readline()
            if "indexed " in line:
                break
            if process.poll() is not None:
                raise RuntimeError(f"watch exited before scanning: {line}")
    finally:
        process.terminate()
        process.wait(timeout=5)
    return time.perf_counter() - started


def query_once(binary: Path, workspace: Path, db: Path, env: dict[str, str], query: dict) -> dict:
    kind = query["kind"]
    if kind == "history":
        tool = "query_history_events"
        arguments = {
            "from_date": query["from_date"],
            "to_date": query["to_date"],
            "include_archive": query.get("include_archive", True),
        }
        if "event" in query:
            arguments["event"] = query["event"]
    else:
        tool = "query_files"
        arguments = {"query": query["query"], "kind": kind}
    arguments["limit"] = query.get("limit", 100)
    messages = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "benchmark", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        request(2, tool, arguments),
    ]
    started = time.perf_counter()
    output = subprocess.run(
        [str(binary), "--db", str(db), "mcp", "--scope", str(workspace)],
        env=env,
        input="\n".join(json.dumps(item) for item in messages) + "\n",
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    latency_ms = round((time.perf_counter() - started) * 1000, 2)
    responses = [json.loads(line) for line in output.stdout.splitlines() if line.startswith("{")]
    response = next(item for item in responses if item.get("id") == 2)
    if "error" in response or response["result"].get("isError"):
        raise RuntimeError(response)
    data = response["result"]["structuredContent"]
    paths = {str(Path(item["path"]).relative_to(workspace)) for item in data["items"]}
    expected = set(query.get("expected", []))
    known_negative = set(query.get("known_negative", []))
    suffixes = tuple(query.get("known_negative_suffixes", []))
    return {
        "query": (
            f"{query['from_date']}..{query['to_date']}" if kind == "history" else query["query"]
        ),
        "kind": kind,
        "latency_ms": latency_ms,
        "response_bytes": len(json.dumps(data).encode()),
        "candidates": len(data["items"]),
        "has_more": data["has_more"],
        "known_positive_recall": len(paths & expected) / len(expected) if expected else None,
        "missing_known_positives": sorted(expected - paths),
        "known_negative_hits": sorted(paths & known_negative),
        "known_negative_suffix_hits": sorted(
            path for path in paths if suffixes and path.endswith(suffixes)
        ),
    }


def main(binary: Path, manifest: Path) -> None:
    projects = json.loads(manifest.read_text())["projects"]
    results = []
    for project in projects:
        workspace = Path(project["root"]).resolve()
        assert workspace.is_dir(), workspace
        with tempfile.TemporaryDirectory(prefix="organon-benchmark-") as temporary:
            root = Path(temporary)
            db = root / "entities.db"
            config = root / "config.toml"
            config.write_text(f"[indexer]\nvectors_path = {json.dumps(str(root / 'vectors'))}\n")
            env = {
                **os.environ,
                "ORGANON_HOME": str(root),
                "ORGANON_CONFIG": str(config),
                "ORGANON_DB": str(db),
                "ORGANON_PYTHON_PROJECT": str(Path(__file__).resolve().parents[1]),
            }
            scan_seconds = round(scan(binary, workspace, db, env), 3)
            started = time.perf_counter()
            index = subprocess.run(
                [str(binary), "--db", str(db), "index", str(workspace)],
                env=env,
                capture_output=True,
                text=True,
                timeout=300,
                check=True,
            )
            if "0 errors" not in index.stderr:
                raise RuntimeError(f"indexer reported errors: {index.stderr[-2000:]}")
            index_seconds = round(time.perf_counter() - started, 3)
            with sqlite3.connect(db) as connection:
                entities = connection.execute("SELECT COUNT(*) FROM entities").fetchone()[0]
                fts_rows = connection.execute("SELECT COUNT(*) FROM entities_fts").fetchone()[0]
            results.append(
                {
                    "project": project["name"],
                    "entities": entities,
                    "fts_rows": fts_rows,
                    "scan_seconds": scan_seconds,
                    "index_seconds": index_seconds,
                    "queries": [
                        query_once(binary, workspace, db, env, item) for item in project["queries"]
                    ],
                }
            )
    print(json.dumps({"results": results}, indent=2))


if __name__ == "__main__":
    main(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
