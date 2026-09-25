# Checkpoint

**Slice:** #21 file-level references, fixed on top of v0.16.0. Unreleased.

## Done

- **v0.16.0 shipped and verified** — both crates on crates.io, merged to
  `main`, CI + CodeQL green. Registry artifact re-ran the MySQL repro.
- **#19 and #20 closed** — both shipped earlier, both resolved *differently*
  than proposed; the comments say how (`parse_sql*` rather than raw
  `extractors::*`; SQLite → `Verbatim` rather than a SQLite grammar, which
  costs `diff`/`reconcile`/snapshots).
- **#21 fixed** (`3daef4a` red, `6d19303` green). `ParsedFile.references`
  (`reads`/`writes`/`calls`) carries what a file refers to outside any
  declaration. Instrumented first: `refer()` was called 43,754 times against an
  independent reader's 43,737, so extraction was never the gap — 20,929 (47.8%)
  were dropped for want of an owner. Deduplicated: 4,059 refs, total
  10,695 → 14,754, and **817 of 2,154 files went from reporting nothing to
  reporting something**. Measured and rejected rather than built: re-attaching
  a later `ALTER TABLE` batch to a table declared earlier, worth 182.

## Next

    cargo test --workspace --all-features   # 1540 pass, clippy + fmt + doc clean

1. **#22** — record whether a `Reference`'s schema was written or inferred from
   `search_path`, so a per-file consumer can decline to trust a guess.
2. **Cut 0.17.0** — `ParsedFile` gained a field, so minor.

Also open: #18 (reconcile non-convergence), #11 (CLI coverage), #7 (tenancy).

## Open questions

Whether sensei's 43,737 counts occurrences or unique pairs — until settled,
neither side should treat a residual count difference as a defect.

## Known-broken / carried forward

- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure);
  `apply`, `deploy`, `import`, `export` work.
- `parse_sql` ignores non-constraint `ALTER`s — fine inside dbd's contract, a
  gap on a foreign corpus.
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a *widening* cast;
  `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
