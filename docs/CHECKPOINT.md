# Checkpoint

**Slice:** #7 Phase 2, slice 7 (`inspect --shared`, onboarding) on
`feat/ownership-inspect`, from `feat/ownership` (slices 1–6: PRs #50–#54).

## Done

- Slices 1–6 in `feat/ownership` (`953cdbe9`): registry, claim, uses; prune;
  uses guard; reset rules and write lock; name reuse and `--adopt-from`.
- Slice 7 implemented (`48c5dbdc`): `inspect --shared` (projects, owns, uses,
  conflicts; non-zero over a conflict); `init --from-db`/`merge` leave out other
  owners' entities, refuse while a project records no uses.
- Embedded 295/295, `cli_live` 59/59, workspace 2007/0, clippy, fmt, doc clean.

## Next

    gh pr create --base feat/ownership --head feat/ownership-inspect
    # review, fixes test-first, CI, local merge; slice 8: SQLite parity
    # (_dbd_objects/_dbd_uses, T18); then the docs slice

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
