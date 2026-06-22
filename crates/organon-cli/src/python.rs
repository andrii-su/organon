use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use anyhow::{bail, Result};
use log::debug;

const INDEXER_MODULE: &str = "ai.indexer";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonInvocation {
    pub program: String,
    pub args: Vec<String>,
}

impl PythonInvocation {
    fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        cmd
    }

    fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Resolve the uv executable.
///
/// `ORGANON_UV` is mainly for tests and unusual installations. The default path
/// is intentionally stable: every Python call from the CLI goes through uv.
pub fn uv_bin() -> String {
    std::env::var("ORGANON_UV").unwrap_or_else(|_| "uv".to_string())
}

pub fn python_invocation(args: &[&str]) -> PythonInvocation {
    let mut full_args = vec!["run".to_string()];
    if let Some(project_root) = python_project_root() {
        full_args.push("--project".to_string());
        full_args.push(project_root.to_string_lossy().to_string());
    }
    full_args.push("python".to_string());
    full_args.extend(args.iter().map(|arg| (*arg).to_string()));
    PythonInvocation {
        program: uv_bin(),
        args: full_args,
    }
}

pub fn python_project_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("ORGANON_PYTHON_PROJECT") {
        return Some(PathBuf::from(root));
    }

    let mut starts = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        let real = std::fs::canonicalize(&exe).unwrap_or(exe);
        if let Some(parent) = real.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }

    for start in starts {
        for dir in start.ancestors() {
            if dir.join("pyproject.toml").exists() && dir.join("ai/indexer.py").exists() {
                return Some(dir.to_path_buf());
            }
        }
    }
    None
}

pub fn indexer_invocation(
    db_path: &Path,
    watch: Option<u64>,
    path_prefixes: &[PathBuf],
) -> PythonInvocation {
    let mut args = vec![
        "-m".to_string(),
        INDEXER_MODULE.to_string(),
        "--db".to_string(),
        db_path.to_string_lossy().to_string(),
    ];
    if let Some(secs) = watch {
        args.push("--watch".to_string());
        args.push(secs.to_string());
    }
    for prefix in path_prefixes {
        args.push("--path-prefix".to_string());
        args.push(prefix.to_string_lossy().to_string());
    }
    let arg_refs: Vec<_> = args.iter().map(String::as_str).collect();
    python_invocation(&arg_refs)
}

pub fn indexer_health_with_env(envs: &[(String, String)]) -> Result<String> {
    python_run_with_env(&["-m", INDEXER_MODULE, "--health"], envs)
}

pub fn spawn_indexer_with_env(
    db_path: &Path,
    watch: Option<u64>,
    path_prefixes: &[PathBuf],
    envs: &[(String, String)],
) -> Result<Child> {
    let invocation = indexer_invocation(db_path, watch, path_prefixes);
    debug!("python_spawn: {}", invocation.display());
    let mut cmd = invocation.command();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.spawn()
        .map_err(|err| python_start_error(err, &invocation.display()))
}

pub fn python_run_with_env(args: &[&str], envs: &[(String, String)]) -> Result<String> {
    let invocation = python_invocation(args);
    debug!("python_run: {}", invocation.display());
    let mut cmd = invocation.command();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd
        .output()
        .map_err(|err| python_start_error(err, &invocation.display()))?;
    if !out.status.success() {
        bail!(
            "python bridge failed while running `{}` (exit {}):\n{}\n{}",
            invocation.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr),
            python_remediation()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn python_exec_invocation_with_env(
    invocation: &PythonInvocation,
    envs: &[(String, String)],
) -> Result<()> {
    debug!("python_exec: {}", invocation.display());
    let mut cmd = invocation.command();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let status = cmd
        .status()
        .map_err(|err| python_start_error(err, &invocation.display()))?;
    if !status.success() {
        bail!(
            "python bridge failed while running `{}` (exit {status}).\n{}",
            invocation.display(),
            python_remediation()
        );
    }
    Ok(())
}

fn python_start_error(err: io::Error, command: &str) -> anyhow::Error {
    if err.kind() == io::ErrorKind::NotFound {
        anyhow::anyhow!(
            "uv executable not found while running `{command}`.\n{}",
            python_remediation()
        )
    } else {
        anyhow::anyhow!("could not start python bridge `{command}`: {err}")
    }
}

fn python_remediation() -> &'static str {
    "Install uv, then run `uv sync --group dev` from the Organon repository. \
     Override the uv executable with ORGANON_UV=/path/to/uv or the project root with \
     ORGANON_PYTHON_PROJECT=/path/to/organon if needed."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_invocation_uses_uv_run_python() {
        let invocation = python_invocation(&["-m", "ai.indexer", "--health"]);
        assert_eq!(invocation.program, "uv");
        assert_eq!(invocation.args.first().unwrap(), "run");
        assert!(invocation.args.ends_with(&[
            "python".to_string(),
            "-m".to_string(),
            "ai.indexer".to_string(),
            "--health".to_string()
        ]));
    }

    #[test]
    fn indexer_invocation_builds_single_stable_entrypoint() {
        let invocation = indexer_invocation(
            Path::new("/tmp/entities.db"),
            Some(30),
            &[PathBuf::from("/repo/a"), PathBuf::from("/repo/b")],
        );
        assert_eq!(invocation.program, "uv");
        assert!(invocation.args.ends_with(&[
            "python".to_string(),
            "-m".to_string(),
            "ai.indexer".to_string(),
            "--db".to_string(),
            "/tmp/entities.db".to_string(),
            "--watch".to_string(),
            "30".to_string(),
            "--path-prefix".to_string(),
            "/repo/a".to_string(),
            "--path-prefix".to_string(),
            "/repo/b".to_string()
        ]));
    }
}
