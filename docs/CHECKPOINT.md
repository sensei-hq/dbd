# Checkpoint

**Slice:** #7 Phase 2, slice 6 (name reuse + transfer) on `feat/ownership-transfer`,
from `feat/ownership` (slices 1–5: PRs #50–#53). Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–5 in `feat/ownership` (`91f00335`): registry, claim, uses; prune by
  ownership; the uses guard; reset rules and the dbd write lock.

## Next

    # slice 6: a design under an owner name that holds entities here, declaring
    # none of them, is refused at claim time (--allow-ownership-change
    # overrides); --adopt-from <owner> takes over, printed, what the design
    # declares that <owner> holds — the giver's next run neither owns nor
    # prunes it. Red tests first (spec T14, T15).

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Docs slice (end of Phase 2) must cover `--prune-unowned`, reset's kept
  lines and the write lock's waits in the guides.
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
