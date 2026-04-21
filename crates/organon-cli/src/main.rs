mod api;
mod format;
mod graph_view;
mod python;
mod queries;
mod search;

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Result};
use chrono::NaiveDate;
use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{generate, Shell};
use log::{debug, info, LevelFilter};
use organon_core::{
    config::OrgConfig,
    entity::Entity,
    graph::{DuplicateGroup, FindFilter, Graph, ImpactEntry},
    ignore::IgnoreSet,
    scanner,
};

use format::{format_ts, human_bytes};
use graph_view::{build_relation_graph, render_graph_dot, render_graph_mermaid, render_graph_text};
use python::{python_exec_with_env, python_run_with_env};
use search::{
    default_search_mode, parse_query_expr, python_env, search_by_example, search_entities,
    SearchMode, SearchParams,
};

#[derive(Parser)]
#[command(name = "organon", about = "Local semantic filesystem layer", version)]
struct Cli {
    /// SQLite DB path (default: config or ~/.organon/entities.db)
    #[arg(long, env = "ORGANON_DB")]
    db: Option<PathBuf>,

    /// Increase log verbosity (`-v`, `-vv`)
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    verbose: u8,

    /// Silence non-error logs
    #[arg(long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum GraphFormat {
    Text,
    Dot,
    Mermaid,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExportFormat {
    Json,
    Csv,
    Dot,
}

#[derive(Subcommand)]
enum Cmd {
    /// Watch a directory and index all changes
    Watch {
        /// Directory to watch (default: current dir)
        path: Option<PathBuf>,

        /// Also run the Python indexer every N seconds
        #[arg(long)]
        index_interval: Option<u64>,

        /// Disable auto-indexer
        #[arg(long)]
        no_index: bool,

        /// After the initial scan, reconcile renames the watcher may have missed
        #[arg(long)]
        detect_renames: bool,
    },

    /// Show metadata and lifecycle state for a file
    Status { path: PathBuf },

    /// List files by lifecycle state
    Ls {
        #[arg(short, long)]
        state: Option<String>,

        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Filter entities by metadata
    #[command(
        after_long_help = "Examples:\n  organon find --state active --ext rs\n  organon find --modified-after 2026-01-01 --larger-than-mb 10\n  organon find --created-after 2026-01-01"
    )]
    Find {
        #[arg(long)]
        state: Option<String>,

        #[arg(long, visible_alias = "ext")]
        extension: Option<String>,

        /// Only files created after YYYY-MM-DD
        #[arg(long, value_name = "YYYY-MM-DD")]
        created_after: Option<String>,

        /// Only files modified after YYYY-MM-DD
        #[arg(long, value_name = "YYYY-MM-DD")]
        modified_after: Option<String>,

        /// Only files modified within the last N days
        #[arg(long)]
        modified_within_days: Option<i64>,

        /// Only files larger than N megabytes
        #[arg(long)]
        larger_than_mb: Option<u64>,

        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Remove dead entities and stale relations
    Clean {
        /// Print what would be removed
        #[arg(long)]
        dry_run: bool,

        /// Actually delete dead entities / stale relations
        #[arg(long)]
        apply: bool,

        /// Only clean dead entities
        #[arg(long)]
        dead_only: bool,

        /// Only clean stale relations
        #[arg(long)]
        stale_relations_only: bool,
    },

    /// Generate shell completion scripts
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },

    /// Write ~/.organon/config.toml with defaults
    Init {
        #[arg(long)]
        force: bool,
    },

    /// Show entity graph statistics
    Stats,

    /// Semantic search (requires Python + indexer)
    #[command(
        after_long_help = "Examples:\n  organon search \"sqlite graph\"\n  organon search \"imports\" --state active --ext rs\n  organon search \"watcher\" --modified-after 2026-01-01 --mode hybrid\n  organon search \"auth token\" --mode hybrid --explain\n  organon search --like src/auth.rs --limit 5"
    )]
    Search {
        /// Query text (mutually exclusive with --like)
        query: Option<String>,

        /// Find files similar to this file using vector embeddings (mutually exclusive with query)
        #[arg(long, value_name = "PATH", conflicts_with = "query")]
        like: Option<PathBuf>,

        #[arg(short, long)]
        limit: Option<usize>,

        #[arg(short, long)]
        dir: Option<PathBuf>,

        #[arg(long, value_enum)]
        mode: Option<SearchMode>,

        #[arg(long)]
        state: Option<String>,

        #[arg(long, visible_alias = "ext")]
        extension: Option<String>,

        #[arg(long, value_name = "YYYY-MM-DD")]
        created_after: Option<String>,

        #[arg(long, value_name = "YYYY-MM-DD")]
        modified_after: Option<String>,

        /// Show why each result ranked: score breakdown, matched terms, semantic signal
        #[arg(long)]
        explain: bool,
    },

    /// Run the Python indexer
    Index {
        #[arg(short, long)]
        watch: Option<u64>,
    },

    /// Compare filesystem state against graph DB
    Diff {
        path: Option<PathBuf>,

        #[arg(long)]
        json: bool,
    },

    /// Export graph/entities
    Export {
        #[arg(long, value_enum)]
        format: ExportFormat,

        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Recompute and store summary for one file
    Summarize {
        path: PathBuf,

        #[arg(long)]
        model: Option<String>,
    },

    /// Start the MCP server
    Mcp {
        #[arg(long)]
        sse: bool,
    },

    /// Start REST API server
    Api {
        #[arg(long)]
        host: Option<String>,

        #[arg(long)]
        port: Option<u16>,
    },

    /// List or move files in 'archived' lifecycle state
    Archive {
        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        apply: bool,

        #[arg(short, long)]
        dir: Option<PathBuf>,
    },

    /// Show import/reference graph for a file
    Graph {
        path: PathBuf,

        #[arg(short, long, default_value = "1")]
        depth: u8,

        #[arg(long, value_enum, default_value = "text")]
        format: GraphFormat,
    },

    /// Show lifecycle and change history for a file
    #[command(
        after_long_help = "Examples:\n  organon history src/main.rs\n  organon history src/main.rs --limit 20\n  organon history src/main.rs --json"
    )]
    History {
        path: PathBuf,

        /// Maximum number of entries to show (default 20)
        #[arg(short, long, default_value = "20")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show reverse dependencies — what would break if this file changes
    #[command(
        after_long_help = "Examples:\n  organon impact src/auth.rs\n  organon impact src/graph.rs --depth 3\n  organon impact src/lib.rs --json"
    )]
    Impact {
        path: PathBuf,

        /// BFS depth (1 = direct importers only, default 5)
        #[arg(short, long, default_value = "5")]
        depth: u8,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Find exact and near-duplicate files
    #[command(
        after_long_help = "Examples:\n  organon duplicates\n  organon duplicates --near\n  organon duplicates --near --threshold 0.92 --limit 20"
    )]
    Duplicates {
        /// Also find near-duplicates by embedding similarity (requires Python + indexer)
        #[arg(long)]
        near: bool,

        /// Similarity threshold for near-duplicates [0..1] (default 0.95)
        #[arg(long, default_value = "0.95")]
        threshold: f64,

        /// Max near-duplicate pairs (default 50)
        #[arg(long, default_value = "50")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Diagnose organon installation and runtime health
    Doctor,

    /// Manage saved search/find queries
    #[command(
        after_long_help = "Examples:\n  organon query save stale-rs --state dormant --ext rs\n  organon query save auth-area --search \"auth token\" --mode hybrid\n  organon query list\n  organon query run stale-rs\n  organon query delete stale-rs"
    )]
    Query {
        #[command(subcommand)]
        action: QueryCmd,
    },
}

#[derive(Subcommand)]
enum QueryCmd {
    /// Save a named query (find by default; use --search for semantic search)
    Save {
        /// Name for the saved query
        name: String,
        /// Search query text — saves as `organon search`; omit to save as `organon find`
        #[arg(long, short = 'q')]
        search: Option<String>,
        /// Search mode (vector/fts/hybrid; search only)
        #[arg(long, value_enum)]
        mode: Option<SearchMode>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, visible_alias = "ext")]
        extension: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        created_after: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        modified_after: Option<String>,
        #[arg(long)]
        larger_than_mb: Option<u64>,
        #[arg(long, default_value = "20")]
        limit: usize,
        #[arg(long, short)]
        description: Option<String>,
    },
    /// List all saved queries
    List,
    /// Run a saved query
    Run {
        name: String,
        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
    /// Delete a saved query
    Delete { name: String },
    /// Show the definition of a saved query
    Show { name: String },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose, cli.quiet);
    let config = OrgConfig::load();
    let db_path = resolve_db(cli.db, &config);
    debug!("db path: {}", db_path.display());

    match cli.command {
        Cmd::Watch {
            path,
            index_interval,
            no_index,
            detect_renames,
        } => cmd_watch(
            path,
            &db_path,
            &config,
            index_interval,
            no_index,
            detect_renames,
        ),
        Cmd::Status { path } => cmd_status(path, &db_path),
        Cmd::Ls { state, limit } => cmd_ls(state.as_deref(), limit, &db_path),
        Cmd::Find {
            state,
            extension,
            created_after,
            modified_after,
            modified_within_days,
            larger_than_mb,
            limit,
        } => cmd_find(
            &db_path,
            state,
            extension,
            created_after,
            modified_after,
            modified_within_days,
            larger_than_mb,
            limit,
        ),
        Cmd::Clean {
            dry_run,
            apply,
            dead_only,
            stale_relations_only,
        } => cmd_clean(
            &db_path,
            &config,
            dry_run,
            apply,
            dead_only,
            stale_relations_only,
        ),
        Cmd::Completions { shell } => cmd_completions(shell),
        Cmd::Init { force } => cmd_init(force),
        Cmd::Stats => cmd_stats(&db_path),
        Cmd::Search {
            query,
            like,
            limit,
            dir,
            mode,
            state,
            extension,
            created_after,
            modified_after,
            explain,
        } => match (query, like) {
            (Some(q), _) => cmd_search(
                &q,
                limit,
                dir.as_deref(),
                mode,
                state,
                extension,
                created_after,
                modified_after,
                explain,
                &config,
                &db_path,
            ),
            (None, Some(path)) => cmd_search_like(&path, limit, dir.as_deref(), &config, &db_path),
            (None, None) => {
                anyhow::bail!("provide a search query or --like <path>")
            }
        },
        Cmd::Index { watch } => cmd_index(watch, &db_path, &config),
        Cmd::Diff { path, json } => cmd_diff(path.as_deref(), json, &db_path, &config),
        Cmd::Export { format, output } => cmd_export(&db_path, format, output.as_deref()),
        Cmd::Summarize { path, model } => cmd_summarize(&path, model, &db_path, &config),
        Cmd::Mcp { sse } => cmd_mcp_with_config(sse, &config),
        Cmd::Api { host, port } => cmd_api(&db_path, &config, host, port),
        Cmd::Archive {
            dry_run,
            apply,
            dir,
        } => cmd_archive(dry_run, apply, dir.as_deref(), &db_path),
        Cmd::Graph {
            path,
            depth,
            format,
        } => cmd_graph(&path, depth, format, &db_path),
        Cmd::History { path, limit, json } => cmd_history(&path, limit, json, &db_path),
        Cmd::Impact { path, depth, json } => cmd_impact(&path, depth, json, &db_path),
        Cmd::Duplicates {
            near,
            threshold,
            limit,
            json,
        } => cmd_duplicates(near, threshold, limit, json, &db_path, &config),
        Cmd::Doctor => cmd_doctor(&db_path, &config),
        Cmd::Query { action } => cmd_query(action, &db_path, &config),
    }
}

fn init_logging(verbose: u8, quiet: bool) {
    let default = if quiet {
        LevelFilter::Error
    } else {
        match verbose {
            0 => LevelFilter::Warn,
            1 => LevelFilter::Info,
            _ => LevelFilter::Debug,
        }
    };

    let env = env_logger::Env::default().default_filter_or(default.as_str());
    let _ = env_logger::Builder::from_env(env).try_init();
}

fn resolve_db(arg: Option<PathBuf>, config: &OrgConfig) -> PathBuf {
    arg.unwrap_or_else(|| PathBuf::from(&config.indexer.db_path))
}

fn config_path() -> PathBuf {
    std::env::var("ORGANON_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(format!("{home}/.organon/config.toml"))
        })
}

fn open_graph(db_path: &Path) -> Result<Graph> {
    if !db_path.exists() {
        bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }
    Graph::open(db_path.to_str().unwrap())
}

fn cmd_watch(
    path: Option<PathBuf>,
    db_path: &Path,
    config: &OrgConfig,
    index_interval: Option<u64>,
    no_index: bool,
    detect_renames: bool,
) -> Result<()> {
    let roots = resolve_watch_roots(path.as_deref(), config)?;
    let watch_roots: Vec<_> = roots
        .iter()
        .map(|root| {
            organon_core::watcher::WatchRoot::new(
                root.clone(),
                Arc::new(IgnoreSet::load(root, &config.watch.ignore_segments)),
            )
        })
        .collect();
    let index_interval = index_interval.unwrap_or(config.watch.index_interval_secs);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let joined_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    info!(
        "organon watch: [{}] | db: {}",
        joined_roots,
        db_path.display()
    );
    let graph = Arc::new(Mutex::new(Graph::open(db_path.to_str().unwrap())?));

    let mut stats = organon_core::scanner::ScanStats {
        total: 0,
        indexed: 0,
        skipped: 0,
        errors: 0,
    };
    for root in &watch_roots {
        let root_stats = scanner::scan(
            root.path.to_string_lossy().as_ref(),
            Arc::clone(&graph),
            &root.ignore_set,
            config.watch.use_git_timestamps,
        )?;
        stats.total += root_stats.total;
        stats.indexed += root_stats.indexed;
        stats.skipped += root_stats.skipped;
        stats.errors += root_stats.errors;
    }
    eprintln!(
        "indexed {} files ({} skipped, {} errors)",
        stats.indexed, stats.skipped, stats.errors
    );

    if detect_renames {
        for root in &watch_roots {
            let root_str = root.path.to_string_lossy();
            match scanner::reconcile_renames(&root_str, Arc::clone(&graph)) {
                Ok(n) if n > 0 => eprintln!("reconciled {n} rename(s) in {root_str}"),
                Ok(_) => {}
                Err(e) => eprintln!("warning: rename reconciliation failed: {e}"),
            }
        }
    }

    scanner::refresh_lifecycle(Arc::clone(&graph), &config.lifecycle)?;

    let _refresh_handle =
        scanner::schedule_lifecycle_refresh(Arc::clone(&graph), 6, config.lifecycle.clone());

    let _indexer_child = if !no_index {
        let interval_str = index_interval.to_string();
        let python = python::python_bin();
        let envs = python_env(config);
        let mut cmd = std::process::Command::new(&python);
        cmd.envs(envs.iter().map(|(k, v)| (k, v)));
        cmd.args([
            "-m",
            "ai.indexer",
            "--db",
            db_path.to_string_lossy().as_ref(),
            "--watch",
            &interval_str,
        ]);
        match cmd.spawn() {
            Ok(child) => {
                eprintln!(
                    "indexer started (every {}s, pid {})",
                    index_interval,
                    child.id()
                );
                Some(child)
            }
            Err(e) => {
                eprintln!(
                    "warning: could not start indexer: {e} (run `organon index --watch {index_interval}` manually)"
                );
                None
            }
        }
    } else {
        eprintln!(
            "indexer disabled (--no-index). Run `organon index --watch {index_interval}` manually."
        );
        None
    };

    organon_core::watcher::watch_many(&watch_roots, graph, config.watch.use_git_timestamps)
}

fn cmd_status(path: PathBuf, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
    let path_str = canonical.to_string_lossy();
    debug!("status: {path_str}");

    match graph.get_by_path(&path_str)? {
        None => bail!("not found in graph: {path_str}"),
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

fn cmd_ls(state: Option<&str>, limit: usize, db_path: &Path) -> Result<()> {
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

#[allow(clippy::too_many_arguments)]
fn cmd_find(
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

fn cmd_clean(
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

fn cmd_completions(shell: Shell) -> Result<()> {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "organon", &mut io::stdout());
    Ok(())
}

fn cmd_init(force: bool) -> Result<()> {
    let path = config_path();
    if path.exists() && !force {
        bail!(
            "config already exists: {}\nUse `organon init --force` to overwrite.",
            path.display()
        );
    }
    OrgConfig::write_default(&path)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn cmd_stats(db_path: &Path) -> Result<()> {
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

#[allow(clippy::too_many_arguments)]
fn cmd_search(
    query: &str,
    limit: Option<usize>,
    dir: Option<&Path>,
    mode: Option<SearchMode>,
    state: Option<String>,
    extension: Option<String>,
    created_after: Option<String>,
    modified_after: Option<String>,
    explain: bool,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<()> {
    // Parse inline field tokens out of the query (e.g. "state:dormant ext:rs auth")
    let pq = parse_query_expr(query);
    if pq.has_filters() {
        debug!(
            "query parse: free_text={:?} state={:?} ext={:?} modified_after={:?} created_after={:?} size>{:?}MB",
            pq.free_text, pq.state, pq.extension, pq.modified_after, pq.created_after, pq.larger_than_mb
        );
    }
    let effective_query = if pq.free_text.is_empty() && pq.has_filters() {
        // All tokens were field filters — run as find
        return cmd_find(
            db_path,
            pq.state.or(state),
            pq.extension.or(extension),
            created_after.or(pq.created_after),
            modified_after.or(pq.modified_after),
            None,
            pq.larger_than_mb,
            limit.unwrap_or(config.search.default_limit),
        );
    } else {
        pq.free_text.clone()
    };
    let effective_query = if effective_query.is_empty() {
        query
    } else {
        &effective_query
    };

    // Merge field-filter overrides (explicit flags win over parsed tokens)
    let merged_state = state.or(pq.state);
    let merged_ext = extension.or(pq.extension);
    let merged_created = created_after.or(pq.created_after);
    let merged_modified = modified_after.or(pq.modified_after);
    let merged_size = pq.larger_than_mb;

    let limit = limit.unwrap_or(config.search.default_limit);
    let mode = mode.unwrap_or_else(|| default_search_mode(config));
    let metadata_filter = build_find_filter(FindFilterParams {
        state: merged_state,
        extension: merged_ext,
        created_after: merged_created,
        modified_after: merged_modified,
        modified_within_days: None,
        larger_than_mb: merged_size,
        limit,
        offset: 0,
    })?;
    info!("search: {effective_query:?} limit={limit} dir={dir:?} mode={mode:?} explain={explain}");
    let results = search_entities(SearchParams {
        query,
        limit,
        offset: 0,
        dir,
        mode,
        metadata_filter: &metadata_filter,
        config,
        db_path,
        explain,
    })?;

    if results.items.is_empty() {
        println!("(no results — run `organon index` first)");
        return Ok(());
    }

    if explain {
        for hit in &results.items {
            println!("{:.3}  {}  {}", hit.score, hit.source, hit.path);
            if let Some(exp) = &hit.explanation {
                for reason in &exp.reasons {
                    println!("  → {reason}");
                }
                if let Some(preview) = &exp.text_preview {
                    let snippet: String = preview.chars().take(120).collect();
                    println!("  preview: {snippet}");
                }
            }
            println!();
        }
    } else {
        println!("{:<6}  {:<7}  PATH", "SCORE", "SOURCE");
        println!("{}", "-".repeat(96));
        for hit in results.items {
            println!("{:.3}   {:<7}  {}", hit.score, hit.source, hit.path);
        }
    }
    Ok(())
}

fn cmd_index(watch: Option<u64>, db_path: &Path, config: &OrgConfig) -> Result<()> {
    let mut args = vec!["-m", "ai.indexer"];
    let db_path_str = db_path.to_string_lossy().to_string();
    args.extend(["--db", &db_path_str]);
    let watch_str;
    if let Some(secs) = watch {
        info!("index watch mode: {secs}s");
        watch_str = secs.to_string();
        args.extend(["--watch", &watch_str]);
    }
    python_exec_with_env(&args, &python_env(config))
}

fn cmd_summarize(
    path: &PathBuf,
    model: Option<String>,
    db_path: &Path,
    config: &OrgConfig,
) -> Result<()> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy().to_string();
    let model_expr = model
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "None".to_string());
    let output = python_run_with_env(
        &[
            "-c",
            &format!(
                "from ai.indexer import summarize_file; from pathlib import Path; import json; \
                 print(json.dumps(summarize_file(Path({:?}), {:?}, model={})))",
                db_path.to_string_lossy(),
                path_str,
                model_expr,
            ),
        ],
        &python_env(config),
    )?;
    let summary: Option<String> = serde_json::from_str(&output)?;

    match summary {
        Some(summary) => {
            println!("path:     {path_str}");
            println!("summary:  {summary}");
            Ok(())
        }
        None => bail!("summary unavailable for {path_str}"),
    }
}

fn cmd_diff(path: Option<&Path>, json: bool, db_path: &Path, config: &OrgConfig) -> Result<()> {
    let root = path.unwrap_or(Path::new("."));
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let ignore_set = IgnoreSet::load(&canonical_root, &config.watch.ignore_segments);
    let graph = open_graph(db_path)?;
    let diff = compute_diff(
        &graph,
        &canonical_root,
        &ignore_set,
        config.watch.use_git_timestamps,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
        return Ok(());
    }

    for path in &diff.new {
        println!("NEW {path}");
    }
    for path in &diff.deleted {
        println!("DELETED {path}");
    }
    for path in &diff.changed {
        println!("CHANGED {path}");
    }
    Ok(())
}

fn cmd_export(db_path: &Path, format: ExportFormat, output: Option<&Path>) -> Result<()> {
    let graph = open_graph(db_path)?;
    let entities = graph.all()?;
    let relations = graph.all_relations()?;
    let rendered = match format {
        ExportFormat::Json => export_as_json(&entities, &relations)?,
        ExportFormat::Csv => export_entities_as_csv(&entities),
        ExportFormat::Dot => export_graph_as_dot(&entities, &relations),
    };

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
    } else {
        print!("{rendered}");
    }
    Ok(())
}

#[derive(Debug, serde::Serialize, PartialEq, Eq)]
struct DiffReport {
    new: Vec<String>,
    deleted: Vec<String>,
    changed: Vec<String>,
}

fn compute_diff(
    graph: &Graph,
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<DiffReport> {
    let root_prefix = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root_prefix = root_prefix.to_string_lossy().to_string();
    let db_entities: BTreeMap<String, Entity> = graph
        .all()?
        .into_iter()
        .filter(|entity| entity.path.starts_with(&root_prefix))
        .map(|entity| (entity.path.clone(), entity))
        .collect();
    let fs_entities = collect_fs_entities(root, ignore_set, use_git_timestamps)?;

    let mut new = Vec::new();
    let mut deleted = Vec::new();
    let mut changed = Vec::new();

    for path in fs_entities.keys() {
        if !db_entities.contains_key(path) {
            new.push(path.clone());
        }
    }

    for path in db_entities.keys() {
        if !fs_entities.contains_key(path) {
            deleted.push(path.clone());
        }
    }

    for (path, current) in &fs_entities {
        if let Some(indexed) = db_entities.get(path) {
            if indexed.size_bytes != current.size_bytes
                || indexed.modified_at != current.modified_at
                || indexed.content_hash != current.content_hash
            {
                changed.push(path.clone());
            }
        }
    }

    new.sort();
    deleted.sort();
    changed.sort();

    Ok(DiffReport {
        new,
        deleted,
        changed,
    })
}

fn collect_fs_entities(
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<BTreeMap<String, Entity>> {
    let mut entities = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if ignore_set.is_ignored(path) {
            continue;
        }
        let path_str = path.to_string_lossy();
        let entity = Entity::from_path_with_options(&path_str, use_git_timestamps)?;
        entities.insert(entity.path.clone(), entity);
    }
    Ok(entities)
}

fn export_as_json(entities: &[Entity], relations: &[(String, String, String)]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "entities": entities,
        "relations": relations.iter().map(|(from, to, kind)| serde_json::json!({
            "from": from,
            "to": to,
            "kind": kind,
        })).collect::<Vec<_>>(),
    }))?)
}

fn export_entities_as_csv(entities: &[Entity]) -> String {
    let mut out = String::from(
        "path,name,extension,size_bytes,created_at,modified_at,accessed_at,lifecycle,content_hash,summary,git_author\n",
    );
    for entity in entities {
        let row = [
            csv_field(&entity.path),
            csv_field(&entity.name),
            csv_field(entity.extension.as_deref().unwrap_or("")),
            entity.size_bytes.to_string(),
            entity.created_at.to_string(),
            entity.modified_at.to_string(),
            entity.accessed_at.to_string(),
            csv_field(entity.lifecycle.as_str()),
            csv_field(entity.content_hash.as_deref().unwrap_or("")),
            csv_field(entity.summary.as_deref().unwrap_or("")),
            csv_field(entity.git_author.as_deref().unwrap_or("")),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn export_graph_as_dot(entities: &[Entity], relations: &[(String, String, String)]) -> String {
    let mut out = String::from("digraph organon {\n");
    for entity in entities {
        out.push_str(&format!("  {:?};\n", entity.path));
    }
    for (from, to, kind) in relations {
        out.push_str(&format!("  {from:?} -> {to:?} [label={kind:?}];\n"));
    }
    out.push_str("}\n");
    out
}

fn cmd_mcp_with_config(sse: bool, config: &OrgConfig) -> Result<()> {
    info!("starting MCP server (sse={sse})");
    let _ = env_logger::try_init();
    let runtime = tokio::runtime::Runtime::new()?;
    if sse {
        return runtime.block_on(organon_mcp::serve_streamable_http_from_config(
            config.clone(),
        ));
    }

    runtime.block_on(organon_mcp::serve_stdio_from_config(config.clone()))
}

fn cmd_api(
    db_path: &Path,
    config: &OrgConfig,
    host: Option<String>,
    port: Option<u16>,
) -> Result<()> {
    let _ = env_logger::try_init();
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(api::serve(
        db_path.to_path_buf(),
        config.clone(),
        host,
        port,
    ))
}

fn cmd_archive(dry_run: bool, apply: bool, dir: Option<&Path>, db_path: &Path) -> Result<()> {
    use organon_core::entity::LifecycleState;

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
                info!("archived {} → {}", e.path, dst.display());
            } else {
                eprintln!("  (skipped — file not on disk: {})", e.path);
            }
        }
    }
    Ok(())
}

fn cmd_graph(path: &PathBuf, depth: u8, format: GraphFormat, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();
    let depth_clamped = depth.min(3);
    info!("graph: {path_str} depth={depth_clamped} format={format:?}");

    let view = build_relation_graph(&graph, path_str.as_ref(), depth_clamped)?;
    let rendered = match format {
        GraphFormat::Text => render_graph_text(&view),
        GraphFormat::Dot => render_graph_dot(&view),
        GraphFormat::Mermaid => render_graph_mermaid(&view),
    };
    print!("{rendered}");
    Ok(())
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
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let modified_after = match (params.modified_after, params.modified_within_days) {
        (Some(date), _) => Some(parse_date_to_timestamp(&date)?),
        (None, Some(days)) => Some(now - days * 86_400),
        (None, None) => None,
    };

    Ok(FindFilter {
        state: params.state,
        extension: params.extension.map(normalize_extension),
        created_after: params
            .created_after
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

fn resolve_watch_roots(path: Option<&Path>, config: &OrgConfig) -> Result<Vec<PathBuf>> {
    let mut raw_roots = Vec::new();
    if let Some(path) = path {
        raw_roots.push(path.to_path_buf());
    }
    raw_roots.extend(config.watch.roots.iter().cloned());
    if raw_roots.is_empty() {
        raw_roots.push(PathBuf::from("."));
    }

    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for root in raw_roots {
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        if seen.insert(canonical.clone()) {
            roots.push(canonical);
        }
    }
    Ok(roots)
}

fn cmd_history(path: &PathBuf, limit: usize, json: bool, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();
    debug!("history: {path_str} limit={limit}");

    let entries = graph.get_history(&path_str, limit)?;

    if entries.is_empty() {
        println!("no history found for: {path_str}");
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    println!("history: {path_str}");
    println!("{:<20}  {:<12}  detail", "time", "event");
    println!("{}", "-".repeat(72));
    for e in &entries {
        let ts = format_ts(e.recorded_at);
        let detail = match e.event.as_str() {
            "created" => format!(
                "lifecycle={}{}",
                e.new_lifecycle.as_deref().unwrap_or("?"),
                e.size_bytes
                    .map(|b| format!("  size={b}"))
                    .unwrap_or_default()
            ),
            "modified" => format!(
                "hash={}",
                e.content_hash
                    .as_deref()
                    .map(|h| &h[..16.min(h.len())])
                    .unwrap_or("?")
            ),
            "lifecycle" => format!(
                "{} → {}",
                e.old_lifecycle.as_deref().unwrap_or("?"),
                e.new_lifecycle.as_deref().unwrap_or("?")
            ),
            "renamed" => format!("from {}", e.old_path.as_deref().unwrap_or("?")),
            "deleted" => format!("was {}", e.old_lifecycle.as_deref().unwrap_or("?")),
            other => other.to_string(),
        };
        println!("{ts:<20}  {:<12}  {detail}", e.event);
    }
    Ok(())
}

fn cmd_search_like(
    like_path: &PathBuf,
    limit: Option<usize>,
    dir: Option<&Path>,
    config: &OrgConfig,
    db_path: &Path,
) -> Result<()> {
    if !db_path.exists() {
        anyhow::bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }
    let canonical = std::fs::canonicalize(like_path).unwrap_or_else(|_| like_path.clone());
    let path_str = canonical.to_string_lossy().to_string();
    let limit = limit.unwrap_or(config.search.default_limit);

    info!("search --like: {path_str} limit={limit}");
    let results = search_by_example(&path_str, limit, 0, dir, config)?;

    if results.items.is_empty() {
        println!("(no results — run `organon index` first or file not indexed)");
        return Ok(());
    }

    println!("{:<6}  PATH", "SCORE");
    println!("{}", "-".repeat(80));
    for hit in results.items {
        println!("{:.3}   {}", hit.score, hit.path);
    }
    Ok(())
}

fn impact_risk_level(total: usize) -> &'static str {
    match total {
        0 => "none",
        1..=3 => "low",
        4..=10 => "medium",
        _ => "high",
    }
}

fn cmd_impact(path: &PathBuf, depth: u8, json: bool, db_path: &Path) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();

    let entries = graph.reverse_deps(&path_str, depth)?;

    if json {
        #[derive(serde::Serialize)]
        struct ImpactReport<'a> {
            path: &'a str,
            depth: u8,
            total: usize,
            direct_dependents: usize,
            risk_level: &'static str,
            entries: &'a [ImpactEntry],
        }
        let direct = entries.iter().filter(|e| e.depth == 1).count();
        println!(
            "{}",
            serde_json::to_string_pretty(&ImpactReport {
                path: &path_str,
                depth,
                total: entries.len(),
                direct_dependents: direct,
                risk_level: impact_risk_level(entries.len()),
                entries: &entries,
            })?
        );
        return Ok(());
    }

    let direct = entries.iter().filter(|e| e.depth == 1).count();
    let risk = impact_risk_level(entries.len());

    println!("impact: {path_str}");
    if entries.is_empty() {
        println!("  risk: {risk} — no dependents found up to depth {depth}");
        return Ok(());
    }
    println!(
        "  risk: {risk} — {} total dependent(s), {} direct (depth 1), max depth {depth}\n",
        entries.len(),
        direct,
    );
    println!("{:<5}  {:<10}  PATH", "DEPTH", "KIND");
    println!("{}", "-".repeat(80));
    for e in &entries {
        println!("  {:<3}  {:<10}  {}", e.depth, e.kind, e.path);
    }
    Ok(())
}

fn cmd_duplicates(
    near: bool,
    threshold: f64,
    limit: usize,
    json: bool,
    db_path: &Path,
    config: &OrgConfig,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let exact = graph.exact_duplicates()?;

    if near {
        // Near-duplicate detection via Python embeddings
        let output = python_run_with_env(
            &[
                "-c",
                &format!(
                    "from ai.embeddings.store import find_near_duplicates; import json; \
                     print(json.dumps(find_near_duplicates(threshold={threshold}, limit={limit}, db_path={:?})))",
                    config.indexer.vectors_path,
                ),
            ],
            &python_env(config),
        )?;
        let near_pairs: Vec<serde_json::Value> = serde_json::from_str(&output)?;

        if json {
            #[derive(serde::Serialize)]
            struct DupReport<'a> {
                exact: &'a [DuplicateGroup],
                near: &'a [serde_json::Value],
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&DupReport {
                    exact: &exact,
                    near: &near_pairs,
                })?
            );
            return Ok(());
        }

        print_exact_dups(&exact);
        println!();
        println!("near-duplicates (similarity >= {threshold:.2}):");
        if near_pairs.is_empty() {
            println!("  (none found)");
        } else {
            for p in &near_pairs {
                let sim = p["similarity"].as_f64().unwrap_or(0.0);
                println!(
                    "  {:.3}  {}  ↔  {}",
                    sim,
                    p["file1"].as_str().unwrap_or("?"),
                    p["file2"].as_str().unwrap_or("?"),
                );
            }
        }
    } else {
        if json {
            println!("{}", serde_json::to_string_pretty(&exact)?);
            return Ok(());
        }
        print_exact_dups(&exact);
    }
    Ok(())
}

fn print_exact_dups(groups: &[DuplicateGroup]) {
    println!("exact duplicates (by content hash):");
    if groups.is_empty() {
        println!("  (none)");
        return;
    }
    for g in groups {
        println!("  {}:", &g.content_hash[..16.min(g.content_hash.len())]);
        for p in &g.paths {
            println!("    {p}");
        }
    }
}

fn cmd_doctor(db_path: &Path, config: &OrgConfig) -> Result<()> {
    println!("organon doctor\n");

    // ── config ────────────────────────────────────────────────────────────────
    let cfg_path = config_path();
    doctor_check(
        "config",
        cfg_path.exists(),
        &cfg_path.display().to_string(),
        if cfg_path.exists() {
            ""
        } else {
            "not found (defaults used)"
        },
    );

    // ── db ────────────────────────────────────────────────────────────────────
    if !db_path.exists() {
        doctor_fail(
            "db",
            &format!(
                "{} — not found; run `organon watch .` first",
                db_path.display()
            ),
        );
        doctor_skip("schema", "db not found");
        doctor_skip("vectors", "skip");
    } else {
        match Graph::open(db_path.to_str().unwrap_or("")) {
            Err(e) => {
                doctor_fail("db", &e.to_string());
                doctor_skip("schema", "db error");
            }
            Ok(graph) => {
                let entities = graph.entity_count().unwrap_or(0);
                let relations = graph.relation_count().unwrap_or(0);
                doctor_ok(
                    "db",
                    &format!(
                        "{} ({entities} entities, {relations} relations)",
                        db_path.display()
                    ),
                );

                let tables = graph.table_names().unwrap_or_default();
                let required = [
                    "entities",
                    "entity_history",
                    "relationships",
                    "entities_fts",
                ];
                let missing: Vec<_> = required
                    .iter()
                    .filter(|t| !tables.iter().any(|n| n == **t))
                    .collect();
                if missing.is_empty() {
                    doctor_ok("schema", &tables.join(", "));
                } else {
                    doctor_fail(
                        "schema",
                        &format!(
                            "missing: {}",
                            missing.iter().map(|t| **t).collect::<Vec<_>>().join(", ")
                        ),
                    );
                }
            }
        }

        // ── vectors dir ───────────────────────────────────────────────────────
        let vectors_path = std::path::PathBuf::from(&config.indexer.vectors_path);
        if vectors_path.exists() {
            doctor_ok("vectors", &config.indexer.vectors_path);
        } else {
            doctor_warn(
                "vectors",
                &format!(
                    "{} (not found; run `organon index`)",
                    config.indexer.vectors_path
                ),
            );
        }
    }

    // ── python ────────────────────────────────────────────────────────────────
    let python_ok = match python_run_with_env(
        &["-c", "import sys; print(sys.version.split()[0])"],
        &python_env(config),
    ) {
        Ok(ver) => {
            doctor_ok("python", &ver);
            true
        }
        Err(e) => {
            doctor_fail("python", &e.to_string());
            false
        }
    };

    // ── python deps ───────────────────────────────────────────────────────────
    if python_ok {
        for dep in &["lancedb", "fastembed"] {
            match python_run_with_env(
                &["-c", &format!("import {dep}; print({dep}.__version__)")],
                &python_env(config),
            ) {
                Ok(ver) => doctor_ok(dep, &ver),
                Err(_) => doctor_fail(dep, "not importable — run `uv sync`"),
            }
        }
    } else {
        doctor_skip("lancedb", "python not available");
        doctor_skip("fastembed", "python not available");
    }

    // ── ollama (optional) ─────────────────────────────────────────────────────
    let ollama_up = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:11434".parse().unwrap(),
        std::time::Duration::from_secs(1),
    )
    .is_ok();
    if ollama_up {
        doctor_ok("ollama", "reachable at localhost:11434");
    } else {
        doctor_skip(
            "ollama",
            "not reachable at localhost:11434 (optional; used for summaries)",
        );
    }

    Ok(())
}

fn doctor_ok(label: &str, detail: &str) {
    println!("  [OK]    {label:<12}  {detail}");
}

fn doctor_fail(label: &str, detail: &str) {
    println!("  [FAIL]  {label:<12}  {detail}");
}

fn doctor_warn(label: &str, detail: &str) {
    println!("  [WARN]  {label:<12}  {detail}");
}

fn doctor_skip(label: &str, detail: &str) {
    println!("  [SKIP]  {label:<12}  {detail}");
}

fn doctor_check(label: &str, ok: bool, _detail: &str, extra: &str) {
    if ok {
        doctor_ok(label, _detail);
    } else {
        doctor_warn(label, extra);
    }
}

fn cmd_query(action: QueryCmd, db_path: &Path, config: &OrgConfig) -> Result<()> {
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
                        // JSON output: run search and emit raw JSON
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

#[cfg(test)]
mod tests {
    use clap::Parser;
    use organon_core::graph::Graph;
    use tempfile::{tempdir, NamedTempFile};

    use super::*;

    #[test]
    fn parse_date_to_timestamp_uses_start_of_day() {
        assert_eq!(
            parse_date_to_timestamp("2026-01-02").unwrap(),
            1_767_312_000
        );
    }

    #[test]
    fn find_accepts_ext_alias() {
        let cli = Cli::try_parse_from(["organon", "find", "--ext", ".rs"]).unwrap();
        match cli.command {
            Cmd::Find { extension, .. } => assert_eq!(extension.as_deref(), Some(".rs")),
            _ => panic!("wrong command parsed"),
        }
    }

    #[test]
    fn build_find_filter_normalizes_extension_and_dates() {
        let filter = build_find_filter(FindFilterParams {
            state: Some("active".to_string()),
            extension: Some(".rs".to_string()),
            created_after: Some("2026-01-01".to_string()),
            modified_after: Some("2026-02-01".to_string()),
            modified_within_days: None,
            larger_than_mb: Some(2),
            limit: 20,
            offset: 5,
        })
        .unwrap();

        assert_eq!(filter.state.as_deref(), Some("active"));
        assert_eq!(filter.extension.as_deref(), Some("rs"));
        assert_eq!(filter.created_after, Some(1_767_225_600));
        assert_eq!(filter.modified_after, Some(1_769_904_000));
        assert_eq!(filter.larger_than, Some(2 * 1024 * 1024));
        assert_eq!(filter.limit, 20);
        assert_eq!(filter.offset, 5);
    }

    #[test]
    fn compute_diff_reports_new_deleted_changed() {
        let dir = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(dir.path()).unwrap();
        let tracked = dir.path().join("tracked.txt");
        let new_file = dir.path().join("new.txt");

        std::fs::write(&tracked, "old").unwrap();
        std::fs::write(&new_file, "brand new").unwrap();

        let db = NamedTempFile::new().unwrap();
        let graph = Graph::open(db.path().to_string_lossy().as_ref()).unwrap();
        graph
            .upsert(&Entity::from_path_with_options(&tracked.to_string_lossy(), false).unwrap())
            .unwrap();

        let mut deleted_entity =
            Entity::from_path_with_options(&tracked.to_string_lossy(), false).unwrap();
        deleted_entity.path = canonical_root
            .join("deleted.txt")
            .to_string_lossy()
            .to_string();
        deleted_entity.name = "deleted.txt".to_string();
        graph.upsert(&deleted_entity).unwrap();

        std::fs::write(&tracked, "new").unwrap();

        let ignore_set = IgnoreSet::load(dir.path(), &[]);
        let diff = compute_diff(&graph, dir.path(), &ignore_set, false).unwrap();
        let tracked_path = std::fs::canonicalize(&tracked).unwrap();
        let new_path = std::fs::canonicalize(&new_file).unwrap();
        let deleted_path = canonical_root.join("deleted.txt");

        assert_eq!(diff.new, vec![new_path.to_string_lossy().to_string()]);
        assert_eq!(
            diff.deleted,
            vec![deleted_path.to_string_lossy().to_string()]
        );
        assert_eq!(
            diff.changed,
            vec![tracked_path.to_string_lossy().to_string()]
        );
    }

    #[test]
    fn export_helpers_render_csv_and_dot() {
        let entity = Entity {
            id: "1".to_string(),
            path: "/tmp/a.rs".to_string(),
            name: "a.rs".to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 10,
            created_at: 1,
            modified_at: 2,
            accessed_at: 3,
            lifecycle: organon_core::entity::LifecycleState::Active,
            content_hash: Some("hash".to_string()),
            summary: Some("summary".to_string()),
            git_author: Some("Alice".to_string()),
        };
        let csv = export_entities_as_csv(std::slice::from_ref(&entity));
        let dot = export_graph_as_dot(
            &[entity],
            &[(
                "/tmp/a.rs".to_string(),
                "/tmp/b.rs".to_string(),
                "imports".to_string(),
            )],
        );

        assert!(csv.starts_with("path,name,extension"));
        assert!(csv.contains("\"/tmp/a.rs\""));
        assert!(dot.contains("digraph organon"));
        assert!(dot.contains("\"/tmp/a.rs\" -> \"/tmp/b.rs\""));
    }

    #[test]
    fn resolve_watch_roots_uses_cli_config_and_fallback() {
        let dir = tempdir().unwrap();
        let cli_root = dir.path().join("cli");
        let cfg_root = dir.path().join("cfg");
        std::fs::create_dir_all(&cli_root).unwrap();
        std::fs::create_dir_all(&cfg_root).unwrap();

        let mut config = OrgConfig::default();
        config.watch.roots = vec![cfg_root.clone()];

        let roots = resolve_watch_roots(Some(&cli_root), &config).unwrap();
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&std::fs::canonicalize(&cli_root).unwrap()));
        assert!(roots.contains(&std::fs::canonicalize(&cfg_root).unwrap()));

        let empty = resolve_watch_roots(None, &OrgConfig::default()).unwrap();
        assert_eq!(empty.len(), 1);
    }

    // ── doctor ────────────────────────────────────────────────────────────────

    #[test]
    fn doctor_parses_as_subcommand() {
        let cli = Cli::try_parse_from(["organon", "doctor"]).unwrap();
        assert!(matches!(cli.command, Cmd::Doctor));
    }

    #[test]
    fn doctor_healthy_state_does_not_error() {
        let db_file = NamedTempFile::new().unwrap();
        let db_path = db_file.path();
        // Create a real graph so the DB is valid.
        Graph::open(db_path.to_str().unwrap()).unwrap();
        let config = OrgConfig::default();
        // Should return Ok even when Python/ollama are unavailable.
        let result = cmd_doctor(db_path, &config);
        assert!(result.is_ok(), "cmd_doctor returned Err: {result:?}");
    }

    #[test]
    fn doctor_degraded_state_does_not_panic() {
        let config = OrgConfig::default();
        // Point at nonexistent DB — doctor should report issues but not panic/err.
        let missing = std::path::Path::new("/tmp/organon_test_missing_db_never_exists.db");
        let result = cmd_doctor(missing, &config);
        assert!(
            result.is_ok(),
            "doctor should return Ok even with degraded state"
        );
    }

    // ── impact risk level ─────────────────────────────────────────────────────

    #[test]
    fn impact_risk_level_thresholds() {
        assert_eq!(impact_risk_level(0), "none");
        assert_eq!(impact_risk_level(1), "low");
        assert_eq!(impact_risk_level(3), "low");
        assert_eq!(impact_risk_level(4), "medium");
        assert_eq!(impact_risk_level(10), "medium");
        assert_eq!(impact_risk_level(11), "high");
        assert_eq!(impact_risk_level(100), "high");
    }
}
