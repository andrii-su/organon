import sqlite3
from pathlib import Path

from ai.routes.fastapi import discover_routes, replace_route_index


def test_discovers_composed_route_and_provenance(tmp_path: Path):
    app = tmp_path / "app"
    routes = app / "routes"
    routes.mkdir(parents=True)
    (app / "main.py").write_text(
        "from app.routes.api import router as api_router\n"
        "application.include_router(api_router, prefix=settings.api_prefix)\n"
    )
    (routes / "api.py").write_text(
        "from app.routes import auth\nrouter.include_router(auth.router, prefix='/users')\n"
    )
    (routes / "auth.py").write_text(
        "from fastapi import APIRouter\n"
        "router = APIRouter()\n"
        "@router.post('/login')\n"
        "def login(): pass\n"
    )
    (app / "settings.py").write_text("class Settings:\n    api_prefix = '/api'\n")

    records = discover_routes(tmp_path)

    assert len(records) == 1
    assert records[0].route == "/api/users/login"
    assert records[0].handler_path == str((routes / "auth.py").resolve())
    assert set(records[0].provenance) == {
        str((app / "main.py").resolve()),
        str((routes / "api.py").resolve()),
        str((routes / "auth.py").resolve()),
        str((app / "settings.py").resolve()),
    }


def test_does_not_infer_ambiguous_dynamic_prefix(tmp_path: Path):
    (tmp_path / "main.py").write_text(
        "import child\nrouter.include_router(child.router, prefix=settings.api_prefix)\n"
    )
    (tmp_path / "child.py").write_text("@router.get('/login')\ndef login(): pass\n")
    assert discover_routes(tmp_path) == []


def test_discovers_router_imported_from_relative_package(tmp_path: Path):
    package = tmp_path / "app"
    package.mkdir()
    (package / "api.py").write_text(
        "from . import auth\nrouter.include_router(auth.router, prefix='/users')\n"
    )
    (package / "auth.py").write_text("@router.get('/login')\ndef login(): pass\n")

    records = discover_routes(tmp_path)

    assert [(record.route, record.handler_path) for record in records] == [
        ("/users/login", str((package / "auth.py").resolve()))
    ]


def test_discovers_absolute_import_when_index_scope_is_package_root(tmp_path: Path):
    app = tmp_path / "app"
    routes = app / "api" / "routes"
    routes.mkdir(parents=True)
    (app / "__init__.py").write_text("")
    (routes / "__init__.py").write_text("")
    (routes / "api.py").write_text(
        "from app.api.routes import authentication\n"
        "router.include_router(authentication.router, prefix='/users')\n"
    )
    handler = routes / "authentication.py"
    handler.write_text("@router.post('/login')\ndef login(): pass\n")

    records = discover_routes(app)

    assert ("/users/login", str(handler.resolve())) in [
        (record.route, record.handler_path) for record in records
    ]


def test_does_not_take_prefix_from_unrelated_object(tmp_path: Path):
    (tmp_path / "main.py").write_text(
        "import child\nrouter.include_router(child.router, prefix=other.api_prefix)\n"
    )
    (tmp_path / "child.py").write_text("@router.get('/login')\ndef login(): pass\n")
    (tmp_path / "settings.py").write_text("api_prefix = '/api'\n")

    assert discover_routes(tmp_path) == []


def test_replace_route_index_removes_stale_routes_in_scope(tmp_path: Path):
    db_path = tmp_path / "entities.db"
    handler = str(tmp_path / "app" / "auth.py")
    from ai.routes.fastapi import RouteRecord

    replace_route_index(
        db_path, [RouteRecord("/old", handler, (handler,))], [str(tmp_path / "app")]
    )
    replace_route_index(
        db_path, [RouteRecord("/new", handler, (handler,))], [str(tmp_path / "app")]
    )

    with sqlite3.connect(db_path) as connection:
        rows = connection.execute("SELECT route, handler_path FROM indexed_routes").fetchall()
    assert rows == [("/new", handler)]
