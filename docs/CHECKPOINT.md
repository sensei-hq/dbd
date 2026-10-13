# Checkpoint

**Slice:** #7 Phase 2, docs slice on `feat/ownership-docs`, from `feat/ownership`
(slices 1–8: PRs #50–#56, `42b4c09d`). Phase 1 is on `develop`, unreleased.

## Done

- Phase 2 code complete in `feat/ownership`: registry, claim, uses; prune by
  ownership; uses guard; reset rules and the write lock; name reuse and
  `--adopt-from`; `inspect --shared`, onboarding; SQLite parity.
- Embedded 295/295, `cli_live` 61/61, workspace 2017/0, clippy, fmt, doc clean.

## Next

    gh pr create --base feat/ownership --head feat/ownership-docs
    # docs written (978b2d7c): guide 14, guide 04/08, llms, both SKILL.md,
    # README. Spec-conformance audit, fixes, CI, merge; then feat/ownership →
    # develop (PR, CI, local merge)

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Delete by hand: `/tmp/dbd-mut` (+ `git worktree prune`), `/tmp/dbd-mut-target`,
  `/tmp/dbd-idx.*`, `/tmp/dbd-review2*`, `/tmp/dbd-lockreview*`.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
