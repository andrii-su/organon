# Organon

> *"The instrument of thought"* — Aristotle

**Organon** is a local-first semantic layer over your filesystem. Every file becomes a living entity — with identity, context, relationships, and a lifecycle. Built for humans and AI agents alike.

```text
Your Filesystem
      ↓
 [ Organon Core ]
  • Entity graph      — what each file is, why it exists, what it relates to
  • Lifecycle engine  — born → active → dormant → archived → dead
  • Semantic search   — find files by meaning, not just name
      ↓                    ↓
 MCP Server           Menu bar app (coming)
 AI agents query      Humans see the living graph
 your file graph
```

## Why

Files are not static blobs. They're created with intent, evolve over time, relate to other files, and eventually become irrelevant. No existing tool treats them this way.

- **For you:** your filesystem organizes itself. Files move, archive, and surface based on context — not manual rules.
- **For your agents:** Claude, Cursor, Nova — they see a semantic graph of your world, not raw bytes.

## Architecture

```text
crates/
  organon-core/   Rust: filesystem watcher, entity graph (SQLite), lifecycle engine
  organon-mcp/    Rust: MCP server exposing the graph to AI agents
  organon-cli/    Rust: CLI for querying and managing entities
ai/
  extractor/      Python: content extraction (text, PDF, code, images)
  embeddings/     Python: local semantic vectors via fastembed + lancedb
  mcp_server/     Python: MCP tools (search, query, relate)
```

## Stack

| Layer | Technology |
| ----- | ---------- |
| Core daemon | Rust (`notify`, `tokio`, `rusqlite`, `tantivy`) |
| Semantic vectors | Python (`fastembed`, `lancedb`) |
| Local LLM | `ollama` |
| Agent protocol | MCP (Model Context Protocol) |
| Storage | SQLite + LanceDB — 100% local |
| Future UI | Tauri |

## Principles

- **Local-first.** Nothing leaves your machine without explicit permission.
- **Open.** MIT license. No telemetry. No accounts.
- **Agent-native.** MCP server from day one.
- **Lifecycle-aware.** Files are organisms, not static objects.

## Status

🌱 Early development — building the core entity graph.

## CLI Highlights

```bash
# watch one or more roots (CLI path + config.watch.roots)
organon watch .

# metadata find with old and new filters
organon find --state active --ext rs
organon find --modified-after 2026-01-01 --larger-than-mb 10
organon find --created-after 2026-01-01

# search with metadata filters across vector / fts / hybrid
organon search "watcher" --state active --ext rs --mode hybrid
organon search "sqlite graph" --modified-after 2026-01-01

# graph output, cycle warnings, export-friendly formats
organon graph path/to/file.rs --depth 2 --format text
organon graph path/to/file.rs --format dot
organon graph path/to/file.rs --format mermaid

# compare filesystem vs DB, export data, recompute one summary
organon diff .
organon diff . --json
organon export --format json
organon export --format csv --output entities.csv
organon export --format dot --output graph.dot
organon summarize path/to/file.rs --model llama3.2
```

Global logging flags:

- `--quiet` keeps output near-silent except errors
- `-v` enables info logs
- `-vv` enables debug logs

## Roadmap

- [ ] `organon-core`: filesystem watcher + SQLite entity graph
- [ ] `organon-core`: lifecycle state machine
- [ ] `ai/extractor`: content extraction (text, PDF, code)
- [ ] `ai/embeddings`: local semantic vectors with fastembed
- [ ] `organon-mcp`: MCP server (search, query, relate)
- [ ] `organon-cli`: CLI for power users
- [ ] Dogfood: integrate with OpenClaw/Nova
- [ ] macOS menu bar app (Tauri)

## License

MIT
