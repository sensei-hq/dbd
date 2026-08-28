# Checkpoint

**Slice:** `dbd diff` never reported "in sync" — live↔design spelling convergence.

## Done

- `canonical_type` rewritten: array suffix + base + modifier + time zone, re-emitted
  in the `format_type` spelling. Fixes `time`/`timestamp`, `timestamptz(3)`, and all
  array types (which never reached the alias table).
- `sql_expr`: `BETWEEN` expansion, uniform array-cast **lift**, and same-operator
  AND/OR flattening in **every** argument position (the adapter reads the pretty
  `pg_get_constraintdef`, which drops those parens wherever they sit).
- Index predicates + expression keys stored **as authored** by parser and adapter;
  `normalize_index` canonicalizes a copy for matching. The canonical form is lossy
  and was being emitted as DDL.
- Nested-`BETWEEN` guard: expansion duplicates operands, so nesting was 2^k and hung
  every command reading the design directory.
- Docs corrected on both surfaces; false snapshot-canonicalization claim in the
  parity gate corrected.

Commits: `94b082f` (code), `4b79792` (docs). Gate: 1273 tests / clippy -D warnings /
fmt all exit 0. Verified live: `dbd diff --scope default --exit-code` = 0, two-reconcile
convergence `0 altered` twice, array-cast partial index applies + diffs clean.

## Next

`git push origin develop && make bump` → v0.12.4 → merge develop → main.

## Known-broken / carried forward

- **`dbd snapshot` does not canonicalize types** (pre-existing, not this slice).
  `prepare_snapshot` diffs previous vs new snapshot raw, so a project holding
  snapshots from the old sqlparser backend gets a spurious
  `ALTER COLUMN … TYPE varchar(30)` on its next snapshot. Fix: run `canonical_type`
  over both sides in `prepare_snapshot`. Needs its own slice + migration thought.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift —
  Postgres emits the elements bare and dbd cannot resolve column types. Fails safe.
