# dbd Pattern Verification + Enum Hints Design

**Date:** 2026-08-03  |  **Status:** Draft
**Scope:** Three related deliverables — (A) an advisory `dbd inspect` **Suggestions** section that flags string-set `CHECK` constraints as enum candidates; (B) a currency + workflow-clarity pass on the shipped **`dbd` skill** plus a new **`dbd-pattern-verifier` agent**; and (C) a **`sensei.library.json`** distribution manifest + static-site/GitHub publishing of the skill and agent, mirroring the Rokkit pattern. Includes an explicit pitfall analysis and an empirical verification strategy for the skill and agent (not just the Rust code).

---

## Overview & motivation

Two field problems motivate this:

1. **String-set CHECKs that should be enums.** A `CHECK (status IN ('active','inactive'))` reinvents a Postgres `ENUM` with none of the type-safety or introspection benefits. dbd can *notice* this and nudge — without forcing it.
2. **LLMs confuse pre-release vs upgrades.** The shipped `docs/skills/dbd/SKILL.md` mentions `reconcile` and `release` but gives no crisp *decision rule*, so assistants mix the pre-v1 `reconcile` (converge-in-place) workflow with the post-release `snapshot`→`apply` (migration/upgrade) workflow. The skill also predates materialized views (v0.9.x).

The fix is a small advisory feature (A) plus a documentation/tooling layer that is **verifiable** and **distributable** the way the Rokkit library ships its skills/agents (B, C).

### Non-goals

- **No auto-conversion** of CHECK → enum (`dbd doctor --fix` stays out of scope). Suggestion only.
- **No exit-code / validity impact** from suggestions — they never fail `inspect`.
- **No new Rust crate / CLI installer.** Distribution rides the existing static site + the `sensei.library.json` convention + raw GitHub. Installation is whatever the sensei tooling already does with that manifest.
- **Numeric/range/length CHECKs** are not enum candidates — strings only.

---

## Part A — enum suggestion in `dbd inspect`

### A.1 Detection (pure, in `dbd-core`)

New pure fn (in `design.rs`, beside `validate_materialized_views`):

```rust
pub struct EnumHint {
    pub entity: String,   // qualified table name, e.g. "config.lookups"
    pub column: String,   // the constrained column
    pub values: Vec<String>, // the literal string set, in source order
}

/// Flag single-column, string-literal-set CHECK constraints as enum candidates.
/// Postgres/Supabase only (SQLite/Convex have no enum type). Advisory — never an error.
pub fn suggest_enum_candidates(entities: &[Entity], dialect: &str) -> Vec<EnumHint>;
```

Detection, per `TableConstraint::Check { expression, .. }` on a `Table` entity:

- Re-parse `expression` with sqlparser (`Parser::parse_expr` under `PostgreSqlDialect`). The stored string is itself sqlparser output (`chk.expr.to_string()`), so it round-trips reliably. Regex fallback (`^\s*"?(\w+)"?\s+in\s*\(\s*'…'(\s*,\s*'…')*\s*\)\s*$`) only if parse fails.
- Match these AST shapes, all requiring a **single column identifier** on the left and **all-string-literal** members:
  - `Expr::InList { expr: Identifier|CompoundIdentifier, list: [Value::SingleQuotedString, …], negated: false }`
  - `col = ANY(ARRAY['a','b',…])` — `Expr::AnyOp`/`BinaryOp` over an `Array` of `SingleQuotedString`.
  - An `OR`-chain of `col = 'lit'` where every leaf is the **same** column `=` a string literal.
- **Reject** (no hint): subqueries (`IN (SELECT …)`), mixed literal/non-literal lists, multi-column expressions, numeric/bool literals, casts on the column (`(status)::text IN …` → skip in v1), negated `NOT IN`.
- One `EnumHint` per qualifying CHECK; dedupe identical (entity,column).

Gating: caller passes `design.config().source.dialect`; emit hints only when it is `"postgresql"`.

### A.2 Surface (CLI)

`cmd_inspect` (`commands/schema.rs`) calls `suggest_enum_candidates(design.entities(), &design.config().source.dialect)` and, if non-empty, prints a dedicated **`Suggestions:`** section via a `print_enum_hints` helper (mirrors the existing `print_matview_errors`). Advisory tone, **not** counted in `output::summary`'s error count and **not** affecting exit code:

```
  Suggestions:
    config.lookups.status: CHECK constrains to a fixed string set {'active','inactive'}
      — a Postgres enum (ddl/enum/config/status.ddl) gives type safety + cleaner
      introspection. Not required.
```

### A.3 Tests

Pure unit tests on `suggest_enum_candidates`: `IN`-list of strings → hint (values preserved, order preserved); `= ANY(ARRAY[...])` of strings → hint; `col='a' OR col='b'` → hint; **no** hint for: numeric `IN (1,2)`, range `x > 0 AND x < 10`, length `char_length(x) < 5`, `IN (SELECT …)`, mixed `IN ('a', other_col)`, multi-column, `NOT IN`, and dialect `"sqlite"`. Plus a CLI test that `print_enum_hints` renders the line and that inspect's exit/validity is unaffected.

---

## Part B — skill currency + verifier agent

### B.1 `docs/skills/dbd/SKILL.md` updates

- **Currency:** add `materialized_view` to the type list; add `dbd refresh` to the command map; add a one-line matview rule (idempotent `CREATE MATERIALIZED VIEW IF NOT EXISTS`; scheduled refresh via pg_cron under `materialized_views:`; reconcile **warns** on drift, never auto-drops).
- **New section — "Workflow: pre-release vs upgrades" (the headline fix).** A decision rule an assistant can't misread:
  - **Which am I in?** Released ⇔ `project.released: true` in `design.yaml` **or** `snapshots/` contains snapshot files. (Concrete, file-checkable signals.)
  - **Pre-release** (not released, schema churning): iterate DDL freely; `dbd reconcile` converges the live dev DB in place (no snapshots, no version bump); fresh DB → `dbd apply`. `reconcile --allow-destructive` / `--prune` for drops.
  - **Released** (upgrades): schema changes are **migrations** → edit DDL → `dbd snapshot` (generates the ALTER migration) → `dbd apply` (runs it). `reconcile` is **disabled** and must not be used. Never hand-edit the live DB.
  - A 2-row table + one worked example per side.
- **New section — "Self-check before you touch a dbd project"** (a checklist the assistant self-applies): correct singular type folder; idempotent DDL (`… IF NOT EXISTS` / `CREATE OR REPLACE VIEW` / matview `IF NOT EXISTS`); secrets via `$ENV_VAR`; right workflow for the release state; string-set CHECK → consider enum; matview drift is warn-only; `inspect` does not check column-level refs (they fail at `apply`).

### B.2 `docs/agents/dbd-pattern-verifier.md`

A reviewer subagent (Rokkit-`rokkit-styles-reviewer` frontmatter style: `name`, `description` with two `<example>` blocks, `tools: Read, Grep, Glob, Bash`, `model: sonnet`, `color`). It reviews a **dbd project or a proposed change** and reports violations, most-severe first:

1. **Wrong workflow for the release state** (headline): `reconcile` used/advised on a released project, or hand-written migrations on a pre-release one. Determines release state from `design.yaml` `project.released` + `snapshots/`.
2. **Non-idempotent DDL** — a `CREATE TABLE`/`MATERIALIZED VIEW`/`INDEX` without `IF NOT EXISTS`, or a view without `OR REPLACE`.
3. **Hardcoded secrets** in `design.yaml` target URLs (not `$ENV`).
4. **Wrong/plural type folders** (`ddl/tables/` etc.), wrong schema nesting.
5. **String-set CHECK → enum** candidate (same rule as Part A).
6. **Matview drift misuse** (expecting reconcile to recreate; unstamped matviews).
7. Optional: runs `dbd inspect`/`dbd diff` **if the binary is present** (never required; the agent works purely from files otherwise).

The agent cites the `dbd` skill for the canonical rules rather than restating them, and only reports **evidence-backed** findings (file:line).

### B.3 Distribution — `sensei.library.json` + static site + GitHub

- **Root `sensei.library.json`** (mirrors Rokkit):
  ```json
  {
    "library": "dbd",
    "version": ">=0.10",
    "repo": "https://github.com/sensei-hq/dbd",
    "branch": "main",
    "site": "https://dbd.sensei-hq.com",
    "skills": [
      { "name": "dbd", "focus": "schema-as-code",
        "path": "docs/skills/dbd/SKILL.md", "url": "/skills/dbd/SKILL.md" }
    ],
    "agents": [
      { "name": "dbd-pattern-verifier", "focus": "pattern-review",
        "path": "docs/agents/dbd-pattern-verifier.md", "url": "/agents/dbd-pattern-verifier.md" }
    ]
  }
  ```
- **Static-site publishing:** extend `site/scripts/copy-content.mjs` (runs on `predev`/`prebuild`) to also copy, into the site's **`static/`** (served as raw files):
  - `docs/skills/**` → `static/skills/**`  →  `…/skills/dbd/SKILL.md`
  - `docs/agents/*.md` → `static/agents/*.md`  →  `…/agents/dbd-pattern-verifier.md`
  - root `sensei.library.json` → `static/sensei.library.json` **and** `static/.well-known/sensei.library.json` (discovery endpoint)
- **Gitignore the generated copies** (`site/static/skills`, `site/static/agents`, `site/static/sensei.library.json`, `site/static/.well-known/`) — canonical source stays in `docs/` + root manifest, exactly as `src/lib/content/` is already generated + ignored.
- **GitHub distribution** falls out of the manifest's `repo`+`branch`+`path` (raw file URLs).

---

## Pitfall analysis (pre-implementation)

**A1 — CHECK-expression parse fragility.** The stored expression is normalized sqlparser output, so `parse_expr` round-trips; but introspected CHECKs (not this path — inspect is design-side) or exotic expressions could fail. *Mitigation:* regex fallback for the `IN ('…')` shape; on any parse failure, emit **no** hint (never crash, never false-positive).

**A2 — False positives / noise.** Over-suggesting erodes trust. *Mitigation:* strict shape match (single column, all string literals, non-negated); reject subqueries/mixed/casts/numeric; one hint per CHECK; advisory section is visually separate and clearly "Not required." A table with legitimately many string-set CHECKs could still be noisy — acceptable for v1 (opt-in mental model; could add a `--no-suggestions` later, out of scope).

**A3 — Dialect gating.** Gate on `source.dialect == "postgresql"`. A Postgres-source project targeting SQLite is a rare edge; source dialect is the right signal (enums are a source-schema concept). *Mitigation:* documented; SQLite/Convex users see nothing.

**A4 — Column extraction with casts.** `(status)::text IN (…)` has a cast, not a bare identifier. v1 skips (no hint) rather than mis-attribute the column. Documented limitation.

**B1 — Skill accuracy drift.** A skill that ships wrong guidance is worse than none. *Mitigation:* the release-state signal is concrete (file-checkable); the skill is verified empirically (below) and re-synced from a single source (`docs/`).

**B2 — Agent false positives / overreach.** An over-eager verifier that flags valid patterns trains users to ignore it. *Mitigation:* verified against a **clean** fixture (must produce zero findings) as well as a planted-violation fixture (must catch each); findings must be file:line evidence-backed; the agent cites the skill rather than inventing rules.

**B3 — Release-state detection ambiguity (skill & agent).** `project.released` may be absent (pre-release default) and `snapshots/` may exist without release. *Mitigation:* rule = released iff `project.released: true` **or** a baseline snapshot exists; both the skill and agent use the identical rule (single definition, cross-referenced).

**C1 — SvelteKit static serving & route collisions.** Files in `static/` serve at root; confirm no existing `/skills` or `/agents` route collides (there is none today). `.well-known/` serves fine. *Mitigation:* build-and-fetch verification (below).

**C2 — Generated-vs-committed confusion.** If the `static/` copies were committed they'd drift from `docs/`. *Mitigation:* gitignore them (matches the existing `src/lib/content/` convention); the manifest `path` points at the committed `docs/` source, `url` at the generated static path.

**C3 — Manifest/site/GitHub drift.** `sensei.library.json` `path`/`url` must match real files, and `branch: main` must be where consumers fetch. *Mitigation:* a verification step asserts every `path` exists in-repo and every `url` resolves in the built site; `branch` is `main` (the release branch), consistent with the tag flow.

**C4 — Vercel/CI build.** The copy step runs in `prebuild`; the Vercel build must run it. *Mitigation:* it's wired into the existing `prebuild` that already copies llms/guide, so it runs wherever the site builds. Verified locally via `bun run sync:content`.

---

## Verification strategy (how each piece is proven, not asserted)

**A — Rust code:** `cargo test -p dbd-core suggest_enum_candidates` (the positive + negative + dialect cases above) and a `dbd-cli` test for the rendered `Suggestions:` line + unaffected exit code. Plus a manual run of the real `dbd inspect` against a fixture project containing a string-set CHECK, confirming the suggestion prints and validity is unchanged.

**B1 — Skill (empirical eval):** Author 3–4 scenario prompts and, in a fresh subagent given **only** `SKILL.md`, confirm it answers correctly:
- "The project has `project.released: true`. I need to add a `notes` column. What do I do?" → must say edit DDL → `dbd snapshot` → `dbd apply`; must **not** say `reconcile`.
- "Fresh project, no snapshots, still iterating. Deployed schema drifted from my DDL. How do I sync?" → `dbd reconcile` (pre-release) is correct.
- "How do I change a materialized view's definition that's already deployed?" → drop + re-apply / reconcile warns; must **not** claim reconcile auto-recreates.
- "Where does `ddl/tables/orders.sql` belong?" → singular `ddl/table/<schema>/orders.ddl`.
Pass = correct answer on each. Also run the repo's `skill-reviewer`/`writing-skills` checks for description-triggering quality.

**B2 — Agent (empirical eval against fixtures):** Create two throwaway fixture projects under a temp dir:
- **`bad/`** with planted violations: a hardcoded password in `design.yaml`, `ddl/tables/` (plural), a `CREATE TABLE` without `IF NOT EXISTS`, a `CHECK (status IN ('a','b'))`, and `project.released: true` alongside a note that "reconcile" was used. Run the agent → assert it flags each planted issue (by category).
- **`good/`** a clean, conformant project → assert the agent returns **zero** findings (no false positives).
This is the core proof the agent works. (Because a subagent can't spawn a subagent, this eval is run by the top-level session, not from inside the implementer.)

**C — Distribution:** run `cd site && bun run sync:content` (or `bun run build`) and assert the generated files exist and are valid:
- `site/static/skills/dbd/SKILL.md`, `site/static/agents/dbd-pattern-verifier.md`, `site/static/sensei.library.json`, `site/static/.well-known/sensei.library.json` all present;
- `sensei.library.json` parses as JSON, and every `path` exists in-repo;
- (optional) a `vite preview` fetch of `/skills/dbd/SKILL.md` + `/.well-known/sensei.library.json` returns 200.

---

## Touchpoints

- `crates/dbd-core/src/design.rs` — `EnumHint`, `suggest_enum_candidates`, unit tests.
- `crates/dbd-cli/src/commands/schema.rs` — `print_enum_hints`, wire into `cmd_inspect`.
- `docs/skills/dbd/SKILL.md` — currency + workflow decision rule + self-check checklist.
- `docs/agents/dbd-pattern-verifier.md` — new agent (Rokkit-style).
- `sensei.library.json` — new root manifest.
- `site/scripts/copy-content.mjs` — copy skills/agents/manifest into `static/` (+ `.well-known/`).
- `site/.gitignore` — ignore the generated `static/{skills,agents,sensei.library.json,.well-known}`.
- `docs/guide/04-commands.md` + `docs/llms/*` — note the `inspect` Suggestions section; mention the skill/agent availability.

## Rollout & version

Branch `feat/dbd-patterns-skill-and-enum-hints` off `develop`. Order: A (code, subagent+review) → B1 skill (+eval) → B2 agent (+fixture eval) → C manifest+site (+build check) → docs/llms currency. Cut **`v0.10.0`** (new `inspect` capability + new skill/agent + distribution = a minor), then merge `develop → main` per the repo flow.
