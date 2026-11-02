"""Protocol-level smoke test for the built MCP binary."""

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> int:
    binary = Path(sys.argv[1])
    with tempfile.TemporaryDirectory(prefix="organon-mcp-smoke-") as tmp:
        env = {"ORGANON_DB": str(Path(tmp) / "entities.db")}
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
        ]
        proc = subprocess.run(
            [str(binary), "mcp", "--global"],
            input="\n".join(json.dumps(message) for message in messages) + "\n",
            text=True,
            capture_output=True,
            env={**os.environ, **env},
            timeout=10,
            check=True,
        )

    responses = [json.loads(line) for line in proc.stdout.splitlines() if line.startswith("{")]
    by_id = {response.get("id"): response for response in responses if "id" in response}
    assert by_id[1]["result"]["serverInfo"]["name"]
    assert by_id[2]["result"]["tools"]
    assert by_id[3]["result"]["resources"] == [
        {"uri": "organon://entities", "name": "entities"}
    ]
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
