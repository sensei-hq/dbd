# Checkpoint

**Slice:** Reconcile convergence fixes for GitHub #16 and #17, on `develop`
(`921370e`). Both filed 2026-09-17; together they made `reconcile` unusable.

## Done

One commit, all gates green: full workspace suite, `clippy -D warnings`, `fmt`,
and 50/50 embedded-Postgres tests (49 pre-existing + 1 new convergence test).

- **#16 — STORED generated columns.** Postgres keeps the `GENERATED ALWAYS AS
  (…) STORED` expression in `pg_attrdef`, where a plain DEFAULT lives, so
  introspection read it as one and planned `DROP DEFAULT` — which Postgres
  refuses, aborting every run. Now reads `attgenerated`; both parsers keep it on
  the new `ColumnDef::generated`. Changed expression → `SET EXPRESSION AS`
  (PG17+); removed → `DROP EXPRESSION`; the emitter renders the clause so
  `reset`/`diff` stop recreating a computed column as a plain one.
- **#17 — varchar's implicit `::text` cast.** Canonicalization now erases a
  `::text` cast on a **binary-coercible** column — `varchar` only (`pg_cast
  castmethod = 'b'`, a runtime no-op). `char(n)` excluded on purpose: its cast
  runs `rtrim1` and strips trailing spaces, so erasing it changes meaning.
  Partial-index predicates fixed alongside CHECKs — same root cause.

**Evidence, not assumption:** on a fixture carrying both repros, dbd 0.13.0 gives
`diff --exit-code` = 2 and plans the DROP DEFAULT plus a drop/re-add of all three
varchar CHECKs; the built binary gives 0, and two consecutive reconciles both
report `0 altered`.

## Next

Decide the release. The Unreleased section no longer describes a byte-identical
Rust tree, so it warrants a **0.13.1** patch rather than merging unversioned.
Then PR `develop` → `main`, confirm CI green, close #16/#17 on merge.

## Open questions

Cut 0.13.1 now or batch with more fixes? The CHANGELOG preamble was rewritten to
say a patch is warranted; **no version bump has been applied yet.**

## Known-broken / carried forward

- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- #11 (CLI live-connection coverage) and #7 (multi-tenant isolation) still open;
  #11's crate-structure question is decided — add a `[lib]` target.
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`, never
  compiled). Re-check on sqlx bumps.
- `site/package.json` `overrides`: `dompurify` and `sharp` are now redundant.
