# Agent Usage

Organon is a local-first semantic filesystem layer for AI agents. It builds and
maintains a graph of your codebase — entities, relationships, embeddings, and
history — and exposes that graph to agents through a single MCP server written
in Rust (`organon-mcp`).

## Quick Start

```bash
organon watch .     # build the SQLite graph and keep it live
organon index       # enrich with FTS, vectors, and relations
```

Then connect your agent client (Claude Desktop, Cursor, etc.) via the MCP
config below. That's it.

## MCP Server

The only MCP server is the Rust one built into the `organon` binary. There is
no separate Python server.

### Start in stdio mode (Claude Desktop, Cursor)

```bash
organon mcp
```

### Start in SSE / HTTP mode

```bash
organon mcp --sse
```

### Scope to a specific directory

```bash
organon mcp --scope /path/to/project
```

By default the server is scoped to the current working directory. Use
`--scope` to choose a different workspace root. Tool-level `path_prefix`
arguments can only narrow that session scope further.

Use `--global` only when the agent should see the entire shared graph across
all workspaces:

```bash
organon mcp --global
```

### Claude Desktop config

Add this to your Claude Desktop `mcpServers` config:

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

## Available MCP Tools

| Tool | What it does |
|---|---|
| `search_files` | Semantic + keyword search over indexed files |
| `get_entity` | Retrieve a single entity (function, class, module, etc.) |
| `get_graph` | Return graph edges for a file or entity |
| `get_file_content` | Read file content from the graph store |
| `build_context` | Assemble a compact context pack for a task description |
| `find_duplicates` | Identify exact duplicate groups |
| `get_history` | File and entity change history |
| `get_impact` | Reverse-dependency impact for a file |
| `list_saved_queries` | List persisted named queries |
| `run_saved_query` | Execute a named query |
| `list_by_lifecycle` | Filter entities by lifecycle state (active, stale, archived) |
| `stats` | Summary statistics for the current scope |

## Recommended Agent Workflow

1. Run `organon watch .` once (or keep it running for live updates).
2. Run `organon index` to build vector embeddings.
3. Connect Claude Desktop or Cursor using the MCP config above.
4. The agent then works through the MCP tools:
   - `search_files` to find relevant code.
   - `get_entity` and `get_graph` to understand entry points and dependencies.
   - `get_file_content` to read the actual source.
   - `get_impact` before editing to understand blast radius.
   - `get_history` to distinguish active files from stale context.
   - `build_context` for a single compact pack when starting a new task.

## Useful Local Commands

These are CLI commands for debugging and local exploration outside of the agent
session:

```bash
organon health .
organon stats
organon search "task description" --mode hybrid --explain
organon context "task description" --scope . --budget 12000 --json
organon context --path src/auth.rs --budget 8000 --json
organon graph path/to/file.rs --depth 2 --format text
organon impact path/to/file.rs --depth 3
organon cleanup --dry-run
```

`organon context` produces the same shape as the `build_context` MCP tool and
is useful for inspecting what the agent receives.

## Scoping

Organon uses a shared SQLite database under `~/.organon` by default. When
indexing or watching a specific workspace, pass the path explicitly to keep
results scoped:

```bash
organon index /path/to/workspace
organon watch /path/to/workspace
```

The MCP server inherits this scoping from `--scope` or from the current working
directory when launched by the agent client.
