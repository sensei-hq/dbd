# Checkpoint

**Slice:** Issue #12 + the Aikido security sweep — shipped as **v0.13.0**.

## Done

Released v0.13.0: tagged on `develop`, merged to `main` (merge commit `c3653d2`),
release workflow green, `dbd-core` and `dbd-cli` both published at 0.13.0, CI
green on `main` including the new `cargo audit` step.

**Minor, not patch.** `TableConstraint::Unique` gained a field, which is
compile-breaking for embedders, and on a `0.x` crate minor is the breaking
position — a patch would have told `dbd-core = "0.12"` consumers it was a safe
auto-upgrade. `generate_data_sql`'s output is now quoted, same reason.

- **#12.1 `unique nulls not distinct` reduced to a plain unique on the ALTER
  path.** The clause had nowhere to live on `TableConstraint::Unique`, and
  UNIQUE was compared by name alone — so reconcile applied a weaker constraint
  and `diff` then certified it in sync. Carried through entity → both parsers →
  introspection → compare → emit.
- **#12.2 enum value removal never applied.** `plan_enum_recreation` performs
  the type swap: drop managed dependent views (deepest first) → drop defaults →
  rename aside → `CREATE TYPE` → `ALTER COLUMN … USING ::text::` → restore
  defaults → drop old. One transaction, so every failure is total and named.
  Declines only on a dependent matview, with the manual steps.
- **Security.** Both `export_data` impls joined a live catalog name into a path
  unchecked; `path_safe` now contains them (the rule previously existed as two
  private copies covering two of four sites). `sql_quote` closes
  identifier/literal injection in both exports and `generate_data_sql`.
  `open::that` takes only plain http(s). `github.rs`'s same-named-but-stricter
  helper renamed to `is_safe_github_ident` so the two rules can't be confused.
- **Dependencies.** `cargo audit` 4+4 → 0; `bun audit` 21 → 0.

Verified four ways: 1013 lib + 49 embedded-Postgres + 196 integration tests;
mutation checks (removing either #12 fix fails its e2e with the original
symptom); the **registry artifact** — `cargo install dbd-cli@0.13.0` re-runs
both halves of the issue's repro correctly; and CHANGELOG + docs + skill (all
tracked copies byte-identical) synced.

## Next

Nothing pending. `develop` == v0.13.0, `main` merged, released.

## Open questions

None blocking.

## Known-broken / carried forward

- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa`, via `sqlx-mysql`, never
  compiled — proved by `cargo tree` + zero build artifacts). Re-check on sqlx bumps.
- `site/package.json` `overrides` pin six transitive deps above their parents'
  ranges; drop each once the parent catches up.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- Local leftovers from verification: databases `dbd_repro12` and
  `dbd_v13_verify` (drop when convenient).
