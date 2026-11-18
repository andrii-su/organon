//! Shared resolution of the Python AI layer.
//!
//! Every Python call from Organon (CLI and MCP server) goes through
//! `uv run --project <organon-root> python ...`, so the AI layer's own
//! environment is used no matter where the binary is installed.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonInvocation {
    pub program: String,
    pub args: Vec<String>,
    /// Working directory for the child; `None` inherits the caller's.
    pub current_dir: Option<PathBuf>,
}

impl PythonInvocation {
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        if let Some(dir) = &self.current_dir {
            cmd.current_dir(dir);
        }
        cmd
    }

    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Resolve the uv executable.
///
/// `ORGANON_UV` is mainly for tests and unusual installations.
pub fn uv_bin() -> String {
    std::env::var("ORGANON_UV").unwrap_or_else(|_| "uv".to_string())
}

/// Build `uv run [--project <root>] python <args...>`.
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
        current_dir: None,
    }
}

/// Build `uv run --project <root> python <args...>` that runs *inside* `root`.
///
/// Running from the trusted root keeps `python -m` and uv project discovery
/// from picking up an `ai/` package or uv config in the caller's workspace.
pub fn python_invocation_in(root: &Path, args: &[&str]) -> PythonInvocation {
    let mut full_args = vec![
        "run".to_string(),
        "--project".to_string(),
        root.to_string_lossy().to_string(),
        "python".to_string(),
    ];
    full_args.extend(args.iter().map(|arg| (*arg).to_string()));
    PythonInvocation {
        program: uv_bin(),
        args: full_args,
        current_dir: Some(root.to_path_buf()),
    }
}

/// Like [`python_project_root`], but never looks at the current directory:
/// only `ORGANON_PYTHON_PROJECT` or the running binary's own location.
///
/// Use this where the current directory is an arbitrary user workspace
/// (the MCP server), so a workspace cannot supply the Python code that runs.
pub fn trusted_python_project_root() -> Option<PathBuf> {
    if let Ok(root) = std::env::var("ORGANON_PYTHON_PROJECT") {
        return Some(PathBuf::from(root));
    }
    let exe = std::env::current_exe().ok()?;
    let real = std::fs::canonicalize(&exe).unwrap_or(exe);
    real.parent()?
        .ancestors()
        .find(|dir| is_python_project(dir))
        .map(Path::to_path_buf)
}

fn is_python_project(dir: &Path) -> bool {
    dir.join("pyproject.toml").exists() && dir.join("ai/indexer.py").exists()
}

/// Locate the Organon Python project (the directory holding `pyproject.toml`
/// and `ai/indexer.py`).
///
/// Precedence: `ORGANON_PYTHON_PROJECT` → ancestors of the running binary →
/// ancestors of the current directory.
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
            if is_python_project(dir) {
                return Some(dir.to_path_buf());
            }
        }
    }
    None
}

/// Shown when the Python layer cannot be started or fails.
pub fn python_remediation() -> &'static str {
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
        assert_eq!(invocation.program, uv_bin());
        assert_eq!(invocation.args.first().unwrap(), "run");
        assert!(invocation.args.ends_with(&[
            "python".to_string(),
            "-m".to_string(),
            "ai.indexer".to_string(),
            "--health".to_string()
        ]));
    }

    #[test]
    fn invocation_in_root_pins_project_and_working_dir() {
        let root = Path::new("/opt/organon");
        let invocation = python_invocation_in(root, &["-m", "ai.bridge"]);
        assert_eq!(
            invocation.args,
            [
                "run",
                "--project",
                "/opt/organon",
                "python",
                "-m",
                "ai.bridge"
            ]
        );
        assert_eq!(invocation.current_dir.as_deref(), Some(root));
    }

    #[test]
    fn display_joins_program_and_args() {
        let invocation = PythonInvocation {
            program: "uv".to_string(),
            args: vec!["run".to_string(), "python".to_string()],
            current_dir: None,
        };
        assert_eq!(invocation.display(), "uv run python");
    }
}
