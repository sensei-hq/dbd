use crate::entity::Entity;

/// Match parsed references against known entities and external entities.
///
/// For each entity, its `references` list contains raw names from FK targets,
/// view dependencies, etc. This function resolves those names against the
/// known entity set and marks unresolved references as warnings.
///
/// A bare cross-schema FK (`REFERENCES t`, no schema) is pre-qualified by the
/// parser to `<default_schema>.t` (the referencing table's own schema). When the
/// real target lives in another schema on the table's `search_path` (e.g. a
/// `dojo` table referencing `sensei.t`), that guess is wrong and would drop the
/// edge — hiding the dependency from scope-gap analysis and the topo sort. So an
/// unresolved reference qualified with the default schema is re-resolved along
/// the table's search_path, mirroring how Postgres itself resolves the bare name.
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
        // The parser qualifies bare references with the first search_path entry.
        let default_schema = entity
            .search_paths
            .first()
            .map(|s| s.as_str())
            .unwrap_or("public");
        // dbd appends `public` to every applied search_path (see
        // `ensure_public_in_search_path`), so a bare name can resolve there too.
        let search_path: Vec<&str> = {
            let mut sp: Vec<&str> = entity.search_paths.iter().map(|s| s.as_str()).collect();
            if !sp.contains(&"public") {
                sp.push("public");
            }
            sp
        };

        let mut resolved_refers = Vec::new();

        for ref_name in &entity.refers {
            if is_ignored(ref_name, ignore) {
                continue;
            }
            if known_names.contains(ref_name) {
                resolved_refers.push(ref_name.clone());
                continue;
            }
            // Unresolved as written. If it carries the default schema, it may be a
            // bare cross-schema FK the parser mis-qualified — re-resolve the bare
            // table name along the search_path (first hit wins, as in Postgres).
            if let Some((schema, table)) = ref_name.split_once('.')
                && schema == default_schema
                && let Some(found) = search_path
                    .iter()
                    .map(|s| format!("{s}.{table}"))
                    .find(|cand| known_names.contains(cand))
            {
                resolved_refers.push(found);
                continue;
            }
            entity
                .warnings
                .push(format!("Unresolved reference: {ref_name}"));
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

    /// Like `entity`, but with an explicit `SET search_path` (first entry is the
    /// schema the parser uses to qualify bare references).
    fn entity_sp(name: &str, refers: &[&str], search_paths: &[&str]) -> Entity {
        let mut e = entity(name, refers);
        e.search_paths = search_paths.iter().map(|s| s.to_string()).collect();
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
    fn bare_cross_schema_fk_resolves_along_search_path() {
        // `dojo.shared_rules` has `SET search_path TO dojo, sensei` and a bare
        // `REFERENCES namespaces`; the parser qualified it as `dojo.namespaces`,
        // but the real target is `sensei.namespaces`. It must resolve there — and
        // stay in `refers` so scope-gap analysis sees the cross-scope edge.
        let mut entities = vec![
            entity("sensei.namespaces", &[]),
            entity_sp("dojo.shared_rules", &["dojo.namespaces"], &["dojo", "sensei"]),
        ];
        resolve_references(&mut entities, &[], &[]);

        assert_eq!(entities[1].refers, vec!["sensei.namespaces"]);
        assert!(
            entities[1].warnings.is_empty(),
            "should not warn once resolved: {:?}",
            entities[1].warnings
        );
    }

    #[test]
    fn bare_ref_resolves_to_public_fallback() {
        // dbd appends `public` to every search_path, so a bare reference from an
        // `app`-scoped table resolves against `public` when nothing local matches.
        let mut entities = vec![
            entity("public.helper", &[]),
            entity_sp("app.widget", &["app.helper"], &["app"]),
        ];
        resolve_references(&mut entities, &[], &[]);

        assert_eq!(entities[1].refers, vec!["public.helper"]);
        assert!(entities[1].warnings.is_empty());
    }

    #[test]
    fn unresolvable_bare_ref_still_warns() {
        // No `ghost` in any schema on the search_path → genuinely unresolved,
        // exactly as Postgres would fail. The re-resolution must not invent a hit.
        let mut entities = vec![entity_sp("dojo.rules", &["dojo.ghost"], &["dojo", "sensei"])];
        resolve_references(&mut entities, &[], &[]);

        assert!(entities[0].refers.is_empty());
        assert_eq!(entities[0].warnings.len(), 1);
        assert!(entities[0].warnings[0].contains("dojo.ghost"));
    }

    #[test]
    fn explicit_wrong_schema_ref_is_not_rewritten() {
        // An explicitly-qualified ref whose schema is NOT the table's default
        // schema is left unresolved (we only recover parser-defaulted bare refs,
        // not silently redirect a deliberate `other.foo` to a different schema).
        let mut entities = vec![
            entity("sensei.namespaces", &[]),
            entity_sp("dojo.rules", &["other.namespaces"], &["dojo", "sensei"]),
        ];
        resolve_references(&mut entities, &[], &[]);

        assert!(entities[1].refers.is_empty());
        assert_eq!(entities[1].warnings.len(), 1);
        assert!(entities[1].warnings[0].contains("other.namespaces"));
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
