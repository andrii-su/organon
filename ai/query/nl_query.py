"""
Natural-language → SQL query over the organon entity graph.

Uses ollama (local LLM) to generate SQL, validates it, then executes against
entities.db. Falls back to a safe default SELECT when generation fails.

Environment:
    ORGANON_OLLAMA_MODEL  override model (default: llama3.2)
"""

import logging
import os
import re
import sqlite3
from pathlib import Path

logger = logging.getLogger(__name__)

DB_PATH = Path("~/.organon/entities.db").expanduser()

OLLAMA_MODEL = os.environ.get("ORGANON_OLLAMA_MODEL", "llama3.2")

SCHEMA_PROMPT = """
You are an SQL expert. Given a natural language question, write a read-only
SQLite SELECT query against this schema:

TABLE entities (
    id           TEXT PRIMARY KEY,
    path         TEXT NOT NULL UNIQUE,   -- absolute file path
    name         TEXT NOT NULL,          -- file name
    extension    TEXT,                   -- e.g. ".rs", ".py"
    size_bytes   INTEGER,                -- file size in bytes
    created_at   INTEGER,                -- unix timestamp
    modified_at  INTEGER,                -- unix timestamp
    accessed_at  INTEGER,                -- unix timestamp
    lifecycle    TEXT,                   -- 'born'|'active'|'dormant'|'archived'|'dead'
    content_hash TEXT,
    summary      TEXT
);

TABLE relationships (
    from_path  TEXT NOT NULL,
    to_path    TEXT NOT NULL,
    kind       TEXT NOT NULL,            -- 'imports'|'mod'
    created_at INTEGER
);

Rules:
- Only SELECT statements are allowed.
- Do not use DROP, DELETE, UPDATE, INSERT, ATTACH, PRAGMA.
- Timestamps are Unix seconds (integer). Use strftime or arithmetic for date maths.
- Lifecycle values: born, active, dormant, archived, dead.
- Return only the SQL query, no explanation.

Question: {question}
"""

_FENCE_RE = re.compile(r"```(?:sql)?\s*(.*?)```", re.DOTALL | re.IGNORECASE)
_SELECT_RE = re.compile(r"(SELECT\b.*)", re.DOTALL | re.IGNORECASE)

_FORBIDDEN = re.compile(
    r"\b(DROP|DELETE|UPDATE|INSERT|ATTACH|PRAGMA)\b",
    re.IGNORECASE,
)

_FALLBACK_SQL = (
    "SELECT path, lifecycle, size_bytes, modified_at "
    "FROM entities ORDER BY accessed_at DESC LIMIT 20"
)


# ── validation + extraction ───────────────────────────────────────────────────


def _validate_sql(sql: str) -> bool:
    """Return True iff sql is a safe SELECT-only statement."""
    stripped = sql.strip()
    if not stripped.upper().startswith("SELECT"):
        logger.debug("_validate_sql: rejected (not SELECT): %r", stripped[:80])
        return False
    if _FORBIDDEN.search(stripped):
        logger.debug("_validate_sql: rejected (forbidden keyword): %r", stripped[:80])
        return False
    return True


def _extract_sql(text: str) -> str | None:
    """Extract a SQL query from an LLM response (code fence or bare SELECT)."""
    m = _FENCE_RE.search(text)
    if m:
        sql = m.group(1).strip()
        if _validate_sql(sql):
            return sql

    m = _SELECT_RE.search(text)
    if m:
        sql = m.group(1).strip()
        if _validate_sql(sql):
            return sql

    return None


# ── ollama call ───────────────────────────────────────────────────────────────


def generate_sql(nl_query: str) -> str | None:
    """Call ollama to generate SQL. Returns None on any failure."""
    try:
        import ollama  # optional dependency
    except ImportError:
        logger.warning("ollama package not installed — NL query unavailable")
        return None

    prompt = SCHEMA_PROMPT.format(question=nl_query)
    try:
        response = ollama.chat(
            model=OLLAMA_MODEL,
            messages=[{"role": "user", "content": prompt}],
            options={"temperature": 0},
        )
        text = response["message"]["content"]
        logger.debug("ollama response: %r", text[:200])
        return _extract_sql(text)
    except Exception as e:
        logger.warning("ollama call failed: %s", e)
        return None


# ── main entry point ──────────────────────────────────────────────────────────


def run_nl_query(nl_query: str, db_path=DB_PATH) -> dict:
    """
    Generate SQL from nl_query, execute against db_path, return results.

    Returns:
        {results: [...], sql: str, mode: "generated"|"fallback"}
    """
    sql = generate_sql(nl_query)
    mode = "generated"
    if not sql:
        logger.info("falling back to default query for: %r", nl_query)
        sql = _FALLBACK_SQL
        mode = "fallback"

    logger.debug("run_nl_query: mode=%s sql=%r", mode, sql[:120])

    try:
        conn = sqlite3.connect(str(db_path))
        conn.row_factory = sqlite3.Row
        rows = conn.execute(sql).fetchall()
        results = [dict(r) for r in rows]
        conn.close()
        logger.info("run_nl_query: %d results (mode=%s)", len(results), mode)
        return {"results": results, "sql": sql, "mode": mode}
    except Exception as e:
        logger.warning("run_nl_query execution error: %s", e)
        return {"results": [], "sql": sql, "mode": mode, "error": str(e)}
