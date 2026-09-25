# Checkpoint

**Slice:** #21 file-level references fixed on top of v0.16.0. Unreleased.

## Done

- **v0.16.0 shipped and verified** — both crates on crates.io, merged to
  `main`, CI + CodeQL green. Registry artifact re-ran the MySQL repro: 0.15.0
  lost the FK edge and emitted one alphabetical layer, exit 0 and no warning.
- **Issues #19 and #20 closed** — both shipped earlier, both resolved
  *differently* than proposed, and the comments say how: #19 exposes
  `parse_sql*`/`ParsedFile` rather than the raw `extractors::*`; #20 routes
  SQLite to `Verbatim` rather than to a SQLite grammar, which costs
  `diff`/`reconcile`/snapshots.
- **#21 fixed** (`3daef4a` red, `6d19303` green). `ParsedFile.references`
  (`reads`/`writes`/`calls`) carries what a file refers to outside any
  declaration. Instrumented first: `refer()` was called 43,754 times against an
  independent reader's 43,737, so extraction was never the gap — 20,929 (47.8%)
  were dropped for want of an owner. Deduplicated that is 4,059 references,
  total 10,695 → 14,754, and **817 of 2,154 files went from reporting nothing
  to reporting something**. Rejected by measurement rather than built:
  re-attaching a later `ALTER TABLE` batch to a table declared earlier in the
  file, worth 182.

## Next

    cargo test --workspace --all-features   # 1540 pass, clippy + fmt + doc clean

1. **#22** — have `Reference`/`ForeignKey.ref_schema` record whether the schema
   was written in the source or inferred from `search_path`, so a per-file
   consumer can decline to trust a guess. Not started; better-scoped than #21.
2. **Cut 0.17.0** — `ParsedFile` gained a field, so minor.

Also open: #18 (reconcile non-convergence on normalized DEFAULTs), #11 (CLI
live-connection coverage), #7 (multi-tenant isolation).

## Open questions

Whether sensei's 43,737 counts occurrences or unique pairs — until that is
settled, neither side should treat a residual count difference as a defect.

## Known-broken / carried forward

- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure);
  `apply`, `deploy`, `import`, `export` work.
- `parse_sql` ignores non-constraint `ALTER`s — fine inside dbd's contract, a
  gap on a foreign corpus.
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a *widening* cast;
  `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
