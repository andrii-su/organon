export type ScreenId = "search" | "graph" | "history" | "impact" | "duplicates";
export type SearchMode = "vector" | "fts" | "hybrid";

export interface ApiBootstrap {
  baseUrl: string;
  dbPath: string;
  source: string;
}

export interface StatsResponse {
  db_path: string;
  total_entities: number;
  total_relations: number;
  total_bytes: number;
  by_lifecycle: Record<string, number>;
}

export interface Entity {
  id: string;
  path: string;
  name: string;
  extension: string | null;
  size_bytes: number;
  created_at: number;
  modified_at: number;
  accessed_at: number;
  lifecycle: string;
  content_hash: string | null;
  summary: string | null;
  git_author: string | null;
}

export interface SearchExplanation {
  vector_score?: number | null;
  vector_contribution?: number | null;
  fts_rank?: number | null;
  fts_score?: number | null;
  fts_contribution?: number | null;
  matched_terms: string[];
  path_match: boolean;
  text_preview?: string | null;
  reasons: string[];
}

export interface SearchHit {
  path: string;
  score: number;
  source: string;
  explanation?: SearchExplanation | null;
}

export interface SearchPage {
  items: SearchHit[];
  total: number;
  limit: number;
  offset: number;
  has_more: boolean;
}

export interface GraphEdge {
  from: string;
  to: string;
  kind: string;
}

export interface GraphResponse {
  path: string;
  depth: number;
  nodes: string[];
  edges: GraphEdge[];
  cycles: string[][];
  text: string;
  dot: string;
  mermaid: string;
}

export interface HistoryEntry {
  id: number;
  entity_id: string;
  path: string;
  event: string;
  old_lifecycle?: string | null;
  new_lifecycle?: string | null;
  old_path?: string | null;
  size_bytes?: number | null;
  content_hash?: string | null;
  recorded_at: number;
}

export interface ImpactEntry {
  path: string;
  kind: string;
  depth: number;
}

export interface ImpactResponse {
  path: string;
  depth: number;
  total: number;
  direct_dependents: number;
  risk_level: string;
  entries: ImpactEntry[];
}

export interface DuplicateGroup {
  content_hash: string;
  paths: string[];
}

export interface NearDuplicatePair {
  file1: string;
  file2: string;
  similarity: number;
}

export interface DuplicatesResponse {
  exact: DuplicateGroup[];
  near?: NearDuplicatePair[] | null;
}
