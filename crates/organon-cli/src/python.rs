use std::path::PathBuf;
use std::process::Command;

use anyhow::{bail, Result};
use log::debug;

/// Resolve Python interpreter.
///
/// Search order:
/// 1. `.venv/bin/python` relative to the organon binary (works when installed to ~/.local/bin)
/// 2. `.venv/bin/python` relative to CWD (works when running from project root)
/// 3. System `python3`
pub fn python_bin() -> PathBuf {
    // Walk up from the binary's real location to find a .venv
    if let Ok(exe) = std::env::current_exe() {
        // Resolve symlinks so ~/.local/bin/organon → actual binary location
        let real = std::fs::canonicalize(&exe).unwrap_or(exe);
        // Try: <binary_dir>/.venv, <binary_dir>/../.venv, etc. (up to 4 levels)
        let mut dir = real.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            if let Some(d) = dir {
                let candidate = d.join(".venv/bin/python");
                if candidate.exists() {
                    debug!("python: using .venv from binary dir: {}", candidate.display());
                    return candidate;
                }
                dir = d.parent().map(|p| p.to_path_buf());
            } else {
                break;
            }
        }
    }

    // Fallback: CWD-relative .venv (project root invocation)
    let cwd_venv = PathBuf::from(".venv/bin/python");
    if cwd_venv.exists() {
        debug!("python: using .venv from CWD");
        return cwd_venv;
    }

    debug!("python: falling back to system python3");
    PathBuf::from("python3")
}

/// Run a Python command, capture stdout. Returns trimmed output or error.
pub fn python_run(args: &[&str]) -> Result<String> {
    debug!("python_run: {:?}", args);
    let out = Command::new(python_bin()).args(args).output()?;
    if !out.status.success() {
        bail!(
            "python error (exit {}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run a Python command, inherit stdio (for interactive processes like MCP server).
pub fn python_exec(args: &[&str]) -> Result<()> {
    debug!("python_exec: {:?}", args);
    let status = Command::new(python_bin()).args(args).status()?;
    if !status.success() {
        bail!("python process exited with: {}", status);
    }
    Ok(())
}
