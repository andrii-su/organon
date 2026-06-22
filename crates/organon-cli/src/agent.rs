//! Agent-oriented commands: `related-tests` and `plan`.
//!
//! These commands are designed to be called by AI agents (via MCP or CLI) to discover
//! test coverage and build a structured change plan for a task.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use organon_core::{config::OrgConfig, graph::Graph};

use crate::graph_cmds::impact_risk_level;
use crate::{open_graph, PlanFormat};

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(crate) struct TestCandidate {
    pub path: String,
    pub reasons: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct RelatedTestsReport {
    inputs: Vec<String>,
    tests: Vec<TestCandidate>,
    commands: Vec<String>,
}

#[derive(Debug, serde::Serialize)]
struct PlanSearchHit {
    path: String,
    score: f64,
}

#[derive(Debug, serde::Serialize)]
struct PlanImpact {
    path: String,
    risk_level: &'static str,
    dependents: usize,
    direct_dependents: usize,
}

#[derive(Debug, serde::Serialize)]
struct ChangePlan {
    task: String,
    candidate_files: Vec<PlanSearchHit>,
    planned_files: Vec<String>,
    impact: Vec<PlanImpact>,
    related_tests: Vec<TestCandidate>,
    commands: Vec<String>,
    workflow: Vec<String>,
}

/// Discover test files related to the given source files.
///
/// Uses name matching, directory proximity, and import graph edges to rank candidates.
pub(crate) fn cmd_related_tests(
    paths: &[PathBuf],
    limit: usize,
    json: bool,
    db_path: &Path,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let inputs = normalize_input_paths(paths);
    let tests = discover_related_tests(&graph, &inputs, limit)?;
    let commands = discover_test_commands(paths);
    let report = RelatedTestsReport {
        inputs,
        tests,
        commands,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    if report.tests.is_empty() {
        println!("no related tests found");
    } else {
        println!("{:<6}  PATH", "WHY");
        println!("{}", "-".repeat(96));
        for test in &report.tests {
            println!("{:<6}  {}", test.reasons.join(","), test.path);
        }
    }
    if !report.commands.is_empty() {
        println!("\ntest commands:");
        for command in &report.commands {
            println!("  {command}");
        }
    }
    Ok(())
}

/// Build a structured change plan combining search, impact analysis, and test discovery.
pub(crate) fn cmd_plan(
    task: &str,
    files: &[PathBuf],
    limit: usize,
    format: PlanFormat,
    db_path: &Path,
    _config: &OrgConfig,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let planned_files = normalize_input_paths(files);

    // FTS search to find candidate files related to the task description
    let mut candidate_files = graph
        .fts_search(task, limit)
        .unwrap_or_default()
        .into_iter()
        .map(|(path, score)| PlanSearchHit { path, score })
        .collect::<Vec<_>>();
    candidate_files.sort_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.cmp(&b.path))
    });

    let mut impact = Vec::new();
    for path in &planned_files {
        let entries = graph.reverse_deps(path, 3)?;
        let direct = entries.iter().filter(|entry| entry.depth == 1).count();
        impact.push(PlanImpact {
            path: path.clone(),
            risk_level: impact_risk_level(entries.len()),
            dependents: entries.len(),
            direct_dependents: direct,
        });
    }

    let related_tests = discover_related_tests(&graph, &planned_files, limit)?;
    let commands = discover_test_commands(files);
    let plan = ChangePlan {
        task: task.to_string(),
        candidate_files,
        planned_files,
        impact,
        related_tests,
        commands,
        workflow: vec![
            "review candidate files and current status".to_string(),
            "check impact for each planned edit".to_string(),
            "edit the smallest coherent set of files".to_string(),
            "run related tests first, then broader format/test checks".to_string(),
            "run `organon health` and `organon cleanup --dry-run` before handoff".to_string(),
        ],
    };

    if matches!(format, PlanFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    println!("change plan: {}", plan.task);
    if !plan.candidate_files.is_empty() {
        println!("\ncandidate files:");
        for hit in &plan.candidate_files {
            println!("  {:.3}  {}", hit.score, hit.path);
        }
    }
    if !plan.planned_files.is_empty() {
        println!("\nplanned files:");
        for path in &plan.planned_files {
            println!("  {path}");
        }
    }
    if !plan.impact.is_empty() {
        println!("\nimpact:");
        for item in &plan.impact {
            println!(
                "  {}: {} risk ({} dependent(s), {} direct)",
                item.path, item.risk_level, item.dependents, item.direct_dependents
            );
        }
    }
    if !plan.related_tests.is_empty() {
        println!("\nrelated tests:");
        for test in &plan.related_tests {
            println!("  {}  ({})", test.path, test.reasons.join(", "));
        }
    }
    if !plan.commands.is_empty() {
        println!("\ncommands:");
        for command in &plan.commands {
            println!("  {command}");
        }
    }
    println!("\nworkflow:");
    for (idx, step) in plan.workflow.iter().enumerate() {
        println!("  {}. {step}", idx + 1);
    }
    Ok(())
}

fn normalize_input_paths(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            std::fs::canonicalize(path)
                .unwrap_or_else(|_| path.clone())
                .to_string_lossy()
                .to_string()
        })
        .collect()
}

/// Score and rank test files against a set of source files.
///
/// Signals (highest weight first):
/// - import graph edge from test to source
/// - file name stem matches source stem
/// - same package/directory area
fn discover_related_tests(
    graph: &Graph,
    inputs: &[String],
    limit: usize,
) -> Result<Vec<TestCandidate>> {
    let entities = graph.all()?;
    let relations = graph.all_relations()?;
    let mut scores: BTreeMap<String, (i32, BTreeSet<String>)> = BTreeMap::new();

    for entity in &entities {
        if is_likely_test_path(&entity.path) {
            scores
                .entry(entity.path.clone())
                .or_insert_with(|| (1, BTreeSet::from(["test file".to_string()])));
        }
    }

    for input in inputs {
        let stem = Path::new(input)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        for entity in &entities {
            if !is_likely_test_path(&entity.path) || entity.path == *input {
                continue;
            }
            if !stem.is_empty() && path_contains_stem(&entity.path, &stem) {
                add_test_score(&mut scores, &entity.path, 10, "name match");
            }
            if same_package_area(input, &entity.path) {
                add_test_score(&mut scores, &entity.path, 4, "same area");
            }
        }
    }

    for (from, to, kind) in relations {
        for input in inputs {
            if to == *input && is_likely_test_path(&from) {
                add_test_score(&mut scores, &from, 15, &format!("{kind} planned file"));
            }
            if from == *input && is_likely_test_path(&to) {
                add_test_score(&mut scores, &to, 8, &format!("linked by {kind}"));
            }
        }
    }

    let mut ranked = scores
        .into_iter()
        .map(|(path, (score, reasons))| {
            (
                score,
                TestCandidate {
                    path,
                    reasons: reasons.into_iter().collect(),
                },
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.path.cmp(&b.1.path)));
    Ok(ranked
        .into_iter()
        .take(limit.max(1))
        .map(|(_, candidate)| candidate)
        .collect())
}

fn add_test_score(
    scores: &mut BTreeMap<String, (i32, BTreeSet<String>)>,
    path: &str,
    score: i32,
    reason: &str,
) {
    let entry = scores
        .entry(path.to_string())
        .or_insert_with(|| (0, BTreeSet::new()));
    entry.0 += score;
    entry.1.insert(reason.to_string());
}

fn is_likely_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_lowercase();
    let name = Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    normalized.contains("/tests/")
        || normalized.contains("/test/")
        || name.starts_with("test_")
        || name.ends_with("_test.rs")
        || name.ends_with("_tests.rs")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".spec.js")
}

fn path_contains_stem(path: &str, stem: &str) -> bool {
    let path = path.to_lowercase();
    let stem = stem.to_lowercase();
    path.contains(&format!("{stem}_test"))
        || path.contains(&format!("{stem}_tests"))
        || path.contains(&format!("test_{stem}"))
        || path.contains(&format!("{stem}.test"))
        || path.contains(&format!("{stem}.spec"))
}

fn same_package_area(source: &str, test: &str) -> bool {
    let source_parts = source.split('/').collect::<Vec<_>>();
    let test_parts = test.split('/').collect::<Vec<_>>();
    source_parts.iter().take(4).any(|part| {
        !part.is_empty() && test_parts.iter().take(6).any(|candidate| candidate == part)
    })
}

fn discover_test_commands(paths: &[PathBuf]) -> Vec<String> {
    let mut commands = BTreeSet::new();
    let has_rs = paths
        .iter()
        .any(|p| p.extension().is_some_and(|e| e == "rs"));
    let has_py = paths
        .iter()
        .any(|p| p.extension().is_some_and(|e| e == "py"));

    if has_rs || Path::new("Cargo.toml").exists() {
        commands.insert("cargo test --workspace --all-targets".to_string());
    }
    if has_py || Path::new("pyproject.toml").exists() {
        commands.insert("uv run --group dev pytest".to_string());
    }
    commands.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use organon_core::{entity::Entity, graph::Graph};
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn test_path_heuristics_match_common_conventions() {
        assert!(is_likely_test_path("/repo/tests/test_indexer.py"));
        assert!(is_likely_test_path("/repo/src/foo_test.rs"));
        assert!(is_likely_test_path("/repo/web/foo.spec.ts"));
        assert!(!is_likely_test_path("/repo/src/foo.rs"));
        assert!(path_contains_stem("/repo/tests/foo_test.rs", "foo"));
        assert!(path_contains_stem("/repo/tests/test_foo.py", "foo"));
    }

    #[test]
    fn discover_related_tests_uses_names_and_relations() {
        let db_file = NamedTempFile::new().unwrap();
        let graph = Graph::open(db_file.path().to_str().unwrap()).unwrap();
        let source = Entity {
            id: "src".to_string(),
            path: "/repo/src/auth.rs".to_string(),
            name: "auth.rs".to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 1,
            created_at: 1,
            modified_at: 1,
            accessed_at: 1,
            lifecycle: organon_core::entity::LifecycleState::Active,
            content_hash: Some("src".to_string()),
            summary: None,
            git_author: None,
        };
        let test = Entity {
            id: "test".to_string(),
            path: "/repo/tests/auth_test.rs".to_string(),
            name: "auth_test.rs".to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 1,
            created_at: 1,
            modified_at: 1,
            accessed_at: 1,
            lifecycle: organon_core::entity::LifecycleState::Active,
            content_hash: Some("test".to_string()),
            summary: None,
            git_author: None,
        };
        graph.upsert(&source).unwrap();
        graph.upsert(&test).unwrap();
        graph
            .upsert_relation("/repo/tests/auth_test.rs", "/repo/src/auth.rs", "imports")
            .unwrap();

        let tests = discover_related_tests(&graph, &["/repo/src/auth.rs".to_string()], 10).unwrap();
        assert_eq!(tests[0].path, "/repo/tests/auth_test.rs");
        assert!(tests[0].reasons.iter().any(|r| r == "name match"));
        assert!(tests[0].reasons.iter().any(|r| r == "imports planned file"));
    }
}
