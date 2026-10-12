# Checkpoint

**Slice:** 0.25.1 on `develop` — #41 and the scoped-reset fix merged; not
released.

## Done

- v0.25.0 shipped (crates.io, `main` `b4f075e3`, site serves 0.25.0).
- #41 (PR #46, `7795094b`): an extension's types spelled as the database
  spells them in `reconcile` and `diff`.
- Scoped reset (PR #47, `a6e39677`): the scope's tables' policies and its
  tables' and views' triggers go first, so one calling an in-scope function no
  longer refuses the reset; FK triggers and a partition's clone of its parent's
  trigger are left. sensei's real `dojo` scope now resets and redeploys.
- Embedded suite 132/132; CI green on both PRs; merges done locally.

## Next

    # 0.25.1 when asked: release notes → make bump patch → release.yml →
    # PR develop → main (merge locally) → install from crates.io and re-run

## Open

- #41 closes when 0.25.1 reaches `main`.
- `6fccb0ff`/`b4f075e3` carry GitHub's noreply identity; not rewritten.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
- CHANGELOG Known limitations; `docs/BACKLOG.md` (0.25.1).
