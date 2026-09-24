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

## Next

Issue #19 — `parse_sql(sql) -> ParsedFile`, deriving entity type/schema/name
from the statement rather than the path. Agreed shape: keep dbd's rich `Entity`
+ `Reference`; sensei derives its nodes/edges from them. Each `pg/*.rs` parser
already walks the node holding the `RangeVar`/`funcname` and discards it.

    cargo test -p dbd-core --lib parser::

## Open questions

- **Dialect parameter.** Agreed it should exist and auto-derive when absent.
  `ParserChoice::for_dialect` is the seam (currently `_dialect`, all → PgQuery).
  Open: does `parse_sql` take a dialect argument, or infer from the SQL?
- **Filing two issues** — drafted, not yet created, awaiting the go-ahead.

## Known-broken / carried forward

- **SQLite source DDL does not round-trip.** `init --from-db sqlite://` writes
  `AUTOINCREMENT`/`WITHOUT ROWID`/`STRICT` verbatim; `reverse::design_yaml`
  writes no `source:` block, so it loads under the `postgresql` default and
  libpg_query rejects all three. `ensure_fully_parsed` then refuses the apply.
  Measured: SQLiteDialect 13/13, sqlparser-Pg 9/13, libpg_query 5/13. Needs a
  real SQLite grammar; pre-existing, not from this slice.
- **The commit gate runs `cargo test` without `--workspace`.** The root package
  is `dbd-cli`, so it tests 224 CLI tests and never touches `dbd-core`'s 977.
  It reported "All checks passed" over three genuinely red tests. CI and
  `make bump` both use `--workspace`, so releases are safe; the local loop is
  not.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- `docs/design/architecture.md:360` still lists `is_identity: bool` on
  `ColumnDef`; now `identity: Option<IdentityKind>`, and predates `generated`.
- 22 pre-existing rustdoc intra-doc-link errors (not gated by CI).
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
