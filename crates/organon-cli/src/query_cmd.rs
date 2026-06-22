//! `organon query` — save, list, run, and delete named queries.

use std::path::Path;

use anyhow::Result;
use organon_core::config::OrgConfig;

use crate::format::format_ts;
use crate::inspect::cmd_find;
use crate::search::{default_search_mode, search_entities, SearchMode, SearchParams};
use crate::search_cmds::cmd_search;
use crate::{build_find_filter, queries, FindFilterParams, QueryCmd};

/// Dispatch query subcommands (save / list / run / delete / show).
pub(crate) fn cmd_query(action: QueryCmd, db_path: &Path, config: &OrgConfig) -> Result<()> {
    match action {
        QueryCmd::Save {
            name,
            search,
            mode,
            state,
            extension,
            created_after,
            modified_after,
            larger_than_mb,
            limit,
            description,
        } => {
            let (kind, query, mode_str) = if let Some(q) = search {
                (
                    "search".to_string(),
                    Some(q),
                    mode.map(|m| format!("{m:?}").to_lowercase()),
                )
            } else {
                ("find".to_string(), None, None)
            };
            let sq = queries::SavedQuery {
                kind,
                query,
                mode: mode_str,
                state,
                extension,
                created_after,
                modified_after,
                larger_than_mb,
                limit,
                description,
                created_at: queries::now_ts(),
            };
            queries::insert(&name, sq)?;
            println!("saved query '{name}'");
        }

        QueryCmd::List => {
            let store = queries::load()?;
            if store.is_empty() {
                println!("(no saved queries — use `organon query save <name>`)");
                return Ok(());
            }
            println!("{:<20}  DEFINITION", "NAME");
            println!("{}", "-".repeat(72));
            for (name, sq) in &store {
                let desc = sq.description.as_deref().unwrap_or("");
                let summary = sq.summary();
                if desc.is_empty() {
                    println!("{name:<20}  {summary}");
                } else {
                    println!("{name:<20}  {summary}  # {desc}");
                }
            }
        }

        QueryCmd::Show { name } => {
            let sq = queries::get(&name)?;
            println!("name:         {name}");
            println!("kind:         {}", sq.kind);
            if let Some(q) = &sq.query {
                println!("query:        {q}");
            }
            if let Some(m) = &sq.mode {
                println!("mode:         {m}");
            }
            if let Some(s) = &sq.state {
                println!("state:        {s}");
            }
            if let Some(e) = &sq.extension {
                println!("extension:    {e}");
            }
            if let Some(ca) = &sq.created_after {
                println!("created_after:{ca}");
            }
            if let Some(ma) = &sq.modified_after {
                println!("modified_after:{ma}");
            }
            if let Some(b) = sq.larger_than_mb {
                println!("larger_than_mb:{b}");
            }
            println!("limit:        {}", sq.limit);
            if let Some(d) = &sq.description {
                println!("description:  {d}");
            }
            println!("created_at:   {}", format_ts(sq.created_at));
        }

        QueryCmd::Delete { name } => {
            queries::remove(&name)?;
            println!("deleted query '{name}'");
        }

        QueryCmd::Run { name, json } => {
            let sq = queries::get(&name)?;
            match sq.kind.as_str() {
                "find" => cmd_find(
                    db_path,
                    sq.state,
                    sq.extension,
                    sq.created_after,
                    sq.modified_after,
                    None,
                    sq.larger_than_mb,
                    sq.limit,
                )?,
                "search" => {
                    let query = sq.query.as_deref().unwrap_or("");
                    let mode = sq.mode.as_deref().and_then(|m| match m {
                        "vector" => Some(SearchMode::Vector),
                        "fts" => Some(SearchMode::Fts),
                        "hybrid" => Some(SearchMode::Hybrid),
                        _ => None,
                    });
                    if json {
                        let limit = sq.limit;
                        let metadata_filter = build_find_filter(FindFilterParams {
                            state: sq.state,
                            extension: sq.extension,
                            created_after: sq.created_after,
                            modified_after: sq.modified_after,
                            modified_within_days: None,
                            larger_than_mb: sq.larger_than_mb,
                            limit,
                            offset: 0,
                        })?;
                        let results = search_entities(SearchParams {
                            query,
                            limit,
                            offset: 0,
                            dir: None,
                            mode: mode.unwrap_or_else(|| default_search_mode(config)),
                            metadata_filter: &metadata_filter,
                            config,
                            db_path,
                            explain: false,
                        })?;
                        println!("{}", serde_json::to_string_pretty(&results.items)?);
                    } else {
                        cmd_search(
                            query,
                            Some(sq.limit),
                            None,
                            mode,
                            sq.state,
                            sq.extension,
                            sq.created_after,
                            sq.modified_after,
                            false,
                            config,
                            db_path,
                        )?;
                    }
                }
                other => anyhow::bail!("unknown query kind '{other}'"),
            }
        }
    }
    Ok(())
}
