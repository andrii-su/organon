import { useEffect, useState } from "react";

import { getEntity } from "../lib/api";
import type { Entity, ScreenId } from "../lib/types";
import { formatBytes, formatTimestamp, lifecycleTone, screenLabel } from "../lib/utils";

interface EntityPanelProps {
  baseUrl: string;
  path: string | null;
  onClose: () => void;
  onSelectPath: (path: string) => void;
  onNavigate: (screen: ScreenId, path: string) => void;
}

export function EntityPanel({
  baseUrl,
  path,
  onClose,
  onSelectPath,
  onNavigate,
}: EntityPanelProps) {
  const [entity, setEntity] = useState<Entity | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!path) {
      setEntity(null);
      setError(null);
      return;
    }

    setLoading(true);
    setError(null);

    getEntity(baseUrl, path)
      .then((next) => setEntity(next))
      .catch((fetchError: unknown) => {
        setEntity(null);
        setError(
          fetchError instanceof Error
            ? fetchError.message
            : "Failed to load entity details.",
        );
      })
      .finally(() => setLoading(false));
  }, [baseUrl, path]);

  return (
    <aside className={`detail-panel ${path ? "open" : ""}`}>
      <div className="detail-header">
        <div>
          <div className="eyebrow">Entity detail</div>
          <h2>{path ? "File inspection" : "Select a file"}</h2>
        </div>
        <button className="ghost-button" onClick={onClose} type="button">
          Close
        </button>
      </div>

      {!path && (
        <div className="state-card">
          Pick any result from Search, Graph, History, Impact, or Duplicates.
        </div>
      )}

      {path && loading && <div className="state-card">Loading entity…</div>}

      {path && error && (
        <div className="state-card error">
          <strong>Entity load failed.</strong>
          <span>{error}</span>
        </div>
      )}

      {entity && (
        <div className="detail-body">
          <div className="detail-path">{entity.path}</div>
          <div className="badge-row">
            <span className={`badge tone-${lifecycleTone(entity.lifecycle)}`}>
              {entity.lifecycle}
            </span>
            {entity.extension && <span className="badge tone-neutral">.{entity.extension}</span>}
          </div>

          <dl className="metric-grid">
            <div>
              <dt>Size</dt>
              <dd>{formatBytes(entity.size_bytes)}</dd>
            </div>
            <div>
              <dt>Created</dt>
              <dd>{formatTimestamp(entity.created_at)}</dd>
            </div>
            <div>
              <dt>Modified</dt>
              <dd>{formatTimestamp(entity.modified_at)}</dd>
            </div>
            <div>
              <dt>Accessed</dt>
              <dd>{formatTimestamp(entity.accessed_at)}</dd>
            </div>
            <div>
              <dt>Git author</dt>
              <dd>{entity.git_author ?? "Unavailable"}</dd>
            </div>
            <div>
              <dt>Hash</dt>
              <dd>{entity.content_hash?.slice(0, 16) ?? "Unavailable"}</dd>
            </div>
          </dl>

          <section className="detail-section">
            <div className="section-title">Summary</div>
            <p>{entity.summary ?? "No summary stored for this file yet."}</p>
          </section>

          <section className="detail-section">
            <div className="section-title">Actions</div>
            <div className="action-grid">
              {(["history", "graph", "impact"] as ScreenId[]).map((screen) => (
                <button
                  className="panel-button"
                  key={screen}
                  onClick={() => onNavigate(screen, entity.path)}
                  type="button"
                >
                  Open {screenLabel(screen)}
                </button>
              ))}
              <button
                className="panel-button"
                onClick={() => onNavigate("search", entity.path)}
                type="button"
              >
                Search similar
              </button>
              <button
                className="panel-button"
                onClick={() => onSelectPath(entity.path)}
                type="button"
              >
                Keep selected
              </button>
            </div>
          </section>
        </div>
      )}
    </aside>
  );
}
