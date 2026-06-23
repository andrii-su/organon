<p align="center">
  <img src="docs/assets/organon-banner.svg" width="760" alt="Organon — local-first semantic layer over your filesystem" />
</p>

<h1 align="center">Organon</h1>

<p align="center">
  <strong>local-first semantic filesystem layer for AI agents — stable identity, lifecycle, relationships, and semantic search over MCP</strong>
</p>

<p align="center">
  <a href="https://github.com/andrii-su/organon/actions/workflows/ci.yml"><img src="https://github.com/andrii-su/organon/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="./crates/"><img src="https://img.shields.io/badge/core-Rust-1a1320?logo=rust&logoColor=white" alt="Rust"></a>
  <a href="./ai/"><img src="https://img.shields.io/badge/ai-Python%203.12%2B-6d28d9?logo=python&logoColor=white" alt="Python"></a>
  <a href="https://modelcontextprotocol.io"><img src="https://img.shields.io/badge/protocol-MCP-0EA5E9" alt="MCP"></a>
  <a href="./LICENSE"><img src="https://img.shields.io/github/license/andrii-su/organon" alt="License: MIT"></a>
</p>

<p align="center">
  <a href="#why">Why</a> •
  <a href="#install">Install</a> •
  <a href="#quick-start">Quick Start</a> •
  <a href="#mcp-integration">MCP</a> •
  <a href="#how-it-works">How It Works</a> •
  <a href="#cli-reference">CLI</a> •
  <a href="./docs/agent-usage.md">Agent guide</a>
</p>

______________________________________________________________________

When an agent edits your code, it usually starts blind: `grep`, open a few files, guess what's related, hope nothing else breaks. Organon gives it a memory of the filesystem instead — every file is a **living entity** with a stable identity, a lifecycle, a relationship graph, change history, and a semantic index. The agent asks Organon for context *before* it touches anything.

Everything runs **100% locally** — SQLite for the graph, LanceDB for vectors, fastembed for embeddings. No accounts, no telemetry, no network in the critical path.

## Why

<table>
<tr>
<td width="50%" valign="top">

### Agent without Organon

```text
$ grep -r "login" src/
src/auth.rs:  pub fn login(...)
src/main.rs:  use auth::login;
... now what?
```

- Reads files blindly, one `grep` at a time
- No idea what *depends on* `auth.rs`
- Can't tell a hot file from one dead for 6 months
- Re-reads the whole tree every session

</td>
<td width="50%" valign="top">

### Agent with Organon

```text
> get_impact("src/auth.rs")
risk: high — 7 dependents, 3 direct
> build_context("auth refactor")
5 ranked files + snippets + lifecycle
```

- Asks for **impact** before changing a file
- Gets a **ranked context pack** for the task
- Sees **lifecycle**: active / dormant / archived
- Queries a persistent graph, not the raw tree

</td>
</tr>
</table>

**Same repo. The agent just stops guessing.**

```text
┌────────────────────────────────────────────────────────┐
│  LIFECYCLE       Born → Active → Dormant → Archived → Dead │
│  SEARCH MODES    vector  ·  full-text (FTS5)  ·  hybrid    │
│  GRAPH           imports · reverse-deps · impact · history │
│  TRANSPORT       MCP stdio  ·  MCP HTTP/SSE  ·  CLI        │
│  STORAGE         SQLite + LanceDB — 100% on disk          │
└────────────────────────────────────────────────────────┘
```

> [!IMPORTANT]
> The entity graph (identity, lifecycle, history, FTS) is built by `organon watch`.
> The **relationship graph and semantic vectors** are built by the Python indexer
> (`organon index`) — `watch` starts it automatically unless you pass `--no-index`.
> `graph`, `impact`, and semantic `search` are empty until indexing has run.

## Install

Requires **Rust stable**, **Python 3.12+**, and [`uv`](https://docs.astral.sh/uv/).

```bash
git clone https://github.com/andrii-su/organon.git
cd organon
bash setup.sh            # builds the binary + installs the Python layer
```

Or build manually:

```bash
cargo build --release   # target/release/organon
uv sync                 # Python AI layer (fastembed, lancedb, ...)
```

Prebuilt binaries for macOS and Linux are attached to each
[GitHub Release](https://github.com/andrii-su/organon/releases) (the graph,
lifecycle, history, and impact tools run from the binary alone; semantic vector
search needs the Python layer).

The first semantic index downloads the embedding model (`BAAI/bge-small-en-v1.5`, ~130 MB) once, then runs fully offline.

### Docker

A self-contained image (Rust binary + Python layer) is published to
`ghcr.io/andrii-su/organon`:

```bash
# Index a project (state persists in a named volume)
docker run --rm -v "$PWD:/workspace" -v organon-data:/data \
  ghcr.io/andrii-su/organon index /workspace

# Serve the MCP server over stdio
docker run -i --rm -v "$PWD:/workspace" -v organon-data:/data \
  ghcr.io/andrii-su/organon mcp --scope /workspace
```

Mount your project at `/workspace` and a named volume at `/data` (graph DB,
vectors, model cache).

## Quick Start

```bash
# 1. Index a workspace (starts the watcher + embedder; runs until stopped)
organon watch .

# 2. ...or index once and exit
organon index .

# 3. Inspect what Organon knows
organon stats
organon status src/auth.rs
organon health
```

```bash
# Ask the questions an agent would ask
organon search "authentication logic" --mode hybrid
organon impact src/auth.rs --depth 3
organon context "authentication refactor" --scope . --json
organon plan "change the auth flow" --file src/auth.rs
organon related-tests src/auth.rs
```

```bash
# Serve the graph to an agent over MCP
organon mcp             # stdio (Claude Desktop, Cursor, ...)
organon mcp --sse       # HTTP / SSE
organon mcp --scope ./src   # limit a session to a subtree
```

## What You Get

| | What |
|---|---|
| `organon` **CLI** | 20+ subcommands: watch, search, graph, impact, context, plan, health, doctor |
| **MCP Server** | 13 Rust-native tools over stdio or HTTP/SSE for any MCP client |
| **Entity graph** | Stable UUID per file, lifecycle, history, duplicates — in SQLite |
| **Semantic retrieval** | Local vector + FTS + hybrid search via fastembed & LanceDB |
| **Relationship graph** | Imports/references, reverse dependencies, impact analysis |
| **Agent helpers** | `context` packs, `plan` scaffolds, `related-tests` discovery |

## How It Works

1. **Watch** — `notify` turns filesystem events into entity create/modify/delete/rename.
1. **Identify** — each file gets a stable UUID, content hash, size, timestamps, and git author.
1. **Classify** — the lifecycle engine assigns `Born → Active → Dormant → Archived → Dead` from access/modify patterns.
1. **Index** — the Python layer extracts text (code, text, PDF), embeds it into LanceDB, and parses imports into the relationship graph.
1. **Serve** — the Rust MCP server answers agent queries directly from SQLite + LanceDB.

```text
fs events ─▶ organon-core (Rust) ─▶ SQLite graph ─┐
                  │                                 ├─▶ organon-mcp ─▶ AI agent
ai/ indexer (Py) ─┴─▶ LanceDB vectors ─────────────┘        (MCP)
```

Every entity carries its lifecycle and relationships, so agents never treat a 6-month-dormant file the same as one edited this morning.

## MCP Integration

**Claude Desktop** — add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "organon": {
      "command": "organon",
      "args": ["mcp", "--scope", "/path/to/your/project"],
      "env": {}
    }
  }
}
```

**Cursor** — add to `.cursor/mcp.json` in the project root (same shape). Full walkthroughs: [docs/claude-desktop.md](./docs/claude-desktop.md) · [docs/cursor.md](./docs/cursor.md).

A session is scoped to the current directory by default; pass a path, `--scope <dir>`, or `--global` to widen it.

### MCP Tools

| Tool | Description |
|---|---|
| `search_files` | Semantic, FTS, or hybrid search with metadata filters |
| `search_similar` | Find files similar to a given file (vector) |
| `get_entity` | Full metadata and lifecycle state for a path |
| `get_graph` | Import/reference graph rooted at a file |
| `get_impact` | Reverse dependencies — what breaks if this file changes |
| `get_history` | Lifecycle and content-change history for a file |
| `get_file_content` | Read file content (optionally a line range) |
| `build_context` | Compact, ranked context pack for a task |
| `find_duplicates` | Files sharing an identical content hash |
| `list_by_lifecycle` | Files filtered by lifecycle state |
| `list_saved_queries` | List named queries saved with `organon query save` |
| `run_saved_query` | Execute a saved query |
| `graph_stats` | Entity-graph statistics |

## Claude Code

Install the plugin from the marketplace:

```bash
claude plugin marketplace add andrii-su/organon
claude plugin install organon@organon
```

Or point Claude Code at the MCP server directly:

```bash
claude mcp add organon -- organon mcp --scope .
```

Then in-session: *"use organon to find what depends on src/auth.rs before we change it"*.

The `organon` binary must be on `PATH`. Other channels and their status are
tracked in [docs/marketplaces.md](./docs/marketplaces.md).

## CLI Reference

```bash
# ── Watcher & indexer ─────────────────────────────────────────
organon watch .                          # watch + auto-index (foreground)
organon watch /path --daemon             # detached background watcher
organon daemon list | status | logs <id> | stop <id>
organon index [path] [--watch <secs>]    # Python indexer (vectors + relations)

# ── Inspect ───────────────────────────────────────────────────
organon stats
organon status <file>
organon ls --state active
organon find --state dormant --ext rs --larger-than-mb 10

# ── Search & context ──────────────────────────────────────────
organon search "query" --mode hybrid --explain
organon search --like src/auth.rs --limit 5
organon context "task" --scope . --budget 12000 --json
organon plan "change X" --file src/x.rs
organon related-tests src/auth.rs

# ── Graph & impact ────────────────────────────────────────────
organon graph src/main.rs --depth 2 --format mermaid
organon impact src/auth.rs --depth 3
organon history src/auth.rs --limit 20
organon duplicates

# ── Maintenance ───────────────────────────────────────────────
organon health        # graph/index freshness
organon doctor        # diagnose install + Python deps
organon clean --dry-run
organon archive --dry-run
organon export --format json | csv | dot

# ── Config, queries, workspaces ───────────────────────────────
organon init
organon query save stale-rs --state dormant --ext rs
organon query list | run <name> | show <name> | delete <name>
organon workspace add . --default
organon completions bash
```

Global flags: `--db <path>` (before the subcommand), `--quiet`, `-v` (info), `-vv` (debug).

## Architecture

```text
crates/
  organon-core/   Rust library — entity graph, lifecycle engine, SQLite, watcher, scanner
  organon-cli/    Rust binary `organon` — 20+ subcommands, modular dispatch, Python bridge
  organon-mcp/    Rust — MCP server (stdio + HTTP/SSE), 13 tools
ai/
  indexer.py      Orchestrates extraction → embedding → relation indexing
  embeddings/     Local semantic vectors via fastembed + LanceDB
  extractor/      Content extraction (text, PDF, code)
  relations/      Import/reference graph extraction
docs/             Landing site + agent integration guides
```

| Layer | Technology |
|---|---|
| Core | Rust — `notify`, `tokio`, `rusqlite` (FTS5) |
| MCP server | Rust — `rmcp`, `axum` (SSE) |
| Semantic vectors | Python — `fastembed`, `lancedb` |
| Content extraction | Python — text, PDF, code |
| Storage | SQLite + LanceDB — 100% on disk |

## Configuration

Config lives at `~/.organon/config.toml` (write defaults with `organon init`). Key environment variables:

| Variable | Purpose |
|---|---|
| `ORGANON_HOME` | Base dir for all state (db, vectors, config, queries) — full sandbox switch |
| `ORGANON_DB` | Override the SQLite graph path |
| `ORGANON_CONFIG` / `ORGANON_QUERIES` | Explicit overrides for config / saved queries |
| `ORGANON_PYTHON_PROJECT` | Path to the organon repo if the CLI can't resolve it |
| `ORGANON_UV` | Path to `uv` for non-standard installs |

Defaults: `dormant_days=30`, `archive_days=90`, embed model `BAAI/bge-small-en-v1.5`, hybrid score `0.3 * fts + 0.7 * vector`.

## Development

```bash
# Rust
cargo build --release
cargo test --workspace --all-targets
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Python
uv run --group dev ruff check ai tests
uv run --group dev pytest
uv run python -m ai.indexer --health
```

The Rust CLI invokes Python via `uv run --project <organon-root> python -m ai.indexer`. If the repo root can't be resolved from the installed binary, set `ORGANON_PYTHON_PROJECT`.

## Principles

- **Local-first** — nothing leaves your machine unless you wire up another tool.
- **Agent-native** — MCP and task context are first-class product surfaces.
- **Lifecycle-aware** — dormant, archived, and dead files stay visible to agents, not anonymous blobs.
- **Small surface area** — fewer workflows, a stronger core.

## Links

- [docs/agent-usage.md](./docs/agent-usage.md) — how agents use Organon
- [docs/claude-desktop.md](./docs/claude-desktop.md) — Claude Desktop setup
- [docs/cursor.md](./docs/cursor.md) — Cursor setup
- [docs/marketplaces.md](./docs/marketplaces.md) — distribution & marketplace status
- [CONTRIBUTING.md](./CONTRIBUTING.md) — contributor workflow
- [Issues](https://github.com/andrii-su/organon/issues) — bugs, features, questions

## License

MIT
