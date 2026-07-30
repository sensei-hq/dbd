# `dbd diff` — design spec

**Date:** 2026-07-30
**Status:** Approved (pending spec review)
**Author:** brainstormed with Claude Code

## Problem

`dbd reconcile --dry-run` previews the drift between the live database and the
design, but its plan is deliberately incomplete: reconcile's `canonicalize()`
(see `crates/dbd-core/src/reconcile.rs:107-144`) **strips foreign keys, CHECK
constraints, indexes, and column comments** before diffing, because the parsed
(desired) and introspected (live) representations of those diverge too much to
compare reliably in an *apply* path.

The result: users who want to know exactly how the live DB differs from the
design — including FK/CHECK/index/comment drift — have to compute it by hand.
(Real report: "I computed the column-level diff against prod; dbd won't emit
it.")

`dbd diff` fills that gap: a **read-only** command that shows the *complete*
difference between the live database and the design.

## Goals

- Read-only. Never writes, never applies, never opens a write transaction.
- Show everything `reconcile --dry-run` shows **plus** FK / CHECK / index /
  comment changes.
- Available always, including after `dbd release` (unlike reconcile, which is
  disabled once `project.released`).
- Machine-readable output for CI/tooling (`--json`) and a CI drift gate
  (`--exit-code`).

## Non-goals (v1 scope boundaries — YAGNI)

- **Entity types beyond tables + enums.** Views, functions, procedures, roles,
  sequences, extensions, schemas are NOT structurally diffed — the snapshot
  model (`Snapshot { tables, enums }`) doesn't represent them for diffing.
  Documented as a known limitation; candidate for a later iteration.
- **Snapshot↔snapshot / design↔snapshot modes.** `dbd diff` compares the live
  DB against the design only. Migration preview between snapshots is deferred.

## Command surface

```
dbd diff [-d <url>] [-s <dir>] [-e <env>] [-c <config>] \
         [--scope <name>] [--deps report|include] [--json] [--exit-code]
```

- No `--allow-destructive` / `--prune`: it never executes, so there is nothing
  to gate.
- **Not** gated on `project.released` — diff is always available.
- `--scope` / `--deps`: honored like the other scope-aware commands
  (gap-gated), consistent with `inspect`/`apply`/`reconcile`.
- `--json`: emit a structured `SchemaDiff` document instead of the human report.
- `--exit-code`: terraform-`plan`-style exit semantics —
  - `0` — live DB matches the design (no differences)
  - `2` — differences present
  - `1` — reserved for genuine errors (no connection, parse failure, …)

  Without `--exit-code`, the command always exits `0` (informational).

## Data flow

1. Load the (scoped) design → desired entities.
2. `adapter.introspect()` → live entities (all types; only tables+enums used).
3. `snapshot_from_entities()` on both sides (existing fn — tables + enums).
4. Apply the **new** `normalize_for_diff()` to both snapshots (see below),
   instead of reconcile's `canonicalize()`.
5. `diff::diff(live, desired)` → `Vec<MigrationDiff>` (columns, PK, unique, FK,
   CHECK, indexes, enum values — the full engine already compares all of these;
   see `crates/dbd-core/src/diff/compare.rs`).
6. `generate_migration_sql()` + `migration_warnings()`.
7. Wrap results in a serializable `SchemaDiff`; render human or JSON.

Everything except step 4 is reuse.

## The normalization split (the core work)

Reconcile's `canonicalize()` today does two jobs at once:

1. **Normalize representation** — lift inline PK/unique into constraints,
   normalize column type spellings (`int4`→`integer`), strip trailing default
   casts (`'{}'::text[]`→`'{}'`), qualify bare enum types, clear inline flags.
   *(These prevent false positives.)*
2. **Strip the hard attributes** — clear indexes, drop FK/CHECK constraints,
   clear column comments. *(These are the "covers all" gap.)*

Refactor: extract job 1 into a shared `normalize_common()`. Reconcile's
`canonicalize()` becomes `normalize_common()` + its existing stripping —
**behavior unchanged; existing reconcile tests must stay green.**

New `normalize_for_diff()` = `normalize_common()` + *normalize instead of
strip*:

- **Foreign keys** — normalize action keywords (`NO ACTION` ≡ absent/default);
  compare `(columns, ref_table, ref_columns, on_delete, on_update)`.
- **CHECK constraints** — canonicalize the expression with `pg_query`
  (libpg_query, already a dependency): parse both sides and compare their
  normalized/deparsed form. Fall back to a paren/whitespace-normalized text
  compare. If an expression cannot be parsed at all, still surface the
  constraint but tag the diff **advisory** (see below).
- **Indexes** — normalize `(columns, method, unique, predicate)`; suppress
  PK/unique-backing indexes that merely restate a constraint introspection
  reports (avoid phantom index diffs).
- **Comments** — direct text compare (already plain text on both sides).

### Advisories

Best-effort normalization can't guarantee zero false positives on exotic
Postgres output. Rather than hide those, `dbd diff` surfaces them and labels
them `advisory` so the user knows to verify manually — honest over silent.

## Result type

`SchemaDiff` in dbd-core (new `schema_diff.rs`, or folded into the `diff`
module), `#[derive(Serialize)]`:

- `changes: Vec<MigrationDiff>` (or a CLI-friendly projection)
- `sql: String` — the generated DDL that would converge the DB to the design
- `warnings: Vec<String>` — from `migration_warnings`
- `advisories: Vec<String>` — best-effort normalization notes
- `is_empty()` — no changes

## Rendering

Human report reuses reconcile's line style (`~ alter`, `+ create`, indented
SQL) but shows the SQL by default (this command's whole point) and adds
advisory lines:

```
  ~ alter  public.users
      ALTER TABLE public.users ALTER COLUMN email TYPE citext;
      ALTER TABLE public.users ADD CONSTRAINT users_org_fk FOREIGN KEY (org_id) REFERENCES org(id);
  + create public.audit_log
  ⚠ advisory: CHECK on public.orders couldn't be normalized — shown as changed; verify manually
```

Rendering goes through a pure `diff_report_lines()` seam (mirroring
`reconcile_plan_lines`) so it is unit-testable without a database.

`--json` serializes `SchemaDiff` directly.

## Placement

- **dbd-cli:** `Commands::Diff { json, exit_code, .. }` in `cli.rs`; a new
  `commands/diff.rs` module with `cmd_diff` (project.rs is already large).
- **dbd-core:** `SchemaDiff` + `normalize_for_diff()` + `normalize_common()`
  refactor. `Design::diff_live(adapter, scope) -> SchemaDiff` (read-only) wiring
  introspection + design, mirroring how `Design::reconcile` is structured.

## Error handling

- No DB connection / introspection failure → error (exit `1`).
- Design load/parse errors → surfaced like other commands.
- Dependency gap under a `--scope`/`deps: report` → same gap error as the other
  scope-aware commands.

## Testing (TDD)

1. **dbd-core normalization unit tests** (mirror
   `reconcile::tests::canonicalize_reconciles_parsed_vs_introspected`): for each
   of FK / CHECK / index / comment, a parsed-desired and introspected-live form
   of the *same* object reconcile to **no diff**, and a genuine change is still
   detected. CHECK tests exercise the `pg_query` path and the unparseable
   advisory fallback.
2. **Backing-index suppression** test: a PK/unique constraint's implicit index
   does not produce a phantom index diff.
3. **CLI rendering tests**: `diff_report_lines` (human) and the `--json`
   serialization shape, using a constructed `SchemaDiff` (no DB) — mirrors the
   `reconcile_plan_*` tests.
4. **Read-only integration test** (embedded/sqlite, mirror
   `reconcile_dry_run_is_read_only`): a drifted DB yields the expected diff, an
   in-sync DB yields empty, and diff never writes.
5. **Exit-code behavior**: `--exit-code` returns 0 / 2 as specified.
6. **Reconcile regression**: existing reconcile canonicalize tests stay green
   after the `normalize_common()` extraction.

## Delivery

Follows the repo release flow: implement on `develop` with TDD, keep the
pipeline green (`cargo test --workspace` + `cargo clippy -- -D warnings`),
commit, `make bump` (patch), merge `develop → main`. Docs: add a `dbd diff`
section to `docs/guide/04-commands.md` and `docs/llms/llms-full.txt`, and a
one-line entry to the terse `docs/llms/llms.txt`.
