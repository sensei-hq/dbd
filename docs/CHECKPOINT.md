# Checkpoint

**Slice:** #7 design at rev 3 (proposed). Website/docs audit done; fixes not started.
Spec: `docs/superpowers/specs/2026-10-06-multi-project-isolation-design.md`.

## Done

- #7 rev 3: entity-level ownership (`dbd.objects`, `dbd.uses`), no special
  schemas, workspaces with one root manifest (`modules:` block,
  `database/<module>/` folders), `dbd split --from-scopes` for sensei.
- Audit of the guides, llms files, SKILL.md and site pages against 0.24.1.

## Remaining

- #7: approval of the 5 decisions in the spec, then phase 1 (patch).
- Audit, code bugs (each test-first): `-e production` passes the reset prod
  guard; `reset --target` ignores the design's target; `deploy` skips grants
  and ignores `-c`; `emit` drops FKs/CHECKs/indexes unreported;
  `GITHUB_TOKEN` is never read.
- Audit, docs: ~20 wrong statements; new guides (scopes, targets, access
  control, import/export, viewer, embedding); home page cards.

## Next

    # on approval: fix/reset-env-guard off develop, red test first

## Open questions

- Fix code or docs for each mismatch? Recommended: code for the 6 bugs above,
  docs for the rest.
- The release checklist cites `the_website_copies_match_the_docs`; no such
  test exists (site content is gitignored and regenerated at build).

## Known-broken / upstream

- The 6 code bugs above.
- History has no views or routines: snapshots hold tables and enums only.
