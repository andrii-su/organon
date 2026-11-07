# Organon — Claude Code plugin

Registers the [Organon](https://github.com/andrii-su/organon) MCP server in Claude Code.

## Install

```bash
claude plugin marketplace add andrii-su/organon
claude plugin install organon@organon
```

## Prerequisite

The `organon` binary must be on your `PATH`. Build it from the repo root:

```bash
cargo build --release   # then put target/release/organon on PATH, or run setup.sh
```

For semantic vector search, also install the Python layer (`uv sync`). The graph,
lifecycle, history, and impact tools work from the binary alone.

## What it registers

An MCP server named `organon` (stdio), scoped to the directory Claude Code is
launched from. It exposes search, entity-graph, lifecycle, impact, history,
duplicate-detection, and context-pack tools. See the
[main README](https://github.com/andrii-su/organon#mcp-tools) for the full tool list.

The `query-files` skill handles questions such as “Which files are responsible
for `/x/sds/sd`?” It asks the `query_route_files` MCP tool for direct indexed
matches and one-hop file relationships, reports the evidence without sending
file contents, then opens selected files if needed. Run `organon index` for the
workspace first. Statically resolvable FastAPI router prefixes are combined
into full routes; dynamic prefixes, unindexed files, or truncated content may
still be absent from the list.
