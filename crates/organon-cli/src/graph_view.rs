use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::Result;
use organon_core::graph::Graph;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeView {
    pub from: String,
    pub to: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationGraphView {
    pub nodes: Vec<String>,
    pub edges: Vec<GraphEdgeView>,
    pub cycles: Vec<Vec<String>>,
}

pub fn build_relation_graph(graph: &Graph, path: &str, depth: u8) -> Result<RelationGraphView> {
    let mut visited = BTreeSet::new();
    let mut seen_edges = BTreeSet::new();
    let mut edges = Vec::new();
    let mut queue = VecDeque::from([(path.to_string(), 0u8)]);

    while let Some((current, level)) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if level >= depth {
            continue;
        }

        for (from, to, kind) in graph.get_relations(&current)? {
            let edge_key = format!("{from}\n{to}\n{kind}");
            if seen_edges.insert(edge_key) {
                edges.push(GraphEdgeView {
                    from: from.clone(),
                    to: to.clone(),
                    kind: kind.clone(),
                });
            }
            let neighbor = if from == current { to } else { from };
            if !visited.contains(&neighbor) {
                queue.push_back((neighbor, level + 1));
            }
        }
    }

    let mut view = RelationGraphView {
        nodes: visited.into_iter().collect(),
        edges,
        cycles: Vec::new(),
    };
    view.cycles = detect_cycles(&view.edges);
    Ok(view)
}

pub fn detect_cycles(edges: &[GraphEdgeView]) -> Vec<Vec<String>> {
    let mut adjacency: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in edges {
        adjacency.entry(&edge.from).or_default().push(&edge.to);
    }

    let mut seen = BTreeSet::new();
    let mut cycles = Vec::new();
    for start in adjacency.keys().copied() {
        let mut path = vec![start.to_string()];
        let mut visited = BTreeSet::from([start.to_string()]);
        detect_cycles_from(
            start,
            start,
            &adjacency,
            &mut visited,
            &mut path,
            &mut seen,
            &mut cycles,
        );
    }

    cycles.sort();
    cycles
}

fn detect_cycles_from(
    start: &str,
    current: &str,
    adjacency: &BTreeMap<&str, Vec<&str>>,
    visited: &mut BTreeSet<String>,
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    cycles: &mut Vec<Vec<String>>,
) {
    if let Some(next_nodes) = adjacency.get(current) {
        for next in next_nodes {
            if *next == start && path.len() > 1 {
                let mut cycle = path.clone();
                cycle.push(start.to_string());
                let key = canonical_cycle_key(&cycle);
                if seen.insert(key) {
                    cycles.push(cycle);
                }
                continue;
            }

            if visited.contains(*next) {
                continue;
            }

            visited.insert((*next).to_string());
            path.push((*next).to_string());
            detect_cycles_from(start, next, adjacency, visited, path, seen, cycles);
            path.pop();
            visited.remove(*next);
        }
    }
}

fn canonical_cycle_key(cycle: &[String]) -> String {
    let nodes = &cycle[..cycle.len().saturating_sub(1)];
    if nodes.is_empty() {
        return String::new();
    }
    let mut best = None::<String>;
    for idx in 0..nodes.len() {
        let mut rotated = Vec::with_capacity(nodes.len());
        rotated.extend_from_slice(&nodes[idx..]);
        rotated.extend_from_slice(&nodes[..idx]);
        let key = rotated.join("\u{1f}");
        if best.as_ref().is_none_or(|current| key < *current) {
            best = Some(key);
        }
    }
    best.unwrap_or_default()
}

pub fn render_graph_text(view: &RelationGraphView) -> String {
    let mut out = String::new();
    out.push_str(&format!("nodes ({}):\n", view.nodes.len()));
    for node in &view.nodes {
        out.push_str(&format!("  {node}\n"));
    }
    if !view.edges.is_empty() {
        out.push_str(&format!("\nedges ({}):\n", view.edges.len()));
        for edge in &view.edges {
            out.push_str(&format!(
                "  {} --[{}]--> {}\n",
                edge.from, edge.kind, edge.to
            ));
        }
    }
    if !view.cycles.is_empty() {
        out.push_str(&format!("\ncycles detected ({}):\n", view.cycles.len()));
        for cycle in &view.cycles {
            out.push_str(&format!("  {}\n", cycle.join(" -> ")));
        }
    }
    out
}

pub fn render_graph_dot(view: &RelationGraphView) -> String {
    let mut out = String::from("digraph organon {\n");
    for node in &view.nodes {
        out.push_str(&format!("  {node:?};\n"));
    }
    for edge in &view.edges {
        out.push_str(&format!(
            "  {:?} -> {:?} [label={:?}];\n",
            edge.from, edge.to, edge.kind
        ));
    }
    out.push_str("}\n");
    if !view.cycles.is_empty() {
        out.push_str("// cycles detected:\n");
        for cycle in &view.cycles {
            out.push_str(&format!("// {}\n", cycle.join(" -> ")));
        }
    }
    out
}

pub fn render_graph_mermaid(view: &RelationGraphView) -> String {
    let mut out = String::from("graph TD\n");
    let mut aliases = BTreeMap::new();
    for (idx, node) in view.nodes.iter().enumerate() {
        let alias = format!("n{idx}");
        aliases.insert(node.clone(), alias.clone());
        out.push_str(&format!("  {alias}[\"{}\"]\n", escape_mermaid(node)));
    }
    for edge in &view.edges {
        let from = aliases
            .get(&edge.from)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let to = aliases
            .get(&edge.to)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!(
            "  {from} -->|{}| {to}\n",
            escape_mermaid(&edge.kind)
        ));
    }
    if !view.cycles.is_empty() {
        out.push_str("%% cycles detected:\n");
        for cycle in &view.cycles {
            out.push_str(&format!("%% {}\n", cycle.join(" -> ")));
        }
    }
    out
}

fn escape_mermaid(value: &str) -> String {
    value.replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_cycles_finds_two_node_cycle() {
        let cycles = detect_cycles(&[
            GraphEdgeView {
                from: "a".to_string(),
                to: "b".to_string(),
                kind: "imports".to_string(),
            },
            GraphEdgeView {
                from: "b".to_string(),
                to: "a".to_string(),
                kind: "imports".to_string(),
            },
        ]);

        assert_eq!(
            cycles,
            vec![vec!["a".to_string(), "b".to_string(), "a".to_string()]]
        );
    }

    #[test]
    fn render_graph_formats_include_cycle_info() {
        let view = RelationGraphView {
            nodes: vec!["a".to_string(), "b".to_string()],
            edges: vec![
                GraphEdgeView {
                    from: "a".to_string(),
                    to: "b".to_string(),
                    kind: "imports".to_string(),
                },
                GraphEdgeView {
                    from: "b".to_string(),
                    to: "a".to_string(),
                    kind: "imports".to_string(),
                },
            ],
            cycles: vec![vec!["a".to_string(), "b".to_string(), "a".to_string()]],
        };

        let text = render_graph_text(&view);
        let dot = render_graph_dot(&view);
        let mermaid = render_graph_mermaid(&view);

        assert!(text.contains("cycles detected (1):"));
        assert!(text.contains("a -> b -> a"));
        assert!(dot.contains("digraph organon"));
        assert!(dot.contains("// cycles detected:"));
        assert!(mermaid.contains("graph TD"));
        assert!(mermaid.contains("%% cycles detected:"));
    }
}
