# Design.yaml Scopes Design

**Date:** 2026-06-04  |  **Status:** Draft
**Scope:** Multi-target deploy from a single design — `scopes:` config block, `scope.rs` resolution + dependency-gap analysis, scope-aware `inspect` / `deploy` / `apply` / `import`

---

## Overview

Today a `design.yaml` describes one full schema, and `deploy`/`apply` always operate on the entire entity set. The Sensei use case needs **one design definition** that can deploy to **more than one database**, where some databases hold only a *subset* of the tables (e.g. a primary DB with the full set, plus a central hub running embedded Postgres with a smaller set).

This feature introduces **scopes**: named, connection-agnostic *selections of entities* defined once in `design.yaml`. A scope is paired with a database at run time (`--scope hub` + `DATABASE_URL`). `deploy`/`apply`/`import` operate only on the scope's entities; `inspect` reports **dependency gaps** — cases where an in-scope entity references a managed entity that the scope omits, transitively / hierarchically.

### Terminology (important)

- **`target`** (existing, unchanged) — the **DB platform/dialect**: `postgres`, `supabase`, … Carries connection/extensions/roles. *Not* a deployment group.
- **`scope`** (new) — a **selection of entities**. Carries **no** connection info.
- **Connection** stays where it is: `--database` / `DATABASE_URL`. You deploy a scope to a database by pairing the two.

### Non-goals (v1)

- Scope-awareness for `reset`, `combine`, `graph`, `dbml`, `export` — these stay full-set (**phase 2** expands to them; signatures already accept the arg so phase 2 adds behavior, not churn).
- Per-scope `external:` declarations — `external:` stays global. The honest rule below makes them unnecessary.
- Fixing dependency edges the parser doesn't already record in `refers` (pre-existing limitation, inherited).

---

## Architecture

**Approach: resolved scope carried at operation boundaries** (chosen over load-time pruning and per-scope materialized designs).

`Design` loads **everything** once, exactly as today. A new pure `scope.rs` module resolves a scope name into a concrete set and runs gap analysis over the already-loaded entities. `apply`/`import`/`deploy` filter their working set by the resolved scope; `inspect` computes gaps against the **full** set ∪ `external`. No `scopes:` block / no `--scope` ⇒ full set ⇒ **100% backwards compatible**.

This is the only approach that preserves rich gap diagnostics (load-time pruning loses the "excluded vs missing" distinction), keeps a single load serving both `inspect` and `deploy`, and isolates the new logic in one testable module.

---

## The honest dependency rule

> A scope's deployable set is always **referentially complete**: every FK / dependency target is **either in-scope or declared `external:`**. There is no "just ignore the gap" door.

`deps` policy is a **dichotomy** (no `ignore` — it would trade a clear pre-deploy error for a confusing runtime FK failure, since shared DDL means the omitted FK still exists physically):

- **`report`** *(default)* — a gap is an **error**: `inspect` lists it; `deploy`/`apply` refuse before any DB write.
- **`include`** — `deploy` auto-expands the working set to the transitive dependency closure; `inspect` shows what would be pulled in.

`external:` (already excluded from gap detection) is the **only** sanctioned "satisfied elsewhere" mechanism, reserved for things truly unmanaged everywhere (e.g. `auth.users`). If a table is *managed* in the primary deploy but referenced from a hub that omits it, the only correct options are **include it** or accept it genuinely can't deploy — integrity demands it.

---

## Config schema (`config.rs`)

```yaml
# NEW top-level block. Absent entirely ⇒ everything is "all" (backwards compatible).
scopes:
  hub:                          # additional scope, selected via --scope hub
    includes:
      - config                  # bare token = a SCHEMA → all entities in it
      - app.users               # dotted = a specific ENTITY
      - app.sessions
    deps: report                # report (default) | include — optional, per-scope
  reporting:
    excludes:                   # denylist form: start from all, drop these
      - staging
      - app.audit_log
    deps: include
# `default:` deliberately omitted ⇒ default = all
```

Rules:
- **You never write `default` when it means `all`.** The block lists only the *additional* scopes. Omitting `default` ⇒ today's behavior.
- **`default` is written only when a bare `dbd deploy` (no `--scope`) should itself deploy a subset.**

```rust
// DesignConfig gains:
#[serde(default)]
pub scopes: IndexMap<String, ScopeEntry>,

// Untagged enum — same idiom as ExtensionEntry / SchemaEntry.
#[serde(untagged)]
pub enum ScopeEntry {
    All(String),                // the literal string "all" (lets you write `default: all` explicitly)
    Spec(ScopeSpec),
}

#[derive(Deserialize, Default)]
pub struct ScopeSpec {
    #[serde(default)] pub includes: Vec<String>,
    #[serde(default)] pub excludes: Vec<String>,
    #[serde(default)] pub deps: DepsPolicy,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DepsPolicy { #[default] Report, Include }
```

> Note: `ScopeEntry::All("all")` must validate that the string is exactly `all`; any other bare string is a config error.

---

## Pure logic (`scope.rs`)

New module, **no I/O**, mirroring `dependency.rs`. Policy-neutral: it computes sets and gaps; the error-vs-expand *decision* lives in the consumer.

```rust
pub struct ResolvedScope {
    pub name: String,                 // "all" | "hub" | "default" | ...
    pub entities: HashSet<String>,    // resolved managed-entity names
    pub deps: DepsPolicy,
    pub is_all: bool,                 // fast-path: skip all filtering
}

pub struct ScopeGap {
    pub missing: String,        // a managed entity needed but not in scope
    pub required_by: String,    // the in-scope entity at the HEAD of `chain`
    pub chain: Vec<String>,     // required_by → … → missing (hierarchical path)
}
// `chain[0]` is always in-scope (`required_by`); `chain.last()` is `missing`.
// Intermediate nodes may themselves be missing — each missing node is its own
// ScopeGap, all sharing the same in-scope `required_by` root.

/// Section 1 algorithm. Validates every includes/excludes item resolves to a
/// real schema/entity; unknown name/item → DbdError::Config (loud, never silent).
pub fn resolve(
    scopes: &IndexMap<String, ScopeEntry>,
    name: Option<&str>,
    deps_override: Option<DepsPolicy>,
    all_entities: &[Entity],
    externals: &[String],
) -> Result<ResolvedScope>;

/// Inspect engine. Full transitive closure of in-scope refers (reusing the
/// reachable_subgraph walk); missing = closure − in_scope − externals; each
/// missing entity gets one BFS path back to an in-scope entity for `chain`.
/// Self-refs and externals are never gaps. Sorted, deterministic.
pub fn analyze_gaps(
    resolved: &ResolvedScope,
    all_entities: &[Entity],
    externals: &[String],
) -> Vec<ScopeGap>;

/// Used only by deps: include. in_scope ∪ all transitively-referenced managed
/// (non-external) entities. Err if the closure pulls in an explicitly-excluded
/// entity ("scope excludes X but in-scope Y requires it").
pub fn closure(
    resolved: &ResolvedScope,
    all_entities: &[Entity],
    externals: &[String],
) -> Result<HashSet<String>>;
```

### Resolution algorithm

1. **Base set** — `includes` non-empty → base = ∪ of entities matched by each include item; else base = all managed entities.
2. **Subtract** — base −= entities matched by `excludes`.
3. **Auto-completion of infra** — always add the `CREATE SCHEMA` entity for every schema present in the resolved set; `external:` entities always retained (ref resolution); `extensions`/`roles` are target-level infrastructure → **always applied, never scoped** (plus the schemas those extensions target).
4. **Matching** — bare token = schema (all its entities); dotted = exact entity. Unqualified/public entities and wildcards are out of scope for v1 (codebase is schema-qualified throughout); `schema.*` can be added later to align with `ignore`'s syntax.

---

## Integration (`design.rs`)

```rust
pub fn resolve_scope(&self, name: Option<&str>, deps_override: Option<DepsPolicy>)
    -> Result<ResolvedScope>;
```

**Working-set filter** — one private helper, the single place filtering happens. Given `&ResolvedScope`, keep entity `E` iff **any**:
- `scope.is_all`, **or**
- `E.entity_type ∈ {Extension, Role, External}` (always-on infra & ref anchors), **or**
- `E.name ∈ working_set`, where `working_set = scope.entities` (`report`) or `scope::closure(...)` (`include`).

In-scope schema entities are already in `scope.entities`; schema entities required by always-on extensions (e.g. `postgis`→`extensions`) are also retained.

### Per-operation behavior

All ops gain `scope: Option<&str>` (+ relevant ops gain `deps_override`). `None` ⇒ `all` ⇒ today's path byte-for-byte.

| Op | Change |
|---|---|
| `report` / **inspect** | Calls `analyze_gaps`. New report section lists gaps **with chains**. `report` → errors (blocks, non-zero exit); `include` → info ("will auto-include: …"). Scope=all → no scope section, identical to today. |
| `apply` | Resolve → **gap-gate** (under `report`, abort *before any DB write* if gaps) → filter to working set → `build_execution_plan` over it. Batch adapters (Convex) filter their entity vec identically. |
| `import_data` | `import_plan` keeps an entry iff its procedure's **write-targets ⊆ working set**; proc-less entries kept iff their staging table is in scope. Same gap-gate first. |
| `deploy` | Resolve + gap-gate **once**, then apply + import with that one `ResolvedScope`. |

### Per-scope migrations (falls out for free)

`build_execution_plan` gains the in-scope name set and **intersects** each pending migration's `added` / `altered` / `dropped` with it — a step targeting an out-of-scope entity is skipped. `SetVersion` still advances. The version meta is already **per-database**, so each physical DB tracks its own version with no new storage; a migration touching only out-of-scope tables advances the hub's version with zero applied steps — correct.

### `deps` precedence

CLI `--deps` > `scopes.<name>.deps` > global default `report`. Applied when building the `ResolvedScope`.

---

## CLI (`cli.rs`, `commands/mod.rs`)

```rust
// Global flags (alongside existing --config/--database/--environment/--source/--target/--verbose)
#[arg(long, global = true)] pub scope: Option<String>,          // long-only; -s is --source
#[arg(long, global = true)] pub deps: Option<DepsPolicy>,       // ValueEnum: report|include; None ⇒ scope default
```

- `--scope` omitted → `default` scope if defined, else `all`. Unknown name → error listing available scopes. Orthogonal to `--target` (no collision).
- `commands/mod.rs::run` gains `scope: Option<&str>` + `deps: Option<DepsPolicy>`, threaded into `cmd_inspect` / `cmd_apply` / `cmd_import` / `cmd_deploy`. Other commands accept and ignore in v1.

### `inspect` UX

```
$ dbd inspect --scope hub
scope 'hub': 7 entities
✗ dependency gap: app.orders requires app.customers (out of scope)
    chain: app.orders → app.customers
✗ dependency gap: app.orders requires products
    chain: app.orders → app.line_items → products
2 gaps — add these to the scope, or run with --deps include
```

Exits non-zero under `report`. With `-v`, also prints the full resolved entity set. Scope=all → output unchanged from today.

---

## Testing

Mirrors existing style (module unit tests + `tests/fixtures` + integration tests). Must end green under the repo's **zero-errors** bar (`cargo test` + `cargo clippy` with no warnings).

- **`scope.rs` units:** `resolve` (includes-only, excludes-only, both, `all`, unknown-name err, unknown-item err, schema-expansion, schema auto-add); `analyze_gaps` (direct gap, transitive/hierarchical gap + correct `chain`, external never a gap, self-ref never a gap, complete-closure ⇒ no gaps); `closure` (include expansion, exclude-conflict err).
- **`config.rs` units:** parse `scopes:` (the `all` string form, the object form, `deps` field, omitted-default, invalid bare-string err).
- **`design.rs` units:** apply-with-scope filters entities (mock `applied_names ⊆ scope`); apply under `report` with gaps → `Err` *and* no writes issued; apply under `include` applies the closure; `import_plan` honors scope; `build_execution_plan` migration intersection (pending migration altering an out-of-scope entity ⇒ no `MigrateEntity` step, but `SetVersion` still advances); deploy-with-scope.
- **Fixtures:** add a `scopes:` block to `tests/fixtures/design.yaml` (additive — existing assertions don't read it) with one **complete** scope and one intentionally **dependency-incomplete** scope to exercise gaps.
- **Integration (`tests/integration_test.rs`):** `dbd inspect --scope <incomplete>` exits non-zero with the gap message; `--deps include` previews the closure; `deploy --dry-run --scope hub` lists only the scoped set; **backwards-compat:** no `--scope` ⇒ identical entity set/behavior to today.

---

## Files touched

| File | Change |
|---|---|
| `crates/dbd-core/src/config.rs` | `scopes` field, `ScopeEntry` / `ScopeSpec` / `DepsPolicy` types + parse tests |
| `crates/dbd-core/src/scope.rs` | **new** — `ResolvedScope`, `ScopeGap`, `resolve` / `analyze_gaps` / `closure` |
| `crates/dbd-core/src/lib.rs` | register `mod scope;` |
| `crates/dbd-core/src/dependency.rs` | expose/reuse the `reachable_subgraph` walk for closure/gap analysis |
| `crates/dbd-core/src/design.rs` | `resolve_scope`, working-set filter, scope params on `report`/`apply`/`import_data`/`deploy`, `build_execution_plan` migration intersection |
| `crates/dbd-cli/src/cli.rs` | global `--scope` / `--deps` flags |
| `crates/dbd-cli/src/commands/mod.rs` | thread scope/deps to inspect/apply/import/deploy |
| `crates/dbd-cli/src/commands/schema.rs`, `data.rs`, `project.rs` | accept + apply scope in `cmd_inspect`/`cmd_apply`/`cmd_import`/`cmd_deploy`; render gap report |
| `tests/fixtures/design.yaml` | additive `scopes:` block |
| `tests/integration_test.rs` | scope integration + backwards-compat tests |

## Phase 2 (later)

Extend scope-awareness to `dbml`, `combine`, `graph`, `export`, `reset`. Signatures already carry the arg; phase 2 adds behavior only. Possible `schema.*` wildcard matching to align with `ignore` syntax.
