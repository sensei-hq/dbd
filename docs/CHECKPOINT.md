# Checkpoint

**Slice:** PRIMARY KEY replacement in reconcile — shipped as **v0.12.6**.

## Done

Released v0.12.6: tag pushed, crates.io published (`dbd-cli` + `dbd-core` both
at 0.12.6), develop merged to main, CI green on main.

The fix — a constraint's *matching key* must never reach SQL as an identifier:

- `ChangeAction::Drop` now carries the dropped object (symmetric with `Add`), so
  the emitter names a drop from the live constraint instead of from
  `field_name`, which is a match key and is synthetic (`pk:tenant_id,metric_id`)
  for anything the design left unnamed.
- `lift_pk_unique_keep_others` **keeps** the PK/UNIQUE name; `constraint_key`
  moved to columns-only so matching stays name-agnostic. Both sides explicitly
  named + different still reads as a deliberate rename.
- Unnamed PK/UNIQUE add drops the `CONSTRAINT` clause (was literally `unnamed`;
  a PK's backing index is schema-scoped, so the 2nd table in a schema collided).
- Constraint drops precede adds, keys walked sorted → deterministic SQL.
  `DROP CONSTRAINT/INDEX IF EXISTS` (a dropped column takes its PK with it).
- Unnamed-and-undroppable → comment + warning, never unrunnable SQL.
- Same fix reaches `dbd diff` (previewed the bad statement) and `dbd migrate`
  (wrote it to a migration file).
- Docs corrected: guide + llms-full claimed CHECK/indexes are not reconciled on
  existing tables; both have convergence passes.

Verified three ways: 1332 tests incl. embedded-Postgres e2e; mutation checks
(reverting either the naming or the ordering fix fails the e2e with the original
Postgres errors); and the **registry** artifact — `cargo install dbd-cli`
v0.12.6 replaced a PK on a table holding rows, rows preserved, converges.

## Next

Nothing pending. `develop` == `main` == v0.12.6.

## Open questions

- `dbd diff --json` shape changed: `"Drop"` → `{"Drop": {…}}`. No doc pins the
  field shape and a Drop now carries what was dropped, but it shipped in a patch
  release with no release note — the repo has no CHANGELOG to put one in.

## Known-broken / carried forward

- **No CHANGELOG exists.** The CLAUDE.md release checklist calls for one; four
  releases have shipped without it. Either add one or drop the checklist item.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- Pre-existing, unrelated: `generate_data_sql` warns "may truncate data" on a
  *widening* cast (varchar(30) → varchar(60)). Cosmetic; own slice.
