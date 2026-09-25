# Checkpoint

**Slice:** v0.16.0 shipped and verified. `develop` and `main` are level;
nothing unreleased.

## Done

- **v0.16.0 released** — tagged, both crates on crates.io, merged to `main`,
  CI + CodeQL green on the merge.
  - **MySQL reader** (`91034b9`) — the T-SQL walk under different `WalkRules`:
    `ALTER` never declares, `a.b` is `database.object`, backticks quote, `#`
    comments. **Fixture-verified only**; `DBD_SQL_CORPUS` awaits a corpus.
  - **Doc gates** (`a2bebd8`, `fc541dc`) — doc `rust` blocks compile as a test
    target, guide facts checked against code, 24 broken links fixed and
    `deny`-ed. All three found real defects on their first run.
  - **`architecture.md` de-rotted** — 693 lines out, 405 in; 16 of 40 field
    declarations had been wrong (`6d64760`…`e1f69c4`).
- **Verified from the registry**, not the working tree: same MySQL project
  through `dbd graph` on both. 0.15.0 lost the FK edge and emitted one
  alphabetical layer — exit 0, no warning. 0.16.0 gets both right.

## Next

    cargo test --workspace --all-features   # 1528 pass, clippy + fmt clean

**Sensei switchover** — pin `dbd-core` v0.16.0, replace
`adapters/manifest/dbd.rs`'s `design.yaml` reader with `project::survey`, route
reading through `parse_sql_as`, delete `indexer/lang/sql/`. One path in sensei.

## Open questions

None blocking.

## Known-broken / carried forward

- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure);
  `apply`, `deploy`, `import`, `export` work.
- `parse_sql` ignores non-constraint `ALTER`s — fine inside dbd's contract, a
  gap on a foreign corpus.
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a *widening* cast;
  `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
