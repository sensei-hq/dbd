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

- **`parse_sql` landed** (`c0cc392` red, `a20156b` green) — identity off the
  statements. New `pg/declarations.rs` classifies declare-vs-attach and slices
  statements by `stmt_location`/`stmt_len`; `parse_sql` reassembles each
  entity's fragment and runs the existing per-type parser. 18 integration + 8
  unit tests. Grouping was initially tested vacuously (index named the *first*
  table, so match-by-name and match-first agree); mutating `owns()` to `true`
  left everything green. Fixtures now name the second table and the mutation
  fails four tests.

- **`ALTER TABLE … ADD CONSTRAINT` fixed** (`2a8c635` red, `1c73fd1` green) —
  it was silently dropped by `parse_entity`, the live apply path, not just by
  `parse_sql`. An FK added that way produced no `refers` edge (wrong apply
  order), no `table_def` constraint (`apply` omitted it), and reconcile planned
  `DROP CONSTRAINT` against a DB that had it. Now routed through the same
  `extract_table_constraint` as inline constraints, so the two spellings are
  indistinguishable downstream. Other `ALTER` subcommands warn instead of
  vanishing. Mutation-checked: forcing the table-name guard to `true` fails
  `an_alter_on_another_table_is_not_absorbed`.

## Next

Nothing queued. Issue #19's ask is delivered; #20 (SQLite grammar) is the next
substantive piece if you want it, and the release must be **0.14.0** — three
breaking changes sit in `[Unreleased]`.

    cargo test --workspace --all-features

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
