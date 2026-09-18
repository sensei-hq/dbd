# Checkpoint

**Slice:** v0.13.1 shipped — reconcile convergence fixes for #16 and #17.
Complete: tagged, published, merged to `main`, CI + CodeQL green, issues closed.

## Done

- **#16 — STORED generated columns.** Postgres keeps the `GENERATED ALWAYS AS
  (…) STORED` expression in `pg_attrdef`, where a plain DEFAULT lives, so
  introspection read it as one and planned `DROP DEFAULT` — which Postgres
  refuses, aborting every run. Now reads `attgenerated`; both parsers keep it on
  the new `ColumnDef::generated`. Changed → `SET EXPRESSION AS` (PG17+); removed
  → `DROP EXPRESSION`; the emitter renders the clause, so `reset`/`diff` stop
  recreating a computed column as a plain one.
- **#17 — varchar's implicit `::text` cast.** Canonicalization erases a `::text`
  cast only on a **binary-coercible** column — `varchar` alone (`pg_cast
  castmethod = 'b'`). `char(n)` excluded deliberately: its cast runs `rtrim1`
  and strips trailing spaces, so erasing it would change meaning. Partial-index
  predicates and `IN` lists fixed alongside CHECKs — same root cause.
- **rustls 0.23.44 → 0.23.45** (RUSTSEC-2026-0285). Advisory published
  2026-09-14; the pin predates v0.13.0, so `cargo audit` went red on the first
  push to `main` after it landed, not on our change. Lockfile only.

**Registry artifact verified:** `cargo install dbd-cli 0.13.1` from crates.io,
run against both repros — `diff --exit-code` = 0 and two consecutive reconciles
both `0 altered`, where 0.13.0 returns 2 and plans the DROP DEFAULT plus a
drop/re-add of three CHECKs.

## Next

Nothing in flight. Remaining open issues, both untouched and unscheduled:
**#11** (cover dbd-cli handlers needing a live connection — the crate-structure
question is decided: add a `[lib]` target) and **#7** (multi-tenant schema
isolation — still a design conversation, five candidate shapes).

## Open questions

None for this slice.

## Known-broken / carried forward

- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- `docs/design/architecture.md:360` still lists `is_identity: bool` on
  `ColumnDef`; that field is now `identity: Option<IdentityKind>` and the
  listing also predates `generated`. Pre-existing drift, not from this slice.
- `site/package.json` `overrides`: `dompurify` and `sharp` are now redundant.
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`, never
  compiled). Re-check on sqlx bumps.
