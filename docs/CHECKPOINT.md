# Checkpoint

**Slice:** 0.25.0 — third re-audit fixes. All four fix branches are merged on
`fix/audit-bugs` (tip `02ccca2`, all gates green); `docs/feature-coverage`
merged it and carries the CHANGELOG and docs pass 3 on top.

## Done

- `fix/migrations-3`, `fix/guards-3`, `fix/reverse-data-3`, `fix/format-dbml-3`
  merged into `fix/audit-bugs` (report: `/tmp/dbd-resume/reaudit3-report.md`).
- `docs/feature-coverage`: CHANGELOG `[Unreleased]` corrected and extended for
  the third round (false bullets fixed, Library API breaks incl.
  `check_scope_guard(&DatabaseMarks)`, known limitations); every guide, README,
  both llms files, both SKILL.md and agent copies, architecture notes. Site
  copies regenerated and byte-identical; site tests 154/154; embedded 125/0.

## Remaining

- Merge `docs/feature-coverage` back into `fix/audit-bugs`.
- Code defect found in docs pass 3 (not fixed): a DBML column name with an
  escaped quote (`"we\"ird"`) makes `init --from-dbml` write a table file that
  does not parse.

## Decisions (user)

- #22 refuse older project; #25 reconcile grants+hooks; prod guard: no
  override (a reset under the all-scope or the pinned scope forgets the mark).
- Full re-audit was run (3rd). Ask before a 4th.
- Repo identity: global Jerry Thomas <me@jerrythomas.name>. After the fixes:
  `git filter-repo --mailmap` all non-Jerry identities → Jerry Thomas; backup
  bundle first; confirm before any force-push.

## Next
    # merge docs/feature-coverage → fix/audit-bugs (all gates) → ask about a
    # 4th audit → author rewrite → PR to develop

## Scratch (rm denied)
- /tmp/dbdaudit-*, /tmp/dbdA3*, /tmp/dbdB_*.txt, /tmp/a1, /tmp/a2, /tmp/f1-*, /tmp/f2_mv.*,
  /tmp/dbddocs3-* (incl. stopped cluster dbddocs3-pg.Z3uWLs), /tmp/dbd-resume/docs3-*
