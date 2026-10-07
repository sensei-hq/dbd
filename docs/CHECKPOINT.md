# Checkpoint

**Slice:** `docs/feature-coverage` — docs + website match dbd 0.24.1, describing the
post-fix behaviour of the six audit bugs (fixed in parallel on `fix/audit-bugs`).
Parallel, unchanged here: #7 design rev 3 (proposed).

## Done

- Site: `/changelog` (last 5 releases, tested parser), Viewer/Changelog in nav,
  footer adds viewer/changelog/llms-full, sitemap; home page names every command.
- Guides 01–06 corrected; new 07 scopes, 08 targets, 09 access control, 10 data,
  11 viewer, 12 embedding (examples compiled by doc_examples), 13 dialects.
- README, llms.txt, llms-full.txt, SKILL.md + verifier agent (both copies).
- Two independent audits of 07–13 against the code; all findings corrected.

## Remaining

- Merge `fix/audit-bugs` first: the docs state its behaviour (-e aliases,
  reset --target default, deploy grants and -c, emit FKs/CHECKs/indexes,
  GITHUB_TOKEN, deploy-only GitHub sources). Re-read 04/08/13 against it.
- Code-side follow-ups found while documenting (see the slice report): cli.rs
  help for `diff`/`--source`/`reset --target`; export skips views; scopes skip
  sequences; SQLite/Convex import TRUNCATE; UTF-16 skipped on apply.

## Next

    git -C /Users/Jerry/Developer/dbd-docs log --oneline develop..docs/feature-coverage

## Open questions

- Should `dbd export` accept views (unblocks the documented migration pattern)?
- `fix/audit-bugs` emit handles some indexes differently from the brief
  (MySQL drops predicates, T-SQL omits expression indexes); docs say "reported,
  and passed through, built without its predicate, or left out".

## Known-broken / upstream

- The code-side items above; `DBD_CATALOG_TTL` is read by dead code only.
