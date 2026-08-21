use std::collections::HashSet;

use crate::entity::{Entity, ForeignKey, REF_TYPE_FUNCTION, TableConstraint};

/// Match parsed references against known entities and external entities.
///
/// For each entity, its `references` list contains raw names from FK targets,
/// view dependencies, etc. This function resolves those names against the
/// known entity set and marks unresolved references as warnings.
///
/// A bare cross-schema FK (`REFERENCES t`, no schema) is pre-qualified by the
/// parser to `<default_schema>.t` (the referencing table's own schema). When the
/// real target lives in another schema on the table's `search_path` (e.g. a
/// `dojo` table referencing `sensei.t`), that guess is wrong. Left uncorrected it
/// (a) drops the edge from `refers` — hiding the dependency from scope-gap
/// analysis and the topo sort — and (b) leaves the FK's `ref_schema` pointing at
/// the wrong schema, so emit/dbml and reconcile's parsed-vs-live FK diff disagree
/// with the database. Both the `refers` strings and the `ForeignKey.ref_schema`
/// values are re-resolved along the table's search_path, mirroring how Postgres
/// itself resolves the bare name (first schema on the path that has the table).
///
/// Also filters out references that match the ignore list (patterns from design.yaml).
pub fn resolve_references(entities: &mut [Entity], external_names: &[String], ignore: &[String]) {
    let known_names: HashSet<String> = entities
        .iter()
        .map(|e| e.name.clone())
        .chain(external_names.iter().cloned())
        .collect();

    for entity in entities.iter_mut() {
        resolve_entity_references(entity, &known_names, ignore);
    }
}

/// Resolve one entity's references against the known entity names.
fn resolve_entity_references(entity: &mut Entity, known: &HashSet<String>, ignore: &[String]) {
    // The parser qualifies bare references with the first search_path entry.
    // dbd appends `public` to every applied search_path (see
    // `ensure_public_in_search_path`), so a bare name can resolve there too.
    let default_schema = entity
        .search_paths
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());
    let search_path: Vec<String> = {
        let mut sp = entity.search_paths.clone();
        if !sp.iter().any(|s| s == "public") {
            sp.push("public".to_string());
        }
        sp
    };

    resolve_entity_refers(entity, &default_schema, &search_path, known, ignore);
    resolve_entity_fks(entity, &default_schema, &search_path, known);
}

/// (1) String references → `refers` (FK targets, view/proc deps). Recovers
/// bare-qualified schemas along the search_path; warns on the unresolvable.
///
/// Function references (tagged [`REF_TYPE_FUNCTION`]) are the exception: they
/// resolve or vanish, silently. A view body's `now()` / `sum()` / `coalesce()`
/// calls are collected the same way a call to a project-managed function is,
/// and only this resolution step can tell them apart — so an unresolved one is
/// a built-in, not a mistake worth reporting.
fn resolve_entity_refers(
    entity: &mut Entity,
    default_schema: &str,
    search_path: &[String],
    known: &HashSet<String>,
    ignore: &[String],
) {
    let soft: HashSet<&str> = entity
        .references
        .iter()
        .filter(|r| r.ref_type.as_deref() == Some(REF_TYPE_FUNCTION))
        .map(|r| r.name.as_str())
        .collect();

    let mut resolved_refers = Vec::new();
    let mut unresolved = Vec::new();
    for ref_name in &entity.refers {
        if is_ignored(ref_name, ignore) {
            continue;
        }
        if known.contains(ref_name) {
            resolved_refers.push(ref_name.clone());
            continue;
        }
        if let Some((schema, table)) = ref_name.split_once('.')
            && let Some(sch) = recover_bare_target(schema, table, default_schema, search_path, known)
        {
            resolved_refers.push(format!("{sch}.{table}"));
            continue;
        }
        if !soft.contains(ref_name.as_str()) {
            unresolved.push(ref_name.clone());
        }
    }
    entity.refers = resolved_refers;
    for ref_name in unresolved {
        entity
            .warnings
            .push(format!("Unresolved reference: {ref_name}"));
    }
}

/// (2) FK structs → `ref_schema` (consumed by emit/dbml/reconcile). Kept in
/// step with the `refers` resolution above so the target schema agrees.
fn resolve_entity_fks(
    entity: &mut Entity,
    default_schema: &str,
    search_path: &[String],
    known: &HashSet<String>,
) {
    let Some(td) = entity.table_def.as_mut() else {
        return;
    };
    for col in td.columns.iter_mut() {
        if let Some(fk) = col.inline_fk.as_mut() {
            fix_fk_schema(fk, default_schema, search_path, known);
        }
    }
    for constraint in td.constraints.iter_mut() {
        if let TableConstraint::ForeignKey(fk) = constraint {
            fix_fk_schema(fk, default_schema, search_path, known);
        }
    }
}

/// If `schema.table` is unknown but `schema` is the table's default schema (the
/// parser's bare-qualification marker), return the first schema on `search_path`
/// that has `<s>.table` among `known`. `None` = leave the reference as written
/// (either it resolves already, or it was deliberately qualified elsewhere).
fn recover_bare_target(
    schema: &str,
    table: &str,
    default_schema: &str,
    search_path: &[String],
    known: &HashSet<String>,
) -> Option<String> {
    if schema != default_schema {
        return None;
    }
    search_path
        .iter()
        .find(|s| known.contains(&format!("{s}.{table}")))
        .cloned()
}

/// Re-point a bare-qualified FK at the schema that actually holds its target,
/// resolved along the referencing table's search_path. No-op when the FK already
/// resolves as written or was explicitly qualified to a non-default schema.
fn fix_fk_schema(
    fk: &mut ForeignKey,
    default_schema: &str,
    search_path: &[String],
    known: &HashSet<String>,
) {
    let cur_schema = fk.ref_schema.as_deref().unwrap_or(default_schema);
    if known.contains(&format!("{cur_schema}.{}", fk.ref_table)) {
        return; // resolves as written
    }
    if let Some(sch) = recover_bare_target(cur_schema, &fk.ref_table, default_schema, search_path, known)
    {
        fk.ref_schema = Some(sch);
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
    fn fk_ref_schema_repointed_along_search_path() {
        use crate::entity::{ColumnDef, ForeignKey, TableConstraint, TableDef};

        // A bare `REFERENCES namespaces` the parser qualified to the table's own
        // schema (`dojo`); the real target is `sensei.namespaces`.
        let bare_fk = |col: &str| ForeignKey {
            columns: vec![col.to_string()],
            ref_schema: Some("dojo".to_string()),
            ref_table: "namespaces".to_string(),
            ref_columns: vec!["id".to_string()],
            ..Default::default()
        };
        let col = ColumnDef {
            name: "ns_inline".to_string(),
            data_type: "int".to_string(),
            nullable: true,
            default_value: None,
            is_pk: false,
            is_unique: false,
            identity: None,
            comment: None,
            inline_fk: Some(bare_fk("ns_inline")),
        };
        let mut shared = entity_sp("dojo.shared_rules", &["dojo.namespaces"], &["dojo", "sensei"]);
        shared.table_def = Some(TableDef {
            columns: vec![col],
            constraints: vec![TableConstraint::ForeignKey(bare_fk("ns_constraint"))],
            indexes: vec![],
            comments: Default::default(),
        });

        let mut entities = vec![entity("sensei.namespaces", &[]), shared];
        resolve_references(&mut entities, &[], &[]);

        // refers AND both FK structs (inline + constraint) now point at sensei.
        assert_eq!(entities[1].refers, vec!["sensei.namespaces"]);
        let td = entities[1].table_def.as_ref().unwrap();
        assert_eq!(
            td.columns[0].inline_fk.as_ref().unwrap().ref_schema.as_deref(),
            Some("sensei"),
            "inline FK ref_schema should be repointed"
        );
        match &td.constraints[0] {
            TableConstraint::ForeignKey(fk) => assert_eq!(
                fk.ref_schema.as_deref(),
                Some("sensei"),
                "constraint FK ref_schema should be repointed"
            ),
            other => panic!("expected FK constraint, got {other:?}"),
        }
    }

    #[test]
    fn fk_ref_schema_unchanged_when_target_is_local() {
        use crate::entity::{ForeignKey, TableConstraint, TableDef};

        // FK resolves within the default schema already → left untouched.
        let fk = ForeignKey {
            columns: vec!["parent_id".to_string()],
            ref_schema: Some("app".to_string()),
            ref_table: "parents".to_string(),
            ref_columns: vec!["id".to_string()],
            ..Default::default()
        };
        let mut child = entity_sp("app.children", &["app.parents"], &["app"]);
        child.table_def = Some(TableDef {
            columns: vec![],
            constraints: vec![TableConstraint::ForeignKey(fk)],
            indexes: vec![],
            comments: Default::default(),
        });

        let mut entities = vec![entity("app.parents", &[]), child];
        resolve_references(&mut entities, &[], &[]);

        match &entities[1].table_def.as_ref().unwrap().constraints[0] {
            TableConstraint::ForeignKey(fk) => {
                assert_eq!(fk.ref_schema.as_deref(), Some("app"))
            }
            other => panic!("expected FK constraint, got {other:?}"),
        }
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
