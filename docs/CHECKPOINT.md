# Checkpoint

**Slice:** #7 Phase 2, slice 5 (reset rules) on `feat/ownership-reset`, from
`feat/ownership` — where slices 1–4 landed as PRs #50, #51, #52 (`64e09594`).
Phase 1 is on `develop`, unreleased.

## Done

- Slices 1–4 in `feat/ownership`: registry, claim, uses; prune by ownership;
  the uses guard on reconcile's drops and apply's migrations.
- Slice 5 implemented (`97d866e7`, docs `732ff4d1`): any project's prod mark
  refuses an unforced reset; entity and dropped-schema uses guard; `--schemas`,
  `--extensions` and roles keep what other projects have, use or depend on,
  named on `ResetComplete.kept`; `reset_preview` for `--dry-run`.
- Embedded 249/249, `cli_live` 49/49, workspace 2006/0, clippy, fmt, doc clean.

## Next

    gh pr create --base feat/ownership --head feat/ownership-reset
    # then focused review, fixes test-first, CI green, local --no-ff merge;
    # slice 6: name reuse (--allow-ownership-change) + transfer (--adopt-from)

## Open

- Pre-existing, outside #7: reconcile re-emits a mixed-case or quoted
  `nextval('App.Seq')` default every run (`canonical_default`) — needs an
  issue and a pg_get_expr probe first.
- Declined: best-effort batch rollback error; dropping the old `dbd.import_tmp`;
  migrating a branch build's `dbd.uses` CHECK (no `owner`) — rebuild those.
- A bare type of another project's is no use (dropped as a built-in).
- An apply in one transaction runs `apply.before` hooks before its claim
  (documented in the spec). `dbd.objects` still allows namespace `type`, unused.
- Docs slice (end of Phase 2) must cover `--prune-unowned` and reset's kept
  lines in the guides.
- Phase 1 is releasable as 0.25.2 when asked.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
