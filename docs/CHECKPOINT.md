# Checkpoint

**Slice:** patch 0.27.2 — #73, #60, #72 merged to `develop` (PRs #76–#78); the
adversarial review's fixes on `fix/review-0.27.2` (PR #79). Awaiting release
sign-off.

## Done

- #73, #60, #72 on `develop` (6b17d98b).
- Review (6 reviewers, approve depth) → `fix/review-0.27.2`, every crit/high/
  medium fixed red-first:
  - G3 released rename/type change in place (RENAME COLUMN; ALTER … TYPE …
    USING; un-castable = data.sql TODO + in-place alter); narrowing refuses a
    too-long value; generated type change; pre-0.27.2 lossy swap refused.
  - G4 column turned generated: every key/constraint/FK/index/comment back,
    reconcile same run, diff shows it, history does not.
  - G1 nextval fully qualified (live via pg_depend; design keeps public).
  - G2 pathless file resets the path (batch too); SET SCHEMA resets the pool.
  - G5 live names quoted in DROP; G6 #73 schema-less table.
- Gate: embedded 361/361, cli_live 82/82, clippy -D warnings (embedded), fmt;
  sensei copy converges; type-change probe keeps every value.

## Next

On sign-off: merge PR #79 locally into `develop`, push; release checklist
(CHANGELOG [Unreleased] → [0.27.2], `make bump`, main merge, CI green,
install and re-run the repros). Close #60/#72/#73 when `main` has them.

## Open

- Low, reported: `diff` omits the FK holder drop/re-add reconcile runs;
  dotted schema names in `schema.table` entity names.
- Unfiled (from before): `init --dry-run` writes files; SQLite scopes cannot
  name a table; a dry run with `--allow-destructive` still says to re-run
  with it; SQLite `deploy --module shared` fails on a bare FK error.
- Scratch to delete (rm -rf was refused): /tmp/dbd-* (0271, 60, 65, 72
  logs/dirs), /tmp/dbd-review-0272, /tmp/dbd-review-target*,
  /tmp/dbd-spec-audit-* and a stopped cluster /tmp/dbd-spec-audit-pg-HIoE.
