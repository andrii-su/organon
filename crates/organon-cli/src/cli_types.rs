//! CLI argument types — all Clap enum definitions for subcommands and value types.

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use crate::search::SearchMode;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum GraphFormat {
    Text,
    Dot,
    Mermaid,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum ExportFormat {
    Json,
    Csv,
    Dot,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum PlanFormat {
    Text,
    Json,
}

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Watch a directory and index all changes
    Watch {
        path: Option<PathBuf>,
        #[arg(long)]
        index_interval: Option<u64>,
        #[arg(long)]
        no_index: bool,
        #[arg(long)]
        detect_renames: bool,
        #[arg(long)]
        daemon: bool,
    },

    /// Manage background watch daemons
    #[command(
        after_long_help = "Examples:\n  organon daemon list\n  organon daemon status\n  organon daemon logs <id>\n  organon daemon stop <id>"
    )]
    Daemon {
        #[command(subcommand)]
        action: DaemonCmd,
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
        #[arg(long, value_name = "YYYY-MM-DD")]
        created_after: Option<String>,
        #[arg(long, value_name = "YYYY-MM-DD")]
        modified_after: Option<String>,
        #[arg(long)]
        modified_within_days: Option<i64>,
        #[arg(long)]
        larger_than_mb: Option<u64>,
        #[arg(short, long, default_value = "20")]
        limit: usize,
    },

    /// Remove dead entities and stale relations
    #[command(visible_alias = "cleanup")]
    Clean {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        dead_only: bool,
        #[arg(long)]
        stale_relations_only: bool,
    },

    /// Generate shell completion scripts
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
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
        after_long_help = "Examples:\n  organon search \"sqlite graph\"\n  organon search \"imports\" --state active --ext rs\n  organon search \"auth token\" --mode hybrid --explain\n  organon search --like src/auth.rs --limit 5"
    )]
    Search {
        query: Option<String>,
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
        #[arg(long)]
        explain: bool,
    },

    /// Build a compact agent context pack from a query and/or path
    #[command(
        after_long_help = "Examples:\n  organon context \"auth refactor\" --scope . --mode hybrid\n  organon context --path src/auth.rs --budget 8000 --json"
    )]
    Context {
        query: Option<String>,
        #[arg(long)]
        path: Option<PathBuf>,
        #[arg(long, visible_alias = "dir")]
        scope: Option<PathBuf>,
        #[arg(long, default_value = "12000")]
        budget: usize,
        #[arg(short, long)]
        limit: Option<usize>,
        #[arg(long, value_enum)]
        mode: Option<SearchMode>,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, visible_alias = "ext")]
        extension: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Run the Python indexer
    Index {
        path: Option<PathBuf>,
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

    /// Start the MCP server
    Mcp {
        path: Option<PathBuf>,
        #[arg(long)]
        scope: Option<PathBuf>,
        #[arg(long)]
        global: bool,
        #[arg(long)]
        sse: bool,
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
        #[arg(short, long, default_value = "20")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Show reverse dependencies — what would break if this file changes
    #[command(
        after_long_help = "Examples:\n  organon impact src/auth.rs\n  organon impact src/graph.rs --depth 3\n  organon impact src/lib.rs --json"
    )]
    Impact {
        path: PathBuf,
        #[arg(short, long, default_value = "5")]
        depth: u8,
        #[arg(long)]
        json: bool,
    },

    /// Find exact duplicate files by content hash
    #[command(after_long_help = "Examples:\n  organon duplicates\n  organon duplicates --json")]
    Duplicates {
        #[arg(long)]
        json: bool,
    },

    /// Check graph/index freshness and runtime health for a workspace
    #[command(after_long_help = "Examples:\n  organon health\n  organon health . --json")]
    Health {
        path: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },

    /// Diagnose organon installation and runtime health
    Doctor,

    /// Discover tests related to changed or planned files
    #[command(
        after_long_help = "Examples:\n  organon related-tests src/lib.rs\n  organon related-tests src/lib.rs crates/core/src/foo.rs --json"
    )]
    RelatedTests {
        paths: Vec<PathBuf>,
        #[arg(short, long, default_value = "20")]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Build a compact change plan from search, impact, and test discovery
    #[command(
        after_long_help = "Examples:\n  organon plan \"add health command\" --file crates/organon-cli/src/main.rs\n  organon plan \"refactor graph helpers\" --limit 5 --format json"
    )]
    Plan {
        task: String,
        #[arg(long = "file", short = 'f')]
        files: Vec<PathBuf>,
        #[arg(short, long, default_value = "8")]
        limit: usize,
        #[arg(long, value_enum, default_value = "text")]
        format: PlanFormat,
    },

    /// Manage saved search/find queries
    #[command(
        after_long_help = "Examples:\n  organon query save stale-rs --state dormant --ext rs\n  organon query save auth-area --search \"auth token\" --mode hybrid\n  organon query list\n  organon query run stale-rs\n  organon query delete stale-rs"
    )]
    Query {
        #[command(subcommand)]
        action: QueryCmd,
    },

    /// Manage registered workspaces and per-workspace storage
    #[command(
        after_long_help = "Examples:\n  organon workspace add . --default\n  organon workspace list\n  organon workspace status\n  organon workspace default my-project\n  organon workspace remove my-project"
    )]
    Workspace {
        #[command(subcommand)]
        action: WorkspaceCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum QueryCmd {
    /// Save a named query (find by default; use --search for semantic search)
    Save {
        name: String,
        #[arg(long, short = 'q')]
        search: Option<String>,
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
        #[arg(long)]
        json: bool,
    },
    /// Delete a saved query
    Delete { name: String },
    /// Show the definition of a saved query
    Show { name: String },
}

#[derive(Subcommand)]
pub(crate) enum WorkspaceCmd {
    /// Register a workspace path
    Add {
        path: PathBuf,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        default: bool,
    },
    /// List registered workspaces
    List,
    /// Remove a workspace by id, name, or path
    Remove { selector: String },
    /// Show or set the default workspace
    Default { selector: Option<String> },
    /// Show registry and effective storage paths
    Status { path: Option<PathBuf> },
}

#[derive(Subcommand)]
pub(crate) enum DaemonCmd {
    /// List known watch daemons
    List,
    /// Show daemon status; omit id to show all
    Status { id: Option<String> },
    /// Stop a watch daemon
    Stop { id: String },
    /// Print recent daemon logs
    Logs {
        id: String,
        #[arg(short, long, default_value = "80")]
        lines: usize,
    },
}
