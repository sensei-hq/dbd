# Lifecycle Script Hooks — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add scope-aware `apply.before` / `apply.after` hooks, and make the existing `import.after` scope-aware, so custom SQL dbd does not model runs at the right phase and only when its dependencies are in scope.

**Architecture:** A shared `ScriptEntry` config type (bare path, or `{script, writes}`) and one shared runner that resolves dependencies, filters by scope, warns on skip, and honours `--dry-run`. Three call sites use it.

**Tech Stack:** Rust 2024, `pg_query` 6, serde untagged enums.

**Spec:** `docs/superpowers/specs/2026-08-25-lifecycle-script-hooks-design.md`

---

## Read the spec's two measured corrections first

Both came from testing against the real `sensei` project, and both change what matters:

1. **Derivation cannot work on the flagship script.** `sensei`'s realtime hook is a `do $$ … $$` block whose table names live in `array[…]` and a `format()` string. Measured: `select_tables`, `dml_tables`, `tables`, `call_functions` all return `[]`. **No parser can derive that.** `writes:` is the primary mechanism for that shape, not an escape hatch.

2. **`import.after` never runs on `dbd apply`.** `cmd_apply` calls `Design::apply` alone; only the deploy path continues into `import_data`. So `apply.after` is the bigger win here — it closes a real gap where the realtime hook runs on deploy and silently not on apply.

Build accordingly: `apply.before`/`apply.after` are the headline; scope filtering is the secondary benefit.

---

## Task 1: `ScriptEntry` config type and dependency resolution

Pure config + logic. No pipeline changes, so nothing is unwired — `ImportConfig.after` changes type, which gives every piece a caller immediately.

**Files:**
- Modify: `crates/dbd-core/src/config.rs` — `ScriptEntry`, `ApplyConfig`, `ImportConfig.after`
- Create: `crates/dbd-core/src/design/hooks.rs` — resolution + scope filtering
- Modify: `crates/dbd-core/src/design/mod.rs` — register the module

- [ ] **Step 1: Write the failing tests**

In `config.rs`'s `mod tests`:

```rust
    #[test]
    fn a_bare_path_script_entry_parses() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\nimport:\n  after:\n    - import/loader.sql\n",
        )
        .unwrap();
        assert_eq!(cfg.import.after.len(), 1);
        assert_eq!(cfg.import.after[0].script(), "import/loader.sql");
        assert!(cfg.import.after[0].declared_writes().is_none());
    }

    #[test]
    fn a_script_entry_with_explicit_writes_parses() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\nimport:\n  after:\n    - script: import/dyn.sql\n      writes: [app.target]\n",
        )
        .unwrap();
        assert_eq!(cfg.import.after[0].script(), "import/dyn.sql");
        assert_eq!(
            cfg.import.after[0].declared_writes(),
            Some(&vec!["app.target".to_string()][..])
        );
    }

    /// The two forms must mix in one list — a project migrating to explicit
    /// writes should not have to convert every entry at once.
    #[test]
    fn both_forms_mix_in_one_list() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\nimport:\n  after:\n    - a.sql\n    - script: b.sql\n      writes: [x.y]\n",
        )
        .unwrap();
        assert_eq!(cfg.import.after.len(), 2);
        assert!(cfg.import.after[0].declared_writes().is_none());
        assert!(cfg.import.after[1].declared_writes().is_some());
    }

    #[test]
    fn the_apply_block_parses_before_and_after() {
        let cfg: DesignConfig = serde_yaml::from_str(
            "project:\n  name: t\napply:\n  before: [pre.sql]\n  after: [post.sql]\n",
        )
        .unwrap();
        assert_eq!(cfg.apply.before.len(), 1);
        assert_eq!(cfg.apply.after.len(), 1);
    }

    /// Every existing project omits `apply:` entirely.
    #[test]
    fn an_absent_apply_block_defaults_to_empty() {
        let cfg: DesignConfig = serde_yaml::from_str("project:\n  name: t\n").unwrap();
        assert!(cfg.apply.before.is_empty());
        assert!(cfg.apply.after.is_empty());
    }
```

In the new `design/hooks.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ws(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_script_whose_tables_are_all_in_scope_runs() {
        let deps = vec!["staging.a".to_string(), "staging.b".to_string()];
        assert!(hook_in_scope(&deps, &ws(&["staging.a", "staging.b"]), false));
    }

    #[test]
    fn a_script_missing_one_table_is_skipped() {
        let deps = vec!["staging.a".to_string(), "staging.b".to_string()];
        assert!(!hook_in_scope(&deps, &ws(&["staging.a"]), false));
    }

    /// Derivation found nothing — the script's dependencies are unknowable, so
    /// it runs. Skipping silently would hide, say, a realtime hook not firing.
    #[test]
    fn a_script_with_no_derivable_dependencies_runs() {
        assert!(hook_in_scope(&[], &ws(&["staging.a"]), false));
    }

    /// The all-scope short-circuits before any derivation cost.
    #[test]
    fn the_all_scope_runs_everything() {
        let deps = vec!["nothing.matching".to_string()];
        assert!(hook_in_scope(&deps, &ws(&[]), true));
    }

    #[test]
    fn plain_sql_dependencies_are_derived_and_qualified() {
        let deps = derive_dependencies(
            "set search_path to app;\ninsert into target select * from staging.a join b on b.id = a.id;",
        );
        assert!(deps.contains(&"app.target".to_string()), "got {deps:?}");
        assert!(deps.contains(&"staging.a".to_string()), "got {deps:?}");
        assert!(deps.contains(&"app.b".to_string()), "got {deps:?}");
    }

    /// Measured against sensei's real realtime hook: its table names live in
    /// `array[…]` and a `format()` string, so no parser can see them. This is
    /// the case `writes:` exists for.
    #[test]
    fn a_do_block_naming_tables_in_data_derives_nothing() {
        let deps = derive_dependencies(
            "do $$ begin\n  execute format('alter publication p add table dojo.%I', 'x');\nend $$;",
        );
        assert!(deps.is_empty(), "expected no derivable deps, got {deps:?}");
    }

    #[test]
    fn unparseable_sql_derives_nothing_rather_than_erroring() {
        assert!(derive_dependencies("NOT SQL AT ALL ;;;").is_empty());
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p dbd-core --lib "config::tests::a_bare_path" > /tmp/t.log 2>&1; echo "exit: $?"; tail -6 /tmp/t.log`
Expected: non-zero — `ScriptEntry` / `apply` do not exist.

- [ ] **Step 3: Implement the config types**

In `config.rs`:

```rust
/// A lifecycle hook script: a project-relative path, optionally with the tables
/// it touches declared explicitly.
///
/// Mirrors [`ImportTableEntry`]'s bare-or-object shape. The object form exists
/// because a script whose table names are *data* — inside `format()` or an
/// `array[…]`, as `sensei`'s realtime hook is — cannot be analysed by any
/// parser, so scope filtering has nothing to go on unless it is told.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ScriptEntry {
    Path(String),
    WithWrites { script: String, writes: Vec<String> },
}

impl ScriptEntry {
    pub fn script(&self) -> &str {
        match self {
            Self::Path(p) => p,
            Self::WithWrites { script, .. } => script,
        }
    }

    /// Explicitly declared tables, or `None` to derive them from the SQL.
    pub fn declared_writes(&self) -> Option<&[String]> {
        match self {
            Self::Path(_) => None,
            Self::WithWrites { writes, .. } => Some(writes),
        }
    }
}

/// Hooks around the DDL apply phase.
#[derive(Debug, Default, Deserialize)]
pub struct ApplyConfig {
    #[serde(default)]
    pub before: Vec<ScriptEntry>,
    #[serde(default)]
    pub after: Vec<ScriptEntry>,
}
```

Add `#[serde(default)] pub apply: ApplyConfig,` to `DesignConfig`, and change `ImportConfig.after` from `Vec<String>` to `Vec<ScriptEntry>`.

Fix the fallout: `config.rs:724`'s existing test asserts `config.import.after == vec!["import/loader.sql"]`. Update it to compare `.script()`. **Report every other call site the type change breaks** — `design/import.rs` iterates it, and `commands/data.rs:95-97` does too.

- [ ] **Step 4: Implement resolution**

Create `crates/dbd-core/src/design/hooks.rs`:

```rust
//! Lifecycle hook scripts: dependency resolution and scope filtering.
//!
//! A hook runs when every table it depends on is in the working set — the same
//! rule `import_entry_in_scope` applies to staging entries, so a scoped run
//! never executes a loader against half-loaded data.
//!
//! Dependencies come from the script's SQL where they can, and from an explicit
//! `writes:` where they cannot. That second case is not exotic: a script naming
//! its tables inside `format()` or an `array[…]` — the shape of a realtime
//! publication hook — is opaque to every parser, measured.

use crate::config::ScriptEntry;

/// Tables a script references, derived from its SQL.
///
/// Empty when the SQL cannot be parsed, or when its table names are data rather
/// than identifiers. Callers treat empty as "unknowable, so run it": silently
/// skipping a hook because analysis came up short would hide, for instance, a
/// realtime hook quietly not firing.
pub(crate) fn derive_dependencies(sql: &str) -> Vec<String> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let default_schema = crate::parser::pg::common::extract_search_paths_via_pg_query(sql)
        .first()
        .cloned()
        .unwrap_or_else(|| "public".to_string());

    let mut out: Vec<String> = Vec::new();
    for name in parsed.tables() {
        if let Some(q) = crate::parser::pg::common::qualify_name_str(&name, &default_schema)
            && !out.contains(&q)
        {
            out.push(q);
        }
    }
    out.sort();
    out
}

/// Whether a hook's dependencies are satisfied by the working set.
pub(crate) fn hook_in_scope(
    deps: &[String],
    working_set: &std::collections::HashSet<String>,
    is_all: bool,
) -> bool {
    if is_all || deps.is_empty() {
        return true;
    }
    deps.iter().all(|d| working_set.contains(d))
}

/// A hook's dependencies: declared if present, derived otherwise.
pub(crate) fn dependencies_of(entry: &ScriptEntry, sql: &str) -> Vec<String> {
    match entry.declared_writes() {
        Some(w) => w.to_vec(),
        None => derive_dependencies(sql),
    }
}
```

`qualify_name_str` and `extract_search_paths_via_pg_query` are `pub(in crate::parser)` — widen to `pub(crate)` if needed and say so. `parsed.tables()` covers both read and write references, which is what scope filtering wants.

Register `mod hooks;` in `design/mod.rs`.

- [ ] **Step 5: Verify**

- `cargo test -p dbd-core --lib config > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo test -p dbd-core --lib design::hooks > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 6: Verify against the real project — parse only, no database**

```bash
cd ~/Developer/sensei-hq/sensei/database
/Users/Jerry/Developer/dbd/target/release/dbd inspect -s . 2>&1 | tail -4
```

Read-only. **Required:** the real project still loads with its existing
`import.after: [import/after/realtime_publication.sql]` — the type change must
not break a shipped config. Report the actual output.

- [ ] **Step 7: Commit**

```
feat(config): add ScriptEntry and the apply hook block

A lifecycle hook is a project-relative path, or an object declaring the
tables it touches. The object form is not an escape hatch for an exotic
edge: measured against sensei's realtime hook, a script naming its tables
inside format() and an array[…] is opaque to every accessor libpg_query
offers — select_tables, dml_tables, tables and call_functions all return
empty. Scope filtering has nothing to go on unless it is told.

import.after changes type but keeps its bare-path form, so shipped configs
are untouched.
```

---

## Task 2: Wire the three hook points

**Files:**
- Modify: `crates/dbd-core/src/design/hooks.rs` — the shared runner
- Modify: `crates/dbd-core/src/design/apply.rs` — `apply.before` / `apply.after`
- Modify: `crates/dbd-core/src/design/import.rs` — scope-filter `import.after`
- Modify: `crates/dbd-core/src/design/mod.rs` — `ApplyComplete` counts
- Modify: `crates/dbd-cli/src/commands/project.rs` — summary rendering

- [ ] **Step 1: Add the shared runner**

One function all three sites call. It must:

- read the script (a missing file is an **error**, not a skip — a hook the user
  declared and dbd cannot find is a real misconfiguration);
- resolve dependencies via `dependencies_of`;
- skip with a warning naming the script AND the out-of-scope table when
  `hook_in_scope` is false;
- honour `dry_run` by reporting without executing, matching
  `import_run_after_scripts`'s existing shape;
- return the count run and the warnings raised.

Warning text, matching the existing staging-table warning's voice:

```
after-script import/loader.sql skipped — needs staging.values, which is outside scope 'partial'
```

- [ ] **Step 2: Wire `apply.before` and `apply.after`**

In `Design::apply`:
- `before` — after `ensure_fully_parsed` and scope resolution, before the first
  `apply_entity`. A bad design must still refuse first.
- `after` — after entities are applied and matviews stamped.

**`apply.after` must run before `policies/`.** In the deploy path
(`design/apply.rs:373`), policies come last; keep it that way.

- [ ] **Step 3: Scope-filter `import.after`**

`import_run_after_scripts` currently takes no scope. Give it the working set and
`is_all`, and replace its doc comment — the existing one states the opposite of
the new behaviour:

> *"These are intentionally NOT scope-filtered — they are post-import hooks, not tied to individual entries; scoped callers ensure their after-scripts are safe."*

- [ ] **Step 4: Counts and rendering**

`ApplyComplete` gains `before_scripts` / `after_scripts`. Skipped hooks go
through the existing `warnings` channel. Follow the house style in
`commands/project.rs` for the summary line.

- [ ] **Step 5: Verify**

- `cargo test --workspace > /tmp/t.log 2>&1; echo "exit: $?"` → 0
- `cargo clippy --workspace --all-targets -- -D warnings > /tmp/c.log 2>&1; echo "exit: $?"` → 0

- [ ] **Step 6: Verify live — a throwaway database, built from the real scenario**

This models `sensei`'s actual shape: a scoped project where a hook depends on a
table the scope excludes.

```bash
cargo build --release
DBD=/Users/Jerry/Developer/dbd/target/release/dbd
R=/tmp/dbd-hooks
rm -rf $R; mkdir -p $R/ddl/table/app $R/ddl/table/staging $R/sql
cat > $R/design.yaml <<'YAML'
project:
  name: hooks
  version: 1
source:
  dialect: postgresql
schemas:
  - app
  - staging
scopes:
  partial:
    excludes: [staging.b]
    deps: include
apply:
  after:
    - sql/post_ddl.sql
import:
  staging: [staging]
  after:
    - sql/loader.sql
    - script: sql/dynamic.sql
      writes: [app.target]
YAML
cat > $R/ddl/table/app/target.ddl <<'DDL'
set search_path to app;
create table if not exists target (id int primary key, n int);
DDL
cat > $R/ddl/table/staging/a.ddl <<'DDL'
set search_path to staging;
create table if not exists a (id int primary key);
DDL
cat > $R/ddl/table/staging/b.ddl <<'DDL'
set search_path to staging;
create table if not exists b (id int primary key);
DDL
# derivable: names staging.a and staging.b as identifiers
cat > $R/sql/loader.sql <<'DDL'
insert into app.target (id, n)
select a.id, count(*) from staging.a a join staging.b b on b.id = a.id group by a.id
on conflict (id) do nothing;
DDL
# NOT derivable: the sensei shape — table name is data
cat > $R/sql/dynamic.sql <<'DDL'
do $$ begin
  execute format('insert into %I.target (id, n) values (1, 1) on conflict do nothing', 'app');
end $$;
DDL
cat > $R/sql/post_ddl.sql <<'DDL'
do $$ begin
  if to_regclass('app.target') is not null then
    execute 'comment on table app.target is ''stamped by apply.after''';
  end if;
end $$;
DDL
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_hooks' -c 'CREATE DATABASE dbd_hooks'
cd $R
echo "=== 1. apply (no scope): apply.after must run — this is the deploy-vs-apply gap ==="
$DBD apply -d postgresql://Jerry@localhost/dbd_hooks -s .
psql -qtA -d dbd_hooks -c "select coalesce(obj_description('app.target'::regclass), '<<NOT STAMPED>>')"
echo "=== 2. import, full scope: both after-scripts run ==="
$DBD import -d postgresql://Jerry@localhost/dbd_hooks -s . 2>&1 | tail -4
echo "=== 3. import, scope 'partial' (excludes staging.b) ==="
$DBD import -d postgresql://Jerry@localhost/dbd_hooks -s . --scope partial 2>&1 | tail -6
psql -q -d postgres -c 'DROP DATABASE IF EXISTS dbd_hooks'
```

**Required:**
- (1) `apply` alone stamps the comment — proving `apply.after` closes the gap where `import.after` never runs on apply.
- (2) full scope runs both after-scripts.
- (3) under `partial`, `sql/loader.sql` is **skipped with a warning naming `staging.b`** (derived), while `sql/dynamic.sql` **still runs** (its declared `writes: [app.target]` is in scope).

That last contrast is the whole feature in one run. Report the ACTUAL output.

- [ ] **Step 7: Verify the real project still works — read-only**

```bash
cd ~/Developer/sensei-hq/sensei/database
/Users/Jerry/Developer/dbd/target/release/dbd inspect -s . 2>&1 | tail -4
/Users/Jerry/Developer/dbd/target/release/dbd inspect -s . --scope dojo 2>&1 | tail -4
```

**Read-only. Do NOT apply, reconcile, deploy or import against any sensei
database.** Required: both load cleanly, as they do today.

- [ ] **Step 8: Commit**

```
feat(design): scope-aware lifecycle hooks around apply and import

apply.before and apply.after are new hook points for SQL dbd does not
model — attaching a table to a publication, granting on objects. They close
a real gap: import.after runs on deploy but never on `dbd apply`, because
cmd_apply calls Design::apply alone, so a project using it for
schema-adjacent work silently gets nothing on apply.

All three hooks now skip, with a warning naming the script and the
offending table, when a scope excludes something they depend on — the same
rule import_entry_in_scope already applies to staging entries. Previously
a loader ran against half-loaded data and only the excluded table was
warned about.

apply.after runs before policies/, since publications and grants attach to
objects that must already exist.
```

---

## Verification checklist

- [ ] `cargo test --workspace` exits 0
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `dbd apply` alone runs `apply.after` (the deploy-vs-apply gap)
- [ ] A derivable hook is skipped when its table is out of scope, warning by name
- [ ] A `writes:`-declared hook still runs when its declared tables are in scope
- [ ] The real sensei project loads unchanged, default and `dojo` scopes
- [ ] No sensei database was written to

## Follow-up this does not do

The real project's realtime hook stays in `import.after`. Moving it to
`apply.after` — where its own comment says it belongs, "once every entity
exists" — is a change to *that* project, not to dbd, and should be a separate
deliberate edit once this ships.
