# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html); while
the crates are `0.x`, the **minor** position is the breaking one, so
`0.12.x → 0.13.0` may require changes in code that embeds `dbd-core`.

## [Unreleased]

### Added

- **A file's references are no longer thrown away** ([#21]). `ParsedFile` grows
  a `references` field — `reads`, `writes`, `calls` — carrying what the file
  referred to outside any declaration it makes.

  The statement-head walk attributed every reference to the most recent
  declaration in its batch and **dropped** anything made before there was one.
  The reasoning was half right: attaching a reference to whatever happens to be
  declared next *would* fabricate an edge. But "it belongs to the file" is a
  third answer, and dbd had nowhere to put it.

  Measured over a 2,154-file T-SQL corpus: the walk calls `refer()` **43,754**
  times and was discarding **20,929 of them (47.8%)**. An independent reader
  over the same corpus found 43,737 references — within 0.04% — so nothing was
  being missed in extraction; it was being dropped at the last step. Two-thirds
  of the loss was 278 pure data scripts, where the references are the whole
  content of the file.

  Deduplicated per file, as entity references already were, that is **4,059
  file-level references**, total 10,695 → 14,754 (+38%). The number that
  matters: **817 of 2,154 files (38%) went from reporting nothing at all to
  reporting something.**

  Nothing is attached to an entity that did not make it. Filled in by the
  `TSql` and `MySql` readers; the PostgreSQL reader leaves it empty, and that is
  not an omission — libpg_query hands back a statement list where a function
  carries its body as one node, so a reference cannot float outside its
  declaration there.

  One hypothesis was measured and rejected rather than built: re-attaching a
  later `ALTER TABLE x` batch to an `x` declared earlier in the same file is
  worth 182 references of the 20,929.

- **A reference says whether its schema was written or guessed** ([#22]).
  `Reference::schema_source` and `ForeignKey::ref_schema_source`, carrying
  `SchemaSource::{Stated, Inferred, Resolved}` (`is_guess()` for the usual
  question).

  The PostgreSQL reader qualifies a bare `REFERENCES parent` with the first
  entry on the entity's `search_path`. The result — `app.parent` — is the same
  string a source that wrote `app.parent` produces, and nothing recorded which
  it was. `resolve_references` corrects a bad guess, but it needs every entity
  in the scan, so a consumer reading one file at a time cannot run it and had
  no way to tell a confident edge from an invented one.

  T-SQL and MySQL never infer — an unqualified name is reported unqualified —
  so everything those readers produce is `Stated`.

- **The resolver no longer re-points a schema the source wrote** ([#22]).
  `recover_bare_target` used *"the schema equals `default_schema`"* as a proxy
  for *"the parser guessed this"* — its own comment called it "the parser's
  bare-qualification marker". A table that deliberately writes `app.parent`
  while its own `search_path` is `app` satisfies that test, so its explicit
  qualification could be silently re-pointed at another schema on the path that
  happened to hold a table of the same name. It now asks the recorded fact.

  Found while implementing the above, not reported.

### Changed

- **`FileKind::Empty` documents what it does and does not mean.** It means
  "declares nothing, changes nothing, moves no rows" — not "says nothing". A
  read-only script lands there and now reports what it reads (140 references
  across 109 such files in the corpus). The variant is not renamed: `"empty"`
  is the serialized value callers match on.

[#21]: https://github.com/sensei-hq/dbd/issues/21
[#22]: https://github.com/sensei-hq/dbd/issues/22

## [0.16.0] — 2026-09-25

**MySQL** joins PostgreSQL, T-SQL and SQLite: `source.dialect: mysql` selects
the same statement-head walk T-SQL uses, under rules that differ where the two
dialects genuinely disagree. It is fixture-verified rather than
corpus-measured, and says so.

The rest of this release is about the documentation, which had been drifting
for want of anything that reads it. `Design::apply`'s example was wrong on six
surfaces at once — every one of them showing seven arguments to a method that
takes five — and nothing noticed, because nothing compiled them. Now three
gates do: every Rust example in the embedder-facing docs is compiled as a test
target, the facts the guides state are checked against the code that decides
them, and broken doc links are `deny`-ed at the crate root. All three found
real defects on their first run.

The design document got the same treatment by hand. It had become a second,
wrong copy of the source — 16 of 40 field declarations inaccurate, three types
gone, and a dependency listing with no `pg_query` in it.

### Fixed

- **Doc examples that did not compile.** Found by the gate below on its first
  run, which is the point of it:
  - `Progress` and `ApplyComplete` were used without being imported, on four
    surfaces. A reader copying any of them got an unresolved-name error.
  - `design.report()` takes `&mut self`, and both `SKILL.md` copies wrote
    `let design` — while `llms-full.txt` correctly wrote `let mut design`. Two
    surfaces documenting the same call, disagreeing.

### Added

- **Every Rust example in the embedder-facing docs is now compiled**
  (`tests/doc_examples.rs`). Extracted from README, both `SKILL.md` copies and
  `llms-full.txt` into a committed file that cargo builds as a test target, so
  an example that does not typecheck is a build failure. A digest of the
  extracted blocks is embedded, and a second test fails if a doc changed
  without regenerating — compiling a stale copy would prove nothing about what
  users read.

  This exists because `Design::apply`'s examples were wrong on **six** surfaces
  at once and nothing noticed for want of anything compiling them. Verified by
  reverting one example to the old 7-argument form: the build fails with the
  original error, `this method takes 5 arguments but 7 arguments were supplied`.

  Scope is deliberate — `architecture.md`'s 34 blocks are design prose, not
  code to copy. A block opts out with ` ```rust,ignore `.

- **Broken documentation links are now an error.** Twenty-four had accumulated
  — links to items since made private, links to items that no longer exist,
  `<type>` read as an HTML tag, bare URLs. Each reads correctly in the source;
  only rustdoc knows it does not resolve.

  All twenty-four fixed, and the lints (`broken_intra_doc_links`,
  `private_intra_doc_links`, `invalid_html_tags`, `bare_urls`) are now
  `deny`-ed at both crate roots. A `deny` in the source rather than a flag in
  CI, so it travels with the crate: a contributor running `cargo doc` locally
  gets the same failure the pipeline does.

  `cargo doc` runs once per push in CI, and before publish in the release
  workflow — docs.rs builds after publish, and a publish cannot be undone.
  Deliberately **not** in the inner loop: a doc build is slow, and a broken
  link is not worth blocking a commit on.

  One scoped exemption, in `src/cli.rs`, with the reason on it: every doc
  comment there is a clap help string, so `dbd --help` is its first reader.
  `REFRESH MATERIALIZED VIEW [CONCURRENTLY]` is how Postgres writes an optional
  keyword and `<dir>/<name>.<fmt>` is how a CLI shows a path template —
  satisfying rustdoc would put backticks in what users see.

- **Facts the docs state are checked against the code**
  (`tests/docs_match_code.rs`): the two `SKILL.md` copies are byte-identical,
  every `source.parser` value the guide lists is one the resolver accepts, the
  guide's dialect→reader table matches `for_dialect_typed`, the readers the
  guide says cannot be diffed are the ones that produce no `table_def`, and
  every scaffolded `ddl/` folder is named somewhere a reader will look.

  Not wording — facts with one right answer. A test that pins a sentence breaks
  on a harmless rewrite and teaches people to delete tests.

- **MySQL is read.** `ParserChoice::MySql`, selected by `source.dialect: mysql`
  (or `mariadb`) and by `Dialect::detect`. The same statement-head walk as
  T-SQL under different rules — the walk is what every SQL dialect has in
  common; what differs is small, specific, and wrong the other way round:

  | | MySQL | T-SQL |
  |---|---|---|
  | `ALTER PROCEDURE` | refers — changes characteristics only | declares — carries the body |
  | `a.b` | `database.object` (no schemas) | `schema.object` |
  | quoting | `` `name` `` | `[name]` |
  | `#` | line comment | starts a temp-table name |

  `a.b` landing in `Entity::catalog` rather than `schema` is what keeps two
  databases' `users` tables from merging into one entity.

  **Fixture-verified only.** The T-SQL reader was measured against 2,154 real
  files; no MySQL corpus was available, so this is tested against cases its
  author thought of rather than against a codebase. The `#[ignore]`d corpus
  gate will measure it when one turns up.

- **`lex::LexRules`** — the lexer is no longer dialect-blind, because two
  dialects disagree about the same character. `#` starts a line comment in
  MySQL and a temp-table name in T-SQL: read one way in the other's file and
  either every comment becomes a phantom table, or every temp table swallows
  the rest of its line.

### Changed

- **`docs/design/architecture.md` no longer describes types that do not
  exist.** It had drifted into a second, wrong copy of the source: of 40 field
  declarations it listed, 16 were inaccurate, three types were gone entirely,
  and the dependency section reproduced all three manifests — claiming
  workspace version `0.1.0` against a released 0.15.0, a `dbd-core`
  requirement of `0.12.2`, features (`supabase`, `convex`) that were never
  built and a `rusqlite` dependency never taken, with **no `pg_query` in the
  listing at all** — the crate that reads every line of DDL dbd parses.

  Every type listing is now prose about what the type is *for*, every manifest
  is a link, and what remains is rationale a manifest cannot carry. Four
  copyable examples joined the compile gate above; the 21 illustrative ones are
  fenced ` ```rust,ignore `. The 18 end-to-end scenarios are now Gherkin — they
  are requirements, and a requirement written as Rust rots when the API moves,
  which is exactly what happened to everything else on this list.

  Net 693 lines deleted against 405 added. No behaviour changed; this is the
  document catching up with eleven releases of code.

## [0.15.0] — 2026-09-25

dbd reads more than PostgreSQL. **T-SQL** is read by a statement-head lexer —
2,737 entities and 11,602 edges from a corpus where libpg_query managed 13
declarations and a 94.5% parse-error rate. **SQLite** round-trips: a project
`init --from-db` exported could not be read back at all, because dbd wrote no
`source:` block and then rejected its own `AUTOINCREMENT`. And **16.2% of a
real SQL Server corpus was invisible** to `std::fs::read_to_string`, which
failed a whole project load rather than one file.

For callers outside dbd: `project::survey` answers "is this a dbd project, and
what is in it" without parsing anything, and `parse_sql` reads entities out of
SQL that is not in dbd's layout at all.

Two things the measurements changed. `reconcile` and `diff` reported **"in
sync" against an empty database** on any project without a structured model —
they now refuse and say why. And every documented `Design::apply` example was
uncompilable; there were no doctests on `Design` at all, which is why nothing
caught it.

Breaking for embedders: new `EntityType` and `ParserChoice` variants, and new
fields on `Entity` and `ParsedFile`.

### Fixed

- **One UTF-16 DDL file failed the whole project load.** `Design::from_config`
  read DDL with `std::fs::read_to_string` and *propagated* the error, so a
  single UTF-16 file under `ddl/` aborted the load — not that file, the load. A
  project authored in SQL Server Management Studio could not be opened at all.

  Every path that reads user-authored SQL now decodes through `source_text`:
  the project scan, RLS policy files, lifecycle hook scripts, migration SQL and
  data SQL. Measured against the corpus, the files dbd cannot read fell from
  **391 to 14**, and the share it can classify rose from 76.2% to 91.8%.

- **A SQLite project exported by dbd could not be read back by dbd** (#20).
  `dbd init --from-db sqlite://…` writes `sqlite_master.sql` into `ddl/`
  verbatim — `AUTOINCREMENT`, `WITHOUT ROWID` and `STRICT` included — but
  `reverse::design_yaml` emitted no `source:` block, so the project loaded under
  the `postgresql` default and libpg_query rejected all three. `apply` then
  refused with *"N file(s) could not be parsed"*. The round-trip dbd advertises
  did not work at all.

  `source.dialect: sqlite` is now written by `init --from-db`, and it selects a
  **verbatim** reader: the DDL file is kept as-is in `Entity::raw_ddl` and
  applied unchanged. That is not a weaker fallback — it is the shape SQLite
  already has on the other side, where `SqliteAdapter::introspect` builds each
  entity from `sqlite_master.sql` with no `table_def` because that text *is* the
  schema. Reading the files any other way made the two sides disagree about what
  a table is.

  Verified end-to-end against a real in-memory database: export a schema,
  re-apply it to an empty database, and compare the definitions both sides
  report — not just the names, since an empty table matches on names alone.

- **`reconcile` and `diff` reported "in sync" for a SQLite project** — against a
  database sharing not one table with the design. A verbatim entity has no
  `table_def`, and both snapshot builders keep only entities that have one, so
  desired and live each reduced to nothing and the comparison succeeded
  trivially. Observed: `added=0 altered=0 dropped=0` against a completely empty
  database.

  Both now refuse, naming the reason, as they already did for batch adapters.
  "In sync" is the one answer that must never be wrong. `apply`, `deploy`,
  `import` and `export` are unaffected.

- **Every documented `Design::apply` / `import_data` example was uncompilable.**
  They showed the three progress callbacks as three separate arguments; both
  methods take five, with the callbacks travelling together in one `Progress`.
  A 7-argument call does not compile.

  The root cause was `apply`'s own doc comment — *"Use `|_| {}` / `|_, _| {}` /
  `|_| {}` when progress reporting is not needed"* — and six downstream surfaces
  had copied the misreading: `README.md`, both `SKILL.md` copies,
  `docs/design/architecture.md` (twice), `docs/llms/llms-full.txt`, the live
  site, and the design mockups.

  All corrected, and `Design::apply` now carries a **doctest**, so `cargo test
  --doc` compiles the canonical example on every run. There were no doctests on
  `Design` at all, which is why CI never caught this. Verified by mutation:
  changing the doctest back to the 7-argument form fails with *"this method
  takes 5 arguments but 7 arguments were supplied"*.

### Added

- **`project::survey` — is this a dbd project, and what is in it?** The cheap
  counterpart to `Design::from_config`: reads the config and walks the layout,
  parses no SQL. For a caller walking a repository, that ordering matters —
  decide *whether* to parse a directory, and with which parser, before paying
  to parse anything.

  ```rust
  if let Some(s) = dbd_core::project::survey(Path::new("."))? {
      for file in &s.ddl_files {
          let sql = std::fs::read_to_string(file)?;
          let entity = dbd_core::parser::parse_entity_with(s.parser, file, &sql)?;
      }
  }
  ```

  Takes a project directory **or** a config path (`dbd -c` accepts a config
  under any name, so recognising only `design.yaml` would disagree with the
  CLI). Reports the project name, version, dialect, the `ParserChoice` that
  dialect resolves to, schemas, and three separate file lists — `ddl_files`,
  `policy_files`, `import_files`. Policies are SQL but not entity definitions,
  so folding them into the DDL list would invent entities.

  "Not a dbd project" is `Ok(None)`, not an error — a scanner meets far more
  non-projects than projects. A `design.yaml` that cannot be read is `Err`, and
  the distinction is deliberate: collapsing them means a malformed project is
  silently skipped as "not dbd".

  **What was excluded is reported, with a reason.** `migrations/` and
  `snapshots/` hold generated SQL — a scanner that indexed them would report
  every historical version of a table as a live entity — and
  `ddl/procedure/staging/import_jsonb_to_table.ddl` is dbd's own plumbing.
  Nothing absent is reported: a project with no generated output has an empty
  exclusion list.

- **`project::survey_json`** — the same as JSON, with `managed` as an explicit
  field rather than "object vs null", and a `reason` when the answer is no.
  `parser` is spelled as `source.parser` accepts it, so the value round-trips
  back into a config.

- **T-SQL is read.** `ParserChoice::TSql`, selected by `source.dialect: tsql`
  (or `mssql`/`sqlserver`) and by `Dialect::detect`. A statement-head walk over
  the token stream: `CREATE PROCEDURE [dbo].[sp_X]` declares; `FROM
  [dbo].[Issues]` refers.

  Measured over 2,154 real T-SQL files, against libpg_query's 13 declarations
  and 94.5% parse-error rate on the same input:

  | | |
  |---|---|
  | entities declared | **2,737** (1,285 procedure, 613 table, 506 view, 220 function, 113 trigger) |
  | edges | 8,558 reads, 1,828 writes, 1,216 calls |
  | files classified | 100% — a lexer has nothing to reject |

  Three dialect-specific rules do the work. **`ALTER PROCEDURE` declares** —
  T-SQL requires it to carry the complete body, so it replaces rather than
  edits (271 files ship procedures that way); `ALTER TABLE` never does.
  **A qualified call is an edge and a bare one is a built-in**, because T-SQL
  *requires* a scalar UDF to be schema-qualified — the distinction is read off
  the grammar rather than a list of built-in names that would go stale. And a
  `DROP x` naming something the same file declares is the **redeploy idiom**,
  not a change: counting it as one put 54.5% of the corpus in `Mixed`, and
  resolving it correctly moved 558 files to `Declaration`.

  A T-SQL entity carries **no `table_def`** — this reads statement heads, not
  column lists — so `diff` and `reconcile` cannot run on T-SQL, the same
  position SQLite is in and for the same reason.

- **`EntityType::Trigger`** — 107 trigger files in the measured corpus, so
  reporting one as a `Function` would be a visible lie. `CREATE TYPE` and
  `CREATE SYNONYM` are deliberately *not* modelled: 3 files each.

- **`parser::lex` — a SQL tokeniser.** Batches, comments, quoting; nothing
  else. The first half of reading T-SQL, and a lexer rather than a grammar
  because dbd measured the alternatives on a 2,154-file corpus and none of them
  can read the statements it wants. After splitting `GO` batches — the most
  generous way to ask — `sqlparser`'s `MsSqlDialect` loses **99% of
  `CREATE PROCEDURE`, 100% of `ALTER PROCEDURE`, 95% of `CREATE TABLE`**. It
  passes `SET`, `IF EXISTS` and `INSERT`, so an 81.9% batch-level pass rate
  hides a near-total loss of exactly the facts a reader is reading for.
  Microsoft's ScriptDom is complete and is .NET, a runtime dependency dbd does
  not have.

  Over the same corpus the lexer reaches **99.57% of batches** (83 of 19,299
  yield no tokens), producing 2.5M tokens of which 1.16M are names.

  `GO` is separated before anything reads a batch: it is a client directive,
  not SQL, so no grammar accepts it. Comments and literals are *consumed* —
  a table named in a comment is not a reference, and a real corpus is full of
  commented-out SQL. Nested block comments, `[bracket]]escapes]`, `''` in
  literals, and `@p`/`@@ROWCOUNT`/`#temp` consumed whole so their tails never
  lex as phantom tables.

- **`ParsedFile::kind` and `ParsedFile::dialect`, plus `parse_sql_as`.** A SQL
  codebase is mostly not declarations — sensei measured `ALTER TABLE`
  outnumbering `CREATE TABLE` 159 to 101 — and a change script that minted an
  identity for the table it alters would give a caller two nodes for one table.
  `FileKind` tells "owns this entity" from "touches it":

  | | |
  |---|---|
  | `Declaration` | declares entities, changes nothing it does not declare |
  | `Migration` | `ALTER`/`DROP` on objects defined elsewhere — edges, not nodes |
  | `Data` | `INSERT`/`UPDATE`/`DELETE`/`MERGE`/`COPY` |
  | `Mixed` | declares *and* changes something else |
  | `Empty` | nothing dbd recognises |

  A declaration's **own** index and comment are part of it, not changes to
  something else — otherwise every ordinary dbd table file would land in
  `Mixed`.

  `parse_sql_as(dialect, sql)` is the multi-dialect entry point; pair it with
  `Dialect::detect`. The result records `Unstated` when nothing identified the
  file, rather than claiming the fallback reader's dialect as the file's own.

- **`source_text` — decoding a file before any parser sees it.** `std::fs::
  read_to_string` rejects anything that is not UTF-8, and SSMS writes UTF-16LE
  by default. Measured over a real SQL Server corpus of 2,421 `.sql`/`.ddl`
  files: **377 UTF-16 with a BOM (15.6%) and 14 other non-UTF-8 (0.6%)** —
  16.2% invisible before any grammar was involved.

  A BOM is a positive statement of encoding and is read **first**, because
  UTF-16LE ASCII is `X 00 X 00` and any null-byte test would otherwise call
  every UTF-16 file binary. The BOM is then *consumed*: a parser handed
  `\u{feff}CREATE` reports a syntax error on line 1 of a valid file.

  No BOM means UTF-8 is required. Charset detection — guessing latin-1 from
  byte frequencies — is deliberately not done: `NotUtf8` is already the
  actionable answer, and guessing invents characters the source never carried.
  A lossy decode is refused for the same reason, since U+FFFD in an identifier
  is a name no use site could mint.

  Ported from sensei's `classifiers::decode_source`, which reads the same trees
  and had measured the same split.

- **`parser::Dialect` — which SQL a file is, stated or detected.** Distinct
  from `ParserChoice`, which is which reader dbd *runs*: several dialects share
  a reader, and a dialect dbd has no reader for still has a name.
  `ParserChoice::for_dialect_typed` is the single place one becomes the other,
  so a config label and a detected dialect can never select different readers
  for the same SQL.

  `Dialect::detect` **fails closed**. `CREATE TABLE t (id int)` is valid in
  every dialect and says nothing about which one it is in, so it is `Unstated`
  — not a default, and not a guess. A tie between two dialects is `Unstated`
  too. Markers are ported from sensei's SQL indexer, where they were scored
  against a real multi-dialect corpus.

  Nothing changes for existing projects: an unrecognised `source.dialect` still
  falls back to libpg_query rather than erroring.

- **`Entity::catalog` and `Entity::qualified_key()`** — the database level,
  for telling two same-named tables in different databases apart.

  `None` for PostgreSQL, always: cross-database references are impossible on
  one connection, so the name would distinguish nothing. `Some` for T-SQL and
  MySQL, where `OtherDb.dbo.Users` is an ordinary reference and MySQL's
  `db.users` puts the *database* where dbd's model expects a schema. Without
  the level, `dbo.Users` in two databases is one entity and a multi-database
  scan merges them silently.

  `resolve_references` now keys on `qualified_key()` (`catalog.schema.name`, or
  `schema.name` without one). A reference naming no catalog resolves within the
  **referring entity's** catalog first, then against a catalog-less entity —
  mirroring how a bare schema already resolves along `search_path`. One naming
  a catalog is taken at its word, and stays unresolved if that catalog is not
  in the scan rather than falling back to a local table of the same name.

  Invisible to every existing project: with no catalog anywhere the key *is*
  the name, so the resolution set is byte-identical. `catalog` is
  `skip_serializing_if = "Option::is_none"`, so snapshots neither churn nor
  need migrating.

- **`config::ProjectConfig::version()` and `DEFAULT_PROJECT_VERSION`** — one
  answer to "what version is this project" when `design.yaml` omits it: **1**.
  `dbd release` used `unwrap_or(1)`, so that behaviour is unchanged; the value
  now has a name and a home.

  `dbd merge`'s version-safety gate deliberately keeps its own floor of **0**
  and is unchanged. It is not asking what version the project is — it is
  choosing how permissive to be, and a project declaring no version has made no
  claim to be ahead of any database. Flooring it at 1 would refuse an ordinary
  first merge, since a managed database with no row for this project reports 0.
  The difference is now documented on both sides and pinned by a test, so it
  cannot be "tidied up" into a bug.

- **`source.parser: verbatim`** — selects the verbatim reader explicitly.
  `dialect: sqlite` implies it; the override exists for anything else whose DDL
  should be applied as written.

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
[Unreleased]: https://github.com/sensei-hq/dbd/compare/v0.16.0...main
[0.16.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.16.0
[0.15.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.15.0
[0.14.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.14.0
[0.13.1]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.1
[0.13.0]: https://github.com/sensei-hq/dbd/releases/tag/v0.13.0
