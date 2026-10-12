# Checkpoint

**Slice:** 0.25.0 — the verification round (`verify4-report.md`). Both fix
branches (`fix/followups-4`, `fix/dbml-format-4`) are merged on
`fix/audit-bugs` (tip `68822e7`); `docs/feature-coverage` merged it and carries
the CHANGELOG and docs pass 4 on top. `make _check-ci` green at every commit.

## Done

- CHANGELOG `[Unreleased]`: every false bullet from verify4 §3 corrected, each
  checked against the code or the built binary; new bullets (emit quoting,
  format layout, data.sql of added tables); Library API additions
  (`foreign_keys`, `live_import_plan`, `keep_authored_triggers`, the
  `check_reset_guard` method); Known limitations (formatter, `init --from-db`
  silent drops, dots in names, DBML backtick/line break, diff quoting).
- Docs: guides, README, both llms files, both SKILL.md copies, both
  pattern-verifier copies, architecture. Site copies synced; site tests 154/154;
  `docs_match_code` 5/5, `doc_examples` 3/3.
- BACKLOG 0.25.1: SQLite (file creation, IF NOT EXISTS, partial truncate,
  offline dry-run order), quoting sites with line numbers, dots, DBML writer
  limits, formatter list words, data.sql of untouched tables.

## Remaining

- Merge `docs/feature-coverage` back into `fix/audit-bugs`.

## Decisions (user)

- #22 refuse older project; #25 reconcile grants+hooks; prod guard: no
  override. Full re-audit run three times; ask before a fourth.
- Repo identity rewrite after the fixes (backup bundle first; confirm before
  any force-push).

## Next
    # merge docs/feature-coverage → fix/audit-bugs (all gates) → author rewrite → PR

## Scratch (rm denied)
- /tmp/dbd-resume/docs4-* (stopped cluster docs4-pg.PDqA), /tmp/dbdfix4, /tmp/dbdV4*-*,
  /tmp/dbdaudit-*, /tmp/dbdA3*, /tmp/dbdB_*.txt, /tmp/a1, /tmp/a2, /tmp/f1-*,
  /tmp/f2_mv.*, /tmp/dbddocs3-*, /tmp/dbd-resume/docs3-*
