//! `organon workspace` — manage registered workspaces and per-workspace storage.

use std::fs;
use std::path::Path;

use anyhow::Result;
use organon_core::{config::OrgConfig, workspace::WorkspaceRegistry};

use crate::WorkspaceCmd;

/// Dispatch workspace subcommands.
pub(crate) fn cmd_workspace(
    action: WorkspaceCmd,
    db_arg: Option<&Path>,
    workspace_hint: Option<&Path>,
    config: &OrgConfig,
) -> Result<()> {
    let mut registry = WorkspaceRegistry::load()?;
    match action {
        WorkspaceCmd::Add {
            path,
            name,
            default,
        } => {
            let entry = registry.add(&path, name, default)?;
            let paths = registry.paths_for(&entry.id);
            fs::create_dir_all(&paths.root)?;
            registry.save()?;
            println!("workspace registered");
            println!("  id:      {}", entry.id);
            println!("  name:    {}", entry.name);
            println!("  path:    {}", entry.path.display());
            println!("  db:      {}", paths.db_path.display());
            println!("  vectors: {}", paths.vectors_path.display());
        }
        WorkspaceCmd::List => {
            if registry.workspaces.is_empty() {
                println!("(no workspaces)");
                return Ok(());
            }
            println!("{:<28}  {:<18}  PATH", "ID", "NAME");
            println!("{}", "-".repeat(96));
            for entry in &registry.workspaces {
                let marker = if registry.default.as_deref() == Some(entry.id.as_str()) {
                    "*"
                } else {
                    " "
                };
                println!(
                    "{marker}{:<27}  {:<18}  {}",
                    entry.id,
                    entry.name,
                    entry.path.display()
                );
            }
        }
        WorkspaceCmd::Remove { selector } => {
            let removed = registry.remove(&selector)?;
            registry.save()?;
            println!("workspace removed");
            println!("  id:   {}", removed.id);
            println!("  path: {}", removed.path.display());
            println!(
                "  storage left intact: {}",
                registry.paths_for(&removed.id).root.display()
            );
        }
        WorkspaceCmd::Default { selector } => {
            if let Some(selector) = selector {
                let entry = registry.set_default(&selector)?;
                registry.save()?;
                println!("default workspace: {} ({})", entry.name, entry.id);
            } else if let Some(entry) = registry.default_workspace() {
                println!("default workspace: {} ({})", entry.name, entry.id);
                println!("  path: {}", entry.path.display());
            } else {
                println!("(no default workspace)");
            }
        }
        WorkspaceCmd::Status { path } => {
            let selected = path.as_deref().or(workspace_hint);
            let matched = selected.and_then(|path| registry.match_path(path));
            println!("workspace registry");
            println!(
                "  registry: {}",
                organon_core::workspace::registry_path().display()
            );
            if let Some(entry) = matched.or_else(|| registry.default_workspace()) {
                let paths = registry.paths_for(&entry.id);
                println!("  active:  {} ({})", entry.name, entry.id);
                println!("  path:    {}", entry.path.display());
                println!("  db:      {}", paths.db_path.display());
                println!("  vectors: {}", paths.vectors_path.display());
            } else {
                println!("  active:  (none)");
                println!("  db:      {}", config.indexer.db_path);
                println!("  vectors: {}", config.indexer.vectors_path);
            }
            if let Some(db_arg) = db_arg {
                println!("  override db: {}", db_arg.display());
            }
            println!("  registered: {}", registry.workspaces.len());
        }
    }
    Ok(())
}
