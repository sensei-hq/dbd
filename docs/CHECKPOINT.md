# Checkpoint

**Slice:** the open issues — #73, #60, #72 — each on its own branch off
`develop`, red tests first. Nothing pushed, no PRs yet.

## Done

- #73 on `fix/73-drop-index-schema`: a migration's `DROP INDEX` names the
  table's schema. Red f342c2e8, fix 0b016139, CHANGELOG 408511b0. Embedded
  suite 339/339 (one run saw a load-only failure in
  `reconcile_changes_a_unique_index_a_foreign_key_rests_on`; 15/15 alone and
  the rerun green — message not captured).
- #60 on `fix/60-nextval-spelling`: a `nextval` default's sequence spelled as
  Postgres prints it; a bare one in the schema its file's path finds it in;
  a script's `SET search_path` reset on the pooled connection after it, and
  before a batch commits. Red 9a1ecc7e + 84802081, fixes d484da8d +
  24e781d8, docs 77819586. Embedded 340/340; CLI: 0.27.1 drifts (`diff
  --exit-code` 2), the branch exits 0.

## Next

#72 on `fix/72-rebuilt-column-keys` from `develop`: a column rebuilt (turned
generated; a migration's type swap) gets back its PRIMARY KEY/UNIQUE, the
foreign keys holding them dropped first and re-added after.
`cargo test -p dbd-core --features embedded-tests --test embedded_test`

## Open

- To ask: push the three branches, open PRs, merge to `develop`.
- Probe after #72: a rebuild likely also loses NOT NULL, default, indexes,
  CHECKs and outgoing FKs (stage 1 adds `<col>_new` with its type only).
- Two-reconcile convergence check on a schema-only sensei copy, once all
  three are merged.
- Unfiled (from before): `init --dry-run` writes files; SQLite scopes cannot
  name a table; a dry run with `--allow-destructive` still says to re-run
  with it; SQLite `deploy --module shared` fails on a bare FK error.
- Scratch to delete: /tmp/dbd-60-cli, /tmp/dbd-60-*.log, /tmp/dbd-emb-run2.log
  and the older /tmp/dbd-* dirs listed at 0.27.1.
