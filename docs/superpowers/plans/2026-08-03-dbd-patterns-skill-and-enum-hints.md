# dbd Pattern Verifier + Enum Hints Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax. NOTE: the skill/agent verification tasks (3, 4) are **empirical evals run by the top-level controller**, not delegated — a subagent cannot spawn the agent under test.

**Goal:** Add an advisory enum-hint to `dbd inspect`, bring the `dbd` skill current + add a workflow decision rule, ship a `dbd-pattern-verifier` agent, and publish the skill+agent via a `sensei.library.json` manifest + static site (mirroring Rokkit).

**Architecture:** (A) a pure `suggest_enum_candidates` in `dbd-core` re-parses each table `CHECK` string with sqlparser and flags single-column string-literal sets, surfaced by `cmd_inspect` in an advisory `Suggestions:` section (no exit-code impact). (B) `docs/skills/dbd/SKILL.md` gains a pre-release-vs-upgrade decision rule + self-check checklist + matview currency; a new `docs/agents/dbd-pattern-verifier.md` reviews projects for conformance. (C) root `sensei.library.json` + an extended `site/scripts/copy-content.mjs` copy the skill/agent/manifest into the site's gitignored `static/` (+ `.well-known/`).

**Tech Stack:** Rust (edition 2024, sqlparser 0.61), SvelteKit site (bun), Claude Code skill/agent markdown. Spec: `docs/superpowers/specs/2026-08-03-dbd-patterns-skill-and-enum-hints-design.md`.

**Commands:** `cargo test -p dbd-core <name>` / `-p dbd-cli`; `cargo clippy --all-targets`; site: `cd site && bun run sync:content`. Pre-commit hook runs full suite + clippy on every commit.

---

## File Structure

- `crates/dbd-core/src/design.rs` — `EnumHint` struct, `suggest_enum_candidates(entities, dialect)`, unit tests.
- `crates/dbd-cli/src/commands/schema.rs` — `print_enum_hints`, wire into `cmd_inspect`.
- `docs/skills/dbd/SKILL.md` — currency + workflow decision rule + self-check checklist.
- `docs/agents/dbd-pattern-verifier.md` — new reviewer agent (create).
- `sensei.library.json` — new root manifest (create).
- `site/scripts/copy-content.mjs` — copy skills/agents/manifest to `static/` + `.well-known/`.
- `site/.gitignore` — ignore generated `static/{skills,agents,sensei.library.json,.well-known}`.
- `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt` — note the Suggestions section + skill/agent availability.

---

## Task 1: `suggest_enum_candidates` (dbd-core, pure + TDD)

**Files:**
- Modify: `crates/dbd-core/src/design.rs` (add struct + fn near `validate_materialized_views`; tests in the inline `mod tests`)

- [ ] **Step 1: Confirm the sqlparser expression-parse API**

The parser stores CHECK expressions as `chk.expr.to_string()` (sqlparser round-trip). Confirm the sqlparser 0.61 API to re-parse a bare expression — likely:
```rust
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
let mut p = Parser::new(&PostgreSqlDialect {}).try_with_sql(expr)?;
let ast: sqlparser::ast::Expr = p.parse_expr()?;
```
Write a throwaway `dbg!` test parsing `"status IN ('active', 'inactive')"` and inspect the `Expr` variant (expect `Expr::InList { expr, list, negated: false }` with `list` of `Expr::Value(Value::SingleQuotedString(..))` — the exact nesting may be `Expr::Value(ValueWithSpan)` in 0.61; adapt the match to the real shape). Remove the probe. Report the exact variant path used.

- [ ] **Step 2: Write failing tests** (in `design.rs` `mod tests`)

```rust
fn tbl_with_check(name: &str, expr: &str) -> Entity {
    let mut e = Entity::new(EntityType::Table, name);
    e.table_def = Some(TableDef {
        columns: vec![],
        constraints: vec![TableConstraint::Check { name: None, expression: expr.to_string() }],
        indexes: vec![],
        comments: Default::default(),
    });
    e
}

#[test]
fn enum_hint_for_in_list_of_strings() {
    let ents = vec![tbl_with_check("config.lookups", "status IN ('active', 'inactive')")];
    let hints = suggest_enum_candidates(&ents, "postgresql");
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].entity, "config.lookups");
    assert_eq!(hints[0].column, "status");
    assert_eq!(hints[0].values, vec!["active".to_string(), "inactive".to_string()]);
}

#[test]
fn enum_hint_for_any_array_of_strings() {
    let ents = vec![tbl_with_check("s.t", "kind = ANY(ARRAY['a','b'])")];
    assert_eq!(suggest_enum_candidates(&ents, "postgresql").len(), 1);
}

#[test]
fn enum_hint_for_or_chain_same_column() {
    let ents = vec![tbl_with_check("s.t", "role = 'admin' OR role = 'user'")];
    let hints = suggest_enum_candidates(&ents, "postgresql");
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].column, "role");
}

#[test]
fn no_hint_for_numeric_range_subquery_mixed_multicol_notin() {
    let cases = [
        "n IN (1, 2, 3)",                       // numeric
        "x > 0 AND x < 10",                     // range
        "char_length(name) < 5",                // length/fn
        "status IN (SELECT s FROM other)",      // subquery
        "status IN ('a', other_col)",           // mixed literal/ident
        "a = 'x' OR b = 'y'",                    // two different columns
        "status NOT IN ('a','b')",              // negated
    ];
    for c in cases {
        let ents = vec![tbl_with_check("s.t", c)];
        assert!(suggest_enum_candidates(&ents, "postgresql").is_empty(), "unexpected hint for: {c}");
    }
}

#[test]
fn no_hint_for_non_postgres_dialect() {
    let ents = vec![tbl_with_check("s.t", "status IN ('a','b')")];
    assert!(suggest_enum_candidates(&ents, "sqlite").is_empty());
}
```

- [ ] **Step 3: Run tests — confirm they fail**

Run: `cargo test -p dbd-core enum_hint no_hint`
Expected: FAIL — `suggest_enum_candidates` / `EnumHint` undefined.

- [ ] **Step 4: Implement**

```rust
/// An advisory hint that a string-set CHECK could be a Postgres enum.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumHint {
    pub entity: String,
    pub column: String,
    pub values: Vec<String>,
}

/// Flag single-column, string-literal-set CHECK constraints as enum candidates.
/// Postgres/Supabase only (`dialect == "postgresql"`); advisory, never an error.
pub fn suggest_enum_candidates(entities: &[Entity], dialect: &str) -> Vec<EnumHint> {
    if dialect != "postgresql" {
        return Vec::new();
    }
    let mut hints = Vec::new();
    for e in entities.iter().filter(|e| e.entity_type == EntityType::Table) {
        let Some(td) = &e.table_def else { continue };
        for c in &td.constraints {
            let TableConstraint::Check { expression, .. } = c else { continue };
            if let Some((column, values)) = string_set_check(expression) {
                let hint = EnumHint { entity: e.name.clone(), column, values };
                if !hints.contains(&hint) {
                    hints.push(hint);
                }
            }
        }
    }
    hints
}

/// Parse a CHECK expression; return (column, string values) if it constrains a
/// single column to a fixed set of string literals, else None.
fn string_set_check(expr: &str) -> Option<(String, Vec<String>)> {
    use sqlparser::ast::Expr;
    use sqlparser::dialect::PostgreSqlDialect;
    use sqlparser::parser::Parser;

    let parsed = Parser::new(&PostgreSqlDialect {})
        .try_with_sql(expr).ok()?
        .parse_expr().ok();
    // Fallback for the trivial `col IN ('a','b')` shape if parse fails.
    let Some(ast) = parsed else { return regex_in_list(expr) };

    match ast {
        // col IN ('a', 'b', ...)  (non-negated, all string literals)
        Expr::InList { expr, list, negated: false } => {
            let col = ident_name(&expr)?;
            let vals = all_string_literals(&list)?;
            Some((col, vals))
        }
        // col = ANY(ARRAY['a','b'])  — adapt to the real 0.61 AST (AnyOp/BinaryOp+Array)
        // and OR-chains of `col = 'lit'`. Implement helpers `any_array_strings` and
        // `or_chain_strings` returning Option<(String, Vec<String>)> with the same
        // "single column + all string literals" rule; try each in turn.
        other => any_array_strings(&other).or_else(|| or_chain_strings(&other)),
    }
}
```
Implement the helpers (`ident_name` → the column for `Expr::Identifier`/`CompoundIdentifier`, None for casts/functions; `all_string_literals` → `Some(vec)` only if every list item is a string literal; `any_array_strings`; `or_chain_strings` → recurse `Expr::BinaryOp{op: Or}` collecting `col = 'lit'` leaves, all the same column; `regex_in_list` → conservative regex fallback for `^\s*"?(\w+)"?\s+IN\s*\(\s*'…'(…)\)\s*$`). Use the REAL sqlparser 0.61 AST shapes confirmed in Step 1. If a shape is awkward in 0.61, it's acceptable to support `InList` + `regex_in_list` robustly and make `any_array_strings`/`or_chain_strings` best-effort — but keep all the negative tests passing (no false positives).

- [ ] **Step 5: Run tests — confirm pass**

Run: `cargo test -p dbd-core enum_hint no_hint`
Expected: PASS (all 5).

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-core/src/design.rs
git commit -m "feat(inspect): detect string-set CHECK constraints as enum candidates"
```

---

## Task 2: surface hints in `dbd inspect`

**Files:**
- Modify: `crates/dbd-cli/src/commands/schema.rs` (`cmd_inspect`, add `print_enum_hints`)
- Test: `crates/dbd-cli/src/commands/schema.rs` inline

- [ ] **Step 1: Read the matview-errors surfacing**

Read how `cmd_inspect` calls `validate_materialized_views` and `print_matview_errors` (added for matviews). Mirror it: after the existing validations, call `suggest_enum_candidates(design.entities(), &design.config().source.dialect)` and print a `Suggestions:` section. Confirm the exact accessors (`design.entities()`, `design.config().source.dialect`, the `output::` helper, `Verbosity`).

- [ ] **Step 2: Write failing test**

```rust
#[test]
fn print_enum_hints_renders_advisory_line() {
    let hints = vec![dbd_core::design::EnumHint {
        entity: "config.lookups".into(), column: "status".into(),
        values: vec!["active".into(), "inactive".into()],
    }];
    let out = render_enum_hints(&hints); // small pure renderer returning Vec<String> or String
    assert!(out.iter().any(|l| l.contains("config.lookups.status")
        && l.contains("'active'") && l.contains("enum")), "got: {out:?}");
}
```
(Extract the rendering into a pure `render_enum_hints(&[EnumHint]) -> Vec<String>` so it's unit-testable without a DB/design; `print_enum_hints` just prints those lines under a `Suggestions:` header via `output::`.)

- [ ] **Step 3: Run — confirm fail**

Run: `cargo test -p dbd-cli print_enum_hints_renders_advisory_line`
Expected: FAIL (undefined).

- [ ] **Step 4: Implement**

Add `render_enum_hints` (pure) + `print_enum_hints` (prints `Suggestions:` + the lines) and call it from `cmd_inspect` after the other validations. Each line:
`  {entity}.{column}: CHECK constrains to a fixed string set {'a','b'} — a Postgres enum (ddl/enum/{schema}/{name}.ddl) gives type safety + cleaner introspection. Not required.`
CRITICAL: do NOT add hint count to `output::summary`'s error count, and do NOT change the exit code — suggestions are advisory. Confirm by reading how matview errors vs the exit path work (matview errors are report-only too).

- [ ] **Step 5: Run tests + manual check**

Run: `cargo test -p dbd-cli print_enum_hints` and `cargo build -p dbd-cli`.
Manual (evidence): scaffold a temp project with a table whose DDL has `CHECK (status IN ('active','inactive'))`, run the built `dbd inspect`, confirm the `Suggestions:` line prints AND `dbd inspect` still exits 0 / reports valid. Paste the output in the task report.

- [ ] **Step 6: Commit**

```bash
git add crates/dbd-cli/src/commands/schema.rs
git commit -m "feat(inspect): advisory Suggestions section for enum candidates"
```

---

## Task 3: `dbd` skill — currency + workflow decision rule + checklist (CONTROLLER-VERIFIED)

**Files:**
- Modify: `docs/skills/dbd/SKILL.md`

- [ ] **Step 1: Edit the skill**

Update `docs/skills/dbd/SKILL.md`:
1. **Currency** — in the type list add `materialized_view`; add `dbd refresh` to the command map; add one matview line (idempotent `CREATE MATERIALIZED VIEW IF NOT EXISTS`; scheduled refresh via pg_cron under `materialized_views:`; reconcile **warns** on drift, never auto-drops).
2. **New `## Workflow — pre-release vs upgrades`** section:
   - "Which am I in?" — **Released** iff `design.yaml` has `project.released: true` **or** `snapshots/` contains a baseline snapshot. Otherwise **pre-release**.
   - **Pre-release:** iterate DDL; `dbd reconcile` converges the live dev DB in place (no snapshots/version bump); fresh DB → `dbd apply`. `--allow-destructive`/`--prune` for drops.
   - **Released (upgrades):** edit DDL → `dbd snapshot` (writes the migration) → `dbd apply` (runs it). `reconcile` is **disabled**; never hand-edit the live DB.
   - A 2-row table + one worked example per side.
3. **New `## Self-check before you touch a dbd project`** checklist (bullets): right workflow for release state; singular type folder; idempotent DDL (`… IF NOT EXISTS` / `CREATE OR REPLACE VIEW` / matview `IF NOT EXISTS`); secrets via `$ENV`; string-set CHECK → enum; matview drift is warn-only; `inspect` doesn't check column-level refs.

Keep the existing prose/voice; do not restate the full CLI reference (link to llms-full).

- [ ] **Step 2: VERIFY (controller-run empirical eval)**

The CONTROLLER (not a subagent) dispatches a fresh general-purpose subagent whose prompt contains ONLY the contents of the updated `SKILL.md` plus each scenario, and checks the answer:
- Released project + "add a `notes` column" → must answer edit DDL → `dbd snapshot` → `dbd apply`; must NOT say `reconcile`.
- Fresh/pre-release + "deployed schema drifted from my DDL, sync it" → `dbd reconcile` is correct.
- "change an already-deployed materialized view's definition" → drop + re-apply / reconcile warns; must NOT claim reconcile auto-recreates.
- "where does `ddl/tables/orders.sql` belong?" → singular `ddl/table/<schema>/orders.ddl`.
PASS = correct on all four. If any fail, tighten the skill wording and re-run. Record the four answers in the task notes.

- [ ] **Step 3: Commit** (only after the eval passes)

```bash
git add docs/skills/dbd/SKILL.md
git commit -m "docs(skill): dbd workflow decision rule, self-check checklist, matview currency"
```

---

## Task 4: `dbd-pattern-verifier` agent (CONTROLLER-VERIFIED)

**Files:**
- Create: `docs/agents/dbd-pattern-verifier.md`

- [ ] **Step 1: Author the agent**

Create `docs/agents/dbd-pattern-verifier.md` with Claude-Code agent frontmatter (model the shape on Rokkit's `packages/cli/agents/rokkit-styles-reviewer.md`):
```markdown
---
name: dbd-pattern-verifier
description: >-
  Use to review a dbd-managed project (or a proposed schema change) for
  convention conformance BEFORE applying it. Catches the big one — using the
  wrong workflow for the project's release state (reconcile on a released
  project, or hand-written migrations pre-release) — plus non-idempotent DDL,
  hardcoded secrets, wrong/plural type folders, string-set CHECKs that should be
  enums, and materialized-view drift misuse. <example>…</example> <example>…</example>
tools: Read, Grep, Glob, Bash
model: sonnet
color: green
---

# dbd Pattern Verifier
<system prompt>
```
System prompt requirements:
- Determine **release state** with the SAME rule as the skill: released iff `project.released: true` in `design.yaml` OR a baseline snapshot exists under `snapshots/`.
- Check, most-severe first: (1) workflow-vs-release-state, (2) non-idempotent DDL (`CREATE TABLE|MATERIALIZED VIEW|INDEX` without `IF NOT EXISTS`, view without `OR REPLACE`), (3) hardcoded secrets in `design.yaml` target URLs, (4) wrong/plural type folders, (5) string-set CHECK → enum, (6) matview drift misuse.
- Cite the `dbd` skill for canonical rules rather than restating them.
- Report only **evidence-backed** findings (file:line). If a `dbd` binary is on PATH, it MAY run `dbd inspect`/`dbd diff` for corroboration, but must work from files alone otherwise.
- Output: ranked findings or an explicit "conformant — no issues."

- [ ] **Step 2: VERIFY (controller-run fixture eval)**

The CONTROLLER creates two throwaway fixture projects in a temp dir and runs the agent (via the Agent tool, `subagent_type: general-purpose`, given the agent's system prompt + "review the project at <path>"):
- **`bad/`**: `design.yaml` with a hardcoded password in the target URL + `project.released: true`; a plural `ddl/tables/orders.ddl` whose DDL is `CREATE TABLE orders (...)` (no `IF NOT EXISTS`) with `CHECK (status IN ('open','closed'))`; and a note/README claiming the dev "ran `dbd reconcile`". Expect the agent to flag: wrong-workflow (reconcile on released), non-idempotent CREATE, hardcoded secret, plural folder, string-set CHECK.
- **`good/`**: a small conformant project (unreleased, `ddl/table/app/orders.ddl` with `create table if not exists`, `$DATABASE_URL`, no string-set CHECK). Expect **zero** findings.
PASS = every planted issue flagged in `bad/` AND zero findings in `good/`. If it misses one or false-positives on `good/`, tighten the agent prompt and re-run. Record both runs.

- [ ] **Step 3: Commit** (only after both fixture runs pass)

```bash
git add docs/agents/dbd-pattern-verifier.md
git commit -m "docs(agent): dbd-pattern-verifier — reviews projects for convention conformance"
```

---

## Task 5: `sensei.library.json` manifest + static-site publishing

**Files:**
- Create: `sensei.library.json` (repo root)
- Modify: `site/scripts/copy-content.mjs`, `site/.gitignore`

- [ ] **Step 1: Create the manifest**

`sensei.library.json` at repo root:
```json
{
  "library": "dbd",
  "version": ">=0.10",
  "repo": "https://github.com/sensei-hq/dbd",
  "branch": "main",
  "site": "https://dbd.sensei-hq.com",
  "skills": [
    { "name": "dbd", "focus": "schema-as-code", "path": "docs/skills/dbd/SKILL.md", "url": "/skills/dbd/SKILL.md" }
  ],
  "agents": [
    { "name": "dbd-pattern-verifier", "focus": "pattern-review", "path": "docs/agents/dbd-pattern-verifier.md", "url": "/agents/dbd-pattern-verifier.md" }
  ]
}
```

- [ ] **Step 2: Extend the copy step**

In `site/scripts/copy-content.mjs`, after the existing llms/guide copies, add (using `cpSync` recursive):
```js
import { cpSync } from 'node:fs';
// 3. skills + agents + manifest → static/ (raw files served at the site root)
const staticDir = join(here, '..', 'static');
cpSync(join(docs, 'skills'), join(staticDir, 'skills'), { recursive: true });
cpSync(join(docs, 'agents'), join(staticDir, 'agents'), { recursive: true });
const manifest = join(repo, 'sensei.library.json');
copyInto(manifest, join(staticDir, 'sensei.library.json'));
copyInto(manifest, join(staticDir, '.well-known', 'sensei.library.json'));
console.log('skills/agents/manifest synced to static/.');
```
(Confirm `docs/agents/` exists — created in Task 4; if a task runs out of order, guard with `existsSync`.)

- [ ] **Step 3: Gitignore the generated copies**

Append to `site/.gitignore`:
```
# Generated by scripts/copy-content.mjs (source: ../docs + ../sensei.library.json)
static/skills/
static/agents/
static/sensei.library.json
static/.well-known/
```

- [ ] **Step 4: VERIFY (build the site + assert artifacts)**

Run: `cd site && bun run sync:content` (the prebuild copy step).
Assert these exist and are valid:
```bash
test -f site/static/skills/dbd/SKILL.md
test -f site/static/agents/dbd-pattern-verifier.md
test -f site/static/sensei.library.json
test -f site/static/.well-known/sensei.library.json
node -e "const m=require('./sensei.library.json'); for (const x of [...m.skills, ...m.agents]) { require('fs').accessSync(x.path); }" && echo "manifest paths all exist"
```
All must pass. (Optional: `bun run build` to confirm the full site build still succeeds.)

- [ ] **Step 5: Commit**

```bash
git add sensei.library.json site/scripts/copy-content.mjs site/.gitignore
git commit -m "feat(site): publish dbd skill + agent via sensei.library.json manifest + static site"
```

---

## Task 6: docs/llms currency

**Files:**
- Modify: `docs/guide/04-commands.md`, `docs/llms/llms.txt`, `docs/llms/llms-full.txt`

- [ ] **Step 1: Edit**

- `docs/guide/04-commands.md` `dbd inspect` section: note the advisory **`Suggestions:`** output (string-set CHECK → enum) and that it never affects validity/exit code.
- `docs/llms/llms.txt` + `llms-full.txt`: add a line to the `dbd inspect` entry about the enum Suggestions, and a short note that the `dbd` skill + `dbd-pattern-verifier` agent are published at `dbd.sensei-hq.com` / `/.well-known/sensei.library.json`.

- [ ] **Step 2: Commit**

```bash
git add docs/guide/04-commands.md docs/llms/llms.txt docs/llms/llms-full.txt
git commit -m "docs: document inspect enum suggestions + published skill/agent"
```

---

## Self-Review

**Spec coverage:** A (enum hint) → Tasks 1, 2; B1 (skill) → Task 3; B2 (agent) → Task 4; C (manifest+site) → Task 5; docs currency → Task 6. Pitfalls A1/A2/A4 handled in Task 1 (parse fallback, strict shape, cast-skip, negative tests); B3 release-state single rule shared by Task 3 + Task 4; B2 overreach caught by Task 4's clean-fixture gate; C1/C2/C3 by Task 5's gitignore + build-and-assert.

**Placeholder scan:** The sqlparser AST match in Task 1 intentionally defers the exact 0.61 variant nesting to Step 1's probe (documented, with a concrete fallback and passing negative tests as the safety net) — not an open TBD. Skill/agent bodies specify required content + a hard eval gate rather than final prose, appropriate for authored docs.

**Type consistency:** `EnumHint { entity, column, values }` used identically in Tasks 1, 2. `suggest_enum_candidates(entities, dialect)` signature matches its Task-2 call. Release-state rule (`project.released` OR baseline snapshot) is identical in Task 3 and Task 4.

**Verification gates:** Tasks 3 and 4 must NOT commit until their controller-run evals pass — these are the skill/agent "tests." Task 5 must not commit until the build-and-assert passes.
