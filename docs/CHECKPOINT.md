# Checkpoint

**Slice:** v0.17.0 shipped and verified. `develop` and `main` level; nothing
unreleased.

## Done

- **v0.17.0 released** — both crates on crates.io, merged to `main`, CI +
  CodeQL green. Closed #19, #20 (shipped earlier, resolved differently than
  proposed), #21, #22.
- **#21** — `ParsedFile.references` carries what a file refers to outside any
  declaration. `refer()` ran 43,754 times against an independent reader's
  43,737, so extraction was never the gap — 47.8% was dropped for want of an
  owner. **817 of 2,154 files went from silent to saying something.**
- **#22** — `SchemaSource` on `Reference`/`ForeignKey`, kept out of the FK's
  `PartialEq` and serialized form or an inferred FK reads as permanent drift.
  Also fixed: the resolver could re-point a schema the source had written.

## Next

    cargo test --workspace --all-features   # 1554 pass, clippy + fmt + doc clean

Nothing queued. Open: #18 (reconcile non-convergence), #11 (CLI coverage),
#7 (multi-tenant isolation).

## Open question

Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
residual difference is not a defect.

## Known-broken / carried forward

- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure);
  `apply`, `deploy`, `import`, `export` work.
- `reads`/`writes`/`refers` are bare strings carrying the pre-qualified guess;
  `references` has the per-name provenance for the same entity.
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a widening cast; audit.toml
  ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
