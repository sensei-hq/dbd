# Checkpoint

**Slice:** v0.15.0 shipped (T-SQL reader, `project::survey`). Eight commits on
`develop` since, all unreleased.

## Done — unreleased

- **MySQL reader** (`91034b9`) — the T-SQL walk under different `WalkRules`:
  `ALTER` never declares, `a.b` is `database.object`, backticks quote, `#`
  comments. **Fixture-verified only** — the `DBD_SQL_CORPUS` gate awaits one.
- **Doc facts made executable** (`a2bebd8`) — doc `rust` blocks compile as a
  test target; parser table, dialect mapping and skill copies checked in code.
- **Every doc link resolved** (`fc541dc`) — 24 fixed, plus a `deny` guard.
- **`architecture.md` de-rotted** (`6d64760`…`e1f69c4`) — dead types dropped;
  13 drifted listings replaced with prose (16 of 40 declarations were wrong);
  examples gated or marked illustrative; 18 scenarios rewritten as Gherkin;
  three copied manifests replaced with links plus their rationale.

## Next

    cargo test --workspace --all-features   # 1528 pass, clippy + fmt clean

1. **Cut 0.16.0** — `[Unreleased]` has the MySQL reader plus tooling. Minor.
2. **Sensei switchover** — pin `dbd-core` v0.15.0, replace
   `adapters/manifest/dbd.rs`'s `design.yaml` reader with `project::survey`,
   route reading through `parse_sql_as`, delete `indexer/lang/sql/`.

## Open questions

None blocking. Order: (1) before (2), or sensei pins an unreleased dbd.

## Known-broken / carried forward

- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure);
  `apply`, `deploy`, `import`, `export` work.
- `parse_sql` ignores non-constraint `ALTER`s — fine inside dbd's contract, a
  gap on a foreign corpus.
- `ARRAY[col]::t[]` where the column is already `t` reads as drift, and
  `generate_data_sql` warns "may truncate" on a *widening* cast.
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
