# Checkpoint

**Slice:** v0.18.0 shipped and verified. `develop` and `main` level.

## Done

- **v0.18.0 released** — both crates on crates.io, merged to `main`, CI +
  CodeQL green. Cut as a **minor**, not the patch first asked for: it removes
  `Entity::search_paths`, and minor is the breaking position in 0.x here.
- **Every entity carries `schema_path`** — where unqualified names resolve and
  *who* said so (`File`/`Project`/`SessionDefault`). Checked against a live
  PostgreSQL: `"$user"` is no longer a schema; an unstated path differs from a
  stated `public`; `USE db` sets the catalog (63 of 77 files, 2 → 123).
- **`source.search_path`** — the fallback is the project's, not a hardcoded
  `public`, and **never silent**: each DDL file stating no path is named with
  what it resolved against; `inspect` counts, `apply`/`deploy` print.
- **Verified from the registry** — a crate on `dbd-core = "0.18.0"` re-ran all
  8 repros green and confirmed `entity.search_paths` no longer compiles.

## Next

    cargo test --workspace --all-features   # 1579 pass, clippy + fmt + doc clean

Nothing queued. Open: #18 (reconcile), #11 (CLI coverage), #7 (tenancy).

## Open question

Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
residual difference is not a defect.

## Known-broken / carried forward

- `reads`/`writes`/`refers` are bare strings carrying the pre-qualified guess;
  `references` has the per-name provenance. Collapsing all four into one
  `Ref { name, kind, schema_source }` is designed, not built.
- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure).
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a widening cast; RUSTSEC-2023-0071
  (`rsa` via `sqlx-mysql`) is ignored in audit.toml.
