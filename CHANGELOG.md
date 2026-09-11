# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html); while
the crates are `0.x`, the **minor** position is the breaking one, so
`0.12.x → 0.13.0` may require changes in code that embeds `dbd-core`.

## [0.13.0] — 2026-09-11

Two `dbd reconcile` non-convergence bugs ([#12]) and a security sweep.

### Breaking (embedders of `dbd-core` only — the `dbd` CLI is unaffected)

- `entity::TableConstraint::Unique` gains a `nulls_not_distinct: bool` field.
  Code that constructs the variant, or pattern-matches it exhaustively, no
  longer compiles. Add `nulls_not_distinct: false` to keep today's behaviour, or
  `..` to the pattern. Snapshots written by earlier versions still deserialize —
  the field is `#[serde(default)]`.
- `diff::generate_data_sql` now emits quoted identifiers and literals
  (`UPDATE "public"."users" SET "status" = …`). Any test pinning the previous
  unquoted output needs updating. See *Security* below for why.

### Fixed

- **`unique nulls not distinct` was silently reduced to a plain unique on the
  `ALTER` path** ([#12]). `dbd apply` on a fresh database ran the DDL verbatim
  and kept the clause; `dbd reconcile` altering an existing table re-derived the
  constraint from the parsed model, which had nowhere to hold it — so the
  constraint that landed was weaker than the one declared. Nothing reported it:
  UNIQUE constraints were compared by name alone, so `dbd diff` then called the
  result in sync. The clause now survives parsing (both the sqlparser and
  libpg_query paths), introspection, comparison and emission.

- **`dbd reconcile` would not remove an enum value, and could not converge**
  ([#12]). Postgres has no `ALTER TYPE … DROP VALUE`, so no SQL was emitted,
  reconcile reported `0 altered`, and `dbd diff` reported the same drift on
  every subsequent run. Reconcile now performs the type recreation under
  `--allow-destructive` — see *Added*.

### Added

- `dbd reconcile --allow-destructive` now converges an enum that lost a value,
  by recreating the type: dependent managed views are dropped (deepest first),
  column defaults are taken off, the type is renamed aside and recreated with
  exactly the declared values in declared order, each column is moved across
  with `USING …::text::<type>` (`::text[]` for an array column), defaults are
  restored and the displaced type dropped. Managed views are re-applied by the
  pass that already re-applies every view on every run.

  The whole batch runs in one implicit transaction, so every failure is total
  and names the object: a row still holding the removed value fails the cast, a
  default naming one fails the `SET DEFAULT`, and an unmanaged dependent view
  fails the `ALTER … TYPE`. A dependent **materialized view** is declined rather
  than attempted — dbd never auto-drops one — and reported with the manual steps.

- `dbd-core` gains `reconcile::plan_enum_recreation`, plus two modules extracted
  from previously-duplicated private helpers: `path_safe`
  (`is_safe_segment`, `safe_relative_path`) and `sql_quote`
  (`literal`, `ident`, `qualified`).

### Security

- **Database-derived names could escape the directory they were written into.**
  `dbd export` names its output file after the table and, without `--out`, the
  directory after the schema. Both come from the live catalog, and a quoted
  identifier may hold anything — `CREATE TABLE "../../x"` is legal in both
  Postgres and SQLite, and `Path::join` with an absolute name discards the base
  outright. Neither adapter checked. Both now refuse and name the object.
  `dbd reverse` and hook-path resolution already had this check, but as two
  private copies covering two of the four sites; the rule now lives in
  `path_safe` and is applied at all of them.

- **Catalog names were interpolated into SQL unescaped.** The Postgres export
  built its identifier by string-replacing the dot, leaving an embedded `"` free
  to close the quoting; the SQLite export did the same; and
  `diff::generate_data_sql` pasted table, column and enum label straight into a
  data-correction script — a file an operator runs by hand, usually with more
  privilege than dbd itself holds. All now go through `sql_quote`, which mirrors
  Postgres's own `quote_literal`/`quote_ident`. Type names stay verbatim
  (`varchar(50)` is a type expression, not an identifier).

- **`dbd diagram` handed an unvalidated URL to the platform's browser opener.**
  The base comes from `--site`/`$DBD_DIAGRAM_URL`, and the opener is not uniform
  — on Windows it runs `cmd /c start`, where a metacharacter is a command, and
  every platform will act on `file://` or `javascript:`. Only plain `http(s)`
  URLs are opened now; anything else is printed with the reason it was not.

- **Dependency advisories cleared.** `cargo audit` 4 vulnerabilities + 4 warnings
  → 0 (`crossbeam-epoch`, `h2`, `quinn-proto`, `anyhow`, `event-listener`,
  `chacha20`, and `indicatif` 0.17 → 0.18 to drop the unmaintained
  `number_prefix`). `bun audit` in `site/` 21 → 0. CI now runs `cargo audit`.

  One advisory is documented rather than fixed, in `.cargo/audit.toml`:
  RUSTSEC-2023-0071 (`rsa`) has no fixed release and is reachable only through
  `sqlx-mysql`, which dbd never enables — it is in `Cargo.lock` only because
  that file is feature-agnostic, so no feature selection can remove it.

[#12]: https://github.com/sensei-hq/dbd/issues/12
[0.13.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.0
