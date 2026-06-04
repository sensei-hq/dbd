//! Pure scope resolution and dependency-gap analysis.
//!
//! No I/O. Operates over the already-loaded entity list. Policy-neutral:
//! computes sets and gaps; the error-vs-expand decision lives in the consumer.

use std::collections::HashSet;

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

/// Resolve one scope item (a bare schema token or a qualified entity name)
/// into the set of scopable entity names it selects. Errors if it matches
/// nothing — typo protection.
fn match_item(item: &str, all_entities: &[Entity]) -> Result<HashSet<String>> {
    if item.contains('.') {
        let exists = all_entities.iter().any(|e| is_scopable(e) && e.name == item);
        if !exists {
            return Err(DbdError::Config(format!(
                "scope item '{item}' matches no known entity"
            )));
        }
        Ok(HashSet::from([item.to_string()]))
    } else {
        let names: HashSet<String> = all_entities
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
    })
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
    fn resolve_schema_token_expands() {
        let scopes = scopes_yaml("hub:\n  includes: [config]\n");
        let s = resolve(&scopes, Some("hub"), None, &world(), &[]).unwrap();
        assert!(!s.is_all);
        assert!(s.entities.contains("config.lookups"));
        assert!(s.entities.contains("config.lookup_values"));
        assert!(s.entities.contains("config")); // schema auto-added
        assert!(!s.entities.contains("app.orders"));
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
}
