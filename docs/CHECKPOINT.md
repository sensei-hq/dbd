# Checkpoint

**Slice:** Issue #12 (reconcile non-convergence) + Aikido security sweep. Both
complete on `develop`, not yet released.

## Done

Five commits, `ab28685`..`7876ca6`. Every one verified red-first; the two #12
fixes also verified against the **release binary** on local PostgreSQL 17 using
the issue's own repro.

- **#12.1 `NULLS NOT DISTINCT` dropped on ALTER** (`ab28685`). The clause had
  nowhere to live on `TableConstraint::Unique`, and `constraint_differs`
  compared UNIQUE by name alone — so reconcile applied a weaker constraint and
  `diff` then called it in sync. Carried through entity → both parsers →
  introspection (`pg_index.indnullsnotdistinct` via `to_jsonb`, PG<15-safe) →
  compare → generate/emit.
- **#12.2 enum value removal** (`fcb5436`). New `plan_enum_recreation`: drop
  managed dependent views (deepest first) → drop defaults → rename aside →
  `CREATE TYPE` → `ALTER COLUMN … USING ::text::new` → restore defaults → drop
  old. One implicit transaction, so every failure is total and named. Declines
  only on a dependent matview (dbd never auto-drops one) with the manual steps.
- **Security** (`57d75cc`, `7876ca6`). Both `export_data` impls joined a live
  catalog name into a path unchecked (`CREATE TABLE "../../x"` is legal);
  `path_safe` now contains them. `sql_quote` closes identifier/literal injection
  in both exports and in `classify::generate_data_sql`. `open::that` now only
  takes plain http(s).
- **Dependencies** (`6d06255`). `cargo audit` 4 vulns + 4 warnings → 0;
  `bun audit` 21 (7 high) → 0. CI now runs `cargo audit`.

## Next

Nothing pending on this slice. Open decision: whether to cut a release.
`develop` is 5 commits ahead of `main` at v0.12.6 — a release means
`make bump minor` (new `plan_enum_recreation` capability + a
`TableConstraint::Unique` variant field, both public API).

Next command: `git log --oneline main..develop` to review, then `make bump minor`.

## Open questions

- `TableConstraint::Unique` gained a field and `generate_data_sql` output is now
  quoted — both are breaking for embedders. Still no CHANGELOG to note it in.
- `dbd diff --json` shape changed back in v0.12.6 with no release note.

## Known-broken / carried forward

- **No CHANGELOG exists.** The CLAUDE.md release checklist calls for one.
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa`, via `sqlx-mysql`, never
  compiled — proved by `cargo tree` + zero build artifacts). Re-check on sqlx bumps.
- `site/package.json` `overrides` pin six transitive deps above their parents'
  ranges; drop each once the parent catches up.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
