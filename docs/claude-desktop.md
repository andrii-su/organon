# Claude Desktop Integration

Organon exposes its entity graph and search through an MCP server. Claude Desktop can connect to that server and use it as a local context source while you work.

## Prerequisites

- Organon installed (`bash setup.sh` from the repository root)
- Claude Desktop installed (download from [claude.ai/download](https://claude.ai/download))

## Step 1 — Index your workspace

Run the watcher first. It builds the SQLite entity graph by scanning the directory tree and tracking filesystem changes:

```bash
organon watch /path/to/your/project
```

Once the initial scan finishes, run the indexer to enrich the graph with full-text search, semantic vectors, and import/reference relations:

```bash
organon index /path/to/your/project
```

For long-running sessions, start the watcher as a background daemon so it keeps the graph warm after the terminal closes:

```bash
organon watch /path/to/your/project --daemon
organon daemon status
```

Check that indexing looks healthy before connecting Claude:

```bash
organon health /path/to/your/project
organon stats
```

## Step 2 — Configure Claude Desktop

Open or create the Claude Desktop configuration file:

```
~/Library/Application Support/Claude/claude_desktop_config.json
```

Add an entry under `mcpServers`. Replace `/path/to/your/project` with the absolute path you want to expose:

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

If you want Claude to see your entire shared Organon graph across all indexed workspaces, use `--global` instead of a path:

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

Restart Claude Desktop after saving the file. The MCP server will appear in the tool panel when you open a conversation.

## Step 3 — Verify the connection

Ask Claude to run a quick sanity check:

```
Run the stats tool to confirm Organon is connected.
```

Claude should respond with entity counts, index freshness, and graph statistics.

## Scope

By default `organon mcp` scopes tool access to the current working directory. You can control this explicitly:

| Invocation | Scope |
|---|---|
| `organon mcp` | Current directory at startup |
| `organon mcp /path/to/project` | That specific directory |
| `organon mcp --scope /path/to/project` | Same as above, explicit flag |
| `organon mcp --global` | Entire shared graph (`~/.organon/entities.db`) |

Scoping matters: a narrower scope keeps Claude from surfacing results from unrelated workspaces. Use `--global` only when you intentionally want cross-workspace queries.

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

The following prompts work well once the MCP server is connected.

**Explore a codebase before making changes:**

```
Use search_files to find all files related to authentication, then
get_graph on the main auth entry point to see what depends on it.
```

**Check impact before editing a file:**

```
I'm going to refactor src/config.rs. Use get_impact to show me
everything that depends on it, then list any active files in that
dependency tree.
```

**Find stale context:**

```
Use list_by_lifecycle with state "dormant" to show me files that
haven't changed recently, then check if any of them appear in the
import graph for src/main.rs.
```

**Build a compact context pack for a task:**

```
Use build_context with the query "add rate limiting to the API"
and a budget of 12000 tokens. Then show me the top files and
explain what changes are needed.
```

**Audit for duplicate files:**

```
Run find_duplicates and group the results. For each duplicate group,
use get_history to see which copy is more recently active.
```

**Understand a file's full history:**

```
Use get_history on src/auth/session.rs to show me all lifecycle
transitions. Then use get_entity to check its current state.
```

**Run a saved query:**

```
List my saved queries with list_saved_queries, then run the one
named "active-api-surface".
```

## Keeping the Index Fresh

The daemon keeps the graph updated as you edit files:

```bash
organon daemon list          # list running watchers
organon daemon logs <id>     # tail logs for a watcher
organon daemon stop <id>     # stop a watcher
```

If you add a large batch of new files outside the watcher (for example after a `git pull`), re-run indexing:

```bash
organon index /path/to/your/project
```

Use `organon health /path/to/your/project` any time you want to check graph freshness before a Claude session.
