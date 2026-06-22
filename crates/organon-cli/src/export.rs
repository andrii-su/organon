//! Data export and filesystem diff commands.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use organon_core::{entity::Entity, graph::Graph, ignore::IgnoreSet};

use crate::{open_graph, ExportFormat};

/// Graph state diffed against the filesystem.
#[derive(Debug, serde::Serialize, PartialEq, Eq)]
pub(crate) struct DiffReport {
    pub new: Vec<String>,
    pub deleted: Vec<String>,
    pub changed: Vec<String>,
}

/// Compare the graph DB against the filesystem and print what has drifted.
pub(crate) fn cmd_diff(
    path: Option<&Path>,
    json: bool,
    db_path: &Path,
    config: &organon_core::config::OrgConfig,
) -> Result<()> {
    let root = path.unwrap_or(Path::new("."));
    let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let ignore_set = IgnoreSet::load(&canonical_root, &config.watch.ignore_segments);
    let graph = open_graph(db_path)?;
    let diff = compute_diff(
        &graph,
        &canonical_root,
        &ignore_set,
        config.watch.use_git_timestamps,
    )?;

    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
        return Ok(());
    }

    for path in &diff.new {
        println!("NEW {path}");
    }
    for path in &diff.deleted {
        println!("DELETED {path}");
    }
    for path in &diff.changed {
        println!("CHANGED {path}");
    }
    Ok(())
}

/// Export entities and/or relations to JSON, CSV, or Graphviz DOT format.
pub(crate) fn cmd_export(
    db_path: &Path,
    format: ExportFormat,
    output: Option<&Path>,
) -> Result<()> {
    let graph = open_graph(db_path)?;
    let entities = graph.all()?;
    let relations = graph.all_relations()?;
    let rendered = match format {
        ExportFormat::Json => export_as_json(&entities, &relations)?,
        ExportFormat::Csv => export_entities_as_csv(&entities),
        ExportFormat::Dot => export_graph_as_dot(&entities, &relations),
    };

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, rendered)?;
    } else {
        print!("{rendered}");
    }
    Ok(())
}

/// Compute the diff between what the graph knows and what is on disk.
pub(crate) fn compute_diff(
    graph: &Graph,
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<DiffReport> {
    let root_prefix = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let root_prefix = root_prefix.to_string_lossy().to_string();
    let db_entities: BTreeMap<String, Entity> = graph
        .all()?
        .into_iter()
        .filter(|entity| entity.path.starts_with(&root_prefix))
        .map(|entity| (entity.path.clone(), entity))
        .collect();
    let fs_entities = collect_fs_entities(root, ignore_set, use_git_timestamps)?;

    let mut new = Vec::new();
    let mut deleted = Vec::new();
    let mut changed = Vec::new();

    for path in fs_entities.keys() {
        if !db_entities.contains_key(path) {
            new.push(path.clone());
        }
    }
    for path in db_entities.keys() {
        if !fs_entities.contains_key(path) {
            deleted.push(path.clone());
        }
    }
    for (path, current) in &fs_entities {
        if let Some(indexed) = db_entities.get(path) {
            if indexed.size_bytes != current.size_bytes
                || indexed.modified_at != current.modified_at
                || indexed.content_hash != current.content_hash
            {
                changed.push(path.clone());
            }
        }
    }

    new.sort();
    deleted.sort();
    changed.sort();

    Ok(DiffReport {
        new,
        deleted,
        changed,
    })
}

fn collect_fs_entities(
    root: &Path,
    ignore_set: &IgnoreSet,
    use_git_timestamps: bool,
) -> Result<BTreeMap<String, Entity>> {
    let mut entities = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        if ignore_set.is_ignored(path) {
            continue;
        }
        let path_str = path.to_string_lossy();
        let entity = Entity::from_path_with_options(&path_str, use_git_timestamps)?;
        entities.insert(entity.path.clone(), entity);
    }
    Ok(entities)
}

fn export_as_json(entities: &[Entity], relations: &[(String, String, String)]) -> Result<String> {
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "entities": entities,
        "relations": relations.iter().map(|(from, to, kind)| serde_json::json!({
            "from": from,
            "to": to,
            "kind": kind,
        })).collect::<Vec<_>>(),
    }))?)
}

fn export_entities_as_csv(entities: &[Entity]) -> String {
    let mut out = String::from(
        "path,name,extension,size_bytes,created_at,modified_at,accessed_at,lifecycle,content_hash,summary,git_author\n",
    );
    for entity in entities {
        let row = [
            csv_field(&entity.path),
            csv_field(&entity.name),
            csv_field(entity.extension.as_deref().unwrap_or("")),
            entity.size_bytes.to_string(),
            entity.created_at.to_string(),
            entity.modified_at.to_string(),
            entity.accessed_at.to_string(),
            csv_field(entity.lifecycle.as_str()),
            csv_field(entity.content_hash.as_deref().unwrap_or("")),
            csv_field(entity.summary.as_deref().unwrap_or("")),
            csv_field(entity.git_author.as_deref().unwrap_or("")),
        ];
        out.push_str(&row.join(","));
        out.push('\n');
    }
    out
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn export_graph_as_dot(entities: &[Entity], relations: &[(String, String, String)]) -> String {
    let mut out = String::from("digraph organon {\n");
    for entity in entities {
        out.push_str(&format!("  {:?};\n", entity.path));
    }
    for (from, to, kind) in relations {
        out.push_str(&format!("  {from:?} -> {to:?} [label={kind:?}];\n"));
    }
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use organon_core::{entity::Entity, graph::Graph, ignore::IgnoreSet};
    use tempfile::{tempdir, NamedTempFile};

    use super::*;

    #[test]
    fn compute_diff_reports_new_deleted_changed() {
        let dir = tempdir().unwrap();
        let canonical_root = std::fs::canonicalize(dir.path()).unwrap();
        let tracked = dir.path().join("tracked.txt");
        let new_file = dir.path().join("new.txt");

        std::fs::write(&tracked, "old").unwrap();
        std::fs::write(&new_file, "brand new").unwrap();

        let db = NamedTempFile::new().unwrap();
        let graph = Graph::open(db.path().to_string_lossy().as_ref()).unwrap();
        graph
            .upsert(&Entity::from_path_with_options(&tracked.to_string_lossy(), false).unwrap())
            .unwrap();

        let mut deleted_entity =
            Entity::from_path_with_options(&tracked.to_string_lossy(), false).unwrap();
        deleted_entity.path = canonical_root
            .join("deleted.txt")
            .to_string_lossy()
            .to_string();
        deleted_entity.name = "deleted.txt".to_string();
        graph.upsert(&deleted_entity).unwrap();

        std::fs::write(&tracked, "new").unwrap();

        let ignore_set = IgnoreSet::load(dir.path(), &[]);
        let diff = compute_diff(&graph, dir.path(), &ignore_set, false).unwrap();
        let tracked_path = std::fs::canonicalize(&tracked).unwrap();
        let new_path = std::fs::canonicalize(&new_file).unwrap();
        let deleted_path = canonical_root.join("deleted.txt");

        assert_eq!(diff.new, vec![new_path.to_string_lossy().to_string()]);
        assert_eq!(
            diff.deleted,
            vec![deleted_path.to_string_lossy().to_string()]
        );
        assert_eq!(
            diff.changed,
            vec![tracked_path.to_string_lossy().to_string()]
        );
    }

    #[test]
    fn export_helpers_render_csv_and_dot() {
        let entity = Entity {
            id: "1".to_string(),
            path: "/tmp/a.rs".to_string(),
            name: "a.rs".to_string(),
            extension: Some("rs".to_string()),
            size_bytes: 10,
            created_at: 1,
            modified_at: 2,
            accessed_at: 3,
            lifecycle: organon_core::entity::LifecycleState::Active,
            content_hash: Some("hash".to_string()),
            summary: Some("summary".to_string()),
            git_author: Some("Alice".to_string()),
        };
        let csv = export_entities_as_csv(std::slice::from_ref(&entity));
        let dot = export_graph_as_dot(
            &[entity],
            &[(
                "/tmp/a.rs".to_string(),
                "/tmp/b.rs".to_string(),
                "imports".to_string(),
            )],
        );

        assert!(csv.starts_with("path,name,extension"));
        assert!(csv.contains("\"/tmp/a.rs\""));
        assert!(dot.contains("digraph organon"));
        assert!(dot.contains("\"/tmp/a.rs\" -> \"/tmp/b.rs\""));
    }
}
