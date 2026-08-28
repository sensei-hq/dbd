# Checkpoint

**Slice:** type/expression spelling convergence — every comparison dbd makes now
normalizes, and none of them emit the lossy form.

## Done

- `canonical_type` re-emits the `format_type` spelling: array suffix + base +
  modifier + time zone. Fixes `time`/`timestamp`, `timestamptz(3)`, `text[3]`,
  and all array types (which never reached the alias table).
- `sql_expr`: `BETWEEN` expansion (with a nested-`BETWEEN` guard — it was 2^k and
  hung every command), uniform array-cast **lift**, same-operator AND/OR
  flattening in **every** position.
- Index predicates + expression keys stored **as authored**; `normalize_index`
  canonicalizes a copy for matching.
- `dbd snapshot` normalizes both diff sites — `prepare_snapshot` and
  `prepare_multi_snapshot` (the one the CLI calls). Previously a parser-spelling
  difference produced a two-stage column rebuild.
- crates.io publish moved into `.github/workflows/release.yml`, tag-triggered,
  gated on the tagged tree, replayable via `workflow_dispatch`.

Released **v0.12.4** (tag + crates.io + main, CI green). `1d414d3` is the
snapshot fix, unreleased — next release carries it.

## Next

`git push origin develop && make bump` → v0.12.5 → merge develop → main.
The tag now triggers the publish workflow automatically.

## Known-broken / carried forward

- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
  Postgres emits those elements bare and dbd cannot resolve column types, so it
  fails safe rather than guessing.
- Pre-existing, unrelated: `generate_data_sql` warns "may truncate data" on a
  *widening* cast (varchar(30) → varchar(60)). The check keys off target category,
  not direction or length. Cosmetic; own slice.
