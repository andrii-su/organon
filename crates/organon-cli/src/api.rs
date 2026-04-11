use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::NaiveDate;
use organon_core::{
    config::OrgConfig,
    graph::{FindFilter, Graph},
    scanner,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tower_http::cors::{Any, CorsLayer};

use crate::{
    python::python_run_with_env,
    search::{default_search_mode, python_env, search_entities, SearchMode, SearchParams},
};

#[derive(Clone)]
pub struct ApiState {
    pub db_path: PathBuf,
    pub config: OrgConfig,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(err: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self::internal(value)
    }
}

type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Debug, Deserialize)]
struct EntitiesQuery {
    state: Option<String>,
    extension: Option<String>,
    ext: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    modified_within_days: Option<i64>,
    larger_than_mb: Option<u64>,
    limit: Option<usize>,
    offset: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct PathQuery {
    path: String,
}

#[derive(Debug, Deserialize)]
struct SearchQuery {
    q: String,
    limit: Option<usize>,
    offset: Option<usize>,
    dir: Option<PathBuf>,
    mode: Option<SearchMode>,
    state: Option<String>,
    extension: Option<String>,
    ext: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    explain: Option<bool>,
}

#[derive(Debug, Serialize)]
struct StatsResponse {
    db_path: String,
    total_entities: usize,
    total_relations: usize,
    total_bytes: u64,
    by_lifecycle: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct PaginatedResponse<T> {
    items: Vec<T>,
    total: usize,
    limit: usize,
    offset: usize,
    has_more: bool,
}

#[derive(Debug, Deserialize, Default)]
struct CleanRequest {
    #[serde(default)]
    dry_run: bool,
    #[serde(default)]
    dead_only: bool,
    #[serde(default)]
    stale_relations_only: bool,
}

#[derive(Debug, Serialize)]
struct CleanResponse {
    dry_run: bool,
    dead_entities: Vec<String>,
    stale_relations: Vec<Value>,
    dead_deleted: usize,
    stale_deleted: usize,
}

#[derive(Debug, Deserialize, Default)]
struct IndexRequest {
    summarize: Option<bool>,
    ollama_model: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IndexResponse {
    total: usize,
    indexed: usize,
    skipped: usize,
    errors: usize,
}

pub fn router(state: ApiState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(docs))
        .route("/stats", get(stats))
        .route("/entities", get(list_entities))
        .route("/entity", get(get_entity))
        .route("/relations", get(get_relations))
        .route("/search", get(search))
        .route("/clean", post(clean))
        .route("/index", post(index))
        .layer(CorsLayer::new().allow_origin(Any).allow_methods(Any))
        .with_state(state)
}

pub async fn serve(
    db_path: PathBuf,
    config: OrgConfig,
    host: Option<String>,
    port: Option<u16>,
) -> Result<()> {
    let host = host.unwrap_or_else(|| config.server.host.clone());
    let port = port.unwrap_or(config.server.port);
    let addr = format!("{host}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    log::info!("REST API listening on http://{addr}");
    axum::serve(listener, router(ApiState { db_path, config })).await?;
    Ok(())
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn openapi_json() -> Json<Value> {
    Json(json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Organon API",
            "version": "0.1.0",
            "description": "REST API for Organon local semantic filesystem graph."
        },
        "servers": [
            { "url": "http://127.0.0.1:7474", "description": "Default local server" }
        ],
        "paths": {
            "/health": {
                "get": {
                    "summary": "Health check",
                    "responses": {
                        "200": json_response("Service healthy", ref_schema("HealthResponse"))
                    }
                }
            },
            "/docs": {
                "get": {
                    "summary": "Swagger UI",
                    "responses": {
                        "200": {
                            "description": "Swagger UI HTML",
                            "content": { "text/html": { "schema": { "type": "string" } } }
                        }
                    }
                }
            },
            "/openapi.json": {
                "get": {
                    "summary": "OpenAPI spec",
                    "responses": {
                        "200": {
                            "description": "OpenAPI document",
                            "content": { "application/json": { "schema": { "type": "object" } } }
                        }
                    }
                }
            },
            "/stats": {
                "get": {
                    "summary": "Graph stats",
                    "responses": {
                        "200": json_response("Graph statistics", ref_schema("StatsResponse")),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/entities": {
                "get": {
                    "summary": "List entities with pagination",
                    "parameters": [
                        query_param("state", "Lifecycle state filter", false, json!({"type": "string", "enum": ["born", "active", "dormant", "archived", "dead"]})),
                        query_param("extension", "File extension without leading dot", false, json!({"type": "string"})),
                        query_param("ext", "Alias for extension", false, json!({"type": "string"})),
                        query_param("created_after", "Only files created after YYYY-MM-DD", false, json!({"type": "string", "format": "date"})),
                        query_param("modified_after", "Only files modified after YYYY-MM-DD", false, json!({"type": "string", "format": "date"})),
                        query_param("modified_within_days", "Only files modified within last N days", false, json!({"type": "integer", "minimum": 0})),
                        query_param("larger_than_mb", "Only files larger than N megabytes", false, json!({"type": "integer", "minimum": 0})),
                        query_param("limit", "Page size", false, json!({"type": "integer", "minimum": 1, "default": 50})),
                        query_param("offset", "Result offset", false, json!({"type": "integer", "minimum": 0, "default": 0}))
                    ],
                    "responses": {
                        "200": json_response("Paginated entities", ref_schema("PaginatedEntityResponse")),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/entity": {
                "get": {
                    "summary": "Get entity by path",
                    "parameters": [
                        query_param("path", "Absolute file path", true, json!({"type": "string"}))
                    ],
                    "responses": {
                        "200": json_response("Entity row", ref_schema("Entity")),
                        "404": error_response("Entity not found"),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/relations": {
                "get": {
                    "summary": "Get relations for path",
                    "parameters": [
                        query_param("path", "Absolute file path", true, json!({"type": "string"}))
                    ],
                    "responses": {
                        "200": json_response("Relation rows", json!({"type": "array", "items": ref_schema("Relation")})),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/search": {
                "get": {
                    "summary": "Search entities with pagination",
                    "parameters": [
                        query_param("q", "Search query", true, json!({"type": "string", "minLength": 1})),
                        query_param("limit", "Page size", false, json!({"type": "integer", "minimum": 1})),
                        query_param("offset", "Result offset", false, json!({"type": "integer", "minimum": 0, "default": 0})),
                        query_param("dir", "Optional directory prefix", false, json!({"type": "string"})),
                        query_param("mode", "Search mode", false, json!({"type": "string", "enum": ["vector", "fts", "hybrid"]})),
                        query_param("state", "Lifecycle state filter", false, json!({"type": "string", "enum": ["born", "active", "dormant", "archived", "dead"]})),
                        query_param("extension", "File extension without leading dot", false, json!({"type": "string"})),
                        query_param("ext", "Alias for extension", false, json!({"type": "string"})),
                        query_param("created_after", "Only files created after YYYY-MM-DD", false, json!({"type": "string", "format": "date"})),
                        query_param("modified_after", "Only files modified after YYYY-MM-DD", false, json!({"type": "string", "format": "date"})),
                        query_param("explain", "Include per-hit explanation block (score breakdown, matched terms, reasons)", false, json!({"type": "boolean", "default": false}))
                    ],
                    "responses": {
                        "200": json_response("Paginated search hits", ref_schema("SearchPage")),
                        "400": error_response("Bad request"),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/clean": {
                "post": {
                    "summary": "Clean dead entities and stale relations",
                    "requestBody": {
                        "required": false,
                        "content": {
                            "application/json": {
                                "schema": ref_schema("CleanRequest")
                            }
                        }
                    },
                    "responses": {
                        "200": json_response("Cleanup preview or result", ref_schema("CleanResponse")),
                        "500": error_response("Internal error")
                    }
                }
            },
            "/index": {
                "post": {
                    "summary": "Run indexer once",
                    "requestBody": {
                        "required": false,
                        "content": {
                            "application/json": {
                                "schema": ref_schema("IndexRequest")
                            }
                        }
                    },
                    "responses": {
                        "200": json_response("Indexer run summary", ref_schema("IndexResponse")),
                        "500": error_response("Internal error")
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "ErrorResponse": {
                    "type": "object",
                    "required": ["error"],
                    "properties": {
                        "error": { "type": "string" }
                    }
                },
                "HealthResponse": {
                    "type": "object",
                    "required": ["ok"],
                    "properties": {
                        "ok": { "type": "boolean" }
                    }
                },
                "Entity": {
                    "type": "object",
                    "required": ["id", "path", "name", "size_bytes", "created_at", "modified_at", "accessed_at", "lifecycle"],
                    "properties": {
                        "id": { "type": "string" },
                        "path": { "type": "string" },
                        "name": { "type": "string" },
                        "extension": { "type": ["string", "null"] },
                        "size_bytes": { "type": "integer", "minimum": 0 },
                        "created_at": { "type": "integer" },
                        "modified_at": { "type": "integer" },
                        "accessed_at": { "type": "integer" },
                        "lifecycle": { "type": "string", "enum": ["born", "active", "dormant", "archived", "dead"] },
                        "content_hash": { "type": ["string", "null"] },
                        "summary": { "type": ["string", "null"] },
                        "git_author": { "type": ["string", "null"] }
                    }
                },
                "Relation": {
                    "type": "object",
                    "required": ["from", "to", "kind"],
                    "properties": {
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                        "kind": { "type": "string" }
                    }
                },
                "SearchHit": {
                    "type": "object",
                    "required": ["path", "score", "source"],
                    "properties": {
                        "path": { "type": "string" },
                        "score": { "type": "number" },
                        "source": { "type": "string", "enum": ["vector", "fts", "hybrid", "-"] }
                    }
                },
                "SearchPage": {
                    "type": "object",
                    "required": ["items", "total", "limit", "offset", "has_more"],
                    "properties": {
                        "items": { "type": "array", "items": ref_schema("SearchHit") },
                        "total": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 0 },
                        "offset": { "type": "integer", "minimum": 0 },
                        "has_more": { "type": "boolean" }
                    }
                },
                "StatsResponse": {
                    "type": "object",
                    "required": ["db_path", "total_entities", "total_relations", "total_bytes", "by_lifecycle"],
                    "properties": {
                        "db_path": { "type": "string" },
                        "total_entities": { "type": "integer", "minimum": 0 },
                        "total_relations": { "type": "integer", "minimum": 0 },
                        "total_bytes": { "type": "integer", "minimum": 0 },
                        "by_lifecycle": {
                            "type": "object",
                            "additionalProperties": { "type": "integer", "minimum": 0 }
                        }
                    }
                },
                "PaginatedEntityResponse": {
                    "type": "object",
                    "required": ["items", "total", "limit", "offset", "has_more"],
                    "properties": {
                        "items": { "type": "array", "items": ref_schema("Entity") },
                        "total": { "type": "integer", "minimum": 0 },
                        "limit": { "type": "integer", "minimum": 0 },
                        "offset": { "type": "integer", "minimum": 0 },
                        "has_more": { "type": "boolean" }
                    }
                },
                "CleanRequest": {
                    "type": "object",
                    "properties": {
                        "dry_run": { "type": "boolean", "default": false },
                        "dead_only": { "type": "boolean", "default": false },
                        "stale_relations_only": { "type": "boolean", "default": false }
                    }
                },
                "CleanResponse": {
                    "type": "object",
                    "required": ["dry_run", "dead_entities", "stale_relations", "dead_deleted", "stale_deleted"],
                    "properties": {
                        "dry_run": { "type": "boolean" },
                        "dead_entities": { "type": "array", "items": { "type": "string" } },
                        "stale_relations": { "type": "array", "items": ref_schema("Relation") },
                        "dead_deleted": { "type": "integer", "minimum": 0 },
                        "stale_deleted": { "type": "integer", "minimum": 0 }
                    }
                },
                "IndexRequest": {
                    "type": "object",
                    "properties": {
                        "summarize": { "type": "boolean" },
                        "ollama_model": { "type": "string" }
                    }
                },
                "IndexResponse": {
                    "type": "object",
                    "required": ["total", "indexed", "skipped", "errors"],
                    "properties": {
                        "total": { "type": "integer", "minimum": 0 },
                        "indexed": { "type": "integer", "minimum": 0 },
                        "skipped": { "type": "integer", "minimum": 0 },
                        "errors": { "type": "integer", "minimum": 0 }
                    }
                }
            }
        }
    }))
}

async fn docs() -> impl IntoResponse {
    axum::response::Html(
        r#"<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>Organon API Docs</title>
    <link rel="stylesheet" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css" />
  </head>
  <body>
    <div id="swagger-ui"></div>
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
    <script>
      window.ui = SwaggerUIBundle({
        url: '/openapi.json',
        dom_id: '#swagger-ui'
      });
    </script>
  </body>
</html>"#,
    )
}

fn ref_schema(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn json_response(description: &str, schema: Value) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": schema
            }
        }
    })
}

fn error_response(description: &str) -> Value {
    json_response(description, ref_schema("ErrorResponse"))
}

fn query_param(name: &str, description: &str, required: bool, schema: Value) -> Value {
    json!({
        "name": name,
        "in": "query",
        "required": required,
        "description": description,
        "schema": schema
    })
}

async fn stats(State(state): State<ApiState>) -> ApiResult<Json<StatsResponse>> {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<StatsResponse> {
        let graph = open_graph(&db_path)?;
        let entities = graph.all()?;
        let relations = graph.all_relations()?;
        let mut by_lifecycle = BTreeMap::new();
        let mut total_bytes = 0u64;
        for entity in &entities {
            *by_lifecycle
                .entry(entity.lifecycle.as_str().to_string())
                .or_insert(0) += 1;
            total_bytes += entity.size_bytes;
        }
        Ok(StatsResponse {
            db_path: db_path.display().to_string(),
            total_entities: entities.len(),
            total_relations: relations.len(),
            total_bytes,
            by_lifecycle,
        })
    })
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(result))
}

async fn list_entities(
    State(state): State<ApiState>,
    Query(query): Query<EntitiesQuery>,
) -> ApiResult<Json<PaginatedResponse<organon_core::entity::Entity>>> {
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(
        move || -> Result<PaginatedResponse<organon_core::entity::Entity>> {
            let graph = open_graph(&db_path)?;
            let filter = build_find_filter(FindFilterParams {
                state: query.state,
                extension: query.extension.or(query.ext),
                created_after: query.created_after,
                modified_after: query.modified_after,
                modified_within_days: query.modified_within_days,
                larger_than_mb: query.larger_than_mb,
                limit: query.limit.unwrap_or(50),
                offset: query.offset.unwrap_or(0),
            })?;
            let total = graph.count_find(&filter)?;
            let items = graph.find(&filter)?;
            Ok(PaginatedResponse {
                has_more: filter.offset + filter.limit < total,
                items,
                total,
                limit: filter.limit,
                offset: filter.offset,
            })
        },
    )
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(result))
}

async fn get_entity(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Json<organon_core::entity::Entity>> {
    let db_path = state.db_path.clone();
    let path = query.path;
    let result =
        tokio::task::spawn_blocking(move || -> Result<Option<organon_core::entity::Entity>> {
            let graph = open_graph(&db_path)?;
            graph.get_by_path(&path)
        })
        .await
        .map_err(|e| ApiError::internal(anyhow!(e)))??;

    match result {
        Some(entity) => Ok(Json(entity)),
        None => Err(ApiError::not_found("entity not found")),
    }
}

async fn get_relations(
    State(state): State<ApiState>,
    Query(query): Query<PathQuery>,
) -> ApiResult<Json<Vec<Value>>> {
    let db_path = state.db_path.clone();
    let path = query.path;
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<(String, String, String)>> {
        let graph = open_graph(&db_path)?;
        graph.get_relations(&path)
    })
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(
        result
            .into_iter()
            .map(|(from, to, kind)| json!({ "from": from, "to": to, "kind": kind }))
            .collect(),
    ))
}

async fn search(
    State(state): State<ApiState>,
    Query(query): Query<SearchQuery>,
) -> ApiResult<Json<crate::search::SearchPage>> {
    if query.q.trim().is_empty() {
        return Err(ApiError::bad_request("missing query parameter `q`"));
    }

    let db_path = state.db_path.clone();
    let config = state.config.clone();
    let limit = query.limit.unwrap_or(config.search.default_limit);
    let offset = query.offset.unwrap_or(0);
    let mode = query.mode.unwrap_or_else(|| default_search_mode(&config));
    let dir = query.dir;
    let q = query.q;
    let metadata_filter = build_find_filter(FindFilterParams {
        state: query.state,
        extension: query.extension.or(query.ext),
        created_after: query.created_after,
        modified_after: query.modified_after,
        modified_within_days: None,
        larger_than_mb: None,
        limit,
        offset,
    })?;

    let explain = query.explain.unwrap_or(false);
    let result = tokio::task::spawn_blocking(move || {
        search_entities(SearchParams {
            query: &q,
            limit,
            offset,
            dir: dir.as_deref(),
            mode,
            metadata_filter: &metadata_filter,
            config: &config,
            db_path: &db_path,
            explain,
        })
    })
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(result))
}

async fn clean(
    State(state): State<ApiState>,
    Json(request): Json<CleanRequest>,
) -> ApiResult<Json<CleanResponse>> {
    let db_path = state.db_path.clone();
    let config = state.config.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<CleanResponse> {
        let graph = std::sync::Arc::new(std::sync::Mutex::new(open_graph(&db_path)?));
        scanner::refresh_lifecycle(std::sync::Arc::clone(&graph), &config.lifecycle)?;

        let graph = graph.lock().unwrap();
        let clean_dead = !request.stale_relations_only;
        let clean_stale_relations = !request.dead_only;

        let dead_entities = if clean_dead {
            graph
                .dead_entities()?
                .into_iter()
                .map(|entity| entity.path)
                .collect()
        } else {
            Vec::new()
        };

        let stale_relations_raw = if clean_stale_relations {
            graph.stale_relations()?
        } else {
            Vec::new()
        };

        let stale_relations = stale_relations_raw
            .iter()
            .map(|(from, to, kind)| json!({ "from": from, "to": to, "kind": kind }))
            .collect::<Vec<_>>();

        let (dead_deleted, stale_deleted) = if request.dry_run {
            (0, 0)
        } else {
            let dead_deleted = if clean_dead {
                graph.delete_dead_entities()?
            } else {
                0
            };
            let stale_deleted = if clean_stale_relations {
                graph.delete_stale_relations()?
            } else {
                0
            };
            (dead_deleted, stale_deleted)
        };

        Ok(CleanResponse {
            dry_run: request.dry_run,
            dead_entities,
            stale_relations,
            dead_deleted,
            stale_deleted,
        })
    })
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(result))
}

async fn index(
    State(state): State<ApiState>,
    Json(request): Json<IndexRequest>,
) -> ApiResult<Json<IndexResponse>> {
    let db_path = state.db_path.clone();
    let config = state.config.clone();

    let result = tokio::task::spawn_blocking(move || -> Result<IndexResponse> {
        if !db_path.exists() {
            bail!("DB not found: {}", db_path.display());
        }

        let summarize = request
            .summarize
            .map(|flag| if flag { "True" } else { "False" }.to_string())
            .unwrap_or_else(|| "None".to_string());
        let ollama_model = request
            .ollama_model
            .map(|model| format!("{model:?}"))
            .unwrap_or_else(|| "None".to_string());

        let output = python_run_with_env(
            &[
                "-c",
                &format!(
                    "from ai.indexer import run_once; from pathlib import Path; import json; \
                     print(json.dumps(run_once(Path({:?}), summarize={}, ollama_model={}, vectors_db_path={:?})))",
                    db_path.to_string_lossy(),
                    summarize,
                    ollama_model,
                    config.indexer.vectors_path,
                ),
            ],
            &python_env(&config),
        )?;

        Ok(serde_json::from_str(&output)?)
    })
    .await
    .map_err(|e| ApiError::internal(anyhow!(e)))??;

    Ok(Json(result))
}

fn open_graph(db_path: &Path) -> Result<Graph> {
    if !db_path.exists() {
        return Err(anyhow!("DB not found: {}", db_path.display()));
    }
    Graph::open(db_path.to_string_lossy().as_ref())
}

struct FindFilterParams {
    state: Option<String>,
    extension: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    modified_within_days: Option<i64>,
    larger_than_mb: Option<u64>,
    limit: usize,
    offset: usize,
}

fn build_find_filter(params: FindFilterParams) -> Result<FindFilter> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let modified_after = match (params.modified_after, params.modified_within_days) {
        (Some(date), _) => Some(parse_date_to_timestamp(&date)?),
        (None, Some(days)) => Some(now - days * 86_400),
        (None, None) => None,
    };

    Ok(FindFilter {
        state: params.state,
        extension: params.extension.map(normalize_extension),
        created_after: params.created_after
            .as_deref()
            .map(parse_date_to_timestamp)
            .transpose()?,
        modified_after,
        larger_than: params.larger_than_mb.map(|mb| mb * 1024 * 1024),
        offset: params.offset,
        limit: params.limit,
    })
}

fn normalize_extension(ext: String) -> String {
    ext.trim_start_matches('.').to_string()
}

fn parse_date_to_timestamp(date: &str) -> Result<i64> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| anyhow!("invalid date `{date}`: {e}"))?;
    Ok(parsed
        .and_hms_opt(0, 0, 0)
        .expect("valid midnight")
        .and_utc()
        .timestamp())
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use organon_core::{
        config::OrgConfig,
        entity::{Entity, LifecycleState},
        graph::Graph,
    };
    use serde_json::json;
    use tower::util::ServiceExt;

    use super::*;

    fn test_entity(path: &str) -> Entity {
        Entity {
            id: path.to_string(),
            path: path.to_string(),
            name: "main.rs".to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 42,
            created_at: 1,
            modified_at: 2,
            accessed_at: 3,
            lifecycle: LifecycleState::Active,
            content_hash: Some("abc".to_string()),
            summary: Some("summary".to_string()),
            git_author: Some("Alice".to_string()),
        }
    }

    #[tokio::test]
    async fn health_route_ok() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let app = router(ApiState {
            db_path: tmp.path().to_path_buf(),
            config: OrgConfig::default(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn openapi_route_ok() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let app = router(ApiState {
            db_path: tmp.path().to_path_buf(),
            config: OrgConfig::default(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["openapi"], "3.1.0");
        assert_eq!(
            body["paths"]["/entities"]["get"]["parameters"][0]["name"],
            "state"
        );
        assert_eq!(
            body["components"]["schemas"]["SearchPage"]["properties"]["items"]["items"]["$ref"],
            "#/components/schemas/SearchHit"
        );
    }

    #[tokio::test]
    async fn entity_route_returns_json() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let graph = Graph::open(tmp.path().to_string_lossy().as_ref()).unwrap();
        graph.upsert(&test_entity("/tmp/main.rs")).unwrap();

        let app = router(ApiState {
            db_path: tmp.path().to_path_buf(),
            config: OrgConfig::default(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/entity?path=/tmp/main.rs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("/tmp/main.rs"));
    }

    #[tokio::test]
    async fn clean_route_dry_run_returns_preview() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let graph = Graph::open(tmp.path().to_string_lossy().as_ref()).unwrap();

        let mut dead = test_entity("/tmp/dead.rs");
        dead.lifecycle = LifecycleState::Dead;
        graph.upsert(&dead).unwrap();
        graph.upsert(&test_entity("/tmp/live.rs")).unwrap();
        graph
            .upsert_relation("/tmp/missing.rs", "/tmp/live.rs", "imports")
            .unwrap();

        let app = router(ApiState {
            db_path: tmp.path().to_path_buf(),
            config: OrgConfig::default(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/clean")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&json!({ "dry_run": true })).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["dead_deleted"], 0);
        assert_eq!(body["stale_deleted"], 0);
        assert_eq!(body["dead_entities"][0], "/tmp/dead.rs");
        assert_eq!(body["stale_relations"][0]["from"], "/tmp/missing.rs");
    }

    #[tokio::test]
    async fn entities_route_paginates() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let graph = Graph::open(tmp.path().to_string_lossy().as_ref()).unwrap();
        for i in 0..3 {
            let mut entity = test_entity(&format!("/tmp/{i}.rs"));
            entity.modified_at = i;
            graph.upsert(&entity).unwrap();
        }

        let app = router(ApiState {
            db_path: tmp.path().to_path_buf(),
            config: OrgConfig::default(),
        });

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/entities?limit=1&offset=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["total"], 3);
        assert_eq!(body["offset"], 1);
    }
}
