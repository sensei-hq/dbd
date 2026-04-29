use crate::entity::Entity;

/// Match parsed references against known entities and external entities.
///
/// For each entity, its `references` list contains raw names from FK targets,
/// view dependencies, etc. This function resolves those names against the
/// known entity set and marks unresolved references as warnings.
///
/// Also filters out references that match the ignore list (patterns from design.yaml).
pub fn resolve_references(
    entities: &mut [Entity],
    external_names: &[String],
    ignore: &[String],
) {
    let known_names: std::collections::HashSet<String> = entities
        .iter()
        .map(|e| e.name.clone())
        .chain(external_names.iter().cloned())
        .collect();

    for entity in entities.iter_mut() {
        let mut resolved_refers = Vec::new();

        for ref_name in &entity.refers {
            if is_ignored(ref_name, ignore) {
                continue;
            }
            if known_names.contains(ref_name) {
                resolved_refers.push(ref_name.clone());
            } else {
                entity
                    .warnings
                    .push(format!("Unresolved reference: {ref_name}"));
            }
        }

        entity.refers = resolved_refers;
    }
}

/// Check if a reference name matches any pattern in the ignore list.
///
/// Supports:
/// - Exact match: "bfs"
/// - Wildcard suffix: "my_company.*"
fn is_ignored(name: &str, ignore: &[String]) -> bool {
    let lower = name.to_lowercase();
    ignore.iter().any(|pattern| {
        let pattern_lower = pattern.to_lowercase();
        if let Some(prefix) = pattern_lower.strip_suffix(".*") {
            lower.starts_with(prefix) && lower.len() > prefix.len() && lower.as_bytes()[prefix.len()] == b'.'
        } else {
            lower == pattern_lower
        }
    })
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
    fn resolves_known_references() {
        let mut entities = vec![
            entity("config.lookups", &[]),
            entity("config.lookup_values", &["config.lookups"]),
        ];
        resolve_references(&mut entities, &[], &[]);

        assert_eq!(entities[1].refers, vec!["config.lookups"]);
        assert!(entities[1].warnings.is_empty());
    }

    #[test]
    fn warns_on_unresolved_reference() {
        let mut entities = vec![entity("config.orders", &["config.nonexistent"])];
        resolve_references(&mut entities, &[], &[]);

        assert!(entities[0].refers.is_empty());
        assert_eq!(entities[0].warnings.len(), 1);
        assert!(entities[0].warnings[0].contains("Unresolved"));
    }

    #[test]
    fn resolves_external_references() {
        let mut entities = vec![entity("config.profiles", &["auth.users"])];
        let externals = vec!["auth.users".to_string()];
        resolve_references(&mut entities, &externals, &[]);

        assert_eq!(entities[0].refers, vec!["auth.users"]);
        assert!(entities[0].warnings.is_empty());
    }

    #[test]
    fn ignores_exact_match() {
        let mut entities = vec![entity("config.graph", &["bfs"])];
        let ignore = vec!["bfs".to_string()];
        resolve_references(&mut entities, &[], &ignore);

        assert!(entities[0].refers.is_empty());
        assert!(entities[0].warnings.is_empty()); // Ignored, not warned
    }

    #[test]
    fn ignores_wildcard_pattern() {
        let mut entities = vec![entity("app.data", &["my_company.utils", "my_company.helpers"])];
        let ignore = vec!["my_company.*".to_string()];
        resolve_references(&mut entities, &[], &ignore);

        assert!(entities[0].refers.is_empty());
        assert!(entities[0].warnings.is_empty());
    }

    #[test]
    fn wildcard_does_not_match_exact_prefix() {
        // "my_company.*" should not match "my_company" (no dot after prefix)
        let mut entities = vec![entity("app.data", &["my_company"])];
        let ignore = vec!["my_company.*".to_string()];
        resolve_references(&mut entities, &[], &ignore);

        // "my_company" is not in known entities and not matched by wildcard
        assert!(entities[0].refers.is_empty());
        assert_eq!(entities[0].warnings.len(), 1); // Unresolved, not ignored
    }

    #[test]
    fn ignore_is_case_insensitive() {
        let mut entities = vec![entity("app.data", &["BFS"])];
        let ignore = vec!["bfs".to_string()];
        resolve_references(&mut entities, &[], &ignore);

        assert!(entities[0].refers.is_empty());
        assert!(entities[0].warnings.is_empty());
    }

    #[test]
    fn mixed_resolved_and_unresolved() {
        let mut entities = vec![
            entity("config.lookups", &[]),
            entity("config.orders", &["config.lookups", "config.missing", "bfs"]),
        ];
        let ignore = vec!["bfs".to_string()];
        resolve_references(&mut entities, &[], &ignore);

        assert_eq!(entities[1].refers, vec!["config.lookups"]);
        assert_eq!(entities[1].warnings.len(), 1);
        assert!(entities[1].warnings[0].contains("config.missing"));
    }
}
