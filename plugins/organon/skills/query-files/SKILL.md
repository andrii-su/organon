---
name: query-files
description: Answer which files implement an endpoint or functionality, which files use a model, or what file changes happened in a period using Organon MCP.
---

# Query Files

Extract the target from the question, then call `query_files` with `kind=route` for `/endpoint`, `kind=functionality` for “де N?”, or `kind=model` for “хто використовує модель X?”. Keep the configured MCP session scope; narrow with `path_prefix` when the user names a folder. Page with `offset` while `has_more` is true when they want the full list.

Report direct matches before connected files. For functionality and model queries, `match_percent` is the share of distinct query terms found literally in indexed text or the path: `matched / total × 100`, rounded down. For routes, it is exact route text or the longest contiguous route-segment match. It is never a probability of responsibility. Connected files have a named relationship and no percentage unless they also match directly. `include_semantic=true` can add vector candidates when literal results are insufficient; those candidates have no percentage. State the `coverage_note` and only claim coverage of indexed files.

For model questions, distinguish files that mention the model name from files connected by an import relation. A textual mention alone does not prove the file uses the model. For FastAPI, composed routes can identify a handler and files defining prefixes; dynamic prefixes may be absent, and partial route matches do not prove the full endpoint exists.

For “what changed between dates?”, call `query_history_events` with `from_date` and `to_date` as UTC `YYYY-MM-DD` dates. These dates are inclusive. Use `event` for created, modified, renamed, deleted, or lifecycle; set `include_archive=true` when the user asks for older or complete history. Page as needed. Report event time, path, type, `archived`, and `observation_gap`; observed events may miss intermediate changes while the watcher was stopped.

The first response should be a compact file list or event list with reasons, not file contents. Use `get_graph`, `get_history`, or `get_file_content` when the user asks for details or a candidate needs verification.

When reading `get_history`, report `text_change.before` and `text_change.after` for text edits when present. If `detail_status` says `binary_or_unavailable`, do not infer a text diff from hashes. If `observation_gap` is true, state that intermediate edits may have happened while the watcher was not running.
