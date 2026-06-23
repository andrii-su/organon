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
