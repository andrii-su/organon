mod format;
mod python;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use log::{debug, info};
use organon_core::{graph::Graph, scanner};

use format::{format_ts, human_bytes};
use python::{python_exec, python_run};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "organon", about = "Local semantic filesystem layer", version)]
struct Cli {
    /// SQLite DB path (default: ~/.organon/entities.db)
    #[arg(long, env = "ORGANON_DB")]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Watch a directory and index all changes
    Watch {
        /// Directory to watch (default: current dir)
        path: Option<PathBuf>,

        /// Also run the Python indexer every N seconds (keeps vectors fresh)
        #[arg(long, default_value = "30")]
        index_interval: u64,

        /// Disable auto-indexer (run `organon index --watch` manually)
        #[arg(long)]
        no_index: bool,
    },

    /// Show metadata and lifecycle state for a file
    Status {
        /// File path to inspect
        path: PathBuf,
    },

    /// List files by lifecycle state
    Ls {
        /// Filter by state: born, active, dormant, archived, dead
        #[arg(short, long)]
        state: Option<String>,

        /// Max results
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Show entity graph statistics
    Stats,

    /// Semantic search (requires Python + indexer)
    Search {
        /// Query string
        query: String,

        /// Max results
        #[arg(short, long, default_value = "10")]
        limit: usize,

        /// Scope results to this directory prefix
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },

    /// Run the Python indexer (embed + vectorize)
    Index {
        /// Watch mode: re-index every N seconds
        #[arg(short, long)]
        watch: Option<u64>,
    },

    /// Start the MCP server (for Claude Desktop / Cursor)
    Mcp {
        /// Use SSE transport instead of stdio
        #[arg(long)]
        sse: bool,
    },

    /// List or move files in 'archived' lifecycle state
    Archive {
        /// Only print candidates, don't move anything (default behaviour)
        #[arg(long)]
        dry_run: bool,

        /// Actually move files to ~/.organon/archive/
        #[arg(long)]
        apply: bool,

        /// Limit to files under this directory prefix
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },

    /// Show import/reference graph for a file
    Graph {
        /// File to root the graph at
        path: PathBuf,

        /// BFS depth (max 3)
        #[arg(short, long, default_value = "1")]
        depth: u8,
    },
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    let db_path = resolve_db(cli.db)?;
    debug!("db path: {}", db_path.display());

    match cli.command {
        Cmd::Watch { path, index_interval, no_index } => cmd_watch(path, &db_path, index_interval, no_index),
        Cmd::Status { path }                   => cmd_status(path, &db_path),
        Cmd::Ls { state, limit }               => cmd_ls(state.as_deref(), limit, &db_path),
        Cmd::Stats                             => cmd_stats(&db_path),
        Cmd::Search { query, limit, dir }      => cmd_search(&query, limit, dir.as_deref()),
        Cmd::Index { watch }                   => cmd_index(watch),
        Cmd::Mcp { sse }                       => cmd_mcp(sse),
        Cmd::Archive { dry_run, apply, dir }   => cmd_archive(dry_run, apply, dir.as_deref(), &db_path),
        Cmd::Graph { path, depth }             => cmd_graph(&path, depth),
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn resolve_db(arg: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = arg { return Ok(p); }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    Ok(PathBuf::from(format!("{}/.organon/entities.db", home)))
}

fn open_graph(db_path: &PathBuf) -> Result<Graph> {
    if !db_path.exists() {
        bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }
    Graph::open(db_path.to_str().unwrap())
}

// ── commands ──────────────────────────────────────────────────────────────────

fn cmd_watch(path: Option<PathBuf>, db_path: &PathBuf, index_interval: u64, no_index: bool) -> Result<()> {
    env_logger::init();

    let watch_path = path.unwrap_or_else(|| PathBuf::from("."));
    let watch_str = watch_path.to_str().unwrap();

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    info!("organon watch: {} | db: {}", watch_str, db_path.display());
    let graph = Arc::new(Mutex::new(Graph::open(db_path.to_str().unwrap())?));

    let stats = scanner::scan(watch_str, Arc::clone(&graph))?;
    eprintln!(
        "indexed {} files ({} skipped, {} errors)",
        stats.indexed, stats.skipped, stats.errors
    );
    scanner::refresh_lifecycle(Arc::clone(&graph))?;

    let _refresh_handle = scanner::schedule_lifecycle_refresh(Arc::clone(&graph), 6);

    // Spawn Python indexer as a background child process so vectors stay fresh.
    let _indexer_child = if !no_index {
        let interval_str = index_interval.to_string();
        let python = python::python_bin();
        match std::process::Command::new(&python)
            .args(["-m", "ai.indexer", "--watch", &interval_str])
            .spawn()
        {
            Ok(child) => {
                eprintln!("indexer started (every {}s, pid {})", index_interval, child.id());
                Some(child)
            }
            Err(e) => {
                eprintln!("warning: could not start indexer: {} (run `organon index --watch {}` manually)", e, index_interval);
                None
            }
        }
    } else {
        eprintln!("indexer disabled (--no-index). Run `organon index --watch {}` manually.", index_interval);
        None
    };

    organon_core::watcher::watch(watch_str, graph)
}

fn cmd_status(path: PathBuf, db_path: &PathBuf) -> Result<()> {
    let graph = open_graph(db_path)?;
    let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
    let path_str = canonical.to_string_lossy();
    debug!("status: {}", path_str);

    match graph.get_by_path(&path_str)? {
        None => bail!("not found in graph: {}", path_str),
        Some(e) => {
            println!("path:       {}", e.path);
            println!("lifecycle:  {}", e.lifecycle.as_str());
            println!("size:       {} bytes", e.size_bytes);
            println!("modified:   {}", format_ts(e.modified_at));
            println!("accessed:   {}", format_ts(e.accessed_at));
            if let Some(h) = &e.content_hash {
                println!("hash:       {}…", &h[..16]);
            }
            if let Some(s) = &e.summary {
                println!("summary:    {}", s);
            }
        }
    }
    Ok(())
}

fn cmd_ls(state: Option<&str>, limit: usize, db_path: &PathBuf) -> Result<()> {
    let graph = open_graph(db_path)?;
    let all = graph.all()?;
    debug!("ls: total={} state={:?} limit={}", all.len(), state, limit);

    let filtered: Vec<_> = all
        .iter()
        .filter(|e| state.map_or(true, |s| e.lifecycle.as_str() == s))
        .take(limit)
        .collect();

    if filtered.is_empty() {
        println!("(no entities)");
        return Ok(());
    }

    let col = 10;
    println!("{:<col$}  {}", "LIFECYCLE", "PATH");
    println!("{}", "-".repeat(72));
    for e in filtered {
        println!("{:<col$}  {}", e.lifecycle.as_str(), e.path);
    }
    Ok(())
}

fn cmd_stats(db_path: &PathBuf) -> Result<()> {
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
        println!("  {:10}  {}", state, count);
    }
    Ok(())
}

fn cmd_search(query: &str, limit: usize, dir: Option<&std::path::Path>) -> Result<()> {
    info!("search: {:?} limit={} dir={:?}", query, limit, dir);
    let prefix_arg = match dir {
        Some(p) => {
            let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            format!(", path_prefix={:?}", abs.to_string_lossy().as_ref())
        }
        None => String::new(),
    };
    let output = python_run(&[
        "-c",
        &format!(
            "from ai.embeddings.store import search; import json; \
             print(json.dumps(search({:?}, limit={}{})))",
            query, limit, prefix_arg
        ),
    ])?;

    let results: Vec<serde_json::Value> = serde_json::from_str(&output)?;
    if results.is_empty() {
        println!("(no results — run `organon index` first)");
        return Ok(());
    }

    println!("{:<6}  {}", "SCORE", "PATH");
    println!("{}", "-".repeat(72));
    for r in &results {
        let score = r["score"].as_f64().unwrap_or(0.0);
        let path  = r["path"].as_str().unwrap_or("?");
        println!("{:.3}   {}", score, path);
    }
    Ok(())
}

fn cmd_index(watch: Option<u64>) -> Result<()> {
    let mut args = vec!["-m", "ai.indexer"];
    let watch_str;
    if let Some(secs) = watch {
        info!("index watch mode: {}s", secs);
        watch_str = secs.to_string();
        args.extend(&["--watch", &watch_str]);
    }
    python_exec(&args)
}

fn cmd_mcp(sse: bool) -> Result<()> {
    info!("starting MCP server (sse={})", sse);
    let mut args = vec!["-m", "ai.mcp_server.server"];
    if sse { args.push("--sse"); }
    python_exec(&args)
}

fn cmd_archive(dry_run: bool, apply: bool, dir: Option<&std::path::Path>, db_path: &PathBuf) -> Result<()> {
    use organon_core::entity::LifecycleState;

    let graph = open_graph(db_path)?;
    let all = graph.all()?;

    let candidates: Vec<_> = all
        .iter()
        .filter(|e| e.lifecycle == LifecycleState::Archived)
        .filter(|e| dir.map_or(true, |d| e.path.starts_with(d.to_string_lossy().as_ref())))
        .collect();

    if candidates.is_empty() {
        println!("no archived files found");
        return Ok(());
    }

    let archive_dir = PathBuf::from(format!(
        "{}/.organon/archive",
        std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
    ));

    println!("{} archived file(s){}:", candidates.len(),
        if dry_run { " (dry run)" } else if !apply { " (use --apply to move)" } else { "" });
    println!("{}", "-".repeat(72));

    for e in &candidates {
        println!("  {}", e.path);
        if apply && !dry_run {
            let src = std::path::Path::new(&e.path);
            if src.exists() {
                // Mirror full absolute path under archive_dir to avoid collisions.
                // e.g. /home/user/project/src/foo.rs → ~/.organon/archive/home/user/project/src/foo.rs
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

fn cmd_graph(path: &PathBuf, depth: u8) -> Result<()> {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
    let path_str = canonical.to_string_lossy();
    let depth_clamped = depth.min(3);
    info!("graph: {} depth={}", path_str, depth_clamped);

    let output = python_run(&[
        "-c",
        &format!(
            "from ai.relations.store import get_graph; import json; \
             print(json.dumps(get_graph({:?}, depth={})))",
            path_str.as_ref(), depth_clamped
        ),
    ])?;

    let result: serde_json::Value = serde_json::from_str(&output)?;
    let nodes = result["nodes"].as_array().map(|v| v.as_slice()).unwrap_or(&[]);
    let edges = result["edges"].as_array().map(|v| v.as_slice()).unwrap_or(&[]);

    println!("nodes ({}):", nodes.len());
    for n in nodes {
        println!("  {}", n.as_str().unwrap_or("?"));
    }
    if !edges.is_empty() {
        println!("\nedges ({}):", edges.len());
        for e in edges {
            let from = e["from"].as_str().unwrap_or("?");
            let to   = e["to"].as_str().unwrap_or("?");
            let kind = e["kind"].as_str().unwrap_or("?");
            println!("  {} --[{}]--> {}", from, kind, to);
        }
    }
    Ok(())
}
