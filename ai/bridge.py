"""Argument-safe bridge between the Rust CLI/MCP server and the Python AI layer.

The Rust side used to build a `python -c "...{query}..."` snippet by
interpolating user-controlled strings (search queries, file paths) into Python
source with Rust's Debug formatting. Rust Debug escaping is not Python string
escaping, so control characters produced invalid Python and the whole call
failed — and interpolating untrusted input into source is an injection-shaped
boundary regardless.

This module removes the boundary entirely: the Rust side sets a single
`ORGANON_BRIDGE_ARGS` environment variable to a JSON object and runs
`python -m ai.bridge`. No caller data ever touches Python source.

Request shape (JSON in ORGANON_BRIDGE_ARGS):
    {"op": "search", "query": str, "limit": int, "db_path": str|null,
     "path_prefix": str|null}
    {"op": "search_by_path", "path": str, "limit": int, "db_path": str|null,
     "path_prefix": str|null}
    {"op": "extract_text", "path": str}

The result (a JSON value) is printed to stdout.
"""

import json
import os
import sys

ARGS_ENV = "ORGANON_BRIDGE_ARGS"


def _run(args: dict) -> object:
    op = args.get("op")
    if op == "search":
        from ai.embeddings.store import search

        return search(
            args["query"],
            limit=args["limit"],
            db_path=args.get("db_path"),
            path_prefix=args.get("path_prefix"),
        )
    if op == "search_by_path":
        from ai.embeddings.store import search_by_path

        return search_by_path(
            args["path"],
            limit=args["limit"],
            db_path=args.get("db_path"),
            path_prefix=args.get("path_prefix"),
        )
    if op == "extract_text":
        from ai.extractor.extract import extract_text

        text = extract_text(args["path"]) or ""
        return {"path": args["path"], "content": text, "chars": len(text)}
    raise ValueError(f"unknown bridge op: {op!r}")


def main() -> int:
    raw = os.environ.get(ARGS_ENV)
    if not raw:
        print(f"{ARGS_ENV} not set", file=sys.stderr)
        return 2
    try:
        args = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"invalid {ARGS_ENV} JSON: {e}", file=sys.stderr)
        return 2
    print(json.dumps(_run(args)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
