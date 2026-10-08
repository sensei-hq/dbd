# Checkpoint

**Slice:** `docs/feature-coverage` — docs + website match the code shipping as 0.25.0,
second pass: `fix/audit-bugs` at 0d6fdb8 merged (d638ff9), every surface synced to it.

## Done

- Guides 02–10, 12, 13; README; llms.txt + llms-full.txt; both SKILL.md and both
  pattern-verifier copies; architecture.md (env guard, full reset, adapter methods).
- Covered: environment guard, older-project guard, reconcile hooks + grants, dry runs
  that ask the database, scope guard on import/export/refresh/policies, export layout,
  import/dev files, JSON keys / generated columns / sequences, reset on SQLite + its
  summary + no-cascade + kept roles, source.search_path at run time, emit type limits,
  DBML stubs as external:, init --from-db output, doctor's extension/schema files.
- Site copies regenerated (`bun run sync:content`), byte-identical; site tests green.

## Remaining

- A final small pass after the third-audit fix agents merge (migrations ordering,
  guards keyed on project name, reverse fidelity, search_path rewrite, formatter
  literals, DBML) — document only what lands.

## Next

    git -C /Users/Jerry/Developer/dbd-docs log --oneline d638ff9..docs/feature-coverage

## Open questions

- CHANGELOG over-claims: emit names only FK and CHECK constraints (not PK/UNIQUE);
  `Design::hook_scope` is crate-private, not new public API.

## Known-broken / upstream (code, not docs)

- apply/deploy/import dry runs skip the env guard; import --dry-run skips the pin.
- A JSONL export drops a json column that is NULL in every row (default on import).
- Formatter splits `$tag$` bodies; `dbd graph` keeps an unparsed entity in `nodes`.
- `dbd diff --help` says "comments"; only column comments are compared.
