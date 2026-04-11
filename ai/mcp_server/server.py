"""
Organon MCP server.

Exposes the local file graph to AI agents (Claude, Cursor, etc.)
via the Model Context Protocol.

Run:
    uv run python -m ai.mcp_server.server       # stdio (for Claude Desktop)
    uv run python -m ai.mcp_server.server --sse  # SSE transport (for Cursor)
"""
import logging
import sqlite3
import sys
from pathlib import Path

from mcp.server.fastmcp import FastMCP

from ai.embeddings.store import get_indexed_hashes
from ai.embeddings.store import search as vector_search
from ai.extractor.extract import extract_text
from ai.relations.store import get_graph as _get_graph
from ai.query.nl_query import run_nl_query

logger = logging.getLogger(__name__)

DB_PATH = Path("~/.organon/entities.db").expanduser()

mcp = FastMCP("organon")


# ── helpers ───────────────────────────────────────────────────────────────────

def _db():
    conn = sqlite3.connect(str(DB_PATH))
    conn.row_factory = sqlite3.Row
    return conn


def _row_to_dict(row) -> dict:
    return dict(row)


# ── tools ─────────────────────────────────────────────────────────────────────

@mcp.tool()
def search_files(query: str, limit: int = 10, path_prefix: str | None = None) -> list[dict]:
    """
    Search files by semantic meaning.
    Returns files most relevant to the query, ranked by similarity.

    Args:
        query: Natural language description of what you're looking for.
        limit: Max results (default 10).
        path_prefix: Optional directory prefix to scope the search (e.g. "src/").
    """
    logger.info("search_files: %r limit=%d prefix=%r", query, limit, path_prefix)
    results = vector_search(query, limit=limit, path_prefix=path_prefix)
    logger.debug("search_files: %d results", len(results))
    return results


@mcp.tool()
def get_entity(path: str) -> dict | None:
    """
    Get full metadata for a file: lifecycle state, size, hash, summary.

    Args:
        path: Absolute path to the file.
    """
    logger.debug("get_entity: %s", path)
    with _db() as conn:
        row = conn.execute(
            "SELECT * FROM entities WHERE path = ?", (path,)
        ).fetchone()
        if row is None:
            logger.debug("get_entity: not found: %s", path)
        return _row_to_dict(row) if row else None


@mcp.tool()
def get_related(path: str, limit: int = 5) -> list[dict]:
    """
    Find files semantically related to a given file.
    Uses vector similarity — finds files with similar content/purpose.

    Args:
        path: Absolute path to the source file.
        limit: Max related files (default 5).
    """
    logger.debug("get_related: %s limit=%d", path, limit)
    text = extract_text(path)
    if not text:
        logger.warning("get_related: no text extracted from %s", path)
        return []
    results = vector_search(text[:1000], limit=limit + 1)
    related = [r for r in results if r["path"] != path][:limit]
    logger.debug("get_related: %d related files", len(related))
    return related


@mcp.tool()
def list_by_lifecycle(state: str, limit: int = 20) -> list[dict]:
    """
    List files by lifecycle state.

    Args:
        state: One of: born, active, dormant, archived, dead.
        limit: Max results (default 20).
    """
    valid = {"born", "active", "dormant", "archived", "dead"}
    if state not in valid:
        return [{"error": f"invalid state '{state}'. Choose from: {sorted(valid)}"}]

    logger.debug("list_by_lifecycle: state=%s limit=%d", state, limit)
    with _db() as conn:
        rows = conn.execute(
            "SELECT path, lifecycle, size_bytes, modified_at, accessed_at "
            "FROM entities WHERE lifecycle = ? ORDER BY accessed_at DESC LIMIT ?",
            (state, limit),
        ).fetchall()
        return [_row_to_dict(r) for r in rows]


@mcp.tool()
def get_file_content(path: str) -> dict:
    """
    Extract and return the text content of a file.
    Supports: source code, markdown, text, PDF.

    Args:
        path: Absolute path to the file.
    """
    logger.debug("get_file_content: %s", path)
    text = extract_text(path)
    if text is None:
        logger.warning("get_file_content: cannot extract: %s", path)
        return {"error": f"cannot extract text from: {path}"}
    return {"path": path, "content": text, "chars": len(text)}


@mcp.tool()
def graph_stats() -> dict:
    """
    Return summary statistics of the organon entity graph.
    Shows total files, lifecycle distribution, and vector index coverage.
    """
    logger.debug("graph_stats called")
    with _db() as conn:
        total = conn.execute("SELECT COUNT(*) FROM entities").fetchone()[0]
        by_lifecycle = {
            row["lifecycle"]: row["cnt"]
            for row in conn.execute(
                "SELECT lifecycle, COUNT(*) as cnt FROM entities GROUP BY lifecycle"
            ).fetchall()
        }
        indexed_count = len(get_indexed_hashes())

    logger.info("graph_stats: total=%d indexed=%d", total, indexed_count)
    return {
        "total_entities": total,
        "by_lifecycle":   by_lifecycle,
        "vector_indexed": indexed_count,
        "db_path":        str(DB_PATH),
    }


@mcp.tool()
def get_graph(path: str, depth: int = 1) -> dict:
    """
    Return the import/reference relationship graph rooted at a file.
    Traverses explicit edges (import, mod, require) up to `depth` hops.

    Args:
        path: Absolute path to the root file.
        depth: BFS depth (1-3, default 1).
    """
    logger.debug("get_graph: %s depth=%d", path, depth)
    result = _get_graph(path, depth=min(depth, 3))
    logger.debug("get_graph: %d nodes %d edges", len(result["nodes"]), len(result["edges"]))
    return result


@mcp.tool()
def query_graph(nl_query: str) -> dict:
    """
    Natural language query over the entity graph.
    Uses ollama (llama3.2 by default) to translate your question into SQL,
    then executes it against the local entities.db.
    Falls back to listing recently accessed files when ollama is unavailable.

    Args:
        nl_query: Plain English question, e.g. "show dormant files larger than 10KB".
    """
    logger.info("query_graph: %r", nl_query)
    result = run_nl_query(nl_query, db_path=DB_PATH)
    logger.debug("query_graph: mode=%s results=%d", result.get("mode"), len(result.get("results", [])))
    return result


# ── resources ─────────────────────────────────────────────────────────────────

@mcp.resource("organon://entities")
def entities_resource() -> str:
    """All entities in the graph as a summary list."""
    with _db() as conn:
        rows = conn.execute(
            "SELECT path, lifecycle, size_bytes FROM entities ORDER BY accessed_at DESC"
        ).fetchall()
    lines = [f"{r['lifecycle']:8s}  {r['path']}" for r in rows]
    return "\n".join(lines)


@mcp.resource("organon://entity/{path}")
def entity_resource(path: str) -> str:
    """Entity metadata for a specific file path."""
    entity = get_entity(path)
    if entity is None:
        return f"not found: {path}"
    return "\n".join(f"{k}: {v}" for k, v in entity.items())


# ── entry point ───────────────────────────────────────────────────────────────

def main() -> None:
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    sse = "--sse" in sys.argv
    logger.info("starting MCP server (transport=%s)", "sse" if sse else "stdio")
    if sse:
        mcp.run(transport="sse")
    else:
        mcp.run(transport="stdio")


if __name__ == "__main__":
    main()
