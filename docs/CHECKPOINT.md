# Checkpoint

**Slice:** Retire the sqlparser DDL path, then expose a per-file parser API for
external embedders (issue #19, sensei's code indexer).

## Done

- **Red tests first** (`8365b9a`) — `ParserChoice` contract: `sqlparser`
  rejected by name as *removed*, non-Postgres dialect → `PgQuery`, same
  rejection through `Design::from_config`. Verified red (exit 101) before
  implementing.
- **The retirement** (`3bffa7f`) — deleted `extractors.rs`, `tables.rs`,
  `preprocess_sql`, `parse_with_sqlparser`, `SqlparserDdl`, `is_valid_postgres`
  and the parity gate. 2,062 lines. Workspace tests exit 0, clippy `-D warnings`
  clean, fmt clean, doctests pass, rustdoc errors back to the 22-error baseline.
  54 deleted lib tests all accounted for (36 + 18 in the two deleted modules;
  lib 1031 → 977 exactly). Docs synced: guide, llms.txt, architecture.md (ADR
  marked superseded, not rewritten). CHANGELOG has the breaking entries.

- **Commit gate fixed** (`154c180` red, `6b69fa2` green) — `.githooks/pre-commit`
  ran cargo unscoped, so it never compiled `dbd-core`. Now delegates to
  `make _check-ci`. Verified by effect: a deliberately panicking dbd-core test
  made the hook exit 1 after running all 977. Probe removed.
- **Issues filed** — #20 (SQLite source DDL has no parser) and a scope
  correction comment on #19 (the six functions it names were the sqlparser
  layer and are now deleted; the real seam is a new `parse_sql`).

## Next

Issue #19 — `parse_sql(sql, dialect) -> ParsedFile`, deriving entity
type/schema/name from the statement rather than the path. Decided: keep dbd's
rich `Entity` + `Reference` (sensei derives its nodes/edges from them);
explicit dialect argument defaulting to PostgreSQL, not sniffed. Each `pg/*.rs`
parser already walks the node holding the `RangeVar`/`funcname` and discards it.

    cargo test -p dbd-core --lib parser::

## Open questions

None blocking. `ParserChoice::for_dialect` is the dialect seam (currently
`_dialect`, everything → PgQuery); #20 is its first real customer.

## Known-broken / carried forward

- **SQLite source DDL does not round-trip** — issue #20. Measured: SQLiteDialect
  13/13, sqlparser-Pg 9/13, libpg_query 5/13. Pre-existing, not from this slice.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- `docs/design/architecture.md:360` still lists `is_identity: bool` on
  `ColumnDef`; now `identity: Option<IdentityKind>`, and predates `generated`.
- 22 pre-existing rustdoc intra-doc-link errors (not gated by CI).
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
