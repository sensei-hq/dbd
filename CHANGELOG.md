# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html); while
the crates are `0.x`, the **minor** position is the breaking one, so
`0.12.x → 0.13.0` may require changes in code that embeds `dbd-core`.

## [Unreleased]

## [0.14.0] — 2026-09-24

One parser. The sqlparser DDL path retires — it was a second *PostgreSQL*
parser, not a dialect, kept as an escape hatch during the libpg_query migration
and unreachable since every entity type went native in 0.13.0. Removing it takes
the last regex out of the DDL parse path with it.

In its place, two things the migration made possible. `parse_sql` gives an
external embedder the statement-level identity the path-derived `parse_entity`
could not (issue #19), and `ALTER TABLE … ADD CONSTRAINT` is finally read —
until now a constraint written that way was silently dropped from the model, on
the live apply path.

Breaking for embedders and for any project that names `source.parser:
sqlparser`; see **Changed** below.

### Added

- **`parser::parse_sql` — statement-level parsing for external embedders**
  (issue #19). Reads every entity a SQL file declares, taking type, schema and
  name from the **statements** rather than from the path.

  `parse_entity` derives identity from `ddl/<type>/<schema>/<name>.ddl`, which
  is correct inside dbd's layout and silently wrong outside it: it falls back to
  `EntityType::Table` and names the entity after a directory, so a stored
  procedure reads as `Table Users.sp_NewMCRIssue` with only `entity.errors` to
  hint otherwise. `parse_sql` asks the SQL instead.

  ```rust
  let parsed = dbd_core::parser::parse_sql(sql)?;
  for entity in &parsed.entities {
      // entity.entity_type, entity.schema, entity.name (qualified),
      // entity.refers / references (typed edges),
      // entity.reads / writes (separated, for routines)
  }
  ```

  Returns `ParsedFile { entities, search_paths, errors }`, holding dbd's own
  `Entity` — the read/write split and the soft/hard reference distinction are
  the parts an embedder cannot get elsewhere, so nothing is flattened. Several
  declarations in one file become several entities; `CREATE INDEX` and
  `COMMENT ON` fold into the entity they *name*, not the nearest preceding one.
  Reference resolution stays in `references::resolve_references`, so the scan
  itself touches no shared state and parallelises.

  `parse_sql_with(ParserChoice, sql)` takes an explicit parser; pair it with
  `ParserChoice::resolve` to derive one from a dialect string.

### Removed

- **The sqlparser DDL path is gone** — `extractors.rs`, `tables.rs`, the
  `preprocess_sql` workarounds, the parser parity gate, and the
  `ParserChoice::Sqlparser` variant. 1,861 lines.

  It was a second *PostgreSQL* parser, not a dialect: `parse_with_sqlparser`
  hardcoded `PostgreSqlDialect` for its entire life. It existed as an escape
  hatch during the libpg_query migration, and the migration's own design note
  named its retirement condition — "once Table is native, `SqlparserDdl` has no
  production callers". Every file-backed entity type has been native since
  0.13.0, so nothing reached it: `PgQueryDdl::native` returns `Some` for all
  eight types, and `EntityType::from_folder_name` cannot produce the four it
  doesn't cover.

  This also removes the last regex in the DDL parse path —
  `extract_proc_reads_writes`, whose own doc comment recorded that it "can
  over-match … and is blind to read/write classification". The libpg_query
  parsers have none.

  `sqlparser-rs` is still a dependency: `dbd format` and enum-candidate
  detection use it. It no longer reads DDL.

### Fixed

- **`ALTER TABLE … ADD CONSTRAINT` was silently dropped from table DDL.** A
  constraint is legitimately written inline *or* as a trailing `ALTER TABLE …
  ADD CONSTRAINT`, and the second form was read by nothing — no parser matched
  `AlterTableStmt`. On a file adding an FK, a UNIQUE and a CHECK that way:

  ```
  errors      : []      ← no error, so `ensure_fully_parsed` did not refuse
  refers      : []      ← the FK was not a dependency edge
  constraints : 0       ← all three gone
  ```

  Three consequences, worst first. The missing `refers` edge let
  `sort_by_dependencies` order a child before its parent, so a fresh `apply`
  could fail. `apply` created the table without the constraints. And
  `reconcile` saw an FK live-but-not-desired and planned
  `DROP CONSTRAINT <live-name>` — gated behind `--allow-destructive`, but a
  project that passes that flag would have lost it.

  dbd never emits this form itself, which is why it survived: it only affected
  a hand-authored file.

  Constraints added this way now go through the same `extract_table_constraint`
  as inline ones — same `TableDef`, same `PRIMARY KEY` column marking, same FK
  edge — so the two spellings are indistinguishable downstream. An `ALTER`
  naming a table the file does not declare is not absorbed.

  Other `ALTER` subcommands remain out of scope: a dbd table file is the full
  and final definition, and `ADD COLUMN` / `ALTER COLUMN` belong to generated
  migrations, which `scanner::scan_ddl` never reads. They now raise a **warning**
  on the entity rather than vanishing — silence is what kept the missing
  constraints invisible.

- **The pre-commit hook gated only the CLI.** `.githooks/pre-commit` ran
  `cargo test` and `cargo clippy` without `--workspace`. The root package is
  `dbd-cli`, deliberately not a workspace member, so both compiled the CLI and
  stopped — `dbd-core` (parser, differ, adapters, 977 tests) was never built.
  The hook printed "All checks passed." over a tree `cargo test --workspace`
  failed with exit 101.

  It now delegates to `make _check-ci`, the same pre-flight `make bump` runs,
  which has had `--workspace` all along. The two were hand-maintained copies of
  one list and drifted; there is one definition of green now, pinned by
  `tests/pre_commit_hook.rs`. Contributor-facing only — CI and `make bump` both
  pass `--workspace`, so no release shipped behind it.

### Changed

- **BREAKING — `source.parser: sqlparser` is rejected.** A project still naming
  it fails to load with a message saying it was removed and naming `pg_query`.
  Silently switching a project to a parser its author did not choose is the
  failure mode `ParserChoice::resolve` already refused for a typo; a retired
  value is held to the same bar.

- **BREAKING — `parser::extract_search_paths` is no longer exported.** It was
  the one public item that leaked `sqlparser::ast::Statement` into `dbd-core`'s
  API. Embedders wanting search paths get them from `Entity::search_paths`.

- **A non-PostgreSQL `source.dialect` now resolves to the PostgreSQL parser**
  rather than to sqlparser. For every project dbd generates this is a no-op —
  `reverse::design_yaml` writes no `source:` block at all, so a project built by
  `dbd init --from-db sqlite://` has always loaded under the `postgresql`
  default. A **hand-written** `dialect: sqlite` previously got sqlparser, which
  reads some SQLite DDL; it now gets libpg_query, which rejects `AUTOINCREMENT`,
  `WITHOUT ROWID` and `STRICT`, so such a project will be refused by
  `ensure_fully_parsed` instead of partially applied. SQLite DDL is not a
  Postgres subset and never parsed correctly here; giving it a real grammar is
  tracked separately.

- **`make install` now reclaims `target/`, matching `make bump`.** `cargo install
  --path .` builds into `target/`, so the bare install left behind roughly a
  gigabyte it had just created — and running it after a release silently undid
  the reclaim the release had performed. Both entry points now share one
  `INSTALL_AND_RECLAIM` block and end in the same state.

  Shared as a plain make variable rather than a recursive `$(MAKE) install`,
  deliberately: make executes any recipe line containing `$(MAKE)` even under
  `-n`, so a recursive call would turn `make -n bump` into a real wipe. Two
  tests pin that by side effect, because the printed recipe looks identical
  either way.

### Security

- **`rustls` 0.23.44 → 0.23.45** — RUSTSEC-2026-0285, "TLS 1.3 handshake messages
  incorrectly accepted across encryption level boundaries" (medium, 5.3). The
  advisory was published 2026-09-14 and the pin predates 0.13.0, so `cargo audit`
  went red on the first push to `main` after it landed rather than on any change
  of ours. Lockfile only — `rustls` reaches the tree transitively through
  `reqwest`/`tokio-rustls`, and nothing in this repo declares it directly.

## [0.13.1] — 2026-09-17

Two reconcile convergence fixes in `dbd-core` (#16, #17), released alongside the
docs-site, CI and repo-metadata work that had been accumulating unversioned. That
work alone left the Rust tree byte-identical to 0.13.0 and so carried no bump;
these two fixes change `dbd-core`, which is what makes this a patch release.

### Security

- **`devalue` 5.8.1 → 5.9.2 in the docs site** — "reject out-of-bounds indices"
  (AIKIDO-2026-869882). The fix was already inside `@sveltejs/kit`'s declared
  `^5.8.1`; only the lockfile was stale. `devalue` is an external import in the
  deployed worker (`output/server/index.js`), so this reaches production, not
  just the build. Exposure was low regardless — every route is prerendered, so
  it only ever parsed build-time-static payloads.

- **`undici` override 7.29.0 → 8.10.2.** The old pin dragged `jsdom@30` off its
  own declared `^8.9.0` and onto the 7 line, and `miniflare` pins 7.29.0
  exactly, which is vulnerable to the 2026-09-04 advisory batch (patched at
  7.29.1 and 8.10.2). Bun does not support nested overrides — it warns and
  ignores them — so one version serves both parents; 8.10.2 is the only fully
  patched release that also satisfies jsdom. Crossing miniflare's major is
  contained: nothing in this repo invokes miniflare, and the deployed worker
  runs on workerd.

- Secret scanning and push protection enabled on the repository. Renovate now
  owns security PRs (`vulnerabilityAlerts` + `osvVulnerabilityAlerts`, the
  latter being what actually covers crates.io) rather than adding Dependabot
  security updates as a second bot on the same job. Every Renovate rule gained a
  7-day `minimumReleaseAge` — the `@rokkit/*` group auto-merges, and automerge
  with no cooldown turns one compromised publish into a merged commit.

### Fixed

- **`reconcile` aborted on any project containing a `STORED` generated column**
  ([#16]). Postgres keeps a `GENERATED ALWAYS AS (…) STORED` expression in
  `pg_attrdef` — the same catalog an ordinary `DEFAULT` lives in — and
  introspection read it as one. Reconcile then saw a default the design never
  declared and planned `ALTER COLUMN … DROP DEFAULT`, which Postgres refuses
  outright (*"column … is a generated column"*), failing the whole run even
  when the column matched the design exactly. Introspection now reads
  `pg_attribute.attgenerated` and both parsers keep the expression on the new
  `ColumnDef::generated`, so the two sides converge. A genuinely changed
  expression now emits the verb Postgres accepts — `SET EXPRESSION AS` (PG17+),
  or `DROP EXPRESSION` when the generation is removed — and the emitter renders
  the `GENERATED … STORED` clause, which stops `reset`/`diff` from silently
  recreating a computed column as a plain one.

- **A `CHECK` or partial-index predicate over a `varchar` column never
  converged** ([#17]). Postgres rewrites such a predicate to add the implicit
  `::text` cast before storing it (`name = lower(name)` becomes
  `(name)::text = lower((name)::text)`), so the design key and the live key
  could never match: the constraint was dropped and re-added with an identical
  definition on every single run. Because those are drops, an otherwise additive
  change could not be applied without also authorising `--allow-destructive`.
  Canonicalization now erases a `::text` cast on a column Postgres
  **binary-coerces** to `text`, which is `varchar` alone — `pg_cast` records it
  as `castmethod = 'b'`, a runtime no-op. `char(n)` is deliberately excluded:
  its cast runs `rtrim1` and strips trailing spaces, so erasing it would change
  what the predicate accepts. A cast on any other column (`n::text` where `n` is
  `integer`) is the author's and still stands.

- **`bun run check` could not run at all.** `svelte-check` 4.x refuses to start
  when the `typescript` package is major 7; the site had been on `typescript@7`
  with nothing in CI to notice. `typescript` is now `~6.0.3` with TypeScript 7
  alongside as `@typescript/native` and `--tsgo` on the script, which is the
  pairing svelte-check documents. It reports 46 files, 0 errors.

- **The `cookie` override was mis-documented.** It was grouped with the
  build-time-only pins, but `cookie` is an external import in the deployed
  worker. It is pinned because `@sveltejs/kit` 2.70.3 still declares `^0.6.0`
  and every 0.6.x carries GHSA-pxg6-pf52-xh8x; 0.7.0 changed no API, so kit is
  safe on it. Renovate is now capped at `<1.0.0` for `cookie`, because 1.0
  delegates quote-parsing to `decode` (kit passes an identity decoder) and 2.0
  renames `parse`/`serialize` outright. No version changed — only the reasoning
  is now recorded and enforced.

### Added

- **CI gates the docs site** — `bun install --frozen-lockfile`, `check`, `test`,
  and a build with `CF_PAGES=1` so it exercises the adapter that actually ships.
  Nothing ran the site before, which is how the broken type-check survived. The
  frozen lockfile is the gate, not a speed-up: it fails when `bun.lock` and
  `package.json` disagree, which is exactly how `devalue` went stale.

- **CodeQL** over `rust`, `javascript-typescript` and `actions`. The `actions`
  pack is deliberate — this repo pins every action to a SHA by hand, and that
  pack checks the posture mechanically.

- **`sensei.library.json` completed against the manifest spec** ([#13]):
  `llms` (pointing at `/llms-full.txt`, the 1103-line corpus, not the 147-line
  summary), `documents`, `ref`, `ecosystem`, `packages` and `install`. The
  `install` block names `dbd install`, the command that exists — the issue's
  suggested `dbd skills add <name>` was a template from another library.
  `make bump` now rewrites `documents` and `ref`, so they cannot silently rot
  into claiming docs describe a release they do not.

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
[#13]: https://github.com/sensei-hq/dbd/issues/13
[#16]: https://github.com/sensei-hq/dbd/issues/16
[#17]: https://github.com/sensei-hq/dbd/issues/17
[Unreleased]: https://github.com/sensei-hq/dbd/compare/v0.13.1...main
[0.14.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.14.0
[0.13.1]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.1
[0.13.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.0
