---
name: query-files
description: Find files for a route or endpoint in an indexed workspace, explain direct matches and file relationships, then inspect selected files through Organon MCP.
---

# Query Files

When the user asks which files implement an endpoint, extract the route path and call Organon's `query_route_files` first. Use the configured MCP session scope; narrow it with `path_prefix` when the user specifies a folder. Page through results with `offset` while `has_more` is true when the user wants the full list.

Report direct matches before connected files. A direct `match_percent` describes route text found in indexed content or a path. It is not a probability of responsibility or task relevance. Connected files have a named relationship and no direct percentage unless they also match directly. State the `coverage_note`; do not claim that an unindexed file was searched.

For FastAPI, Organon can index statically resolvable router registrations and prefixes from several files. A composed `100%` match identifies the handler and lists files that define its prefixes. Dynamic prefixes that cannot be resolved statically may still be absent; partial segment matches are not proof that the complete endpoint exists.

The first response should be a compact file list with reasons, not file contents. Use `get_graph`, `get_history`, or `get_file_content` only when the user asks for details or the candidate list needs verification. If the question is not a route path, use `search_files` with `explain=true` and metadata filters as appropriate; do not invent a match percentage from its score.

When reading `get_history`, report `text_change.before` and `text_change.after` for text edits when present. If `detail_status` says `binary_or_unavailable`, do not infer a text diff from hashes. If `observation_gap` is true, state that intermediate edits may have happened while the watcher was not running.
