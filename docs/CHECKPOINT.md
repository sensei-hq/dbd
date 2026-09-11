# Checkpoint

**Slice:** Issue #12 (reconcile non-convergence) + Aikido security sweep.

## Done

- **#12.1 — `NULLS NOT DISTINCT` dropped on the ALTER path.** Committed
  (`ab28685`). `TableConstraint::Unique` had nowhere to hold the clause, so it
  died at parse time: `apply` ran the DDL verbatim and kept it, `reconcile`
  re-derived a plain UNIQUE, and `constraint_differs` compared UNIQUE by name
  alone so `diff` reported "in sync" over a weaker constraint. Carried through
  entity → both parsers → introspection (`pg_index.indnullsnotdistinct` via
  `to_jsonb`, PG<15-safe) → compare → generate/emit. Two embedded-Postgres e2e
  tests, including the issue's duplicate-row repro and the reverse direction
  (design stops declaring it → constraint must be weakened).

## Next

1. `#12.2` — enum value removal. `generate_field_sql` emits nothing for an
   EnumValue drop (`diff/generate.rs:268`), so `plan_reconcile` skips the empty
   SQL and reports `0 altered` forever. **Decision taken: implement full type
   recreation** (drop dependent managed views → `ALTER TYPE RENAME` → `CREATE
   TYPE` → per-column `DROP DEFAULT` / `TYPE … USING ::text::new` / `SET
   DEFAULT` → `DROP TYPE` old; pass C re-applies views).
   Next command: `cargo test -p dbd-core --features embedded-tests --test embedded_test enum_value`
2. Path-traversal hardening (Aikido critical: `mod.rs`, `sqlite.rs` + 7 others).
3. `classify.rs` `generate_data_sql` — quote literals/identifiers.
4. Dependency advisories (Rust ~18, JS 4 in `site/`).

## Open questions

- `dbd diff --json` shape changed in v0.12.6 (`"Drop"` → `{"Drop": {…}}`) with
  no release note; repo still has no CHANGELOG.

## Known-broken / carried forward

- **No CHANGELOG exists.** The CLAUDE.md release checklist calls for one.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
