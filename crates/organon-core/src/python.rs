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

/// Build `uv run --no-dev --project <root> python <args...>` inside the
/// Organon Python project.
///
/// The root comes only from `ORGANON_PYTHON_PROJECT` or the binary's own
/// location ([`trusted_python_project_root`]), never from the current
/// directory: a workspace must not be able to supply the `ai` package that
/// runs, and uv must not sync the workspace's own project.
pub fn python_invocation(args: &[&str]) -> anyhow::Result<PythonInvocation> {
    Ok(python_invocation_in(&trusted_python_project_root()?, args))
}

/// Build `uv run --project <root> python <args...>` that runs *inside* `root`.
///
/// Running from the trusted root keeps `python -m` and uv project discovery
/// from picking up an `ai/` package or uv config in the caller's workspace.
pub fn python_invocation_in(root: &Path, args: &[&str]) -> PythonInvocation {
    let mut full_args = vec![
        "run".to_string(),
        // Runtime only: don't install the dev group (pytest, ruff, ...) into
        // an installed copy on first use.
        "--no-dev".to_string(),
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

/// The Organon Python project root (`pyproject.toml` + `ai/indexer.py`):
/// `ORGANON_PYTHON_PROJECT`, else the nearest ancestor of the running
/// binary. Never the current directory, which is an arbitrary workspace.
pub fn trusted_python_project_root() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()
        .ok()
        .map(|exe| std::fs::canonicalize(&exe).unwrap_or(exe));
    resolve_python_root(std::env::var("ORGANON_PYTHON_PROJECT").ok(), exe.as_deref())
}

/// An empty `ORGANON_PYTHON_PROJECT` counts as unset; a relative one is made
/// absolute. A set value must point at the Organon project itself.
fn resolve_python_root(env_root: Option<String>, exe: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(raw) = env_root.filter(|value| !value.is_empty()) {
        let path = PathBuf::from(&raw);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        if !is_python_project(&path) {
            anyhow::bail!(
                "ORGANON_PYTHON_PROJECT={raw} is not the Organon Python project \
                 (expected pyproject.toml and ai/indexer.py in {}).\n{}",
                path.display(),
                python_remediation()
            );
        }
        return Ok(path);
    }
    exe.and_then(Path::parent)
        .and_then(|dir| dir.ancestors().find(|d| is_python_project(d)))
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the Organon Python layer could not be found from the organon binary \
                 location.\n{}",
                python_remediation()
            )
        })
}

fn is_python_project(dir: &Path) -> bool {
    dir.join("pyproject.toml").exists() && dir.join("ai/indexer.py").exists()
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

    fn project(dir: &Path) {
        std::fs::create_dir_all(dir.join("ai")).unwrap();
        std::fs::write(dir.join("pyproject.toml"), "").unwrap();
        std::fs::write(dir.join("ai/indexer.py"), "").unwrap();
    }

    #[test]
    fn root_from_binary_location() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let exe = dir.path().join("target/release/organon");
        assert_eq!(
            resolve_python_root(None, Some(&exe)).unwrap(),
            dir.path().to_path_buf()
        );
    }

    #[test]
    fn no_root_is_an_error_not_the_current_directory() {
        let err = resolve_python_root(None, Some(Path::new("/usr/local/bin/organon")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("ORGANON_PYTHON_PROJECT"), "{err}");
    }

    #[test]
    fn env_root_wins_and_must_be_the_project() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let env = Some(dir.path().to_string_lossy().into_owned());
        assert_eq!(
            resolve_python_root(env, None).unwrap(),
            dir.path().to_path_buf()
        );

        let other = tempfile::tempdir().unwrap();
        let err = resolve_python_root(Some(other.path().to_string_lossy().into_owned()), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("is not the Organon Python project"), "{err}");
    }

    #[test]
    fn empty_env_root_counts_as_unset() {
        let dir = tempfile::tempdir().unwrap();
        project(dir.path());
        let exe = dir.path().join("bin/organon");
        assert_eq!(
            resolve_python_root(Some(String::new()), Some(&exe)).unwrap(),
            dir.path().to_path_buf()
        );
    }

    #[test]
    fn relative_env_root_is_made_absolute() {
        let err = resolve_python_root(Some("no/such/dir".to_string()), None)
            .unwrap_err()
            .to_string();
        let expected = std::env::current_dir().unwrap().join("no/such/dir");
        assert!(err.contains(&expected.display().to_string()), "{err}");
    }

    #[test]
    fn invocation_in_root_pins_project_and_working_dir() {
        let root = Path::new("/opt/organon");
        let invocation = python_invocation_in(root, &["-m", "ai.bridge"]);
        assert_eq!(
            invocation.args,
            [
                "run",
                "--no-dev",
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
