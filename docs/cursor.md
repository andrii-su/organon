# Cursor IDE Integration

Organon exposes its entity graph and search through an MCP server. Cursor can connect to that server and use it as a local context source from within the editor.

## Prerequisites

- Organon installed (`bash setup.sh` from the repository root)
- Cursor 0.40 or later (MCP support is available in recent builds)

## Step 1 — Index your workspace

Run the watcher first. It builds the SQLite entity graph by scanning the directory tree and tracking filesystem changes:

```bash
organon watch /path/to/your/project
```

Once the initial scan finishes, run the indexer to enrich the graph with full-text search, semantic vectors, and import/reference relations:

```bash
organon index /path/to/your/project
```

For long-running editor sessions, start the watcher as a background daemon:

```bash
organon watch /path/to/your/project --daemon
organon daemon status
```

Check that indexing looks healthy before opening Cursor:

```bash
organon health /path/to/your/project
organon stats
```

## Step 2 — Configure Cursor

Cursor reads MCP configuration from `.cursor/mcp.json` in the project root. Create the file if it does not exist:

```bash
mkdir -p /path/to/your/project/.cursor
```

Add the following content. Replace `/path/to/your/project` with the absolute path to the directory you want to expose:

```json
{
  "mcpServers": {
    "organon": {
      "command": "organon",
      "args": ["mcp", "/path/to/your/project"]
    }
  }
}
```

To expose the entire shared Organon graph across all indexed workspaces, use `--global`:

```json
{
  "mcpServers": {
    "organon": {
      "command": "organon",
      "args": ["mcp", "--global"]
    }
  }
}
```

Reload the Cursor window after saving the file (`Cmd+Shift+P` > "Developer: Reload Window"). The Organon tools will appear in the agent tool list.

## Step 3 — Verify the connection

Open Cursor's AI chat and ask:

```
Run the stats tool to confirm Organon is connected.
```

The agent should respond with entity counts, index coverage, and graph statistics from your local workspace.

## Scope

By default `organon mcp` scopes tool access to the current working directory at startup. You can control this explicitly:

| Invocation | Scope |
|---|---|
| `organon mcp` | Current directory at startup |
| `organon mcp /path/to/project` | That specific directory |
| `organon mcp --scope /path/to/project` | Same as above, explicit flag |
| `organon mcp --global` | Entire shared graph (`~/.organon/entities.db`) |

For a per-project `.cursor/mcp.json`, pass the project path explicitly in `args` so the scope is fixed regardless of which directory Cursor launches from. Use `--global` only when you intentionally want cross-workspace queries.

Individual tool calls can narrow scope further with a `path_prefix` argument, but they cannot widen beyond the session scope.

## MCP Tools

| Tool | Description |
|---|---|
| `search_files(query, mode, limit)` | Search indexed files. `mode` is `semantic`, `fts`, or `hybrid`. Returns ranked file paths and snippets. |
| `get_entity(path)` | Full metadata for a file: lifecycle state, size, content hash, summary, last modified. |
| `get_graph(path, depth)` | Import and reference graph rooted at a file. Returns edges up to the given depth. |
| `get_file_content(path)` | Read a file's content through Organon. Respects session scope. |
| `build_context(query, scope, budget)` | Compact context pack for a task. Returns ranked snippets and metadata within a token budget. |
| `find_duplicates()` | Files that share identical content hashes. |
| `get_history(path)` | Lifecycle transitions and change history for a file. |
| `get_impact(path, depth)` | Reverse dependency analysis: which files would be affected if this file changes. |
| `list_saved_queries()` | Named queries previously saved with `organon query save`. |
| `run_saved_query(name)` | Execute a saved query by name. |
| `list_by_lifecycle(state)` | Files in a given lifecycle state: `born`, `active`, `dormant`, `archived`, or `dead`. |
| `stats()` | Entity graph statistics: file counts, index coverage, graph size. |

## Example Prompts

The following prompts work well in Cursor's agent mode once the MCP server is connected.

**Understand a file before editing it:**

```
Use get_entity on src/server.rs, then get_graph with depth 2 to show
its dependencies and the files that import it.
```

**Check impact before a refactor:**

```
I'm going to change the signature of the Config struct in src/config.rs.
Use get_impact with depth 3 to show me everything that would break.
```

**Get a context pack for a task:**

```
Use build_context with the query "implement request retry logic"
and a budget of 10000 tokens. Use the result as context before
suggesting an implementation plan.
```

**Find recently changed active files:**

```
Use list_by_lifecycle with state "active" to list recently active files,
then search_files with mode "hybrid" for "error handling" to find
relevant ones.
```

**Locate duplicate assets or generated files:**

```
Run find_duplicates. For each group with more than one copy,
use get_entity on each to compare their lifecycle states.
```

**Review the history of a file:**

```
Use get_history on src/db/migrations.rs to show all lifecycle transitions
and recent changes, then summarize whether it is safe to delete.
```

**Use a saved query:**

```
Run list_saved_queries to show available named queries, then run
the one named "untested-modules".
```

## Keeping the Index Fresh

The daemon keeps the graph updated as you edit files inside Cursor:

```bash
organon daemon list          # list running watchers
organon daemon logs <id>     # tail logs for a watcher
organon daemon stop <id>     # stop a watcher
```

After a large batch of file changes (for example after switching branches or pulling upstream):

```bash
organon index /path/to/your/project
```

Use `organon health /path/to/your/project` to check freshness at any time.

## Committing `.cursor/mcp.json`

It is safe to commit `.cursor/mcp.json` to the repository. Teammates who have Organon installed can index their own copy of the workspace and connect with the same configuration. Add the file to `.gitignore` only if the path in `args` is machine-specific and you do not want to share it.
