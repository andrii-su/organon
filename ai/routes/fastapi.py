"""Statically discover FastAPI routes assembled from included routers."""

import ast
import json
import sqlite3
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class RouteRecord:
    route: str
    handler_path: str
    provenance: tuple[str, ...]


def _route(path: str) -> str:
    return "/" + path.strip("/") if path.strip("/") else "/"


def _join(prefix: str, path: str) -> str:
    return _route(prefix.rstrip("/") + "/" + path.lstrip("/"))


def _module_path(root: Path, module: str) -> Path | None:
    target = root.joinpath(*module.split("."))
    for candidate in (target.with_suffix(".py"), target / "__init__.py"):
        if candidate.is_file():
            return candidate.resolve()
    return None


def _aliases(tree: ast.AST, root: Path, file: Path) -> dict[str, Path]:
    aliases = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ImportFrom):
            module = node.module or ""
            if node.level:
                package = file.relative_to(root).with_suffix("").parts[:-1]
                if node.level > len(package) + 1:
                    continue
                module = ".".join((*package[: len(package) - node.level + 1], module))
            for alias in node.names:
                target = _module_path(root, f"{module}.{alias.name}")
                if target is None:
                    target = _module_path(root, module)
                if target is not None:
                    aliases[alias.asname or alias.name] = target
        elif isinstance(node, ast.Import):
            for alias in node.names:
                target = _module_path(root, alias.name)
                if target is not None:
                    aliases[alias.asname or alias.name.split(".")[0]] = target
    return aliases


def _target(node: ast.AST, aliases: dict[str, Path]) -> Path | None:
    if isinstance(node, ast.Name):
        return aliases.get(node.id)
    if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
        return aliases.get(node.value.id)
    return None


def _keyword(call: ast.Call, name: str) -> ast.AST | None:
    return next((item.value for item in call.keywords if item.arg == name), None)


def _prefix(
    node: ast.AST | None, settings: dict[str, list[tuple[str, Path]]]
) -> tuple[str, Path | None] | None:
    if node is None:
        return "", None
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value, None
    if (
        isinstance(node, ast.Attribute)
        and isinstance(node.value, ast.Name)
        and node.value.id == "settings"
    ):
        values = settings.get(node.attr, [])
        if len(values) == 1:
            return values[0]
    return None


def discover_routes(root: Path, paths: list[str] | None = None) -> list[RouteRecord]:
    root = root.resolve()
    files = [Path(path) for path in paths] if paths is not None else root.rglob("*.py")
    trees: dict[Path, ast.AST] = {}
    for file in files:
        if file.suffix != ".py" or not file.is_file():
            continue
        try:
            trees[file.resolve()] = ast.parse(file.read_text(encoding="utf-8"))
        except (OSError, UnicodeError, SyntaxError):
            continue

    settings: dict[str, list[tuple[str, Path]]] = {}
    for file, tree in trees.items():
        for node in ast.walk(tree):
            if isinstance(node, (ast.Assign, ast.AnnAssign)):
                names = node.targets if isinstance(node, ast.Assign) else [node.target]
                value = node.value
                if isinstance(value, ast.Constant) and isinstance(value.value, str):
                    for name in names:
                        if isinstance(name, ast.Name):
                            settings.setdefault(name.id, []).append((value.value, file))

    mounts: dict[Path, list[tuple[Path, str, tuple[Path, ...]]]] = {}
    unresolved: set[Path] = set()
    endpoints: list[tuple[Path, str]] = []
    for file, tree in trees.items():
        aliases = _aliases(tree, root, file)
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                for decorator in node.decorator_list:
                    if (
                        isinstance(decorator, ast.Call)
                        and isinstance(decorator.func, ast.Attribute)
                        and decorator.func.attr
                        in {"get", "post", "put", "patch", "delete", "options", "head", "api_route"}
                        and decorator.args
                        and isinstance(decorator.args[0], ast.Constant)
                        and isinstance(decorator.args[0].value, str)
                    ):
                        endpoints.append((file, decorator.args[0].value))
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "include_router"
                and node.args
            ):
                child = _target(node.args[0], aliases)
                if child is None or child not in trees:
                    continue
                resolved = _prefix(_keyword(node, "prefix"), settings)
                if resolved is None:
                    unresolved.add(child)
                    continue
                prefix, source = resolved
                origins = (file,) if source is None or source == file else (file, source)
                mounts.setdefault(child, []).append((file, prefix, origins))

    records: set[RouteRecord] = set()

    def ascend(file: Path, route: str, origins: tuple[Path, ...], visited: frozenset[Path]) -> None:
        if file in visited:
            return
        parents = mounts.get(file, [])
        if not parents:
            if file not in unresolved:
                records.add(
                    RouteRecord(
                        _route(route), str(origins[0]), tuple(dict.fromkeys(map(str, origins)))
                    )
                )
            return
        for parent, prefix, prefix_origins in parents:
            ascend(parent, _join(prefix, route), origins + prefix_origins, visited | {file})

    for file, route in endpoints:
        ascend(file, route, (file,), frozenset())
    return sorted(records, key=lambda record: (record.route, record.handler_path))


def replace_route_index(
    db_path: Path, records: list[RouteRecord], path_prefixes: list[str]
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "CREATE TABLE IF NOT EXISTS indexed_routes ("
            "route TEXT NOT NULL, handler_path TEXT NOT NULL, provenance TEXT NOT NULL, "
            "PRIMARY KEY(route, handler_path))"
        )
        if not path_prefixes:
            connection.execute("DELETE FROM indexed_routes")
        for prefix in path_prefixes:
            connection.execute(
                "DELETE FROM indexed_routes WHERE handler_path = ? OR substr(handler_path, 1, length(?)) = ?",
                (prefix, f"{prefix}/", f"{prefix}/"),
            )
        connection.executemany(
            "INSERT OR REPLACE INTO indexed_routes(route, handler_path, provenance) VALUES (?, ?, ?)",
            [
                (record.route, record.handler_path, json.dumps(record.provenance))
                for record in records
            ],
        )
