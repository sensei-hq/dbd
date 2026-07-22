use std::collections::{HashMap, HashSet};

use crate::entity::Entity;

/// Result of building a dependency graph for visualization.
#[derive(Debug, Clone)]
pub struct GraphResult {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub layers: Vec<Vec<String>>,
}

#[derive(Debug, Clone)]
pub struct GraphNode {
    pub name: String,
    pub entity_type: String,
    pub schema: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub from: String,
    pub to: String,
}

/// Build an adjacency map from entity names to their dependencies.
pub fn build_dependency_graph(entities: &[Entity]) -> HashMap<String, HashSet<String>> {
    entities
        .iter()
        .map(|e| {
            let deps: HashSet<String> = e.refers.iter().cloned().collect();
            (e.name.clone(), deps)
        })
        .collect()
}

/// Topological sort using iterative layer extraction.
///
/// Entities with no in-group dependencies are extracted first.
/// Cyclic entities get an error added and are appended at the end.
pub fn sort_by_dependencies(entities: &[Entity]) -> Vec<Entity> {
    if entities.is_empty() {
        return Vec::new();
    }

    let entity_map: HashMap<String, Entity> =
        entities.iter().map(|e| (e.name.clone(), e.clone())).collect();
    let mut remaining = build_dependency_map(entities);
    let mut sorted = Vec::with_capacity(entities.len());

    // Kahn's algorithm: iteratively extract entities with no remaining in-group
    // dependencies, in deterministic (sorted) batches.
    loop {
        let mut ready: Vec<String> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(name, _)| name.clone())
            .collect();

        if ready.is_empty() {
            break;
        }
        ready.sort();

        for name in &ready {
            remaining.remove(name);
            if let Some(entity) = entity_map.get(name) {
                sorted.push(entity.clone());
            }
        }

        // Remove resolved names from remaining deps
        for deps in remaining.values_mut() {
            for name in &ready {
                deps.remove(name);
            }
        }
    }

    // Anything left is cyclic — mark with error and append.
    if !remaining.is_empty() {
        append_cyclic(&remaining, &entity_map, &mut sorted);
    }

    sorted
}

/// Build the in-group dependency map: entity name → the subset of its `refers`
/// that are also in this set, excluding self-references (e.g. a `folders.parent_id`
/// → `folders.id` back-reference).
fn build_dependency_map(entities: &[Entity]) -> HashMap<String, HashSet<String>> {
    let entity_names: HashSet<String> = entities.iter().map(|e| e.name.clone()).collect();
    entities
        .iter()
        .map(|e| {
            let deps: HashSet<String> = e
                .refers
                .iter()
                .filter(|dep| entity_names.contains(*dep) && **dep != e.name)
                .cloned()
                .collect();
            (e.name.clone(), deps)
        })
        .collect()
}

/// Append the entities still in `remaining` (a dependency cycle) to `sorted`,
/// each flagged with a cyclic-dependency error, in deterministic name order.
fn append_cyclic(
    remaining: &HashMap<String, HashSet<String>>,
    entity_map: &HashMap<String, Entity>,
    sorted: &mut Vec<Entity>,
) {
    let mut cyclic_names: Vec<String> = remaining.keys().cloned().collect();
    cyclic_names.sort();

    for name in cyclic_names {
        if let Some(mut entity) = entity_map.get(&name).cloned() {
            entity
                .errors
                .push("Cyclic dependency detected".to_string());
            sorted.push(entity);
        }
    }
}

/// Group entities by dependency level (layer 0 = no deps, layer 1 = depends on layer 0, etc.)
pub fn group_by_dependency_level(entities: &[Entity]) -> Vec<Vec<String>> {
    let mut remaining = build_dependency_map(entities);

    let mut layers = Vec::new();

    loop {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|(_, deps)| deps.is_empty())
            .map(|(name, _)| name.clone())
            .collect();

        if ready.is_empty() {
            break;
        }

        let mut layer = ready;
        layer.sort();

        for name in &layer {
            remaining.remove(name);
        }
        for deps in remaining.values_mut() {
            for name in &layer {
                deps.remove(name);
            }
        }

        layers.push(layer);
    }

    // Cyclic entities form a final layer
    if !remaining.is_empty() {
        let mut cyclic: Vec<String> = remaining.keys().cloned().collect();
        cyclic.sort();
        layers.push(cyclic);
    }

    layers
}

/// Build a graph result for visualization, optionally scoped to one entity's subgraph.
pub fn graph_from_entities(entities: &[Entity], scope: Option<&str>) -> GraphResult {
    let filtered = match scope {
        Some(name) => reachable_subgraph(entities, name),
        None => entities.to_vec(),
    };

    let nodes: Vec<GraphNode> = filtered
        .iter()
        .map(|e| GraphNode {
            name: e.name.clone(),
            entity_type: format!("{:?}", e.entity_type).to_lowercase(),
            schema: e.schema.clone(),
        })
        .collect();

    let entity_names: HashSet<String> = filtered.iter().map(|e| e.name.clone()).collect();
    let edges: Vec<GraphEdge> = filtered
        .iter()
        .flat_map(|e| {
            e.refers
                .iter()
                .filter(|dep| entity_names.contains(*dep))
                .map(|dep| GraphEdge {
                    from: e.name.clone(),
                    to: dep.clone(),
                })
        })
        .collect();

    let layers = group_by_dependency_level(&filtered);

    GraphResult {
        nodes,
        edges,
        layers,
    }
}

/// Collect the transitive closure of dependencies reachable from a named entity.
fn reachable_subgraph(entities: &[Entity], start: &str) -> Vec<Entity> {
    let entity_map: HashMap<&str, &Entity> =
        entities.iter().map(|e| (e.name.as_str(), e)).collect();

    let mut visited = HashSet::new();
    let mut stack = vec![start.to_string()];

    while let Some(name) = stack.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some(entity) = entity_map.get(name.as_str()) {
            for dep in &entity.refers {
                if !visited.contains(dep) {
                    stack.push(dep.clone());
                }
            }
        }
    }

    entities
        .iter()
        .filter(|e| visited.contains(&e.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::EntityType;

    fn entity(name: &str, refers: &[&str]) -> Entity {
        let mut e = Entity::new(EntityType::Table, name);
        e.refers = refers.iter().map(|s| s.to_string()).collect();
        e
    }

    #[test]
    fn sorts_simple_chain() {
        let entities = vec![
            entity("c", &["b"]),
            entity("a", &[]),
            entity("b", &["a"]),
        ];
        let sorted = sort_by_dependencies(&entities);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn sorts_independent_entities_alphabetically() {
        let entities = vec![
            entity("c", &[]),
            entity("a", &[]),
            entity("b", &[]),
        ];
        let sorted = sort_by_dependencies(&entities);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn handles_diamond_dependency() {
        let entities = vec![
            entity("d", &["b", "c"]),
            entity("b", &["a"]),
            entity("c", &["a"]),
            entity("a", &[]),
        ];
        let sorted = sort_by_dependencies(&entities);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        // a must come first, d must come last
        assert_eq!(names[0], "a");
        assert_eq!(names[3], "d");
    }

    #[test]
    fn detects_simple_cycle() {
        let entities = vec![entity("a", &["b"]), entity("b", &["a"])];
        let sorted = sort_by_dependencies(&entities);
        assert!(sorted.iter().all(|e| e.has_errors()));
        assert!(sorted[0]
            .errors
            .iter()
            .any(|e| e.contains("Cyclic")));
    }

    #[test]
    fn partial_cycle_preserves_non_cyclic() {
        let entities = vec![
            entity("a", &[]),
            entity("b", &["a", "c"]),
            entity("c", &["b"]),
        ];
        let sorted = sort_by_dependencies(&entities);
        assert_eq!(sorted[0].name, "a");
        assert!(!sorted[0].has_errors());
        // b and c are cyclic
        assert!(sorted[1].has_errors());
        assert!(sorted[2].has_errors());
    }

    #[test]
    fn ignores_external_dependencies() {
        let entities = vec![
            entity("b", &["a", "external.thing"]),
            entity("a", &[]),
        ];
        let sorted = sort_by_dependencies(&entities);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(!sorted[1].has_errors());
    }

    #[test]
    fn self_reference_is_not_a_cycle() {
        let entities = vec![
            entity("a", &[]),
            entity("b", &["a", "b"]),  // b references itself (self-FK)
        ];
        let sorted = sort_by_dependencies(&entities);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(!sorted[1].has_errors()); // Not cyclic
    }

    #[test]
    fn empty_input() {
        let sorted = sort_by_dependencies(&[]);
        assert!(sorted.is_empty());
    }

    #[test]
    fn group_by_level() {
        let entities = vec![
            entity("c", &["a", "b"]),
            entity("a", &[]),
            entity("b", &["a"]),
        ];
        let layers = group_by_dependency_level(&entities);
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0], vec!["a"]);
        assert_eq!(layers[1], vec!["b"]);
        assert_eq!(layers[2], vec!["c"]);
    }

    #[test]
    fn graph_from_entities_full() {
        let entities = vec![
            entity("b", &["a"]),
            entity("a", &[]),
        ];
        let graph = graph_from_entities(&entities, None);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, "b");
        assert_eq!(graph.edges[0].to, "a");
        assert_eq!(graph.layers.len(), 2);
    }

    #[test]
    fn graph_scoped_to_entity() {
        let entities = vec![
            entity("a", &[]),
            entity("b", &["a"]),
            entity("c", &[]),  // unrelated
        ];
        let graph = graph_from_entities(&entities, Some("b"));
        let names: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(!names.contains(&"c"));
    }

    // ── DG1: Build graph from entities with deps ─────────

    #[test]
    fn dg1_build_graph_from_entities_with_deps() {
        let entities = vec![
            entity("A", &["B"]),
            entity("B", &["C"]),
            entity("C", &[]),
        ];
        let graph = build_dependency_graph(&entities);

        assert_eq!(graph.len(), 3);
        assert!(graph["A"].contains("B"));
        assert_eq!(graph["A"].len(), 1);
        assert!(graph["B"].contains("C"));
        assert_eq!(graph["B"].len(), 1);
        assert!(graph["C"].is_empty());
    }

    // ── DG2: Empty entities ──────────────────────────────

    #[test]
    fn dg2_build_graph_empty_entities() {
        let graph = build_dependency_graph(&[]);
        assert!(graph.is_empty());
    }
}
