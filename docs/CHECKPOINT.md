# Checkpoint

**Slice:** #24 schema model v2 — complete on `develop`, unreleased.

## Done

- **#24 schema model v2** (`316b06c` red, `d55e2b5` green, `e38d28e` docs).
  `version: 2`; `entities` (views, matviews, functions, procedures); `deps`
  (reads/writes/calls/member, projected from `Entity::refs`, `unresolved`
  rather than dropped); `fk`/`uq` on `Column`. `tables`/`refs` keep their v1
  shape so the viewer extraction is not invalidated.
- **`COMMENT ON` for non-tables** (`389f36c` red, `d55e2b5` green). New
  `Entity::comment` — a view/matview/routine has no `TableDef`, so its comment
  was parsed and dropped, blanking every non-table row of the diagram's
  entity description table.
- **Docs on all four surfaces** — guide, both llms files, both SKILL.md
  copies. Website is generated from `docs/` by `copy-content.mjs`; no agent
  describes the model shape.
- Earlier in `[Unreleased]`: #7 carve-outs — exposed/internal schemas, and
  `reconcile --prune` no longer dropping platform-owned objects.

## Next

    make minor      # breaking: config::SchemaGrantConfig → SchemaOptions

Workspace green (1023 dbd-core + CLI suites), clippy + fmt + doc-examples
clean. Release checklist item 5 — install the artifact and re-run the repro —
still to do after the bump.

## Open questions

- Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
  residual difference is not a defect.
- #7 (multi-tenant) stays open but is largely superseded by scopes; prune and
  exposed schemas were carved out of it.

## Known-broken / carried forward

- The viewer does not yet render `entities`/`deps` — that is rokkit#159,
  running in a separate session.
- `emit` covers tables and views only; routines are skipped and reported.
- `CREATE ROLE … IN ROLE …` is not read as a membership; `GRANT … TO …` is.
- `cmd_format --check` still calls `std::process::exit(1)`, so that path
  cannot be tested in-process.
- The commit gate tests the **worktree**, not the index — a staged-only
  change is not what it verifies.
