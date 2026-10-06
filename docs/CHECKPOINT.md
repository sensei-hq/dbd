# Checkpoint

**Slice:** #40 fixed on `develop` (`98480df`) — `reconcile --prune --scope` no
longer drops the project's own out-of-scope tables. Not yet on `main` or released.

## Done

- v0.24.0 released and verified (crates.io, `main` `d36902c`).
- #40: `declared_out_of_scope` + `hide_declared` in reconcile and diff_live;
  3 embedded-PG tests (scoped reconcile, scoped diff, unscoped control).
- Docs: guide 04, llms.txt, llms-full.txt; CHANGELOG [Unreleased] Fixed entry.

## Verified

    embedded suite 55/55 · workspace 1635/0 · clippy 1.99 --all-features
    PR #42 green on every check (incl. the embedded CI job)

## Next

    # if releasing the fix: cut 0.24.1 (patch) per the Release Checklist —
    # PR develop → main (closes #40), then `make bump` on develop

## Open questions

- Cut 0.24.1 for #40 (data loss in released versions)?
- #7: keep for separately-maintained designs on one DB, or close in favour of
  "one design + scopes" now that #40 makes scopes safe?

## Known-broken / upstream

- History has no views/routines: snapshots hold tables and enums only.
- Snapshots cut before inline FKs were recorded show a one-time FK "change".
