# Checkpoint

**Slice:** PRIMARY KEY replacement in reconcile — a constraint's *matching key*
must never reach SQL as an identifier.

## Done

- `ChangeAction::Drop` now carries the dropped object (symmetric with `Add`), so
  the emitter names a drop from the live constraint instead of from
  `field_name`, which is a match key and is synthetic (`pk:tenant_id,metric_id`)
  for anything the design left unnamed.
- `lift_pk_unique_keep_others` **keeps** the PK/UNIQUE name; `constraint_key`
  moves to columns-only so matching stays name-agnostic. Both sides explicitly
  named + different still reads as a deliberate rename.
- Unnamed PK/UNIQUE add drops the `CONSTRAINT` clause (was literally `unnamed`;
  the PK's backing index is schema-scoped, so the 2nd table collided).
- Constraint drops now precede adds, keys walked sorted → deterministic SQL.
  `DROP CONSTRAINT/INDEX IF EXISTS` (a dropped column takes its PK with it).
- Unnamed-and-undroppable → comment + warning, never unrunnable SQL.
- Same fix reaches `dbd diff` (previewed the bad statement) and `dbd migrate`
  (wrote it to a migration file).
- Docs: guide + llms-full corrected — they claimed CHECK/indexes are not
  reconciled on existing tables; both have convergence passes.

`cf8cb45`, unreleased on `develop`. 1332 tests green incl. embedded-Postgres
e2e; clippy + fmt clean. Mutation-checked: reverting either the naming or the
ordering fix fails the e2e with the original Postgres errors.

## Next

`git push origin develop`, then decide whether this rides the next `make bump`.

## Open questions

- `dbd diff --json` shape changed: `"Drop"` → `{"Drop": {…}}`. No doc pins the
  field shape, and a Drop now carries what was dropped, but it is a structured
  output change worth a release note.

## Known-broken / carried forward

- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- Pre-existing, unrelated: `generate_data_sql` warns "may truncate data" on a
  *widening* cast (varchar(30) → varchar(60)). Cosmetic; own slice.
