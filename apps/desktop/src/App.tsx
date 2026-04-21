import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { Dispatch, SetStateAction } from "react";

import { EntityPanel } from "./components/EntityPanel";
import { MermaidPreview } from "./components/MermaidPreview";
import {
  getDuplicates,
  getGraph,
  getHistory,
  getImpact,
  getStats,
  searchFiles,
} from "./lib/api";
import { useStoredState } from "./lib/storage";
import type {
  ApiBootstrap,
  DuplicatesResponse,
  GraphResponse,
  HistoryEntry,
  ImpactResponse,
  ScreenId,
  SearchMode,
  SearchPage,
  StatsResponse,
} from "./lib/types";
import {
  clampThreshold,
  eventTone,
  formatBytes,
  formatTimestamp,
  groupImpactEntries,
  screenLabel,
} from "./lib/utils";

type SearchKind = "query" | "file";
type GraphView = "mermaid" | "text" | "dot";

interface SearchFormState {
  kind: SearchKind;
  query: string;
  likePath: string;
  mode: SearchMode;
  state: string;
  extension: string;
  modifiedAfter: string;
  createdAfter: string;
  explain: boolean;
  limit: number;
}

interface GraphFormState {
  path: string;
  depth: number;
}

interface HistoryFormState {
  path: string;
  limit: number;
}

interface ImpactFormState {
  path: string;
  depth: number;
}

interface DuplicatesFormState {
  near: boolean;
  threshold: number;
  limit: number;
}

const NAV_ITEMS: ScreenId[] = [
  "search",
  "graph",
  "history",
  "impact",
  "duplicates",
];

const DEFAULT_SEARCH: SearchFormState = {
  kind: "query",
  query: "",
  likePath: "",
  mode: "hybrid",
  state: "",
  extension: "",
  modifiedAfter: "",
  createdAfter: "",
  explain: true,
  limit: 25,
};

const DEFAULT_GRAPH: GraphFormState = {
  path: "",
  depth: 2,
};

const DEFAULT_HISTORY: HistoryFormState = {
  path: "",
  limit: 50,
};

const DEFAULT_IMPACT: ImpactFormState = {
  path: "",
  depth: 3,
};

const DEFAULT_DUPLICATES: DuplicatesFormState = {
  near: true,
  threshold: 0.95,
  limit: 30,
};

export default function App() {
  const [activeScreen, setActiveScreen] = useStoredState<ScreenId>(
    "organon-desktop:screen",
    "search",
  );
  const [searchState, setSearchState] = useStoredState<SearchFormState>(
    "organon-desktop:search",
    DEFAULT_SEARCH,
  );
  const [graphState, setGraphState] = useStoredState<GraphFormState>(
    "organon-desktop:graph",
    DEFAULT_GRAPH,
  );
  const [historyState, setHistoryState] = useStoredState<HistoryFormState>(
    "organon-desktop:history",
    DEFAULT_HISTORY,
  );
  const [impactState, setImpactState] = useStoredState<ImpactFormState>(
    "organon-desktop:impact",
    DEFAULT_IMPACT,
  );
  const [duplicatesState, setDuplicatesState] =
    useStoredState<DuplicatesFormState>(
      "organon-desktop:duplicates",
      DEFAULT_DUPLICATES,
    );

  const [baseUrl, setBaseUrl] = useState<string | null>(null);
  const [boot, setBoot] = useState<{
    status: "booting" | "ready" | "error";
    message: string;
    source?: string;
    dbPath?: string;
  }>({
    status: "booting",
    message: "Starting local Organon API…",
  });
  const [stats, setStats] = useState<StatsResponse | null>(null);
  const [detailPath, setDetailPath] = useState<string | null>(null);
  const [screenRuns, setScreenRuns] = useState<Record<ScreenId, number>>({
    search: 0,
    graph: 0,
    history: 0,
    impact: 0,
    duplicates: 0,
  });

  useEffect(() => {
    invoke<ApiBootstrap>("bootstrap_api")
      .then((result) => {
        setBaseUrl(result.baseUrl);
        setBoot({
          status: "ready",
          message: `Connected to ${result.baseUrl}`,
          source: result.source,
          dbPath: result.dbPath,
        });
        return getStats(result.baseUrl);
      })
      .then((result) => setStats(result))
      .catch((error: unknown) => {
        setBoot({
          status: "error",
          message:
            error instanceof Error
              ? error.message
              : "Failed to bootstrap local Organon API.",
        });
      });
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey)) {
        return;
      }

      const index = Number(event.key) - 1;
      if (index >= 0 && index < NAV_ITEMS.length) {
        event.preventDefault();
        setActiveScreen(NAV_ITEMS[index]);
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [setActiveScreen]);

  const headerStats = stats
    ? [
        { label: "Entities", value: stats.total_entities.toLocaleString() },
        { label: "Relations", value: stats.total_relations.toLocaleString() },
        { label: "Volume", value: formatBytes(stats.total_bytes) },
      ]
    : [];

  function bumpScreen(screen: ScreenId) {
    setScreenRuns((current) => ({
      ...current,
      [screen]: current[screen] + 1,
    }));
  }

  function openEntity(path: string) {
    setDetailPath(path);
  }

  function navigateTo(screen: ScreenId, path: string) {
    if (screen === "search") {
      setSearchState((current) => ({
        ...current,
        kind: "file",
        likePath: path,
      }));
    }

    if (screen === "graph") {
      setGraphState((current) => ({ ...current, path }));
    }

    if (screen === "history") {
      setHistoryState((current) => ({ ...current, path }));
    }

    if (screen === "impact") {
      setImpactState((current) => ({ ...current, path }));
    }

    setActiveScreen(screen);
    setDetailPath(path);
    bumpScreen(screen);
  }

  if (boot.status === "booting") {
    return <BootScreen title="Bootstrapping Organon" message={boot.message} />;
  }

  if (boot.status === "error" || !baseUrl) {
    return (
      <BootScreen
        title="Desktop shell unavailable"
        message={`${boot.message} The UI expects a local REST API and valid Organon config.`}
        error
      />
    );
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand-block">
          <div className="eyebrow">Organon desktop</div>
          <h1>Useful surface over the existing graph.</h1>
          <p>
            Thin Tauri shell backed by the current Rust core and REST API.
          </p>
        </div>

        <nav className="nav-list" aria-label="Sections">
          {NAV_ITEMS.map((screen, index) => (
            <button
              key={screen}
              className={`nav-item ${activeScreen === screen ? "active" : ""}`}
              onClick={() => setActiveScreen(screen)}
              type="button"
            >
              <span>{screenLabel(screen)}</span>
              <small>⌘{index + 1}</small>
            </button>
          ))}
        </nav>

        <div className="sidebar-footer">
          <div className="connection-chip">
            <span className="status-dot" />
            {boot.message} {boot.source ? `(${boot.source})` : ""}
          </div>
          <div className="sidebar-path">{boot.dbPath}</div>
        </div>
      </aside>

      <main className="content-area">
        <header className="topbar">
          <div>
            <div className="eyebrow">Current surface</div>
            <h2>{screenLabel(activeScreen)}</h2>
          </div>
          <div className="stat-strip">
            {headerStats.map((item) => (
              <div className="stat-card" key={item.label}>
                <span>{item.label}</span>
                <strong>{item.value}</strong>
              </div>
            ))}
          </div>
        </header>

        <section className="content-panel">
          {activeScreen === "search" && (
            <SearchScreen
              baseUrl={baseUrl}
              onSelectPath={openEntity}
              runToken={screenRuns.search}
              state={searchState}
              onChange={setSearchState}
            />
          )}
          {activeScreen === "graph" && (
            <GraphScreen
              baseUrl={baseUrl}
              onSelectPath={openEntity}
              runToken={screenRuns.graph}
              state={graphState}
              onChange={setGraphState}
            />
          )}
          {activeScreen === "history" && (
            <HistoryScreen
              baseUrl={baseUrl}
              onSelectPath={openEntity}
              runToken={screenRuns.history}
              state={historyState}
              onChange={setHistoryState}
            />
          )}
          {activeScreen === "impact" && (
            <ImpactScreen
              baseUrl={baseUrl}
              onSelectPath={openEntity}
              runToken={screenRuns.impact}
              state={impactState}
              onChange={setImpactState}
            />
          )}
          {activeScreen === "duplicates" && (
            <DuplicatesScreen
              baseUrl={baseUrl}
              onSelectPath={openEntity}
              runToken={screenRuns.duplicates}
              state={duplicatesState}
              onChange={setDuplicatesState}
            />
          )}
        </section>
      </main>

      <EntityPanel
        baseUrl={baseUrl}
        path={detailPath}
        onClose={() => setDetailPath(null)}
        onSelectPath={openEntity}
        onNavigate={navigateTo}
      />
    </div>
  );
}

function BootScreen({
  title,
  message,
  error = false,
}: {
  title: string;
  message: string;
  error?: boolean;
}) {
  return (
    <div className="boot-shell">
      <div className={`boot-card ${error ? "error" : ""}`}>
        <div className="eyebrow">Organon desktop</div>
        <h1>{title}</h1>
        <p>{message}</p>
      </div>
    </div>
  );
}

function SearchScreen({
  baseUrl,
  state,
  onChange,
  onSelectPath,
  runToken,
}: {
  baseUrl: string;
  state: SearchFormState;
  onChange: Dispatch<SetStateAction<SearchFormState>>;
  onSelectPath: (path: string) => void;
  runToken: number;
}) {
  const [results, setResults] = useState<SearchPage | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runSearch() {
    const queryValue = state.query.trim();
    const likeValue = state.likePath.trim();

    if ((state.kind === "query" && !queryValue) || (state.kind === "file" && !likeValue)) {
      setError("Provide a query or a file path to search against.");
      return;
    }

    setLoading(true);
    setError(null);
    try {
      setResults(
        await searchFiles(
          baseUrl,
          state.kind === "query"
            ? {
                q: queryValue,
                mode: state.mode,
                state: state.state,
                extension: state.extension,
                modified_after: state.modifiedAfter,
                created_after: state.createdAfter,
                explain: state.explain,
                limit: state.limit,
              }
            : {
                like: likeValue,
                limit: state.limit,
              },
        ),
      );
    } catch (searchError: unknown) {
      setError(
        searchError instanceof Error
          ? searchError.message
          : "Search request failed.",
      );
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (runToken > 0) {
      void runSearch();
    }
    // runToken intentionally drives re-runs from cross-screen navigation.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runToken]);

  return (
    <div className="screen-grid">
      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Semantic retrieval</div>
            <h3>Search files without rewriting the backend.</h3>
          </div>
        </div>

        <div className="segmented">
          {(["query", "file"] as SearchKind[]).map((kind) => (
            <button
              className={state.kind === kind ? "active" : ""}
              key={kind}
              onClick={() => onChange((current) => ({ ...current, kind }))}
              type="button"
            >
              {kind === "query" ? "Search text" : "Similar to file"}
            </button>
          ))}
        </div>

        <form
          className="form-grid"
          onSubmit={(event) => {
            event.preventDefault();
            void runSearch();
          }}
        >
          {state.kind === "query" ? (
            <label className="field wide">
              <span>Query</span>
              <input
                autoFocus
                onChange={(event) =>
                  onChange((current) => ({
                    ...current,
                    query: event.target.value,
                  }))
                }
                placeholder="authentication logic, import graph, stale watcher code…"
                value={state.query}
              />
            </label>
          ) : (
            <label className="field wide">
              <span>File path</span>
              <input
                autoFocus
                onChange={(event) =>
                  onChange((current) => ({
                    ...current,
                    likePath: event.target.value,
                  }))
                }
                placeholder="/absolute/path/to/file.rs"
                value={state.likePath}
              />
            </label>
          )}

          <label className="field">
            <span>Mode</span>
            <select
              disabled={state.kind === "file"}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  mode: event.target.value as SearchMode,
                }))
              }
              value={state.mode}
            >
              <option value="vector">vector</option>
              <option value="fts">fts</option>
              <option value="hybrid">hybrid</option>
            </select>
          </label>

          <label className="field">
            <span>Lifecycle</span>
            <select
              onChange={(event) =>
                onChange((current) => ({ ...current, state: event.target.value }))
              }
              value={state.state}
            >
              <option value="">all</option>
              <option value="born">born</option>
              <option value="active">active</option>
              <option value="dormant">dormant</option>
              <option value="archived">archived</option>
              <option value="dead">dead</option>
            </select>
          </label>

          <label className="field">
            <span>Extension</span>
            <input
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  extension: event.target.value.replace(/^\./, ""),
                }))
              }
              placeholder="rs"
              value={state.extension}
            />
          </label>

          <label className="field">
            <span>Modified after</span>
            <input
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  modifiedAfter: event.target.value,
                }))
              }
              type="date"
              value={state.modifiedAfter}
            />
          </label>

          <label className="field">
            <span>Created after</span>
            <input
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  createdAfter: event.target.value,
                }))
              }
              type="date"
              value={state.createdAfter}
            />
          </label>

          <label className="field">
            <span>Limit</span>
            <input
              min={1}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  limit: Number(event.target.value) || 25,
                }))
              }
              type="number"
              value={state.limit}
            />
          </label>

          <label className="checkbox-field">
            <input
              checked={state.explain}
              disabled={state.kind === "file"}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  explain: event.target.checked,
                }))
              }
              type="checkbox"
            />
            <span>Include ranking explanation</span>
          </label>

          <div className="field-actions">
            <button className="primary-button" disabled={loading} type="submit">
              {loading ? "Searching…" : "Run search"}
            </button>
          </div>
        </form>
      </div>

      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Results</div>
            <h3>
              {results
                ? `${results.total} result${results.total === 1 ? "" : "s"}`
                : "No search run yet"}
            </h3>
          </div>
        </div>

        {error && (
          <div className="state-card error">
            <strong>Search failed.</strong>
            <span>{error}</span>
          </div>
        )}

        {!error && !results && !loading && (
          <div className="state-card">
            Run a query or use “Similar to file” to hit `/search?like=...`.
          </div>
        )}

        {!error && loading && <div className="state-card">Fetching search hits…</div>}

        {results && results.items.length === 0 && !loading && (
          <div className="state-card">
            No matches. Widen lifecycle/extension filters or switch search mode.
          </div>
        )}

        {results && results.items.length > 0 && (
          <div className="result-list">
            {results.items.map((item) => (
              <button
                className="result-card"
                key={`${item.path}-${item.score}`}
                onClick={() => onSelectPath(item.path)}
                type="button"
              >
                <div className="result-head">
                  <strong>{item.path}</strong>
                  <span>{item.score.toFixed(3)}</span>
                </div>
                <div className="badge-row">
                  <span className="badge tone-accent">{item.source}</span>
                </div>
                {state.explain && item.explanation && (
                  <div className="explanation-block">
                    <div>{item.explanation.reasons.slice(0, 2).join(" • ")}</div>
                    {item.explanation.text_preview && (
                      <p>{item.explanation.text_preview}</p>
                    )}
                  </div>
                )}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function GraphScreen({
  baseUrl,
  state,
  onChange,
  onSelectPath,
  runToken,
}: {
  baseUrl: string;
  state: GraphFormState;
  onChange: Dispatch<SetStateAction<GraphFormState>>;
  onSelectPath: (path: string) => void;
  runToken: number;
}) {
  const [graph, setGraph] = useState<GraphResponse | null>(null);
  const [activeView, setActiveView] = useState<GraphView>("mermaid");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runGraph() {
    if (!state.path.trim()) {
      setError("Provide a file path for graph inspection.");
      return;
    }

    setLoading(true);
    setError(null);
    try {
      setGraph(await getGraph(baseUrl, state.path.trim(), state.depth));
    } catch (graphError: unknown) {
      setError(
        graphError instanceof Error ? graphError.message : "Graph request failed.",
      );
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (runToken > 0) {
      void runGraph();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runToken]);

  const rawOutput =
    activeView === "mermaid"
      ? graph?.mermaid
      : activeView === "dot"
        ? graph?.dot
        : graph?.text;

  return (
    <div className="screen-grid">
      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Relation graph</div>
            <h3>Useful fallback first: renderings plus node and edge tables.</h3>
          </div>
        </div>

        <form
          className="form-grid compact"
          onSubmit={(event) => {
            event.preventDefault();
            void runGraph();
          }}
        >
          <label className="field wide">
            <span>Root path</span>
            <input
              autoFocus
              onChange={(event) =>
                onChange((current) => ({ ...current, path: event.target.value }))
              }
              placeholder="/absolute/path/to/file.rs"
              value={state.path}
            />
          </label>

          <label className="field">
            <span>Depth</span>
            <input
              max={3}
              min={1}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  depth: Number(event.target.value) || 1,
                }))
              }
              type="number"
              value={state.depth}
            />
          </label>

          <div className="field-actions">
            <button className="primary-button" disabled={loading} type="submit">
              {loading ? "Building…" : "Build graph"}
            </button>
          </div>
        </form>

        {error && (
          <div className="state-card error">
            <strong>Graph fetch failed.</strong>
            <span>{error}</span>
          </div>
        )}

        {graph?.cycles.length ? (
          <div className="state-card warning">
            <strong>Cycle warning</strong>
            <span>{graph.cycles[0].join(" → ")}</span>
          </div>
        ) : null}

        <div className="stat-strip wide">
          <div className="stat-card">
            <span>Nodes</span>
            <strong>{graph?.nodes.length ?? 0}</strong>
          </div>
          <div className="stat-card">
            <span>Edges</span>
            <strong>{graph?.edges.length ?? 0}</strong>
          </div>
          <div className="stat-card">
            <span>Cycles</span>
            <strong>{graph?.cycles.length ?? 0}</strong>
          </div>
        </div>
      </div>

      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Renderings</div>
            <h3>Mermaid preview with text and DOT fallback.</h3>
          </div>
          {rawOutput && (
            <button
              className="ghost-button"
              onClick={() => navigator.clipboard.writeText(rawOutput)}
              type="button"
            >
              Copy current
            </button>
          )}
        </div>

        {!graph && !loading && !error && (
          <div className="state-card">
            Use the current graph logic through `/graph`; the response already
            carries Mermaid, DOT, text, nodes, edges, and cycles.
          </div>
        )}

        {loading && <div className="state-card">Fetching graph…</div>}

        {graph && (
          <>
            <div className="segmented">
              {(["mermaid", "text", "dot"] as GraphView[]).map((view) => (
                <button
                  className={activeView === view ? "active" : ""}
                  key={view}
                  onClick={() => setActiveView(view)}
                  type="button"
                >
                  {view}
                </button>
              ))}
            </div>

            {activeView === "mermaid" ? (
              <MermaidPreview chart={graph.mermaid} />
            ) : (
              <pre className="code-block">{rawOutput}</pre>
            )}

            <div className="data-columns">
              <section>
                <div className="section-title">Nodes</div>
                <div className="table-list">
                  {graph.nodes.map((node) => (
                    <button
                      className="table-row button-row"
                      key={node}
                      onClick={() => onSelectPath(node)}
                      type="button"
                    >
                      <span>{node}</span>
                    </button>
                  ))}
                </div>
              </section>

              <section>
                <div className="section-title">Edges</div>
                <div className="table-list">
                  {graph.edges.map((edge) => (
                    <div className="table-row" key={`${edge.from}-${edge.to}-${edge.kind}`}>
                      <button
                        className="inline-link"
                        onClick={() => onSelectPath(edge.from)}
                        type="button"
                      >
                        {edge.from}
                      </button>
                      <span className="badge tone-neutral">{edge.kind}</span>
                      <button
                        className="inline-link"
                        onClick={() => onSelectPath(edge.to)}
                        type="button"
                      >
                        {edge.to}
                      </button>
                    </div>
                  ))}
                </div>
              </section>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function HistoryScreen({
  baseUrl,
  state,
  onChange,
  onSelectPath,
  runToken,
}: {
  baseUrl: string;
  state: HistoryFormState;
  onChange: Dispatch<SetStateAction<HistoryFormState>>;
  onSelectPath: (path: string) => void;
  runToken: number;
}) {
  const [entries, setEntries] = useState<HistoryEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runHistory() {
    if (!state.path.trim()) {
      setError("Provide a file path to inspect history.");
      return;
    }

    setLoading(true);
    setError(null);
    try {
      setEntries(await getHistory(baseUrl, state.path.trim(), state.limit));
    } catch (historyError: unknown) {
      setError(
        historyError instanceof Error
          ? historyError.message
          : "History request failed.",
      );
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (runToken > 0) {
      void runHistory();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runToken]);

  return (
    <div className="screen-grid single">
      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Rename continuity</div>
            <h3>Newest-first timeline for lifecycle and path changes.</h3>
          </div>
        </div>

        <form
          className="form-grid compact"
          onSubmit={(event) => {
            event.preventDefault();
            void runHistory();
          }}
        >
          <label className="field wide">
            <span>Path</span>
            <input
              autoFocus
              onChange={(event) =>
                onChange((current) => ({ ...current, path: event.target.value }))
              }
              placeholder="/absolute/path/to/file.rs"
              value={state.path}
            />
          </label>
          <label className="field">
            <span>Limit</span>
            <input
              min={1}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  limit: Number(event.target.value) || 50,
                }))
              }
              type="number"
              value={state.limit}
            />
          </label>
          <div className="field-actions">
            <button className="primary-button" disabled={loading} type="submit">
              {loading ? "Loading…" : "Load history"}
            </button>
          </div>
        </form>

        {error && (
          <div className="state-card error">
            <strong>History fetch failed.</strong>
            <span>{error}</span>
          </div>
        )}

        {!error && !loading && entries.length === 0 && (
          <div className="state-card">
            Run history for a file to inspect rename continuity and lifecycle
            transitions.
          </div>
        )}

        {loading && <div className="state-card">Fetching history…</div>}

        {entries.length > 0 && (
          <div className="timeline">
            {entries.map((entry) => (
              <div className="timeline-item" key={entry.id}>
                <div className="timeline-head">
                  <span className={`badge tone-${eventTone(entry.event)}`}>
                    {entry.event}
                  </span>
                  <time>{formatTimestamp(entry.recorded_at)}</time>
                </div>
                <button
                  className="inline-link"
                  onClick={() => onSelectPath(entry.path)}
                  type="button"
                >
                  {entry.path}
                </button>
                {(entry.old_lifecycle || entry.new_lifecycle) && (
                  <div className="meta-line">
                    <span>{entry.old_lifecycle ?? "—"}</span>
                    <span>→</span>
                    <span>{entry.new_lifecycle ?? "—"}</span>
                  </div>
                )}
                {entry.old_path && <div className="meta-line">from {entry.old_path}</div>}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function ImpactScreen({
  baseUrl,
  state,
  onChange,
  onSelectPath,
  runToken,
}: {
  baseUrl: string;
  state: ImpactFormState;
  onChange: Dispatch<SetStateAction<ImpactFormState>>;
  onSelectPath: (path: string) => void;
  runToken: number;
}) {
  const [impact, setImpact] = useState<ImpactResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runImpact() {
    if (!state.path.trim()) {
      setError("Provide a file path for reverse dependency analysis.");
      return;
    }

    setLoading(true);
    setError(null);
    try {
      setImpact(await getImpact(baseUrl, state.path.trim(), state.depth));
    } catch (impactError: unknown) {
      setError(
        impactError instanceof Error
          ? impactError.message
          : "Impact request failed.",
      );
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (runToken > 0) {
      void runImpact();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runToken]);

  const grouped = impact ? groupImpactEntries(impact.entries) : {};

  return (
    <div className="screen-grid">
      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Blast radius</div>
            <h3>Reverse dependencies grouped by distance.</h3>
          </div>
        </div>

        <form
          className="form-grid compact"
          onSubmit={(event) => {
            event.preventDefault();
            void runImpact();
          }}
        >
          <label className="field wide">
            <span>Path</span>
            <input
              autoFocus
              onChange={(event) =>
                onChange((current) => ({ ...current, path: event.target.value }))
              }
              placeholder="/absolute/path/to/file.rs"
              value={state.path}
            />
          </label>
          <label className="field">
            <span>Depth</span>
            <input
              min={1}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  depth: Number(event.target.value) || 1,
                }))
              }
              type="number"
              value={state.depth}
            />
          </label>
          <div className="field-actions">
            <button className="primary-button" disabled={loading} type="submit">
              {loading ? "Analyzing…" : "Analyze impact"}
            </button>
          </div>
        </form>

        {impact && (
          <div className="stat-strip wide">
            <div className="stat-card">
              <span>Total dependents</span>
              <strong>{impact.total}</strong>
            </div>
            <div className="stat-card">
              <span>Direct</span>
              <strong>{impact.direct_dependents}</strong>
            </div>
            <div className="stat-card">
              <span>Risk</span>
              <strong>{impact.risk_level}</strong>
            </div>
          </div>
        )}

        {error && (
          <div className="state-card error">
            <strong>Impact fetch failed.</strong>
            <span>{error}</span>
          </div>
        )}
      </div>

      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Dependents</div>
            <h3>Depth buckets make the blast radius readable.</h3>
          </div>
        </div>

        {!impact && !loading && !error && (
          <div className="state-card">
            Reverse dependencies will appear here once you run the analysis.
          </div>
        )}
        {loading && <div className="state-card">Fetching reverse dependencies…</div>}
        {impact && impact.entries.length === 0 && !loading && (
          <div className="state-card">No reverse dependencies found at this depth.</div>
        )}

        {Object.entries(grouped).map(([depth, entries]) => (
          <section className="depth-group" key={depth}>
            <div className="section-title">Depth {depth}</div>
            <div className="table-list">
              {entries.map((entry) => (
                <div className="table-row" key={`${entry.path}-${entry.kind}-${entry.depth}`}>
                  <button
                    className="inline-link"
                    onClick={() => onSelectPath(entry.path)}
                    type="button"
                  >
                    {entry.path}
                  </button>
                  <span className="badge tone-neutral">{entry.kind}</span>
                  <span className="badge tone-warning">depth {entry.depth}</span>
                </div>
              ))}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}

function DuplicatesScreen({
  baseUrl,
  state,
  onChange,
  onSelectPath,
  runToken,
}: {
  baseUrl: string;
  state: DuplicatesFormState;
  onChange: Dispatch<SetStateAction<DuplicatesFormState>>;
  onSelectPath: (path: string) => void;
  runToken: number;
}) {
  const [duplicates, setDuplicates] = useState<DuplicatesResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function runDuplicates() {
    setLoading(true);
    setError(null);
    try {
      setDuplicates(
        await getDuplicates(baseUrl, {
          near: state.near,
          threshold: state.threshold,
          limit: state.limit,
        }),
      );
    } catch (duplicatesError: unknown) {
      setError(
        duplicatesError instanceof Error
          ? duplicatesError.message
          : "Duplicates request failed.",
      );
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    if (runToken > 0) {
      void runDuplicates();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runToken]);

  return (
    <div className="screen-grid">
      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Duplicate detection</div>
            <h3>Exact hashes plus optional near-duplicate vectors.</h3>
          </div>
        </div>

        <form
          className="form-grid compact"
          onSubmit={(event) => {
            event.preventDefault();
            void runDuplicates();
          }}
        >
          <label className="checkbox-field">
            <input
              checked={state.near}
              onChange={(event) =>
                onChange((current) => ({ ...current, near: event.target.checked }))
              }
              type="checkbox"
            />
            <span>Enable near-duplicates</span>
          </label>

          <label className="field">
            <span>Threshold</span>
            <input
              max={1}
              min={0}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  threshold: clampThreshold(Number(event.target.value)),
                }))
              }
              step={0.01}
              type="number"
              value={state.threshold}
            />
          </label>

          <label className="field">
            <span>Limit</span>
            <input
              min={1}
              onChange={(event) =>
                onChange((current) => ({
                  ...current,
                  limit: Number(event.target.value) || 30,
                }))
              }
              type="number"
              value={state.limit}
            />
          </label>

          <div className="field-actions">
            <button className="primary-button" disabled={loading} type="submit">
              {loading ? "Scanning…" : "Find duplicates"}
            </button>
          </div>
        </form>

        {error && (
          <div className="state-card error">
            <strong>Duplicates fetch failed.</strong>
            <span>{error}</span>
          </div>
        )}

        {!duplicates && !loading && !error && (
          <div className="state-card">
            Exact duplicates come from the graph; near duplicates reuse the
            existing Python vector store.
          </div>
        )}
      </div>

      <div className="panel">
        <div className="panel-header">
          <div>
            <div className="eyebrow">Results</div>
            <h3>Grouped for cleanup, not for demo theater.</h3>
          </div>
        </div>

        {loading && <div className="state-card">Scanning for duplicates…</div>}

        {duplicates && (
          <>
            <section className="depth-group">
              <div className="section-title">
                Exact duplicates ({duplicates.exact.length})
              </div>
              {duplicates.exact.length === 0 ? (
                <div className="state-card">No exact duplicate groups found.</div>
              ) : (
                <div className="group-list">
                  {duplicates.exact.map((group) => (
                    <div className="group-card" key={group.content_hash}>
                      <div className="group-header">
                        <span className="badge tone-neutral">
                          {group.content_hash.slice(0, 12)}
                        </span>
                        <strong>{group.paths.length} files</strong>
                      </div>
                      {group.paths.map((path) => (
                        <button
                          className="inline-link block-link"
                          key={path}
                          onClick={() => onSelectPath(path)}
                          type="button"
                        >
                          {path}
                        </button>
                      ))}
                    </div>
                  ))}
                </div>
              )}
            </section>

            <section className="depth-group">
              <div className="section-title">
                Near duplicates ({duplicates.near?.length ?? 0})
              </div>
              {!state.near ? (
                <div className="state-card">
                  Near-duplicate scan is disabled. Toggle it on when you need
                  vector-based similarity.
                </div>
              ) : duplicates.near && duplicates.near.length > 0 ? (
                <div className="table-list">
                  {duplicates.near.map((pair) => (
                    <div
                      className="table-row"
                      key={`${pair.file1}-${pair.file2}`}
                    >
                      <button
                        className="inline-link"
                        onClick={() => onSelectPath(pair.file1)}
                        type="button"
                      >
                        {pair.file1}
                      </button>
                      <span className="badge tone-accent">
                        {(pair.similarity * 100).toFixed(1)}%
                      </span>
                      <button
                        className="inline-link"
                        onClick={() => onSelectPath(pair.file2)}
                        type="button"
                      >
                        {pair.file2}
                      </button>
                    </div>
                  ))}
                </div>
              ) : (
                <div className="state-card">No near-duplicate pairs found.</div>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  );
}
