# Checkpoint

**Slice:** #7 Phase 2, slice 8 (SQLite parity) on `feat/ownership-sqlite`, from
`feat/ownership` (slices 1–7: PRs #50–#55).

## Done

- Slices 1–7 in `feat/ownership` (`01631bb3`): registry, claim, uses; prune;
  uses guard; reset rules and write lock; name reuse, `--adopt-from`;
  `inspect --shared`, onboarding beside other projects.
- Slice 8 implemented (`94e59e45`): SQLite `_dbd_objects`/`_dbd_uses`, T18.
- Embedded 295/295, `cli_live` 61/61, workspace 2013/0, clippy, fmt, doc clean.

## Next

    gh pr create --base feat/ownership --head feat/ownership-sqlite
    # review, CI, local merge; then the docs slice (guides 03/04, a sharing
    # guide, llms.txt/llms-full.txt, both SKILL.md, site mirrors,
    # dbd-pattern-verifier); then feat/ownership → develop

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
