//! Entity inspection commands: status, ls, find, clean, stats, init, completions, archive.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{generate, Shell};
use log::debug;
use organon_core::{config::OrgConfig, entity::LifecycleState, scanner};

use crate::format::{format_ts, human_bytes};
use crate::{build_find_filter, open_graph, Cli, FindFilterParams};

/// Print metadata and lifecycle state for a single file.
pub(crate) fn cmd_status(path: PathBuf, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
    let path_str = canonical.to_string_lossy();
    debug!("status: {path_str}");

    match graph.get_by_path(&path_str)? {
        None => anyhow::bail!("not found in graph: {path_str}"),
        Some(e) => {
            println!("path:       {}", e.path);
            println!("lifecycle:  {}", e.lifecycle.as_str());
            println!("size:       {} bytes", e.size_bytes);
            println!("created:    {}", format_ts(e.created_at));
            println!("modified:   {}", format_ts(e.modified_at));
            println!("accessed:   {}", format_ts(e.accessed_at));
            if let Some(h) = &e.content_hash {
                println!("hash:       {}...", &h[..16.min(h.len())]);
            }
            if let Some(author) = &e.git_author {
                println!("git author: {author}");
            }
            if let Some(s) = &e.summary {
                println!("summary:    {s}");
            }
        }
    }
    Ok(())
}

/// List entities, optionally filtered by lifecycle state.
pub(crate) fn cmd_ls(state: Option<&str>, limit: usize, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let all = graph.all()?;
    debug!("ls: total={} state={:?} limit={}", all.len(), state, limit);

    let filtered: Vec<_> = all
        .iter()
        .filter(|e| state.is_none_or(|s| e.lifecycle.as_str() == s))
        .take(limit)
        .collect();

    if filtered.is_empty() {
        println!("(no entities)");
        return Ok(());
    }

    let col = 10;
    println!("{:<col$}  PATH", "LIFECYCLE");
    println!("{}", "-".repeat(72));
    for e in filtered {
        println!("{:<col$}  {}", e.lifecycle.as_str(), e.path);
    }
    Ok(())
}

/// Filter and list entities by metadata predicates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_find(
    db_path: &Path,
    state: Option<String>,
    extension: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    modified_within_days: Option<i64>,
    larger_than_mb: Option<u64>,
    limit: usize,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let filter = build_find_filter(FindFilterParams {
        state,
        extension,
        created_after,
        modified_after,
        modified_within_days,
        larger_than_mb,
        limit,
        offset: 0,
    })?;

    let results = graph.find(&filter)?;
    if results.is_empty() {
        println!("(no matches)");
        return Ok(());
    }

    println!("{:<10}  {:>8}  {:<16}  PATH", "STATE", "SIZE", "MODIFIED");
    println!("{}", "-".repeat(96));
    for entity in results {
        println!(
            "{:<10}  {:>8}  {:<16}  {}",
            entity.lifecycle.as_str(),
            human_bytes(entity.size_bytes),
            format_ts(entity.modified_at),
            entity.path,
        );
    }
    Ok(())
}

/// Remove dead entities and stale relations from the graph.
pub(crate) fn cmd_clean(
    db_path: &Path,
    config: &OrgConfig,
    dry_run: bool,
    apply: bool,
    dead_only: bool,
    stale_relations_only: bool,
) -> Result<()> {
    let graph = Arc::new(Mutex::new(open_graph(db_path)?));
    scanner::refresh_lifecycle(Arc::clone(&graph), &config.lifecycle)?;

    let graph = graph.lock().unwrap();
    let clean_dead = !stale_relations_only;
    let clean_stale_relations = !dead_only;
    let dead = if clean_dead {
        graph.dead_entities()?
    } else {
        Vec::new()
    };
    let stale = if clean_stale_relations {
        graph.stale_relations()?
    } else {
        Vec::new()
    };
    let apply = apply && !dry_run;

    if dead.is_empty() && stale.is_empty() {
        println!("nothing to clean");
        return Ok(());
    }

    if !apply {
        if !dead.is_empty() {
            println!("dead entities ({}):", dead.len());
            for entity in &dead {
                println!("  {}", entity.path);
            }
        }
        if !stale.is_empty() {
            if !dead.is_empty() {
                println!();
            }
            println!("stale relations ({}):", stale.len());
            for (from, to, kind) in &stale {
                println!("  {from} --[{kind}]--> {to}");
            }
        }
        println!();
        println!("re-run with `organon clean --apply` to delete them");
        return Ok(());
    }

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

    println!("removed {dead_deleted} dead entities and {stale_deleted} stale relations");
    Ok(())
}

/// Print shell completion script to stdout.
pub(crate) fn cmd_completions(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "organon", &mut io::stdout());
    Ok(())
}

/// Write default config to `~/.organon/config.toml`.
pub(crate) fn cmd_init(force: bool) -> Result<()> {
    let path = crate::config_path();
    if path.exists() && !force {
        anyhow::bail!(
            "config already exists: {}\nUse `organon init --force` to overwrite.",
            path.display()
        );
    }
    OrgConfig::write_default(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Print entity count and total size broken down by lifecycle state.
pub(crate) fn cmd_stats(db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let all = graph.all()?;

    let mut counts = std::collections::BTreeMap::new();
    let mut total_bytes: u64 = 0;
    for e in &all {
        *counts.entry(e.lifecycle.as_str()).or_insert(0u32) += 1;
        total_bytes += e.size_bytes;
    }

    println!("db:          {}", db_path.display());
    println!("total:       {}", all.len());
    println!("total size:  {}", human_bytes(total_bytes));
    println!();
    println!("by lifecycle:");
    for (state, count) in &counts {
        println!("  {state:10}  {count}");
    }
    Ok(())
}

/// List or physically move files in the `archived` lifecycle state.
pub(crate) fn cmd_archive(
    dry_run: bool,
    apply: bool,
    dir: Option<&Path>,
    db_path: &Path,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let all = graph.all()?;
    let dir_prefix = dir.map(|path| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .to_string()
    });

    let candidates: Vec<_> = all
        .iter()
        .filter(|e| e.lifecycle == LifecycleState::Archived)
        .filter(|e| {
            dir_prefix
                .as_ref()
                .is_none_or(|prefix| e.path.starts_with(prefix))
        })
        .collect();

    if candidates.is_empty() {
        println!("no archived files found");
        return Ok(());
    }

    let archive_dir = PathBuf::from(format!(
        "{}/.organon/archive",
        std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
    ));

    println!(
        "{} archived file(s){}:",
        candidates.len(),
        if dry_run {
            " (dry run)"
        } else if !apply {
            " (use --apply to move)"
        } else {
            ""
        }
    );
    println!("{}", "-".repeat(72));

    for e in &candidates {
        println!("  {}", e.path);
        if apply && !dry_run {
            let src = Path::new(&e.path);
            if src.exists() {
                let relative = src.strip_prefix("/").unwrap_or(src);
                let dst = archive_dir.join(relative);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(src, &dst)?;
                log::info!("archived {} → {}", e.path, dst.display());
            } else {
                eprintln!("  (skipped — file not on disk: {})", e.path);
            }
        }
    }
    Ok(())
}
