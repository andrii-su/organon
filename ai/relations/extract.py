"""
Extract explicit import/reference relationships from source files.

Supports: Python (.py), Rust (.rs), TypeScript/JavaScript (.ts/.tsx/.js/.jsx).
Returns (from_path, to_path, kind) triples — only where to_path exists on disk.
Never raises; returns [] on any failure.
"""
import logging
import re
from pathlib import Path

logger = logging.getLogger(__name__)

# ── public API ────────────────────────────────────────────────────────────────

def extract_relations(path: str) -> list[tuple[str, str, str]]:
    """
    Return explicit file-level relations originating from `path`.
    Each relation is (from_path, to_path, kind).
    """
    try:
        p = Path(path)
        content = p.read_text(encoding="utf-8", errors="replace")
        base_dir = p.parent
        ext = p.suffix.lower()

        if ext == ".py":
            return _extract_python(path, content, base_dir)
        elif ext == ".rs":
            return _extract_rust(path, content, base_dir)
        elif ext in {".ts", ".tsx", ".js", ".jsx"}:
            return _extract_ts(path, content, base_dir)
        return []
    except Exception as e:
        logger.debug("extract_relations failed for %s: %s", path, e)
        return []


# ── extractors ────────────────────────────────────────────────────────────────

# `from .foo.bar import x` or `from ..foo import x`
_PY_REL_IMPORT = re.compile(
    r"^\s*from\s+(\.+)([\w.]*)\s+import\s+([\w,\s*]+)",
    re.MULTILINE,
)
# `from foo.bar import x` (absolute)
_PY_ABS_FROM = re.compile(
    r"^\s*from\s+([A-Za-z][\w.]*)\s+import",
    re.MULTILINE,
)
# `import foo.bar`
_PY_IMPORT = re.compile(
    r"^\s*import\s+([A-Za-z][\w.]*)",
    re.MULTILINE,
)

def _extract_python(from_path: str, content: str, base_dir: Path) -> list[tuple[str, str, str]]:
    rels = []

    # 1. Relative imports: `from . import utils` / `from ..common import foo`
    for m in _PY_REL_IMPORT.finditer(content):
        dots   = m.group(1)          # "." or ".."
        module = m.group(2).strip()  # module after dots (may be empty)
        names  = [n.strip() for n in m.group(3).split(",") if n.strip() and n.strip() != "*"]

        level = len(dots)
        anchor = base_dir
        for _ in range(level - 1):
            anchor = anchor.parent

        if module:
            # `from ..common import foo` → resolve common package, then look for foo inside
            pkg = _resolve_python_module(module, anchor)
            if pkg:
                rels.append((from_path, str(pkg), "imports"))
            # Also try resolving individual names as submodules of module
            pkg_dir = anchor
            for part in module.split("."):
                pkg_dir = pkg_dir / part
            for name in names:
                sub = _resolve_python_module(name, pkg_dir)
                if sub and str(sub) != str(pkg):
                    rels.append((from_path, str(sub), "imports"))
        else:
            # `from . import utils, helpers` → check each name in anchor dir
            for name in names:
                candidate = _resolve_python_module(name, anchor)
                if candidate:
                    rels.append((from_path, str(candidate), "imports"))

    # 2. Absolute from-imports: `from ai.embeddings.store import search`
    for m in _PY_ABS_FROM.finditer(content):
        candidate = _resolve_python_module(m.group(1), base_dir)
        if candidate:
            rels.append((from_path, str(candidate), "imports"))

    # 3. Bare imports: `import os.path` (only resolve if file exists)
    for m in _PY_IMPORT.finditer(content):
        candidate = _resolve_python_module(m.group(1), base_dir)
        if candidate:
            rels.append((from_path, str(candidate), "imports"))

    # Deduplicate
    seen: set[tuple[str, str, str]] = set()
    unique = []
    for r in rels:
        if r not in seen:
            seen.add(r)
            unique.append(r)
    return unique


def _resolve_python_module(module: str, base_dir: Path) -> Path | None:
    """Walk up from base_dir looking for a matching .py file or package."""
    if not module:
        return None
    parts = module.split(".")
    # Search from base_dir upward (up to 5 levels) to find a project root
    search_roots = [base_dir]
    cur = base_dir
    for _ in range(5):
        cur = cur.parent
        search_roots.append(cur)

    for root in search_roots:
        # e.g. ai.embeddings.store → ai/embeddings/store.py
        candidate = root.joinpath(*parts).with_suffix(".py")
        if candidate.exists():
            return candidate.resolve()
        # package init: ai/embeddings/__init__.py
        pkg = root.joinpath(*parts, "__init__.py")
        if pkg.exists():
            return pkg.resolve()
    return None


_RS_MOD = re.compile(r"^\s*mod\s+(\w+)\s*;", re.MULTILINE)

def _extract_rust(from_path: str, content: str, base_dir: Path) -> list[tuple[str, str, str]]:
    rels = []
    for m in _RS_MOD.finditer(content):
        name = m.group(1)
        # mod foo; → sibling foo.rs or foo/mod.rs
        for candidate in [
            base_dir / f"{name}.rs",
            base_dir / name / "mod.rs",
        ]:
            if candidate.exists():
                rels.append((from_path, str(candidate.resolve()), "mod"))
                break
    return rels


_TS_IMPORT = re.compile(
    r"""(?:import|export)\s+.*?from\s+['"]([^'"]+)['"]""",
    re.MULTILINE,
)
_TS_REQUIRE = re.compile(r"""require\s*\(\s*['"]([^'"]+)['"]\s*\)""")

_TS_EXTENSIONS = [".ts", ".tsx", ".js", ".jsx", "/index.ts", "/index.tsx", "/index.js"]

def _extract_ts(from_path: str, content: str, base_dir: Path) -> list[tuple[str, str, str]]:
    rels = []
    specifiers: list[str] = []
    for m in _TS_IMPORT.finditer(content):
        specifiers.append(m.group(1))
    for m in _TS_REQUIRE.finditer(content):
        specifiers.append(m.group(1))

    for spec in specifiers:
        if not spec.startswith("."):
            continue  # skip node_modules
        resolved = _resolve_ts(spec, base_dir)
        if resolved:
            rels.append((from_path, str(resolved), "imports"))
    return rels


def _resolve_ts(spec: str, base_dir: Path) -> Path | None:
    base = (base_dir / spec).resolve()
    # Try exact path first
    if base.exists() and base.is_file():
        return base
    # Try with extensions
    for ext in _TS_EXTENSIONS:
        candidate = Path(str(base) + ext) if not ext.startswith("/") else base / ext.lstrip("/")
        if candidate.exists():
            return candidate
    return None
