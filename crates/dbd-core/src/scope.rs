//! Pure scope resolution and dependency-gap analysis.
//!
//! No I/O. Operates over the already-loaded entity list. Policy-neutral:
//! computes sets and gaps; the error-vs-expand decision lives in the consumer.

use std::collections::{HashMap, HashSet, VecDeque};

use indexmap::IndexMap;

use crate::config::{DepsPolicy, ScopeEntry};
use crate::entity::{Entity, EntityType};
use crate::error::{DbdError, Result};

/// A fully resolved scope — the concrete working set for an operation.
#[derive(Debug, Clone)]
pub struct ResolvedScope {
    pub name: String,
    pub entities: HashSet<String>,
    /// Entities explicitly removed via `excludes` (used by `closure` conflict check).
    pub excluded: HashSet<String>,
    pub deps: DepsPolicy,
    pub is_all: bool,
    /// Per-scope extension allowlist resolved from config. None = all extensions.
    pub extensions: Option<HashSet<String>>,
}

/// One dependency gap surfaced by inspect.
#[derive(Debug, Clone)]
pub struct ScopeGap {
    pub missing: String,
    /// The in-scope entity at the HEAD of `chain`.
    pub required_by: String,
    /// `required_by` → … → `missing`. `chain[0]` is in-scope; `chain.last()` is `missing`.
    pub chain: Vec<String>,
}

/// Entity types a scope can select (schemas handled separately via auto-add).
fn is_scopable(e: &Entity) -> bool {
    matches!(
        e.entity_type,
        EntityType::Enum
            | EntityType::Table
            | EntityType::View
            | EntityType::Function
            | EntityType::Procedure
    )
}

/// All deployable entity names: scopable entities + schema entities.
fn universe(all_entities: &[Entity]) -> HashSet<String> {
    all_entities
        .iter()
        .filter(|e| is_scopable(e) || e.entity_type == EntityType::Schema)
        .map(|e| e.name.clone())
        .collect()
}

/// Resolve one scope item into the set of scopable entity names it selects.
/// Errors if it matches nothing — typo protection.
///
/// Three forms:
/// - `schema.*` — wildcard: every scopable entity under `schema` (same `prefix.*`
///   syntax as the `ignore:` list). Unlike a bare schema token it does NOT carry
///   the `CREATE SCHEMA` entity itself, so `excludes: [schema.*]` drops the tables
///   but keeps the schema. (`add_present_schemas` re-adds it for `includes`.)
/// - `schema.entity` — one qualified entity.
/// - `schema` — a bare schema token: every scopable entity under the schema PLUS
///   the schema entity itself. A declared-but-empty schema resolves to just its
///   `CREATE SCHEMA` entity rather than an error (the schema is real, not a typo).
fn match_item(item: &str, all_entities: &[Entity]) -> Result<HashSet<String>> {
    if let Some(prefix) = item.strip_suffix(".*") {
        // `prefix.*` — every scopable entity whose name is `prefix.<something>`,
        // matched the same way the `ignore:` list matches `prefix.*` patterns.
        let names: HashSet<String> = all_entities
            .iter()
            .filter(|e| is_scopable(e))
            .filter(|e| {
                e.name.starts_with(prefix)
                    && e.name.len() > prefix.len()
                    && e.name.as_bytes()[prefix.len()] == b'.'
            })
            .map(|e| e.name.clone())
            .collect();
        // A real-but-empty schema is not a typo; an unknown prefix is.
        let schema_exists = all_entities
            .iter()
            .any(|e| e.entity_type == EntityType::Schema && e.name == prefix);
        if names.is_empty() && !schema_exists {
            return Err(DbdError::Config(format!(
                "scope item '{item}' matches no known entity"
            )));
        }
        return Ok(names);
    }

    if item.contains('.') {
        let exists = all_entities.iter().any(|e| is_scopable(e) && e.name == item);
        if !exists {
            return Err(DbdError::Config(format!(
                "scope item '{item}' matches no known entity"
            )));
        }
        Ok(HashSet::from([item.to_string()]))
    } else {
        let mut names: HashSet<String> = all_entities
            .iter()
            .filter(|e| is_scopable(e) && e.schema.as_deref() == Some(item))
            .map(|e| e.name.clone())
            .collect();
        let schema_exists = all_entities
            .iter()
            .any(|e| e.entity_type == EntityType::Schema && e.name == item);
        if names.is_empty() && !schema_exists {
            return Err(DbdError::Config(format!(
                "scope item '{item}' matches no known schema or entity"
            )));
        }
        // A bare schema token also selects the schema entity itself, so that
        // `excludes: [app]` drops `CREATE SCHEMA app` (not just app's tables)
        // and `includes: [app]` carries the schema explicitly.
        if schema_exists {
            names.insert(item.to_string());
        }
        Ok(names)
    }
}

/// Add the `CREATE SCHEMA` entity for every schema present in `set`.
fn add_present_schemas(set: &mut HashSet<String>, all_entities: &[Entity]) {
    let schemas: HashSet<String> = set
        .iter()
        .filter_map(|n| {
            all_entities
                .iter()
                .find(|e| &e.name == n)
                .and_then(|e| e.schema.clone())
        })
        .collect();
    for sch in schemas {
        if all_entities
            .iter()
            .any(|e| e.entity_type == EntityType::Schema && e.name == sch)
        {
            set.insert(sch);
        }
    }
}

/// BFS from the in-scope roots along `refers` edges into managed, non-external
/// entities. Returns (visited, parent) where `parent` maps node → predecessor.
/// Self-refs, externals, and unresolved (non-managed) targets are not traversed.
fn traverse(
    resolved: &ResolvedScope,
    all_entities: &[Entity],
    externals: &HashSet<String>,
) -> (HashSet<String>, HashMap<String, String>) {
    let managed: HashSet<&str> = all_entities
        .iter()
        .filter(|e| is_scopable(e))
        .map(|e| e.name.as_str())
        .collect();
    let refers: HashMap<&str, &Vec<String>> = all_entities
        .iter()
        .map(|e| (e.name.as_str(), &e.refers))
        .collect();

    let mut visited: HashSet<String> = resolved.entities.clone();
    let mut parent: HashMap<String, String> = HashMap::new();
    // Seed from sorted roots so that when a missing node is reachable from
    // several in-scope roots, the recorded chain/required_by is deterministic
    // across runs (HashSet iteration order is not).
    let mut roots: Vec<String> = resolved.entities.iter().cloned().collect();
    roots.sort();
    let mut queue: VecDeque<String> = roots.into_iter().collect();

    while let Some(cur) = queue.pop_front() {
        if let Some(deps) = refers.get(cur.as_str()) {
            for dep in deps.iter() {
                if dep == &cur || externals.contains(dep) || !managed.contains(dep.as_str()) {
                    continue;
                }
                if visited.insert(dep.clone()) {
                    parent.insert(dep.clone(), cur.clone());
                    queue.push_back(dep.clone());
                }
            }
        }
    }
    (visited, parent)
}

/// Resolve a scope name against the loaded entities.
///
/// `name = None` → the `default` scope if defined, else `all`.
/// `name = Some("all")` → the full set regardless of config.
/// `deps_override` (CLI `--deps`) wins over the scope's own `deps`.
pub fn resolve(
    scopes: &IndexMap<String, ScopeEntry>,
    name: Option<&str>,
    deps_override: Option<DepsPolicy>,
    all_entities: &[Entity],
    _externals: &[String],
) -> Result<ResolvedScope> {
    let (scope_name, entry): (String, Option<&ScopeEntry>) = match name {
        Some("all") => ("all".to_string(), None),
        Some(n) => match scopes.get(n) {
            Some(e) => (n.to_string(), Some(e)),
            None => {
                let avail: Vec<&str> = scopes.keys().map(|s| s.as_str()).collect();
                return Err(DbdError::Config(format!(
                    "unknown scope '{n}'. Available: all, {}",
                    avail.join(", ")
                )));
            }
        },
        None => match scopes.get("default") {
            Some(e) => ("default".to_string(), Some(e)),
            None => ("all".to_string(), None),
        },
    };

    // `all` — implicit (None / no default), explicit name, or `default: all`.
    let all_scope = |nm: String, deps: DepsPolicy| ResolvedScope {
        name: nm,
        entities: universe(all_entities),
        excluded: HashSet::new(),
        deps,
        is_all: true,
        extensions: None,
    };

    let spec = match entry {
        None => return Ok(all_scope(scope_name, deps_override.unwrap_or_default())),
        Some(ScopeEntry::All(kw)) => {
            if kw != "all" {
                return Err(DbdError::Config(format!(
                    "scope '{scope_name}': bare string must be \"all\", got \"{kw}\""
                )));
            }
            return Ok(all_scope(scope_name, deps_override.unwrap_or_default()));
        }
        Some(ScopeEntry::Spec(s)) => s,
    };

    // Base = include-union, or full universe when includes is empty.
    let mut base: HashSet<String> = if spec.includes.is_empty() {
        universe(all_entities)
    } else {
        let mut acc = HashSet::new();
        for item in &spec.includes {
            acc.extend(match_item(item, all_entities)?);
        }
        acc
    };

    // Subtract excludes.
    let mut excluded = HashSet::new();
    for item in &spec.excludes {
        let m = match_item(item, all_entities)?;
        for n in &m {
            base.remove(n);
        }
        excluded.extend(m);
    }

    add_present_schemas(&mut base, all_entities);

    Ok(ResolvedScope {
        name: scope_name,
        entities: base,
        excluded,
        deps: deps_override.unwrap_or(spec.deps),
        is_all: false,
        extensions: spec.extensions.as_ref().map(|v| v.iter().cloned().collect()),
    })
}

/// Inspect engine: the full set of managed entities reachable from the scope
/// but not in it. Each missing entity gets one path back to an in-scope root.
pub fn analyze_gaps(
    resolved: &ResolvedScope,
    all_entities: &[Entity],
    externals: &[String],
) -> Vec<ScopeGap> {
    if resolved.is_all {
        return Vec::new();
    }
    let ext: HashSet<String> = externals.iter().cloned().collect();
    let (visited, parent) = traverse(resolved, all_entities, &ext);

    let mut gaps: Vec<ScopeGap> = visited
        .iter()
        .filter(|n| !resolved.entities.contains(*n))
        .map(|missing| {
            let mut chain = vec![missing.clone()];
            let mut cur = missing.clone();
            while let Some(p) = parent.get(&cur) {
                chain.push(p.clone());
                cur = p.clone();
            }
            chain.reverse();
            ScopeGap {
                // Every missing node was reached via `parent` from an in-scope
                // root, so the reversed chain always starts in-scope.
                required_by: chain
                    .first()
                    .cloned()
                    .expect("gap node always has an in-scope parent"),
                missing: missing.clone(),
                chain,
            }
        })
        .collect();
    gaps.sort_by(|a, b| a.missing.cmp(&b.missing));
    gaps
}

/// `deps: include` expansion: in-scope ∪ all transitively-referenced managed,
/// non-external entities. Errors if the closure pulls in an explicitly-excluded
/// entity.
pub fn closure(
    resolved: &ResolvedScope,
    all_entities: &[Entity],
    externals: &[String],
) -> Result<HashSet<String>> {
    if resolved.is_all {
        return Ok(resolved.entities.clone());
    }
    let ext: HashSet<String> = externals.iter().cloned().collect();
    let (mut visited, _parent) = traverse(resolved, all_entities, &ext);

    // A node the closure pulled in but the scope explicitly excluded is a
    // contradiction. `entities` and `excluded` are disjoint post-resolve, so a
    // conflict can only be a freshly-traversed node. Collect all of them,
    // sorted, so the error is deterministic and lists every conflict at once.
    let mut conflicts: Vec<&str> = visited
        .iter()
        .filter(|n| !resolved.entities.contains(*n) && resolved.excluded.contains(*n))
        .map(String::as_str)
        .collect();
    if !conflicts.is_empty() {
        conflicts.sort_unstable();
        return Err(DbdError::Config(format!(
            "scope '{}' excludes {} but an in-scope entity requires it",
            resolved.name,
            conflicts
                .iter()
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    add_present_schemas(&mut visited, all_entities);
    Ok(visited)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(t: EntityType, name: &str, refers: &[&str]) -> Entity {
        let mut e = Entity::new(t, name);
        e.refers = refers.iter().map(|s| s.to_string()).collect();
        e
    }

    /// config.lookup_values → config.lookups; app.orders → app.customers; + schemas.
    fn world() -> Vec<Entity> {
        vec![
            Entity::schema("config"),
            Entity::schema("app"),
            ent(EntityType::Table, "config.lookups", &[]),
            ent(EntityType::Table, "config.lookup_values", &["config.lookups"]),
            ent(EntityType::Table, "app.customers", &[]),
            ent(EntityType::Table, "app.orders", &["app.customers"]),
        ]
    }

    fn scopes_yaml(yaml: &str) -> IndexMap<String, ScopeEntry> {
        let v: IndexMap<String, ScopeEntry> = serde_yaml::from_str(yaml).unwrap();
        v
    }

    #[test]
    fn resolve_none_is_all() {
        let s = resolve(&IndexMap::new(), None, None, &world(), &[]).unwrap();
        assert!(s.is_all);
        assert_eq!(s.name, "all");
    }

    #[test]
    fn resolve_none_uses_default_scope() {
        // name = None with a configured `default` scope uses it (not `all`).
        let scopes = scopes_yaml("default:\n  includes: [config]\n");
        let s = resolve(&scopes, None, None, &world(), &[]).unwrap();
        assert_eq!(s.name, "default");
        assert!(!s.is_all);
        assert!(s.entities.contains("config.lookups"));
        assert!(!s.entities.contains("app.orders"));
    }

    #[test]
    fn resolve_schema_token_expands() {
        let scopes = scopes_yaml("hub:\n  includes: [config]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        assert!(!s.is_all);
        assert!(s.entities.contains("config.lookups"));
        assert!(s.entities.contains("config.lookup_values"));
        assert!(s.entities.contains("config")); // schema auto-added
        assert!(!s.entities.contains("app.orders"));
        assert!(!s.entities.contains("app")); // other schema entity absent
    }

    #[test]
    fn resolve_wildcard_include_selects_schema_entities() {
        // `config.*` selects every entity under config; add_present_schemas
        // re-adds the schema entity, so the net effect matches bare `config`.
        let scopes = scopes_yaml("hub:\n  includes: [config.*]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        assert!(s.entities.contains("config.lookups"));
        assert!(s.entities.contains("config.lookup_values"));
        assert!(s.entities.contains("config")); // schema re-added
        assert!(!s.entities.contains("app.orders"));
        assert!(!s.entities.contains("app"));
    }

    #[test]
    fn resolve_wildcard_exclude_keeps_schema_drops_tables() {
        // `excludes: [config.*]` drops config's tables but leaves the CREATE
        // SCHEMA entity intact — the distinction from bare `excludes: [config]`.
        let scopes = scopes_yaml("rep:\n  excludes: [config.*]\n");
        let s = resolve(&scopes, Some("rep"), None, &world(), &[]).unwrap();
        assert!(!s.entities.contains("config.lookups"));
        assert!(!s.entities.contains("config.lookup_values"));
        assert!(s.entities.contains("config")); // schema entity preserved
        assert!(s.entities.contains("app.orders")); // other schema untouched
        assert!(s.excluded.contains("config.lookups"));
    }

    #[test]
    fn resolve_wildcard_unknown_prefix_errors() {
        let scopes = scopes_yaml("hub:\n  includes: [ghost.*]\n");
        let err = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap_err();
        assert!(err.to_string().contains("matches no known"));
    }

    #[test]
    fn resolve_specific_entity() {
        let scopes = scopes_yaml("hub:\n  includes: [app.orders]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        assert!(s.entities.contains("app.orders"));
        assert!(!s.entities.contains("app.customers")); // not auto-pulled (that's gaps/closure)
        assert!(s.entities.contains("app")); // schema auto-added
    }

    #[test]
    fn resolve_excludes_subtracts() {
        let scopes = scopes_yaml("rep:\n  excludes: [app]\n");
        let s = resolve(&scopes, Some("rep"), None, &world(), &[]).unwrap();
        assert!(s.entities.contains("config.lookups"));
        assert!(!s.entities.contains("app.orders"));
        assert!(!s.entities.contains("app")); // excluded schema entity not re-added
        assert!(s.excluded.contains("app.orders"));
    }

    #[test]
    fn resolve_deps_override_wins() {
        let scopes = scopes_yaml("hub:\n  includes: [config]\n  deps: report\n");
        let s = resolve(&scopes, Some("hub"), Some(DepsPolicy::Include), &world(), &[]).unwrap();
        assert_eq!(s.deps, DepsPolicy::Include);
    }

    #[test]
    fn resolve_unknown_name_errors() {
        let scopes = scopes_yaml("hub:\n  includes: [config]\n");
        let err = resolve(&scopes, Some("nope"), None, &world(), &[]).unwrap_err();
        assert!(err.to_string().contains("unknown scope 'nope'"));
    }

    #[test]
    fn resolve_unknown_item_errors() {
        let scopes = scopes_yaml("hub:\n  includes: [ghost]\n");
        let err = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap_err();
        assert!(err.to_string().contains("matches no known"));
    }

    #[test]
    fn gaps_direct_reference() {
        // include only the child table; parent is out of scope → gap
        let scopes = scopes_yaml("hub:\n  includes: [config.lookup_values]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        let gaps = analyze_gaps(&s, &world(), &[]);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].missing, "config.lookups");
        assert_eq!(gaps[0].required_by, "config.lookup_values");
        assert_eq!(gaps[0].chain, vec!["config.lookup_values", "config.lookups"]);
    }

    #[test]
    fn gaps_hierarchical_chain() {
        // a → b → c; scope has only `a`; b and c both missing, chained.
        let world = vec![
            Entity::schema("s"),
            ent(EntityType::Table, "s.c", &[]),
            ent(EntityType::Table, "s.b", &["s.c"]),
            ent(EntityType::Table, "s.a", &["s.b"]),
        ];
        let scopes = scopes_yaml("hub:\n  includes: [s.a]\n");
        let s = resolve(&scopes, Some("hub"), None, &world, &[]).unwrap();
        let gaps = analyze_gaps(&s, &world, &[]);
        let missing: Vec<&str> = gaps.iter().map(|g| g.missing.as_str()).collect();
        assert_eq!(missing, vec!["s.b", "s.c"]); // sorted
        let c = gaps.iter().find(|g| g.missing == "s.c").unwrap();
        assert_eq!(c.chain, vec!["s.a", "s.b", "s.c"]);
        assert_eq!(c.required_by, "s.a");
    }

    #[test]
    fn gaps_external_is_not_a_gap() {
        let mut world = world();
        world.push(ent(EntityType::Table, "app.orders2", &["auth.users"]));
        let scopes = scopes_yaml("hub:\n  includes: [app.orders2]\n");
        let s = resolve(&scopes, Some("hub"), None, &world, &[]).unwrap();
        let gaps = analyze_gaps(&s, &world, &["auth.users".to_string()]);
        assert!(gaps.is_empty());
    }

    #[test]
    fn gaps_mixed_external_and_managed_refs() {
        // One entity references an external (not a gap) AND a missing managed
        // table (a gap) — only the managed one is reported.
        let mut world = world();
        world.push(ent(EntityType::Table, "app.mixed", &["auth.users", "config.lookups"]));
        let scopes = scopes_yaml("hub:\n  includes: [app.mixed]\n");
        let s = resolve(&scopes, Some("hub"), None, &world, &[]).unwrap();
        let gaps = analyze_gaps(&s, &world, &["auth.users".to_string()]);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].missing, "config.lookups");
    }

    #[test]
    fn gaps_self_reference_is_not_a_gap() {
        let world = vec![
            Entity::schema("s"),
            ent(EntityType::Table, "s.tree", &["s.tree"]),
        ];
        let scopes = scopes_yaml("hub:\n  includes: [s.tree]\n");
        let s = resolve(&scopes, Some("hub"), None, &world, &[]).unwrap();
        assert!(analyze_gaps(&s, &world, &[]).is_empty());
    }

    #[test]
    fn gaps_complete_closure_none() {
        let scopes = scopes_yaml("hub:\n  includes: [config]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        assert!(analyze_gaps(&s, &world(), &[]).is_empty());
    }

    #[test]
    fn gaps_all_scope_none() {
        let s = resolve(&IndexMap::new(), None, None, &world(), &[]).unwrap();
        assert!(analyze_gaps(&s, &world(), &[]).is_empty());
    }

    #[test]
    fn closure_expands_dependencies() {
        let scopes = scopes_yaml("hub:\n  includes: [config.lookup_values]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        let c = closure(&s, &world(), &[]).unwrap();
        assert!(c.contains("config.lookup_values"));
        assert!(c.contains("config.lookups")); // pulled in
        assert!(c.contains("config")); // schema
    }

    #[test]
    fn closure_exclude_conflict_errors() {
        // include the child but explicitly exclude the parent it needs.
        let scopes = scopes_yaml(
            "hub:\n  includes: [config.lookup_values]\n  excludes: [config.lookups]\n",
        );
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        let err = closure(&s, &world(), &[]).unwrap_err();
        assert!(err.to_string().contains("excludes 'config.lookups'"));
        assert!(err.to_string().contains("'hub'")); // names the offending scope
    }

    #[test]
    fn closure_all_scope_is_full_set() {
        let s = resolve(&IndexMap::new(), None, None, &world(), &[]).unwrap();
        let c = closure(&s, &world(), &[]).unwrap();
        assert!(c.contains("app.orders"));
    }
}
