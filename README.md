# Organon

[![CI](https://github.com/andrii-su/organon/actions/workflows/ci.yml/badge.svg)](https://github.com/andrii-su/organon/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/core-Rust-1a1320?logo=rust&logoColor=white)](./crates/)
[![Python](https://img.shields.io/badge/ai-Python%203.12%2B-6d28d9?logo=python&logoColor=white)](./ai/)
[![License: MIT](https://img.shields.io/github/license/andrii-su/organon)](./LICENSE)

**Organon** is a local-first semantic filesystem layer for AI agents. It indexes
files into a local graph with stable identity, lifecycle state, history,
relationships, and semantic search, then exposes that graph to agents over MCP.

The goal is simple: give agents the right local context before they change code.

## Core Capabilities

- **Entity graph**: stable file identities, lifecycle state, history,
  relationships, and duplicate detection backed by SQLite.
- **Semantic retrieval**: local vector search via LanceDB and fastembed, FTS,
  and hybrid ranking — no external API calls.
- **Relationship awareness**: import/reference graph, reverse dependencies, and
  impact analysis.
- **MCP server**: stdio and HTTP (SSE) modes exposing 10+ tools for agent
  clients such as Claude Desktop and Cursor.
- **Local-first storage**: SQLite + LanceDB on disk. No accounts, no telemetry.

## Installation

Requires Rust stable, Python 3.12+, and `uv`.

```bash
bash setup.sh
cargo build --release
```

## Quick Start

Bootstrap and index a workspace:

```bash
organon watch .        # start watcher + auto-indexer
organon index          # run Python indexer manually (one-shot)
```

Inspect context:

```bash
organon health
organon status src/auth.rs
organon search "authentication logic" --mode hybrid
organon context "authentication refactor" --scope . --json
organon plan "change authentication flow" --file src/auth.rs
organon related-tests src/auth.rs
```

Start the MCP server for an agent client:

```bash
organon mcp            # stdio (default, for Claude Desktop / Cursor)
organon mcp --sse      # HTTP SSE server
```

MCP sessions are scoped to the current directory by default. Pass a workspace
path or `--scope <dir>` to choose an explicit scope.

## MCP Integration

Add to `~/.claude/claude_desktop_config.json` (Claude Desktop):

```json
{
  "mcpServers": {
    "organon": {
      "command": "organon",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

For Cursor or any MCP-compatible client, point it at `organon mcp` (stdio) or
`organon mcp --sse` (HTTP).

### Available MCP Tools

| Tool | Description |
| ---- | ----------- |
| `search_files` | Semantic, FTS, or hybrid search |
| `get_entity` | Entity metadata and lifecycle state |
| `get_graph` | Dependency graph around a file |
| `get_file_content` | File content with optional line range |
| `build_context` | Compact agent context pack for a task |
| `find_duplicates` | Identify duplicate or near-duplicate files |
| `get_history` | Change history for a file |
| `get_impact` | Files impacted by changing a target file |
| `list_saved_queries` | List saved search queries |
| `run_saved_query` | Execute a saved query |
| `list_by_lifecycle` | Files filtered by lifecycle state |
| `stats` | Workspace statistics |

## CLI Reference

```bash
# Watcher and indexer
organon watch .
organon watch /path/to/workspace --daemon
organon daemon list
organon daemon logs <id>
organon daemon stop <id>
organon index
organon index /path/to/workspace

# Entity inspection
organon status <file>
organon find --state dormant --ext rs
organon find --modified-after 2026-01-01 --larger-than-mb 10

# Search
organon search "query"
organon search "watcher" --state active --mode hybrid --explain
organon search --like src/auth.rs --limit 5

# Agent context
organon context "task description" --scope /path/to/workspace --budget 12000 --json
organon context --path src/auth.rs --budget 8000
organon plan "change lifecycle rules" --file crates/organon-core/src/lifecycle.rs

# Graph and impact
organon graph src/main.rs --depth 2 --format text
organon impact src/auth.rs --depth 3
organon history src/auth.rs --limit 20
organon related-tests src/auth.rs

# Maintenance
organon health
organon doctor
organon duplicates
organon clean --dry-run
organon cleanup --dry-run
```

Global flags: `--quiet`, `-v` (info), `-vv` (debug).

## Architecture

```
crates/
  organon-core/   Rust library: entity graph, lifecycle engine, SQLite, watcher, scanner
  organon-cli/    Rust binary (organon): 20+ subcommands, Python bridge
  organon-mcp/    Rust: MCP server (stdio + HTTP/SSE)
ai/
  indexer.py      Entry point: orchestrates extraction, embedding, relation indexing
  embeddings/     Local semantic vectors via fastembed + LanceDB
  extractor/      Content extraction (text, PDF, code)
  relations/      Import/reference graph extraction
```

## Stack

| Layer | Technology |
| ----- | ---------- |
| Core | Rust: `notify`, `tokio`, `rusqlite` |
| Semantic vectors | Python: `fastembed`, `lancedb` |
| Content extraction | Python: text, PDF, code |
| Agent protocol | MCP (stdio + HTTP/SSE) |
| Storage | SQLite + LanceDB |

## Development

```bash
# Rust build and tests
cargo build --release
cargo test --workspace --all-targets

# Rust formatting and linting
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Python linting and tests
uv run --group dev ruff check ai tests
uv run --group dev pytest

# Python indexer health check
uv run python -m ai.indexer --health
```

The Rust CLI invokes Python via `uv run --project <organon-root> python -m ai.indexer`.
If the repository root cannot be resolved from the installed binary or current
directory, set `ORGANON_PYTHON_PROJECT=/path/to/organon`. For non-standard uv
installations, set `ORGANON_UV=/path/to/uv`.

## Principles

- **Local-first**: nothing leaves your machine unless you configure another tool
  to send it.
- **Agent-native**: MCP and task context are first-class product surfaces.
- **Small surface area**: fewer workflows, stronger core.
- **Lifecycle-aware**: stale, dormant, archived, and dead files remain visible
  to agents rather than being treated as anonymous blobs.

## License

MIT
