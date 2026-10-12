# Checkpoint

**Slice:** #7 Phase 1 merged to `develop` (PR #49, `2cd4b683`); unreleased.
Next: Phase 2, entity ownership.

## Done

- Phase 1 of #7 (spec `docs/superpowers/specs/2026-10-06-multi-project-isolation-design.md`):
  - 1c heal: folds only its own legacy rows; layered reads; locks legacy tables
    (READ COMMITTED, migrations before meta, 5s timeout, one heal at a time);
    passes over tables the role cannot reach; refuses one holding its own row
    it cannot take; keeps an emptied table it cannot drop.
  - 1b cron: `dbd-refresh:<owner>:<schema>.<name>` (out of an older dbd's
    `dbd:refresh:%` sweep), ≤63 bytes, prefix measured by Postgres, legacy
    jobs replaced for declared views and unscheduled once their view is gone
    (catalog lookup, names as Postgres cuts them), syncs serialized and
    reading committed on the pool.
  - 1d import: `dbd."import_tmp:<project>"`, hashed past 63 bytes.
- Three review rounds, every finding fixed test-first; embedded 153/153,
  `cli_live` 41/41; `quinn-proto` 0.11.19 for RUSTSEC-2026-0338/0340/0343.
- Decisions settled (spec "Decisions"): per-module versions, `<project>/<module>`,
  one root manifest, no `provides:`; unowned entities in a shared schema prune
  only with `--prune-unowned`.

## Next

    # Phase 2, slice 1: dbd.objects/dbd.uses in LAYOUT_DDL; claim the whole
    # design's entities at apply/deploy/reconcile/reset, refusing one another
    # owner holds before any DDL; backfill existing unowned declared entities.

## Open

- Phase 1 is releasable as 0.25.2 when asked.
- Not covered by a test: a legacy table dropped between listing and reading
  (handled by construction: lock-or-skip under a savepoint, 42P01 = no rows).
- Declined: dropping the old shared `dbd.import_tmp` (an older co-tenant may be
  mid-import in it).

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
