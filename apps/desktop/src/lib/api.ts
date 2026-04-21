import type {
  DuplicatesResponse,
  Entity,
  GraphResponse,
  HistoryEntry,
  ImpactResponse,
  SearchMode,
  SearchPage,
  StatsResponse,
} from "./types";

type QueryValue = string | number | boolean | undefined | null;

function buildQuery(params: Record<string, QueryValue>) {
  const query = new URLSearchParams();
  Object.entries(params).forEach(([key, value]) => {
    if (value === undefined || value === null || value === "") {
      return;
    }
    query.set(key, String(value));
  });
  return query.toString();
}

async function requestJson<T>(
  baseUrl: string,
  path: string,
  params: Record<string, QueryValue> = {},
): Promise<T> {
  const query = buildQuery(params);
  const response = await fetch(`${baseUrl}${path}${query ? `?${query}` : ""}`);

  if (!response.ok) {
    let message = `${response.status} ${response.statusText}`;
    try {
      const payload = (await response.json()) as { error?: string };
      if (payload.error) {
        message = payload.error;
      }
    } catch {
      // Keep the default message if the payload is not JSON.
    }
    throw new Error(message);
  }

  return (await response.json()) as T;
}

export function getStats(baseUrl: string) {
  return requestJson<StatsResponse>(baseUrl, "/stats");
}

export function getEntity(baseUrl: string, path: string) {
  return requestJson<Entity>(baseUrl, "/entity", { path });
}

export function searchFiles(
  baseUrl: string,
  params: {
    q?: string;
    like?: string;
    mode?: SearchMode;
    state?: string;
    extension?: string;
    modified_after?: string;
    created_after?: string;
    explain?: boolean;
    limit?: number;
  },
) {
  return requestJson<SearchPage>(baseUrl, "/search", params);
}

export function getGraph(baseUrl: string, path: string, depth: number) {
  return requestJson<GraphResponse>(baseUrl, "/graph", { path, depth });
}

export function getHistory(baseUrl: string, path: string, limit = 50) {
  return requestJson<HistoryEntry[]>(baseUrl, "/history", { path, limit });
}

export function getImpact(baseUrl: string, path: string, depth: number) {
  return requestJson<ImpactResponse>(baseUrl, "/impact", { path, depth });
}

export function getDuplicates(
  baseUrl: string,
  params: {
    near: boolean;
    threshold: number;
    limit: number;
  },
) {
  return requestJson<DuplicatesResponse>(baseUrl, "/duplicates", params);
}
