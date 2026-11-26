use std::io;
use std::path::{Path, PathBuf};
use std::process::Child;

use anyhow::{bail, Result};
use log::debug;

const INDEXER_MODULE: &str = "ai.indexer";

pub use organon_core::python::{python_invocation, python_remediation, PythonInvocation};

pub fn indexer_invocation(
    db_path: &Path,
    watch: Option<u64>,
    path_prefixes: &[PathBuf],
) -> Result<PythonInvocation> {
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

/// Vector coverage for one scope, as reported by `ai.indexer --coverage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct VectorCoverage {
    /// Vector rows in scope.
    pub vectors: usize,
    /// Files with text whose current content has no vector yet.
    pub pending: usize,
    /// Files the indexer never embeds: ignored, sensitive, empty or binary.
    pub not_embeddable: usize,
}

pub fn vector_coverage_with_env(
    db_path: &Path,
    path_prefix: Option<&Path>,
    envs: &[(String, String)],
) -> Result<VectorCoverage> {
    let mut args = vec![
        "-m".to_string(),
        INDEXER_MODULE.to_string(),
        "--coverage".to_string(),
        "--db".to_string(),
        db_path.to_string_lossy().to_string(),
    ];
    if let Some(prefix) = path_prefix {
        args.push("--path-prefix".to_string());
        args.push(prefix.to_string_lossy().to_string());
    }
    let refs: Vec<_> = args.iter().map(String::as_str).collect();
    let output = python_run_with_env(&refs, envs)?;
    Ok(serde_json::from_str(output.trim())?)
}

pub fn spawn_indexer_with_env(
    db_path: &Path,
    watch: Option<u64>,
    path_prefixes: &[PathBuf],
    envs: &[(String, String)],
) -> Result<Child> {
    let invocation = indexer_invocation(db_path, watch, path_prefixes)?;
    debug!("python_spawn: {}", invocation.display());
    let mut cmd = invocation.command();
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.spawn()
        .map_err(|err| python_start_error(err, &invocation.display()))
}

pub fn python_run_with_env(args: &[&str], envs: &[(String, String)]) -> Result<String> {
    let invocation = python_invocation(args)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexer_invocation_builds_single_stable_entrypoint() {
        let invocation = indexer_invocation(
            Path::new("/tmp/entities.db"),
            Some(30),
            &[PathBuf::from("/repo/a"), PathBuf::from("/repo/b")],
        )
        .expect("tests run from inside the Organon repository");
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
