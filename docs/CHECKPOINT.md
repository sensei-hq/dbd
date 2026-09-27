# Checkpoint

**Slice:** v0.22.0 shipped and verified. `develop` and `main` level.

## Done

- **v0.22.0 released** — crates.io (`dbd-cli` and `dbd-core` both 0.22.0),
  merged to `main`, Release + CI + CodeQL all green. #24 closed; only #7
  (multi-tenant) remains open.
- **#24 schema model v2** — `version: 2`; `entities` (views, matviews,
  functions, procedures); `deps` (reads/writes/calls/member, projected from
  `Entity::refs`, `unresolved` rather than dropped); `fk`/`uq` on `Column`.
  `tables`/`refs` keep their v1 shape so the viewer extraction stays valid.
- **`COMMENT ON` for non-tables** — new `Entity::comment`. A view, matview or
  routine has no `TableDef`, so its comment was parsed and dropped, blanking
  every non-table row of the diagram's entity description table.
- **#7 carve-outs** — exposed/internal schemas (`dbd inspect` reports exposed
  tables with no RLS policy), and `reconcile --prune` no longer dropping
  platform-owned objects.
- **Docs on all four surfaces** — guide, both llms files, both SKILL.md
  copies. Website is generated from `docs/`; no agent describes the model.

## Verified against the shipped artifact

    dbd diagram --json -f model.json      # dbd 0.22.0 from the tag
    version: 2 · entities carry their notes · deps graph correct
    (`count` marked unresolved as a built-in) · pk/uq flags correct

## Next

Nothing queued. #7 (multi-tenant isolation) is the only open issue — largely
superseded by scopes; prune and exposed schemas were carved out of it.

## Open questions

Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
residual difference is not a defect.

## Known-broken / carried forward

- The viewer does not render `entities`/`deps` yet — rokkit#159, running in a
  separate session.
- `emit` covers tables and views only; routines are skipped and reported.
- `CREATE ROLE … IN ROLE …` is not read as a membership; `GRANT … TO …` is.
- `cmd_format --check` calls `std::process::exit(1)`, so that path cannot be
  tested in-process.
- The commit gate tests the **worktree**, not the index — a staged-only change
  is not what it verifies.
