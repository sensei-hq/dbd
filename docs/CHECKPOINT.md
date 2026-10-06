# Checkpoint

**Slice:** #7 multi-project isolation — design proposed, awaiting approval.
Spec: `docs/superpowers/specs/2026-10-06-multi-project-isolation-design.md`.

## Done

- v0.24.1 released and verified against the crates.io artifact: 0.24.0 pruned
  2 tables in the #40 repro (`orders` lost, FK gone), 0.24.1 pruned only `stray`.
- #40 closed (PR #43 → `main` `d71fdd6`, every check green).
- #7 design written: exclusive schema ownership in `dbd.ownership`, a core
  `preflight` on every mutating entry point, and ownership-aware reset.

## Remaining (once approved)

- Phase 1 (patch): roles and extensions leave `managed_schemas`; cron jobs
  tagged by project; heal folds only its own legacy rows; import staging moves
  into `dbd`, one table per project.
- Phase 2 (minor): `dbd.ownership`, `preflight`, reset guards, `init --from-db`
  and `merge` onboarding, `inspect --shared`, SQLite parity.
- Phase 3: docs, skills, site mirrors, `dbd-pattern-verifier`.

## Next

    # after approval: phase 1, test-first, starting with T1 (role/extension
    # no longer pulls `public` into managed_schemas) on a fix/ branch off develop

## Open questions

- The spec's three "Decisions to confirm": `public` is exclusive; name reuse
  is detected without a `project.id` field; a schema is released when the
  design drops it.

## Known-broken / upstream

- History has no views or routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
