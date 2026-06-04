# Design.yaml Scopes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one `design.yaml` deploy to multiple databases by defining named `scopes:` (entity selections), with scope-aware `inspect`/`deploy`/`apply`/`import` and a dependency-gap check.

**Architecture:** A new pure `scope.rs` module resolves a scope name → a concrete entity set and runs gap analysis over the already-loaded entities (reusing `dependency.rs`'s reachable-subgraph walk). `Design` resolves a scope once; `apply`/`import_data`/`deploy`/`report` take an optional `&ResolvedScope` and filter their working set by it. `None ⇒ all ⇒` today's behavior, byte-for-byte.

**Tech Stack:** Rust 2024, `serde`/`serde_yaml`, `indexmap`, `clap`, `tokio`, `thiserror`. Tests: `cargo test`; lint: `cargo clippy`. Workspace crates: `dbd-core` (logic), `dbd-cli` (CLI).

**Spec:** `docs/superpowers/specs/2026-06-04-design-yaml-scopes-design.md`

**Conventions:**
- Run all commands from repo root `/Users/Jerry/Developer/dbd-rs`.
- Core tests: `cargo test -p dbd-core`. Full: `cargo test`. Lint: `cargo clippy --all-targets`.
- A pre-commit hook runs the full suite + clippy on every commit; commits fail if anything is red (this is the zero-errors gate).
- Branch is already `feat/design-yaml-scopes`.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `crates/dbd-core/src/config.rs` | Parse `scopes:` block | Add `DepsPolicy`, `ScopeSpec`, `ScopeEntry`, `scopes` field |
| `crates/dbd-core/src/scope.rs` | **NEW** — pure scope resolution + gap analysis | `ResolvedScope`, `ScopeGap`, `resolve`/`analyze_gaps`/`closure` |
| `crates/dbd-core/src/dependency.rs` | Dependency graph | Make `reachable_subgraph` reusable (not needed if scope.rs has its own walk — see Task 3 note) |
| `crates/dbd-core/src/lib.rs` | Crate root | Register `pub mod scope;` + re-exports |
| `crates/dbd-core/src/design.rs` | Orchestrator | `resolve_scope`, working-set helpers, scope params on `report`/`apply`/`import_data`/`deploy`, `build_execution_plan` intersection |
| `crates/dbd-cli/src/cli.rs` | CLI args | Global `--scope` / `--deps` |
| `crates/dbd-cli/src/commands/mod.rs` | Command dispatch | Thread scope/deps; map `DepsArg`→`DepsPolicy` |
| `crates/dbd-cli/src/main.rs` | Entry | Pass new args into `run` |
| `crates/dbd-cli/src/commands/schema.rs` | inspect/apply | Resolve scope, render gaps, pass scope to `apply` |
| `crates/dbd-cli/src/commands/data.rs` | import | Pass scope to `import_data` |
| `crates/dbd-cli/src/commands/project.rs` | deploy | Pass scope to apply+import |
| `tests/fixtures/design.yaml` | Test fixture | Additive `scopes:` block |
| `crates/dbd-core/tests/integration_test.rs` | Integration | Scope + backwards-compat tests |

---

## Task 1: Config types for `scopes:`

**Files:**
- Modify: `crates/dbd-core/src/config.rs`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `crates/dbd-core/src/config.rs` (before the closing `}`):

```rust
    #[test]
    fn parses_scope_object_form() {
        let yaml = "\
project:
  name: t
scopes:
  hub:
    includes: [config, app.users]
    deps: include
  reporting:
    excludes: [staging]
";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        let hub = match config.scopes.get("hub").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(hub.includes, vec!["config", "app.users"]);
        assert_eq!(hub.deps, DepsPolicy::Include);
        let rep = match config.scopes.get("reporting").unwrap() {
            ScopeEntry::Spec(s) => s,
            _ => panic!("expected spec"),
        };
        assert_eq!(rep.excludes, vec!["staging"]);
        assert_eq!(rep.deps, DepsPolicy::Report); // default
    }

    #[test]
    fn parses_scope_all_string_form() {
        let yaml = "project:\n  name: t\nscopes:\n  default: all\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(config.scopes.get("default"), Some(ScopeEntry::All(s)) if s == "all"));
    }

    #[test]
    fn scopes_default_empty_when_absent() {
        let yaml = "project:\n  name: t\n";
        let config: DesignConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.scopes.is_empty());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core --lib config::tests::parses_scope_object_form`
Expected: FAIL — `DepsPolicy` / `ScopeEntry` / `scopes` do not exist (compile error).

- [ ] **Step 3: Add the types and field**

In `crates/dbd-core/src/config.rs`, add the `scopes` field to `DesignConfig` (right after the `target` field, around line 17):

```rust
    #[serde(default)]
    pub scopes: IndexMap<String, ScopeEntry>,
```

Then add this block immediately after the `DesignConfig` `impl` (around line 52, before `// ── Project ──`):

```rust
// ── Scopes ──────────────────────────────────────────────

/// Dependency-gap policy for a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DepsPolicy {
    /// Gaps are errors; deploy refuses (default).
    #[default]
    Report,
    /// Deploy auto-expands to the dependency closure.
    Include,
}

/// A scope entry: either the literal string `all` or an include/exclude spec.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ScopeEntry {
    All(String),
    Spec(ScopeSpec),
}

/// Include/exclude selection plus dependency policy for one scope.
#[derive(Debug, Default, Deserialize)]
pub struct ScopeSpec {
    #[serde(default)]
    pub includes: Vec<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    #[serde(default)]
    pub deps: DepsPolicy,
}
```

> `IndexMap` is already imported at the top of `config.rs`. The `All(String)` arm being a plain string is why `untagged` is required — the same pattern as `ExtensionEntry`/`SchemaEntry`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib config::tests`
Expected: PASS (all config tests, including the 3 new ones).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/config.rs
git commit -m "feat(scopes): config types for design.yaml scopes block"
```

---

## Task 2: `scope.rs` — `resolve()`

**Files:**
- Create: `crates/dbd-core/src/scope.rs`
- Modify: `crates/dbd-core/src/lib.rs`

- [ ] **Step 1: Create the module with `resolve` + failing tests**

Create `crates/dbd-core/src/scope.rs`:

```rust
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
```

- [ ] **Step 2: Register the module**

In `crates/dbd-core/src/lib.rs`, add `pub mod scope;` in the module list (alphabetically near `scanner`), and add a re-export line after the existing `pub use` block:

```rust
pub use scope::{ResolvedScope, ScopeGap};
```

> `ScopeGap` is added in Task 3; if compiling Task 2 standalone, temporarily re-export only `ResolvedScope` and add `ScopeGap` to the re-export in Task 3.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib scope::tests`
Expected: PASS (7 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/dbd-core/src/scope.rs crates/dbd-core/src/lib.rs
git commit -m "feat(scopes): scope.rs resolve() with include/exclude + validation"
```

---

## Task 3: `scope.rs` — `analyze_gaps()`

**Files:**
- Modify: `crates/dbd-core/src/scope.rs`, `crates/dbd-core/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Add to `scope.rs` `tests` module:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core --lib scope::tests::gaps_direct_reference`
Expected: FAIL — `analyze_gaps` / `ScopeGap` not defined.

- [ ] **Step 3: Implement `ScopeGap`, `traverse`, `analyze_gaps`**

Add to `scope.rs` (after the `ResolvedScope` struct, before `is_scopable`):

```rust
/// One dependency gap surfaced by inspect.
#[derive(Debug, Clone)]
pub struct ScopeGap {
    pub missing: String,
    /// The in-scope entity at the HEAD of `chain`.
    pub required_by: String,
    /// `required_by` → … → `missing`. `chain[0]` is in-scope; `chain.last()` is `missing`.
    pub chain: Vec<String>,
}
```

Add (after `add_present_schemas`, before `resolve`):

```rust
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
    let mut queue: VecDeque<String> = resolved.entities.iter().cloned().collect();

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
```

Add (after `resolve`):

```rust
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
                required_by: chain.first().cloned().unwrap_or_default(),
                missing: missing.clone(),
                chain,
            }
        })
        .collect();
    gaps.sort_by(|a, b| a.missing.cmp(&b.missing));
    gaps
}
```

- [ ] **Step 4: Ensure `ScopeGap` is re-exported**

Confirm `crates/dbd-core/src/lib.rs` has `pub use scope::{ResolvedScope, ScopeGap};`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib scope::tests`
Expected: PASS (13 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/scope.rs crates/dbd-core/src/lib.rs
git commit -m "feat(scopes): analyze_gaps() with hierarchical dependency chains"
```

---

## Task 4: `scope.rs` — `closure()`

**Files:**
- Modify: `crates/dbd-core/src/scope.rs`

- [ ] **Step 1: Write failing tests**

Add to `scope.rs` `tests` module:

```rust
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
    }

    #[test]
    fn closure_all_scope_is_full_set() {
        let s = resolve(&IndexMap::new(), None, None, &world(), &[]).unwrap();
        let c = closure(&s, &world(), &[]).unwrap();
        assert!(c.contains("app.orders"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core --lib scope::tests::closure_expands_dependencies`
Expected: FAIL — `closure` not defined.

- [ ] **Step 3: Implement `closure`**

Add to `scope.rs` (after `analyze_gaps`):

```rust
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

    for n in visited.iter() {
        if !resolved.entities.contains(n) && resolved.excluded.contains(n) {
            return Err(DbdError::Config(format!(
                "scope '{}' excludes '{}' but an in-scope entity requires it",
                resolved.name, n
            )));
        }
    }

    add_present_schemas(&mut visited, all_entities);
    Ok(visited)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib scope::tests`
Expected: PASS (16 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/scope.rs
git commit -m "feat(scopes): closure() with exclude-conflict detection"
```

---

## Task 5: `Design::resolve_scope` + working-set predicates

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module in `design.rs` (use the existing `fixture_dir()` helper):

```rust
    #[test]
    fn resolve_scope_all_when_none() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(None, None).unwrap();
        assert!(scope.is_all);
    }

    #[test]
    fn working_set_filters_to_scope() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        // fixture has a `config_only` scope (added in Task 12 fixture edit);
        // here build a ResolvedScope directly to keep this test self-contained.
        let scope = design
            .resolve_scope(Some("all"), None)
            .unwrap();
        let ws = design.working_set(&scope).unwrap();
        // all-scope working set contains config.lookups
        assert!(ws.contains("config.lookups"));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core --lib design::tests::resolve_scope_all_when_none`
Expected: FAIL — `resolve_scope` / `working_set` not defined.

- [ ] **Step 3: Implement the methods**

Add `use crate::scope::{self, DepsPolicy, ResolvedScope};` to the imports at the top of `design.rs` (note `DepsPolicy` lives in `config`, re-imported via `scope` is wrong — import it directly):

Replace the intended import with these two lines among the existing `use` statements:

```rust
use crate::config::DepsPolicy;
use crate::scope::{self, ResolvedScope};
```

Add these methods inside `impl Design` (near `config()` / `entities()`, around line 493):

```rust
    /// External entity names from config (for ref resolution / gap analysis).
    fn external_names(&self) -> Vec<String> {
        self.config.external.iter().map(|e| e.name.clone()).collect()
    }

    /// Resolve a scope by name. `None` ⇒ `default` scope if defined, else `all`.
    /// `deps_override` (CLI `--deps`) wins over the scope's own `deps`.
    pub fn resolve_scope(
        &self,
        name: Option<&str>,
        deps_override: Option<DepsPolicy>,
    ) -> Result<ResolvedScope> {
        scope::resolve(
            &self.config.scopes,
            name,
            deps_override,
            &self.entities,
            &self.external_names(),
        )
    }

    /// The set of entity names an operation should act on under this scope.
    /// `include` policy expands to the dependency closure.
    pub fn working_set(&self, scope: &ResolvedScope) -> Result<std::collections::HashSet<String>> {
        match scope.deps {
            DepsPolicy::Include => scope::closure(scope, &self.entities, &self.external_names()),
            DepsPolicy::Report => Ok(scope.entities.clone()),
        }
    }

    /// Whether an entity is kept under a resolved scope's working set.
    /// Extensions/roles/externals are always-on infrastructure.
    fn entity_in_scope(
        entity: &Entity,
        scope: &ResolvedScope,
        working_set: &std::collections::HashSet<String>,
    ) -> bool {
        scope.is_all
            || matches!(
                entity.entity_type,
                EntityType::Extension | EntityType::Role | EntityType::External
            )
            || working_set.contains(&entity.name)
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib design::tests::resolve_scope_all_when_none design::tests::working_set_filters_to_scope`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(scopes): Design::resolve_scope + working-set predicates"
```

---

## Task 6: Scope-aware `report` (inspect gaps)

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing test**

Add to `design.rs` `tests`:

```rust
    #[test]
    fn report_surfaces_scope_gaps() {
        // Synthesize a Design-independent check via resolve + analyze_gaps is
        // covered in scope.rs; here verify report threads gaps through.
        let config_path = fixture_dir().join("design.yaml");
        let mut design = Design::from_config(&config_path, "dev").unwrap();
        let scope = design.resolve_scope(Some("all"), None).unwrap();
        let report = design.report(None, Some(&scope));
        assert!(report.gaps.is_empty()); // all-scope ⇒ no gaps
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core --lib design::tests::report_surfaces_scope_gaps`
Expected: FAIL — `report` takes 1 arg, `Report` has no `gaps` field.

- [ ] **Step 3: Add `gaps` to `Report` and a scope param to `report`**

In `design.rs`, modify the `Report` struct (around line 242):

```rust
pub struct Report {
    pub entity: Option<Entity>,
    pub issues: Vec<Entity>,
    pub warnings: Vec<Entity>,
    pub gaps: Vec<scope::ScopeGap>,
}
```

Change the `report` method signature and add gap computation (around line 635). Replace the whole method with:

```rust
    /// Generate a validation report, optionally scoped to one entity and/or a scope.
    pub fn report(&mut self, name: Option<&str>, scope: Option<&ResolvedScope>) -> Report {
        if !self.validated {
            self.validate();
        }

        let entity = name.and_then(|n| self.entities.iter().find(|e| e.name == n).cloned());

        let issues: Vec<Entity> = self
            .entities
            .iter()
            .chain(self.import_tables.iter())
            .filter(|e| !e.errors.is_empty())
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .cloned()
            .collect();

        let warnings: Vec<Entity> = self
            .entities
            .iter()
            .chain(self.import_tables.iter())
            .filter(|e| !e.warnings.is_empty())
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .cloned()
            .collect();

        let gaps = match scope {
            Some(s) => scope::analyze_gaps(s, &self.entities, &self.external_names()),
            None => Vec::new(),
        };

        Report {
            entity,
            issues,
            warnings,
            gaps,
        }
    }
```

- [ ] **Step 4: Update existing `report` call sites in core tests**

These call `report(None)` / `report(Some(..))` and must add the scope arg `None`:

- `crates/dbd-core/src/design.rs:1288` → `design.report(None, None)`
- `crates/dbd-core/tests/integration_test.rs:293` → `d.report(None, None)`
- `crates/dbd-core/tests/integration_test.rs:310` → `d.report(Some("config.lookups"), None)`

(The `dbd-cli` call sites `schema.rs:56` and `project.rs:195` are updated in Task 11.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib design::tests::report_surfaces_scope_gaps`
Expected: PASS.
Run: `cargo test -p dbd-core --test integration_test`
Expected: PASS (compile fixed by the call-site edits).

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs crates/dbd-core/tests/integration_test.rs
git commit -m "feat(scopes): scope-aware report() with gaps field"
```

---

## Task 7: `build_execution_plan` scope intersection

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing test**

Add to `design.rs` `tests` (alongside the other execution-plan tests; reuses `test_entity`/`test_migration` helpers defined there):

```rust
    #[test]
    fn execution_plan_skips_out_of_scope_migration_steps() {
        use std::collections::HashSet;
        let entities = vec![test_entity("a"), test_entity("b")];
        // migration drops "c" (not in scope) and alters "b" (in scope)
        let migrations = vec![test_migration(1, 2, vec![], vec!["b"], vec!["c"])];
        let in_scope: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();

        let plan = build_execution_plan(&entities, 1, 2, &migrations, Some(&in_scope));

        // No DropEntity for "c"
        assert!(!plan.steps.iter().any(|s| matches!(
            s, ExecutionStep::DropEntity { entity_name, .. } if entity_name == "c"
        )));
        // SetVersion still advances
        assert!(plan.steps.iter().any(|s| matches!(s, ExecutionStep::SetVersion(2))));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core --lib design::tests::execution_plan_skips_out_of_scope_migration_steps`
Expected: FAIL — `build_execution_plan` takes 4 args.

- [ ] **Step 3: Add `scope_names` param + guards**

Change the `build_execution_plan` signature (around line 106):

```rust
pub fn build_execution_plan(
    entities: &[Entity],
    db_version: u32,
    latest_version: u32,
    pending_migrations: &[PendingMigration],
    scope_names: Option<&std::collections::HashSet<String>>,
) -> ExecutionPlan {
```

Add a helper closure at the top of the function body (right after the signature, before the `valid_entities` filter):

```rust
    let in_scope = |n: &str| scope_names.map_or(true, |s| s.contains(n));
```

In the **Migrate** branch, guard the `CreateEntity` and `MigrateEntity` emission and the per-entity `ApplyEntity` by `in_scope`. The simplest correct change: at the start of the `for entity in &valid_entities` loop body, add:

```rust
        if !in_scope(entity.name.as_str()) {
            continue;
        }
```

And in the dropped-entities loop, guard each drop:

```rust
    for migration in pending_migrations {
        for table_name in &migration.dropped {
            if !in_scope(table_name) {
                continue;
            }
            // ... existing DropEntity push ...
```

(The `Fresh` and `Current` branches operate on `valid_entities`, which the caller already filters to the scope — see Task 8 — so they need no change. But add the same `in_scope` guard to the `Fresh` and `Current` map closures defensively is unnecessary; leave them.)

- [ ] **Step 4: Update all existing `build_execution_plan` call sites**

Add `, None` as the 5th argument to each of these (the test added in Step 1 already passes `Some(..)`):

- `crates/dbd-core/src/design.rs:740` — the `apply` call: this becomes scope-aware in Task 8; **for now** change to `&pending, None)`. Task 8 replaces `None` with the real scope set.
- `crates/dbd-core/src/design.rs:1554, 1579, 1607, 1637, 1664, 1687, 1713, 1731, 1746, 1756, 1777, 1802` — append `, None` before the closing `)`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib design::tests`
Expected: PASS (all execution-plan tests + the new one).

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(scopes): build_execution_plan intersects migration steps with scope"
```

---

## Task 8: Scope-aware `apply`

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing test**

Add to `design.rs` `tests`:

```rust
    #[tokio::test]
    async fn apply_with_report_gaps_errors_before_writing() {
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;
        use crate::config::DepsPolicy;

        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        // Hand-build a report-policy scope that includes only config.lookup_values
        // (which needs config.lookups) → one gap.
        let scope = ResolvedScope {
            name: "test".into(),
            entities: HashSet::from(["config.lookup_values".to_string(), "config".to_string()]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
        };

        let result = design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await;
        assert!(result.is_err());
        assert!(mock.applied_names().is_empty()); // no writes
    }

    #[tokio::test]
    async fn apply_with_scope_filters_entities() {
        use crate::scope::ResolvedScope;
        use std::collections::HashSet;
        use crate::config::DepsPolicy;

        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();

        // Complete scope: whole config schema.
        let scope = ResolvedScope {
            name: "config_only".into(),
            entities: HashSet::from([
                "config".to_string(),
                "config.lookups".to_string(),
                "config.lookup_values".to_string(),
            ]),
            excluded: HashSet::new(),
            deps: DepsPolicy::Report,
            is_all: false,
        };

        design
            .apply(&mock, None, false, Some(&scope), |_| {}, |_, _| {}, |_| {})
            .await
            .unwrap();
        let applied = mock.applied_names();
        assert!(applied.iter().any(|n| n == "config.lookups"));
        assert!(!applied.iter().any(|n| n.starts_with("staging.")));
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dbd-core --lib design::tests::apply_with_scope_filters_entities`
Expected: FAIL — `apply` takes 6 args (no scope).

- [ ] **Step 3: Add the `scope` param + gap-gate + filtering**

Change the `apply` signature (around line 676) to insert `scope: Option<&ResolvedScope>` after `dry_run`:

```rust
    pub async fn apply<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_start: S,
        mut on_done: D,
        mut on_complete: C,
    ) -> Result<()>
    where
        S: FnMut(&str),
        D: FnMut(&str, Option<&str>),
        C: FnMut(ApplyComplete),
    {
```

At the very top of the body, compute the working set and gate on gaps:

```rust
        // Resolve scope → working set, gap-gate under `report`.
        let working_set: Option<std::collections::HashSet<String>> = match scope {
            Some(s) if !s.is_all => {
                if s.deps == DepsPolicy::Report {
                    let gaps = scope::analyze_gaps(s, &self.entities, &self.external_names());
                    if !gaps.is_empty() {
                        let detail: String = gaps
                            .iter()
                            .map(|g| format!("  {} requires {} ({})", g.required_by, g.missing, g.chain.join(" → ")))
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Err(DbdError::Config(format!(
                            "scope '{}' has {} dependency gap(s) — add them or use --deps include:\n{detail}",
                            s.name,
                            gaps.len()
                        )));
                    }
                }
                Some(self.working_set(s)?)
            }
            _ => None,
        };
```

Update the `valid_entities` filter (around line 690) to also apply the scope filter. Replace the existing filter chain with:

```rust
        let valid_entities: Vec<&Entity> = self
            .entities
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| match (&working_set, scope) {
                (Some(ws), Some(s)) => Self::entity_in_scope(e, s, ws),
                _ => true,
            })
            .collect();
```

For the **batch adapter** path (Convex, around line 703), the `owned` vec is built from `valid_entities`, which is now already scope-filtered — no further change needed.

Update the `build_execution_plan` call (now around line 740) to pass the scope set:

```rust
        let plan = build_execution_plan(
            &scoped_entities,
            db_version,
            latest_version,
            &pending,
            working_set.as_ref(),
        );
```

- [ ] **Step 4: Update existing `apply` call sites (pass `None`)**

Insert `None,` as the 4th argument (after the `dry_run` bool) in each:

- `crates/dbd-core/src/design.rs:1078` (inside `deploy`) — updated properly in Task 10; for now pass `None`.
- `crates/dbd-core/src/design.rs:1311, 1321, 1336, 1853, 1897` (tests).
- `crates/dbd-core/tests/embedded_test.rs:293, 307`.
- `crates/dbd-core/tests/integration_test.rs:512, 520, 528`.

Example: `design.apply(&mock, None, true, |_| {}, ...)` → `design.apply(&mock, None, true, None, |_| {}, ...)`.

(The `dbd-cli` sites `schema.rs:185` and `project.rs:212` are updated in Task 11 / Task 10.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib design::tests`
Expected: PASS (including the two new apply tests).
Run: `cargo test -p dbd-core`
Expected: PASS (integration + embedded compile via the call-site edits; embedded tests need a DB and may be `ignored` — confirm they at least compile).

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs crates/dbd-core/tests/embedded_test.rs crates/dbd-core/tests/integration_test.rs
git commit -m "feat(scopes): scope-aware apply with report-mode gap gate"
```

---

## Task 9: Scope-aware `import_data`

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing test**

Add to `design.rs` `tests`:

```rust
    #[test]
    fn import_entry_in_scope_predicate() {
        use std::collections::HashSet;
        let mut entry = ImportPlanEntry {
            table: Entity::new(EntityType::Import, "staging.lookups"),
            procedure: Some("staging.import_lookups".to_string()),
            writes: vec!["config.lookups".to_string()],
        };
        let ws: HashSet<String> = ["config.lookups".to_string()].into_iter().collect();
        assert!(import_entry_in_scope(&entry, &ws, false));

        // write-target out of scope → excluded
        entry.writes = vec!["config.other".to_string()];
        assert!(!import_entry_in_scope(&entry, &ws, false));

        // is_all bypasses
        assert!(import_entry_in_scope(&entry, &ws, true));
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core --lib design::tests::import_entry_in_scope_predicate`
Expected: FAIL — `import_entry_in_scope` not defined.

- [ ] **Step 3: Add the predicate + scope param**

Add this free function near `build_execution_plan` in `design.rs`:

```rust
/// Whether an import plan entry runs under a scope's working set.
/// An entry with write-targets is kept only if ALL targets are in scope;
/// a proc-less entry is kept if its staging table is in scope.
fn import_entry_in_scope(
    entry: &ImportPlanEntry,
    working_set: &std::collections::HashSet<String>,
    is_all: bool,
) -> bool {
    if is_all {
        return true;
    }
    if !entry.writes.is_empty() {
        entry.writes.iter().all(|w| working_set.contains(w))
    } else {
        working_set.contains(&entry.table.name)
    }
}
```

Change the `import_data` signature (around line 971) to insert `scope: Option<&ResolvedScope>` after `dry_run`:

```rust
    pub async fn import_data<S, D, C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        name: Option<&str>,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_start: S,
        mut on_done: D,
        mut on_complete: C,
    ) -> Result<()>
```

Just after `let plan = self.import_plan(name);` (around line 985), filter the plan and gap-gate:

```rust
        let plan: Vec<ImportPlanEntry> = match scope {
            Some(s) if !s.is_all => {
                if s.deps == DepsPolicy::Report {
                    let gaps = scope::analyze_gaps(s, &self.entities, &self.external_names());
                    if !gaps.is_empty() {
                        return Err(DbdError::Config(format!(
                            "scope '{}' has {} dependency gap(s) — resolve before importing",
                            s.name,
                            gaps.len()
                        )));
                    }
                }
                let ws = self.working_set(s)?;
                plan.into_iter()
                    .filter(|e| import_entry_in_scope(e, &ws, false))
                    .collect()
            }
            _ => plan,
        };
```

- [ ] **Step 4: Update existing `import_data` call sites (pass `None`)**

Insert `None,` after the `dry_run` bool in each **Design::import_data** call (NOT the `adapter.import_data` trait calls in `sqlite.rs`/`convex.rs` — leave those untouched):

- `crates/dbd-core/src/design.rs:1083` (inside `deploy`) — finalized in Task 10; pass `None` for now.
- `crates/dbd-core/src/design.rs:1508` (test).

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core --lib design::tests::import_entry_in_scope_predicate`
Expected: PASS.
Run: `cargo test -p dbd-core`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(scopes): scope-aware import_data with write-target filtering"
```

---

## Task 10: Scope-aware `deploy`

**Files:**
- Modify: `crates/dbd-core/src/design.rs`

- [ ] **Step 1: Write failing test**

Add to `design.rs` `tests`:

```rust
    #[tokio::test]
    async fn deploy_with_all_scope_applies_everything() {
        let config_path = fixture_dir().join("design.yaml");
        let design = Design::from_config(&config_path, "dev").unwrap();
        let mock = MockAdapter::new();
        let scope = design.resolve_scope(Some("all"), None).unwrap();

        design.deploy(&mock, false, Some(&scope), |_| {}).await.unwrap();
        assert!(!mock.applied_names().is_empty());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p dbd-core --lib design::tests::deploy_with_all_scope_applies_everything`
Expected: FAIL — `deploy` takes 3 args.

- [ ] **Step 3: Add `scope` param and thread it**

Change the `deploy` signature (around line 1066) to insert `scope: Option<&ResolvedScope>` after `dry_run`:

```rust
    pub async fn deploy<C>(
        &self,
        adapter: &dyn DatabaseAdapter,
        dry_run: bool,
        scope: Option<&ResolvedScope>,
        mut on_complete: C,
    ) -> Result<()>
    where
        C: FnMut(DeployComplete),
    {
```

Update the internal `apply` and `import_data` calls to pass `scope`:

```rust
        self.apply(adapter, None, dry_run, scope, |_| {}, |_, _| {}, |s| {
            apply_summary = Some(s);
        })
        .await?;

        self.import_data(adapter, None, dry_run, scope, |_| {}, |_, _| {}, |s| {
            import_summary = Some(s);
        })
        .await?;
```

> The gap-gate in `apply` runs first; if it passes, `import_data`'s gate is a redundant no-op — harmless and keeps each method correct when called directly.

- [ ] **Step 4: Update existing `deploy` call sites (pass `None`)**

Insert `None,` after the `dry_run` bool:

- `crates/dbd-core/src/design.rs:1923, 2028` (tests).
- `crates/dbd-core/tests/embedded_test.rs:140, 167, 173, 198, 236`.

Example: `design.deploy(&mock, true, |_| {})` → `design.deploy(&mock, true, None, |_| {})`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dbd-core`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs crates/dbd-core/tests/embedded_test.rs
git commit -m "feat(scopes): scope-aware deploy threads scope to apply+import"
```

---

## Task 11: CLI — `--scope` / `--deps` flags + wiring + gap rendering

**Files:**
- Modify: `crates/dbd-cli/src/cli.rs`, `crates/dbd-cli/src/main.rs`, `crates/dbd-cli/src/commands/mod.rs`, `crates/dbd-cli/src/commands/schema.rs`, `crates/dbd-cli/src/commands/data.rs`, `crates/dbd-cli/src/commands/project.rs`

- [ ] **Step 1: Add CLI flags + `DepsArg`**

In `crates/dbd-cli/src/cli.rs`, add to the global args block of `struct Cli` (after `target`, around line 28):

```rust
    /// Scope name from design.yaml (default: full set)
    #[arg(long, global = true)]
    pub scope: Option<String>,

    /// Dependency policy override: report | include
    #[arg(long, global = true, value_enum)]
    pub deps: Option<DepsArg>,
```

Add at the top of `cli.rs` (after the `use` lines):

```rust
#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum DepsArg {
    Report,
    Include,
}

impl From<DepsArg> for dbd_core::config::DepsPolicy {
    fn from(a: DepsArg) -> Self {
        match a {
            DepsArg::Report => dbd_core::config::DepsPolicy::Report,
            DepsArg::Include => dbd_core::config::DepsPolicy::Include,
        }
    }
}
```

> `DepsPolicy` must be reachable as `dbd_core::config::DepsPolicy`. It is `pub` in `config.rs` and `config` is `pub mod` in `lib.rs`, so this path works. (Optionally also re-export it from the crate root; not required.)

- [ ] **Step 2: Thread args through `main.rs`**

In `crates/dbd-cli/src/main.rs`, update the `commands::run(...)` call to pass the two new args:

```rust
    if let Err(e) = commands::run(
        &args.command,
        &config,
        &args.environment,
        args.database.as_deref(),
        &project_dir,
        &args.source,
        args.scope.as_deref(),
        args.deps.map(Into::into),
        verbosity,
    )
    .await
```

- [ ] **Step 3: Thread through `commands/mod.rs::run`**

In `crates/dbd-cli/src/commands/mod.rs`, change the `run` signature to add the params (before `verbosity`):

```rust
pub async fn run(
    command: &Commands,
    config: &Path,
    env: &str,
    database_url: Option<&str>,
    project_dir: &Path,
    source: &str,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
```

Update the four phase-1 command dispatches to forward `scope`/`deps`:

```rust
        Commands::Inspect { name, fix, database } => {
            schema::cmd_inspect(config, env, project_dir, database_url, name.as_deref(), *fix, *database, scope, deps, verbosity).await
        }
        // ...
        Commands::Apply { name, dry_run, with_policies } => {
            schema::cmd_apply(config, env, project_dir, database_url, name.as_deref(), *dry_run, *with_policies, scope, deps, verbosity).await
        }
        // ...
        Commands::Import { name, dry_run } => {
            if *dry_run {
                data::cmd_import_dry_run(config, env, project_dir, name.as_deref(), scope, deps, verbosity)
            } else {
                data::cmd_import(config, env, project_dir, database_url, name.as_deref(), scope, deps, verbosity).await
            }
        }
        // ...
        Commands::Deploy { dry_run } => {
            project::cmd_deploy(source, config, env, database_url, *dry_run, scope, deps, verbosity).await
        }
```

> All other match arms keep their existing calls (the params are simply not forwarded to them in v1).

- [ ] **Step 4: `cmd_inspect` — resolve scope, render gaps, exit non-zero**

In `crates/dbd-cli/src/commands/schema.rs`, add `scope`/`deps` params to `cmd_inspect` (before `verbosity`):

```rust
pub async fn cmd_inspect(
    config: &Path,
    env: &str,
    project_dir: &Path,
    database_url: Option<&str>,
    name: Option<&str>,
    fix: bool,
    use_database: bool,
    scope: Option<&str>,
    deps: Option<dbd_core::config::DepsPolicy>,
    verbosity: Verbosity,
) -> Result<()> {
```

Replace the `let report = design.report(name);` line (around line 56) with scope resolution + gap rendering:

```rust
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
    let report = design.report(name, Some(&resolved));

    if !resolved.is_all {
        output::info(verbosity, &format!("scope '{}': {} entities", resolved.name, resolved.entities.len()));
        for gap in &report.gaps {
            output::always(&format!(
                "✗ dependency gap: {} requires {} (out of scope)\n    chain: {}",
                gap.required_by, gap.missing, gap.chain.join(" → ")
            ));
        }
        if !report.gaps.is_empty() {
            match resolved.deps {
                dbd_core::config::DepsPolicy::Report => {
                    anyhow::bail!(
                        "{} dependency gap(s) in scope '{}' — add them to the scope, or run with --deps include",
                        report.gaps.len(), resolved.name
                    );
                }
                dbd_core::config::DepsPolicy::Include => {
                    output::info(verbosity, &format!("{} gap(s) will be auto-included (--deps include)", report.gaps.len()));
                }
            }
        }
    }
```

> Keep the rest of `cmd_inspect` (the existing entity/error/warning rendering) below this. The `--database`/`--cache` ref-resolution block above it is unchanged.

- [ ] **Step 5: `cmd_apply` — resolve + pass scope**

In `schema.rs`, add `scope`/`deps` params to `cmd_apply` (before `verbosity`), then resolve and pass to `design.apply`. After `let design = Design::from_config_with_dir(...)?;` add:

```rust
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
```

Update the dry-run entity listing filter to honor scope (replace the filter chain in the `if dry_run` block):

```rust
        let ws = design.working_set(&resolved).unwrap_or_default();
        let entities: Vec<_> = design
            .entities()
            .iter()
            .filter(|e| e.errors.is_empty())
            .filter(|e| e.entity_type != dbd_core::EntityType::External)
            .filter(|e| name.is_none() || e.name == name.unwrap_or(""))
            .filter(|e| resolved.is_all || ws.contains(&e.name)
                || matches!(e.entity_type, dbd_core::EntityType::Extension | dbd_core::EntityType::Role))
            .collect();
```

Update the real `design.apply(...)` call (around line 185) to pass `Some(&resolved)` as the 4th arg:

```rust
        .apply(
            &*adapter,
            name,
            false,
            Some(&resolved),
            |desc| spinner.start(desc),
            |desc, err| spinner.done(desc, err),
            |s| apply_summary = Some(s),
        )
```

- [ ] **Step 6: `cmd_import` / `cmd_import_dry_run` — pass scope**

In `crates/dbd-cli/src/commands/data.rs`, add `scope`/`deps` params to both `cmd_import_dry_run` and `cmd_import` (before `verbosity`). In each, after the `Design::from_config_with_dir(...)` line, add:

```rust
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
```

`cmd_import` — update the `design.import_data(...)` call (around line 68) to pass `Some(&resolved)` as the 4th arg.
`cmd_import_dry_run` — update `let plan = design.import_plan(name);` (line 20) to filter by scope:

```rust
    let plan = design.import_plan(name);
    let ws = design.working_set(&resolved).unwrap_or_default();
    let plan: Vec<_> = plan
        .into_iter()
        .filter(|e| resolved.is_all || e.writes.iter().all(|w| ws.contains(w))
            || (e.writes.is_empty() && ws.contains(&e.table.name)))
        .collect();
```

- [ ] **Step 7: `cmd_deploy` — pass scope**

In `crates/dbd-cli/src/commands/project.rs`, add `scope`/`deps` params to `cmd_deploy` (before `verbosity`). After `let mut design = Design::from_config_with_dir(&config_path, ...)?;` add:

```rust
    let resolved = design.resolve_scope(scope, deps).context("Failed to resolve scope")?;
```

Update the dry-run report call (line 195) to `design.report(None, Some(&resolved))`.
Update the `design.apply(...)` call (around line 212) to pass `Some(&resolved)` as the 4th arg.
Update the `design.import_plan(None)` call (line 226) to filter by scope (same pattern as Step 6), and the `design.import_data(...)` call (around line 231) to pass `Some(&resolved)` as the 4th arg.

- [ ] **Step 8: Build and run the full suite**

Run: `cargo build`
Expected: clean build.
Run: `cargo test`
Expected: PASS.
Run: `cargo clippy --all-targets`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/dbd-cli
git commit -m "feat(scopes): CLI --scope/--deps flags, gap rendering, scope wiring"
```

---

## Task 12: Fixture + integration tests

**Files:**
- Modify: `tests/fixtures/design.yaml`
- Modify: `crates/dbd-core/tests/integration_test.rs`

- [ ] **Step 1: Add a `scopes:` block to the fixture**

Append to `tests/fixtures/design.yaml` (additive — existing assertions don't read `scopes`):

```yaml
scopes:
  config_only:
    includes:
      - config
  incomplete:
    includes:
      - config.lookup_values   # needs config.lookups → dependency gap
  incomplete_auto:
    includes:
      - config.lookup_values
    deps: include
```

- [ ] **Step 2: Write failing integration tests**

Add to `crates/dbd-core/tests/integration_test.rs`:

```rust
#[test]
fn scope_complete_has_no_gaps() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let mut design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("config_only"), None).unwrap();
    let report = design.report(None, Some(&scope));
    assert!(report.gaps.is_empty());
    assert!(scope.entities.contains("config.lookups"));
    assert!(scope.entities.contains("config.lookup_values"));
}

#[test]
fn scope_incomplete_reports_gap() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let mut design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("incomplete"), None).unwrap();
    let report = design.report(None, Some(&scope));
    assert_eq!(report.gaps.len(), 1);
    assert_eq!(report.gaps[0].missing, "config.lookups");
    assert_eq!(report.gaps[0].required_by, "config.lookup_values");
}

#[test]
fn scope_include_policy_closes_gap() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(Some("incomplete_auto"), None).unwrap();
    let ws = design.working_set(&scope).unwrap();
    assert!(ws.contains("config.lookups")); // pulled in by include policy
}

#[test]
fn no_scope_is_full_set() {
    let config_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/design.yaml");
    let design = dbd_core::Design::from_config(&config_path, "dev").unwrap();
    let scope = design.resolve_scope(None, None).unwrap();
    assert!(scope.is_all);
    // resolved set spans config + staging entities (full project)
    assert!(scope.entities.iter().any(|n| n.starts_with("staging.")));
    assert!(scope.entities.iter().any(|n| n.starts_with("config.")));
}
```

> If `Design::report`/`resolve_scope`/`working_set` aren't visible, confirm `Design` is re-exported (it is, via `pub use design::Design`) and that `report` is `pub`.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p dbd-core --test integration_test scope_`
Expected: PASS (4 tests).
Run: `cargo test`
Expected: PASS (full suite).

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures/design.yaml crates/dbd-core/tests/integration_test.rs
git commit -m "test(scopes): fixture scopes + integration coverage for gaps/closure"
```

---

## Task 13: Docs + README

**Files:**
- Modify: `README.md` (and any `docs/guide/` page that documents `design.yaml`)

- [ ] **Step 1: Document the `scopes:` block**

Add a "Scopes" subsection to `README.md` near the `design.yaml` reference. Include:
- The YAML example from Task 12's fixture.
- The `--scope <name>` and `--deps report|include` flags.
- The rule: omitting `default` ⇒ `all`; `external:` is the only "satisfied elsewhere" mechanism.
- A one-line Sensei example: `dbd deploy --scope hub --database $HUB_URL`.

- [ ] **Step 2: Verify build of any doc tests**

Run: `cargo test --doc -p dbd-core`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add README.md docs/
git commit -m "docs(scopes): document scopes block and --scope/--deps flags"
```

---

## Self-Review

**Spec coverage:**
- `scopes:` schema (string `all` + object, `deps`) → Task 1. ✓
- `resolve` / `analyze_gaps` / `closure` → Tasks 2/3/4. ✓
- `report` policy (error) + `include` (closure) → gap-gate in Tasks 8/9; inspect render in Task 11. ✓
- `external` never a gap; self-ref never a gap → Task 3 tests. ✓
- `exclude` conflict under `include` → Task 4. ✓
- Working-set filter (extensions/roles/externals always-on) → Task 5. ✓
- Per-op scope (`report`/`apply`/`import_data`/`deploy`) → Tasks 6/8/9/10. ✓
- Per-scope migrations (intersection, per-DB version) → Task 7. ✓
- `deps` precedence (CLI > scope > report) → `deps_override` in Tasks 2/5/11. ✓
- CLI `--scope`/`--deps`, gap UX, exit non-zero → Task 11. ✓
- Fixture + integration + backwards-compat → Task 12. ✓
- Docs → Task 13. ✓
- Phase-2 commands (`dbml`/`combine`/`graph`/`export`/`reset`) intentionally NOT scoped — consistent with spec non-goals.

**Type consistency:** `ResolvedScope { name, entities, excluded, deps, is_all }` and `ScopeGap { missing, required_by, chain }` used identically across Tasks 2–12. `DepsPolicy` defined once in `config.rs`, imported in `scope.rs`/`design.rs`, mapped from CLI `DepsArg`. `build_execution_plan`'s 5th param is `Option<&HashSet<String>>` everywhere. `apply`/`import_data`/`deploy`/`report` all take `Option<&ResolvedScope>` in the same position (after `dry_run` / as the scope arg).

**Placeholder scan:** No TBD/TODO; every code step shows complete code; call-site edits enumerate exact file:line + the old→new transformation.

**Note for executor:** Tasks 7–10 each leave a temporary `None` at the `deploy`→`apply`/`import_data` call sites that a later task finalizes (Task 10 wires `deploy`'s real scope through). This is intentional so each task compiles and commits green.
