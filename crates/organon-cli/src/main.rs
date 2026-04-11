mod api;
mod format;
mod python;
mod search;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
    graph::{FindFilter, Graph},
    ignore::IgnoreSet,
    scanner,
};

use format::{format_ts, human_bytes};
use python::{python_exec_with_env, python_run_with_env};
use search::{default_search_mode, python_env, search_entities, SearchMode, SearchParams};

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
        after_long_help = "Examples:\n  organon search \"sqlite graph\"\n  organon search \"imports\" --state active --ext rs\n  organon search \"watcher\" --modified-after 2026-01-01 --mode hybrid\n  organon search \"auth token\" --mode hybrid --explain"
    )]
    Search {
        query: String,

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
        } => cmd_watch(path, &db_path, &config, index_interval, no_index),
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
            limit,
            dir,
            mode,
            state,
            extension,
            created_after,
            modified_after,
            explain,
        } => cmd_search(
            &query,
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
    let limit = limit.unwrap_or(config.search.default_limit);
    let mode = mode.unwrap_or_else(|| default_search_mode(config));
    let metadata_filter = build_find_filter(FindFilterParams {
        state,
        extension,
        created_after,
        modified_after,
        modified_within_days: None,
        larger_than_mb: None,
        limit,
        offset: 0,
    })?;
    info!("search: {query:?} limit={limit} dir={dir:?} mode={mode:?} explain={explain}");
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct GraphEdgeView {
    from: String,
    to: String,
    kind: String,
}

#[derive(Clone, Debug, Default)]
struct RelationGraphView {
    nodes: Vec<String>,
    edges: Vec<GraphEdgeView>,
    cycles: Vec<Vec<String>>,
}

fn build_relation_graph(graph: &Graph, path: &str, depth: u8) -> Result<RelationGraphView> {
    let mut visited = BTreeSet::new();
    let mut seen_edges = BTreeSet::new();
    let mut edges = Vec::new();
    let mut queue = VecDeque::from([(path.to_string(), 0u8)]);

    while let Some((current, level)) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if level >= depth {
            continue;
        }

        for (from, to, kind) in graph.get_relations(&current)? {
            let edge_key = format!("{from}\n{to}\n{kind}");
            if seen_edges.insert(edge_key) {
                edges.push(GraphEdgeView {
                    from: from.clone(),
                    to: to.clone(),
                    kind: kind.clone(),
                });
            }
            let neighbor = if from == current { to } else { from };
            if !visited.contains(&neighbor) {
                queue.push_back((neighbor, level + 1));
            }
        }
    }

    let mut view = RelationGraphView {
        nodes: visited.into_iter().collect(),
        edges,
        cycles: Vec::new(),
    };
    view.cycles = detect_cycles(&view.edges);
    Ok(view)
}

fn detect_cycles(edges: &[GraphEdgeView]) -> Vec<Vec<String>> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        adjacency.entry(&edge.from).or_default().push(&edge.to);
    }

    let mut seen = BTreeSet::new();
    let mut cycles = Vec::new();
    for start in adjacency.keys().copied() {
        let mut path = vec![start.to_string()];
        let mut visited = BTreeSet::from([start.to_string()]);
        detect_cycles_from(
            start,
            start,
            &adjacency,
            &mut visited,
            &mut path,
            &mut seen,
            &mut cycles,
        );
    }

    cycles.sort();
    cycles
}

fn detect_cycles_from(
    start: &str,
    current: &str,
    adjacency: &BTreeMap<&str, Vec<&str>>,
    visited: &mut BTreeSet<String>,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    if let Some(next_nodes) = adjacency.get(current) {
        for next in next_nodes {
            if *next == start && path.len() > 1 {
                let mut cycle = path.clone();
                cycle.push(start.to_string());
                let key = canonical_cycle_key(&cycle);
                if seen.insert(key) {
                    cycles.push(cycle);
                }
                continue;
            }

            if visited.contains(*next) {
                continue;
            }

            visited.insert((*next).to_string());
            path.push((*next).to_string());
            detect_cycles_from(start, next, adjacency, visited, path, seen, cycles);
            path.pop();
            visited.remove(*next);
        }
    }
}

fn canonical_cycle_key(cycle: &[String]) -> String {
    let nodes = &cycle[..cycle.len().saturating_sub(1)];
    if nodes.is_empty() {
        return String::new();
    }
    let mut best = None::<String>;
    for idx in 0..nodes.len() {
        let mut rotated = Vec::with_capacity(nodes.len());
        rotated.extend_from_slice(&nodes[idx..]);
        rotated.extend_from_slice(&nodes[..idx]);
        let key = rotated.join("\u{1f}");
        if best.as_ref().is_none_or(|current| key < *current) {
            best = Some(key);
        }
    }
    best.unwrap_or_default()
}

fn render_graph_text(view: &RelationGraphView) -> String {
    let mut out = String::new();
    out.push_str(&format!("nodes ({}):\n", view.nodes.len()));
    for node in &view.nodes {
        out.push_str(&format!("  {node}\n"));
    }
    if !view.edges.is_empty() {
        out.push_str(&format!("\nedges ({}):\n", view.edges.len()));
        for edge in &view.edges {
            out.push_str(&format!(
                "  {} --[{}]--> {}\n",
                edge.from, edge.kind, edge.to
            ));
        }
    }
    if !view.cycles.is_empty() {
        out.push_str(&format!("\ncycles detected ({}):\n", view.cycles.len()));
        for cycle in &view.cycles {
            out.push_str(&format!("  {}\n", cycle.join(" -> ")));
        }
    }
    out
}

fn render_graph_dot(view: &RelationGraphView) -> String {
    let mut out = String::from("digraph organon {\n");
    for node in &view.nodes {
        out.push_str(&format!("  {node:?};\n"));
    }
    for edge in &view.edges {
        out.push_str(&format!(
            "  {:?} -> {:?} [label={:?}];\n",
            edge.from, edge.to, edge.kind
        ));
    }
    out.push_str("}\n");
    if !view.cycles.is_empty() {
        out.push_str("// cycles detected:\n");
        for cycle in &view.cycles {
            out.push_str(&format!("// {}\n", cycle.join(" -> ")));
        }
    }
    out
}

fn render_graph_mermaid(view: &RelationGraphView) -> String {
    let mut out = String::from("graph TD\n");
    let mut aliases = BTreeMap::new();
    for (idx, node) in view.nodes.iter().enumerate() {
        let alias = format!("n{idx}");
        aliases.insert(node.clone(), alias.clone());
        out.push_str(&format!("  {alias}[\"{}\"]\n", escape_mermaid(node)));
    }
    for edge in &view.edges {
        let from = aliases
            .get(&edge.from)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let to = aliases
            .get(&edge.to)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!(
            "  {from} -->|{}| {to}\n",
            escape_mermaid(&edge.kind)
        ));
    }
    if !view.cycles.is_empty() {
        out.push_str("%% cycles detected:\n");
        for cycle in &view.cycles {
            out.push_str(&format!("%% {}\n", cycle.join(" -> ")));
        }
    }
    out
}

fn escape_mermaid(value: &str) -> String {
    value.replace('"', "\\\"")
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
    fn detect_cycles_finds_two_node_cycle() {
        let cycles = detect_cycles(&[
            GraphEdgeView {
                from: "a".to_string(),
                to: "b".to_string(),
                kind: "imports".to_string(),
            },
            GraphEdgeView {
                from: "b".to_string(),
                to: "a".to_string(),
                kind: "imports".to_string(),
            },
        ]);

        assert_eq!(
            cycles,
            vec![vec!["a".to_string(), "b".to_string(), "a".to_string()]]
        );
    }

    #[test]
    fn render_graph_formats_include_cycle_info() {
        let view = RelationGraphView {
            nodes: vec!["a".to_string(), "b".to_string()],
            edges: vec![
                GraphEdgeView {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    kind: "imports".to_string(),
                },
                GraphEdgeView {
                    from: "b".to_string(),
                    to: "a".to_string(),
                    kind: "imports".to_string(),
                },
            ],
            cycles: vec![vec!["a".to_string(), "b".to_string(), "a".to_string()]],
        };

        let text = render_graph_text(&view);
        let dot = render_graph_dot(&view);
        let mermaid = render_graph_mermaid(&view);

        assert!(text.contains("cycles detected (1):"));
        assert!(text.contains("a -> b -> a"));
        assert!(dot.contains("digraph organon"));
        assert!(dot.contains("// cycles detected:"));
        assert!(mermaid.contains("graph TD"));
        assert!(mermaid.contains("%% cycles detected:"));
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
}
