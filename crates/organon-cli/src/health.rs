//! `organon health` and `organon doctor` — runtime diagnostics.

use std::path::{Path, PathBuf};

use anyhow::Result;
use organon_core::{config::OrgConfig, graph::Graph, ignore::IgnoreSet};

use crate::export::{compute_diff, DiffReport};
use crate::python;
use crate::search::python_env;

#[derive(Debug, serde::Serialize)]
pub(crate) struct HealthReport {
    db_path: String,
    db_exists: bool,
    entities: usize,
    relations: usize,
    vectors_exists: bool,
    freshness: Option<DiffReport>,
    status: String,
    notes: Vec<String>,
}

/// Check graph/index freshness and overall runtime health for a workspace.
pub(crate) fn cmd_health(
    path: Option<&Path>,
    json: bool,
    db_path: &Path,
    config: &OrgConfig,
) -> Result<()> {
    let vectors_exists = Path::new(&config.indexer.vectors_path).exists();
    let mut report = HealthReport {
        db_path: db_path.display().to_string(),
        db_exists: db_path.exists(),
        entities: 0,
        relations: 0,
        vectors_exists,
        freshness: None,
        status: "ok".to_string(),
        notes: Vec::new(),
    };

    if !report.db_exists {
        report.status = "fail".to_string();
        report
            .notes
            .push("db missing; run `organon watch <path>`".to_string());
    } else {
        let graph = Graph::open(db_path.to_str().unwrap_or(""))?;
        report.entities = graph.entity_count().unwrap_or(0);
        report.relations = graph.relation_count().unwrap_or(0);

        let root = path.unwrap_or(Path::new("."));
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let ignore_set = IgnoreSet::load(&canonical_root, &config.watch.ignore_segments);
        let diff = compute_diff(
            &graph,
            &canonical_root,
            &ignore_set,
            config.watch.use_git_timestamps,
        )?;
        let stale_count = diff.new.len() + diff.deleted.len() + diff.changed.len();
        if stale_count > 0 {
            report.status = "warn".to_string();
            report.notes.push(format!(
                "{stale_count} freshness issue(s); run `organon watch {}` or `organon index {}`",
                canonical_root.display(),
                canonical_root.display()
            ));
        }
        report.freshness = Some(diff);
    }

    if !report.vectors_exists {
        if report.status == "ok" {
            report.status = "warn".to_string();
        }
        report
            .notes
            .push("vectors missing; run `organon index` for semantic search".to_string());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("organon health: {}", report.status);
    println!("  db:       {}", report.db_path);
    println!("  entities: {}", report.entities);
    println!("  relations: {}", report.relations);
    println!(
        "  vectors:  {}",
        if report.vectors_exists {
            "present"
        } else {
            "missing"
        }
    );
    if let Some(diff) = &report.freshness {
        println!(
            "  freshness: {} new, {} deleted, {} changed",
            diff.new.len(),
            diff.deleted.len(),
            diff.changed.len()
        );
    }
    for note in &report.notes {
        println!("  note: {note}");
    }
    Ok(())
}

/// Diagnose the organon installation: config, db, schema, vectors, Python deps.
pub(crate) fn cmd_doctor(db_path: &Path, config: &OrgConfig) -> Result<()> {
    println!("organon doctor\n");

    let cfg_path = crate::config_path();
    doctor_check(
        "config",
        cfg_path.exists(),
        &cfg_path.display().to_string(),
        if cfg_path.exists() {
            ""
        } else {
            "not found (defaults used)"
        },
    );

    if !db_path.exists() {
        doctor_fail(
            "db",
            &format!(
                "{} — not found; run `organon watch .` first",
                db_path.display()
            ),
        );
        doctor_skip("schema", "db not found");
        doctor_skip("vectors", "skip");
    } else {
        match Graph::open(db_path.to_str().unwrap_or("")) {
            Err(e) => {
                doctor_fail("db", &e.to_string());
                doctor_skip("schema", "db error");
            }
            Ok(graph) => {
                let entities = graph.entity_count().unwrap_or(0);
                let relations = graph.relation_count().unwrap_or(0);
                doctor_ok(
                    "db",
                    &format!(
                        "{} ({entities} entities, {relations} relations)",
                        db_path.display()
                    ),
                );

                let tables = graph.table_names().unwrap_or_default();
                let required = [
                    "entities",
                    "entity_history",
                    "relationships",
                    "entities_fts",
                ];
                let missing: Vec<_> = required
                    .iter()
                    .filter(|t| !tables.iter().any(|n| n == **t))
                    .collect();
                if missing.is_empty() {
                    doctor_ok("schema", &tables.join(", "));
                } else {
                    doctor_fail(
                        "schema",
                        &format!(
                            "missing: {}",
                            missing.iter().map(|t| **t).collect::<Vec<_>>().join(", ")
                        ),
                    );
                }
            }
        }

        let vectors_path = PathBuf::from(&config.indexer.vectors_path);
        if vectors_path.exists() {
            doctor_ok("vectors", &config.indexer.vectors_path);
        } else {
            doctor_warn(
                "vectors",
                &format!(
                    "{} (not found; run `organon index`)",
                    config.indexer.vectors_path
                ),
            );
        }
    }

    let bridge_env = python_env(config);
    let python_ok = match python::indexer_health_with_env(&bridge_env) {
        Ok(health) => {
            doctor_ok("python", &health);
            true
        }
        Err(e) => {
            doctor_fail("python", &e.to_string());
            false
        }
    };

    if python_ok {
        for dep in &["lancedb", "fastembed"] {
            match python::python_run_with_env(
                &["-c", &format!("import {dep}; print({dep}.__version__)")],
                &bridge_env,
            ) {
                Ok(ver) => doctor_ok(dep, &ver),
                Err(_) => doctor_fail(dep, "not importable — run `uv sync`"),
            }
        }
    } else {
        doctor_skip("lancedb", "python not available");
        doctor_skip("fastembed", "python not available");
    }

    Ok(())
}

fn doctor_ok(label: &str, detail: &str) {
    println!("  [OK]    {label:<12}  {detail}");
}

fn doctor_fail(label: &str, detail: &str) {
    println!("  [FAIL]  {label:<12}  {detail}");
}

fn doctor_warn(label: &str, detail: &str) {
    println!("  [WARN]  {label:<12}  {detail}");
}

fn doctor_skip(label: &str, detail: &str) {
    println!("  [SKIP]  {label:<12}  {detail}");
}

fn doctor_check(label: &str, ok: bool, detail: &str, extra: &str) {
    if ok {
        doctor_ok(label, detail);
    } else {
        doctor_warn(label, extra);
    }
}

#[cfg(test)]
mod tests {
    use organon_core::{config::OrgConfig, graph::Graph};
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn doctor_healthy_state_does_not_error() {
        let db_file = NamedTempFile::new().unwrap();
        let db_path = db_file.path();
        Graph::open(db_path.to_str().unwrap()).unwrap();
        let config = OrgConfig::default();
        let result = cmd_doctor(db_path, &config);
        assert!(result.is_ok(), "cmd_doctor returned Err: {result:?}");
    }

    #[test]
    fn doctor_degraded_state_does_not_panic() {
        let config = OrgConfig::default();
        let missing = std::path::Path::new("/tmp/organon_test_missing_db_never_exists.db");
        let result = cmd_doctor(missing, &config);
        assert!(
            result.is_ok(),
            "doctor should return Ok even with degraded state"
        );
    }
}
