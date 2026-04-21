import type { HistoryEntry, ImpactEntry, ScreenId } from "./types";

export function formatBytes(value: number) {
  if (value >= 1024 ** 3) {
    return `${(value / 1024 ** 3).toFixed(1)} GB`;
  }
  if (value >= 1024 ** 2) {
    return `${(value / 1024 ** 2).toFixed(1)} MB`;
  }
  if (value >= 1024) {
    return `${(value / 1024).toFixed(1)} KB`;
  }
  return `${value} B`;
}

export function formatTimestamp(value: number) {
  return new Date(value * 1000).toLocaleString();
}

export function eventTone(event: HistoryEntry["event"]) {
  switch (event) {
    case "created":
      return "success";
    case "modified":
      return "accent";
    case "lifecycle":
      return "warning";
    case "renamed":
      return "neutral";
    case "deleted":
      return "danger";
    default:
      return "neutral";
  }
}

export function lifecycleTone(lifecycle: string) {
  switch (lifecycle) {
    case "active":
      return "success";
    case "born":
      return "accent";
    case "dormant":
      return "warning";
    case "archived":
      return "neutral";
    case "dead":
      return "danger";
    default:
      return "neutral";
  }
}

export function groupImpactEntries(entries: ImpactEntry[]) {
  return entries.reduce<Record<number, ImpactEntry[]>>((acc, entry) => {
    acc[entry.depth] ??= [];
    acc[entry.depth].push(entry);
    return acc;
  }, {});
}

export function screenLabel(screen: ScreenId) {
  switch (screen) {
    case "search":
      return "Search";
    case "graph":
      return "Graph";
    case "history":
      return "History";
    case "impact":
      return "Impact";
    case "duplicates":
      return "Duplicates";
  }
}

export function clampThreshold(value: number) {
  return Math.max(0, Math.min(1, value));
}
