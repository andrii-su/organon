"""MCP server exposing Organon file graph to AI agents."""


def search_files(query: str, limit: int = 10) -> list[dict]:
    """Search files by semantic meaning."""
    # TODO: query lancedb vectors
    return []


def get_entity(path: str) -> dict | None:
    """Get full entity info for a file."""
    # TODO: query SQLite entity graph
    return None


def get_related(path: str, limit: int = 5) -> list[dict]:
    """Get files semantically related to this file."""
    # TODO: query relationship graph
    return []


def query_graph(nl_query: str) -> list[dict]:
    """Natural language query over the entire file graph."""
    # TODO: LLM-powered graph query
    return []
