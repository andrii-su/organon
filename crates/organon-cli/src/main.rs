//! `organon` — local semantic filesystem layer.
//!
//! Entry point for the CLI binary. Defines the top-level `Cli` parser and dispatches
//! to the command modules. All subcommand arg types live in `cli_types`.

mod agent;
mod cli_types;
mod context;
mod export;
mod format;
mod graph_cmds;
mod graph_view;
mod health;
mod inspect;
mod mcp_cmd;
mod python;
mod queries;
mod query_cmd;
mod search;
mod search_cmds;
mod watch;
mod workspace_cmd;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use chrono::NaiveDate;
use clap::{ArgAction, Parser};
use log::{debug, LevelFilter};
use organon_core::{
    config::OrgConfig,
    graph::{FindFilter, Graph},
    workspace::WorkspaceRegistry,
};

use cli_types::{Cmd, DaemonCmd, ExportFormat, GraphFormat, PlanFormat, QueryCmd, WorkspaceCmd};

// ── Top-level CLI parser ──────────────────────────────────────────────────────

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

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose, cli.quiet);
    let config = OrgConfig::load();
    let workspace_hint = workspace_path_hint(&cli.command);
    let (config, db_path) =
        resolve_effective_config(cli.db.as_deref(), &config, workspace_hint.as_deref())?;
    debug!("db path: {}", db_path.display());

    match cli.command {
        Cmd::Watch {
            path,
            index_interval,
            no_index,
            detect_renames,
            daemon,
        } => watch::cmd_watch(
            path,
            &db_path,
            &config,
            index_interval,
            no_index,
            detect_renames,
            daemon,
        ),
        Cmd::Daemon { action } => watch::cmd_daemon(action),
        Cmd::Status { path } => inspect::cmd_status(path, &db_path),
        Cmd::Ls { state, limit } => inspect::cmd_ls(state.as_deref(), limit, &db_path),
        Cmd::Find {
            state,
            extension,
            created_after,
            modified_after,
            modified_within_days,
            larger_than_mb,
            limit,
        } => inspect::cmd_find(
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
        } => inspect::cmd_clean(
            &db_path,
            &config,
            dry_run,
            apply,
            dead_only,
            stale_relations_only,
        ),
        Cmd::Completions { shell } => inspect::cmd_completions(shell),
        Cmd::Init { force } => inspect::cmd_init(force),
        Cmd::Stats => inspect::cmd_stats(&db_path),
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
            (Some(q), _) => search_cmds::cmd_search(
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
            (None, Some(path)) => {
                search_cmds::cmd_search_like(&path, limit, dir.as_deref(), &config, &db_path)
            }
            (None, None) => anyhow::bail!("provide a search query or --like <path>"),
        },
        Cmd::Context {
            query,
            path,
            scope,
            budget,
            limit,
            mode,
            state,
            extension,
            json,
        } => search_cmds::cmd_context(
            query.as_deref(),
            path.as_deref(),
            scope.as_deref(),
            budget,
            limit,
            mode,
            state,
            extension,
            json,
            &config,
            &db_path,
        ),
        Cmd::Index { path, watch } => {
            search_cmds::cmd_index(path.as_deref(), watch, &db_path, &config)
        }
        Cmd::Diff { path, json } => export::cmd_diff(path.as_deref(), json, &db_path, &config),
        Cmd::Export { format, output } => export::cmd_export(&db_path, format, output.as_deref()),
        Cmd::Mcp {
            path,
            scope,
            global,
            sse,
        } => mcp_cmd::cmd_mcp_with_config(sse, path.as_deref(), scope.as_deref(), global, &config),
        Cmd::Archive {
            dry_run,
            apply,
            dir,
        } => inspect::cmd_archive(dry_run, apply, dir.as_deref(), &db_path),
        Cmd::Graph {
            path,
            depth,
            format,
        } => graph_cmds::cmd_graph(&path, depth, format, &db_path),
        Cmd::History { path, limit, json } => graph_cmds::cmd_history(&path, limit, json, &db_path),
        Cmd::Impact { path, depth, json } => graph_cmds::cmd_impact(&path, depth, json, &db_path),
        Cmd::Duplicates { json } => graph_cmds::cmd_duplicates(json, &db_path),
        Cmd::Health { path, json } => health::cmd_health(path.as_deref(), json, &db_path, &config),
        Cmd::Doctor => health::cmd_doctor(&db_path, &config),
        Cmd::RelatedTests { paths, limit, json } => {
            agent::cmd_related_tests(&paths, limit, json, &db_path)
        }
        Cmd::Plan {
            task,
            files,
            limit,
            format,
        } => agent::cmd_plan(&task, &files, limit, format, &db_path, &config),
        Cmd::Query { action } => query_cmd::cmd_query(action, &db_path, &config),
        Cmd::Workspace { action } => workspace_cmd::cmd_workspace(
            action,
            cli.db.as_deref(),
            workspace_hint.as_deref(),
            &config,
        ),
    }
}

// ── Shared helpers ────────────────────────────────────────────────────────────

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

/// Resolve effective config and DB path, preferring workspace registry over config defaults.
fn resolve_effective_config(
    db_arg: Option<&Path>,
    config: &OrgConfig,
    workspace_hint: Option<&Path>,
) -> Result<(OrgConfig, PathBuf)> {
    let mut effective = config.clone();
    if let Some(db) = db_arg {
        let db_path = db.to_path_buf();
        effective.indexer.db_path = db_path.to_string_lossy().to_string();
        return Ok((effective, db_path));
    }

    let registry = WorkspaceRegistry::load()?;
    let workspace = workspace_hint
        .and_then(|path| registry.match_path(path))
        .or_else(|| registry.default_workspace());
    if let Some(workspace) = workspace {
        let paths = registry.paths_for(&workspace.id);
        effective.indexer.db_path = paths.db_path.to_string_lossy().to_string();
        effective.indexer.vectors_path = paths.vectors_path.to_string_lossy().to_string();
        return Ok((effective, paths.db_path));
    }

    let db_path = PathBuf::from(&effective.indexer.db_path);
    Ok((effective, db_path))
}

/// Derive a filesystem path hint from the command to look up the active workspace.
fn workspace_path_hint(command: &Cmd) -> Option<PathBuf> {
    match command {
        Cmd::Watch { path, .. } => path.clone().or_else(|| Some(PathBuf::from("."))),
        Cmd::Status { path } => Some(path.clone()),
        Cmd::Search { dir, like, .. } => dir.clone().or_else(|| like.clone()),
        Cmd::Context { path, scope, .. } => scope.clone().or_else(|| path.clone()),
        Cmd::Index { path, .. } => path.clone(),
        Cmd::Diff { path, .. } => path.clone().or_else(|| Some(PathBuf::from("."))),
        Cmd::Mcp {
            path,
            scope,
            global,
            ..
        } => {
            if *global {
                None
            } else {
                scope
                    .clone()
                    .or_else(|| path.clone())
                    .or_else(|| Some(PathBuf::from(".")))
            }
        }
        Cmd::Archive { dir, .. } => dir.clone(),
        Cmd::Graph { path, .. } => Some(path.clone()),
        Cmd::History { path, .. } => Some(path.clone()),
        Cmd::Impact { path, .. } => Some(path.clone()),
        Cmd::Health { path, .. } => path.clone().or_else(|| Some(PathBuf::from("."))),
        Cmd::RelatedTests { paths, .. } => paths.first().cloned(),
        Cmd::Plan { files, .. } => files.first().cloned(),
        Cmd::Workspace {
            action:
                WorkspaceCmd::Add { path, .. }
                | WorkspaceCmd::Status {
                    path: Some(path), ..
                },
        } => Some(path.clone()),
        _ => None,
    }
}

/// Resolve config file path.
///
/// Precedence: explicit `ORGANON_CONFIG` override → `ORGANON_HOME`/config.toml →
/// `~/.organon/config.toml`. Routing through [`organon_home`] keeps `ORGANON_HOME`
/// a complete isolation switch for sandboxed runs.
pub(crate) fn config_path() -> PathBuf {
    if let Ok(path) = std::env::var("ORGANON_CONFIG") {
        return PathBuf::from(path);
    }
    organon_home().join("config.toml")
}

/// Open an existing graph DB; bail with a helpful message if it doesn't exist.
pub(crate) fn open_graph(db_path: &Path) -> Result<Graph> {
    if !db_path.exists() {
        anyhow::bail!(
            "DB not found: {}\nRun `organon watch <dir>` first.",
            db_path.display()
        );
    }
    Graph::open(db_path.to_str().unwrap())
}

/// Return the organon home directory (`~/.organon` or `$ORGANON_HOME`).
pub(crate) fn organon_home() -> PathBuf {
    std::env::var("ORGANON_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".organon")
        })
}

/// Resolve watch roots from CLI arg and config, deduplicating by canonical path.
pub(crate) fn resolve_watch_roots(path: Option<&Path>, config: &OrgConfig) -> Result<Vec<PathBuf>> {
    let mut raw_roots = Vec::new();
    if let Some(path) = path {
        raw_roots.push(path.to_path_buf());
    }
    raw_roots.extend(config.watch.roots.iter().cloned());
    if raw_roots.is_empty() {
        raw_roots.push(PathBuf::from("."));
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut roots = Vec::new();
    for root in raw_roots {
        let canonical = std::fs::canonicalize(&root).unwrap_or(root);
        if seen.insert(canonical.clone()) {
            roots.push(canonical);
        }
    }
    Ok(roots)
}

/// Shared find-filter builder used by search, find, and saved-query commands.
pub(crate) struct FindFilterParams {
    pub state: Option<String>,
    pub extension: Option<String>,
    pub created_after: Option<String>,
    pub modified_after: Option<String>,
    pub modified_within_days: Option<i64>,
    pub larger_than_mb: Option<u64>,
    pub limit: usize,
    pub offset: usize,
}

/// Build a `FindFilter` from user-supplied parameters, parsing dates and converting MB → bytes.
pub(crate) fn build_find_filter(params: FindFilterParams) -> Result<FindFilter> {
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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use clap::Parser;

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
    fn resolve_watch_roots_uses_cli_config_and_fallback() {
        let dir = tempfile::tempdir().unwrap();
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

    #[test]
    fn doctor_parses_as_subcommand() {
        let cli = Cli::try_parse_from(["organon", "doctor"]).unwrap();
        assert!(matches!(cli.command, Cmd::Doctor));
    }

    #[test]
    fn cleanup_alias_parses_as_clean() {
        let cli = Cli::try_parse_from(["organon", "cleanup", "--dry-run"]).unwrap();
        match cli.command {
            Cmd::Clean { dry_run, .. } => assert!(dry_run),
            _ => panic!("wrong command parsed"),
        }
    }

    #[test]
    fn health_related_tests_and_plan_parse() {
        let health = Cli::try_parse_from(["organon", "health", ".", "--json"]).unwrap();
        assert!(matches!(health.command, Cmd::Health { json: true, .. }));

        let related =
            Cli::try_parse_from(["organon", "related-tests", "src/lib.rs", "--limit", "3"])
                .unwrap();
        match related.command {
            Cmd::RelatedTests { paths, limit, .. } => {
                assert_eq!(paths, vec![PathBuf::from("src/lib.rs")]);
                assert_eq!(limit, 3);
            }
            _ => panic!("wrong command parsed"),
        }

        let plan = Cli::try_parse_from([
            "organon",
            "plan",
            "change cli",
            "--file",
            "crates/organon-cli/src/main.rs",
        ])
        .unwrap();
        assert!(matches!(plan.command, Cmd::Plan { .. }));
    }

    #[test]
    fn workspace_commands_parse() {
        let add = Cli::try_parse_from(["organon", "workspace", "add", ".", "--default"]).unwrap();
        match add.command {
            Cmd::Workspace {
                action: WorkspaceCmd::Add { path, default, .. },
            } => {
                assert_eq!(path, PathBuf::from("."));
                assert!(default);
            }
            _ => panic!("wrong command parsed"),
        }

        let status = Cli::try_parse_from(["organon", "workspace", "status", "."]).unwrap();
        assert!(matches!(
            status.command,
            Cmd::Workspace {
                action: WorkspaceCmd::Status { .. }
            }
        ));
    }

    #[test]
    fn daemon_commands_parse() {
        let list = Cli::try_parse_from(["organon", "daemon", "list"]).unwrap();
        assert!(matches!(
            list.command,
            Cmd::Daemon {
                action: DaemonCmd::List
            }
        ));

        let logs =
            Cli::try_parse_from(["organon", "daemon", "logs", "abc123", "--lines", "5"]).unwrap();
        match logs.command {
            Cmd::Daemon {
                action: DaemonCmd::Logs { id, lines },
            } => {
                assert_eq!(id, "abc123");
                assert_eq!(lines, 5);
            }
            _ => panic!("wrong command parsed"),
        }
    }
}
