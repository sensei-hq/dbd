# Checkpoint

**Slice:** v0.14.0 shipped (one parser, `parse_sql`, ALTER-ADD-CONSTRAINT).
SQLite round-trip fixed on top, unreleased.

## Done

- **v0.14.0 released** — tag pushed, crates.io published, develop merged to
  main, CI + CodeQL green. Registry artifact verified against both repros:
  0.13.1 emits no `Ref:` edge for an ALTER-added FK and orders the child before
  the parent; 0.14.0 emits the edge and orders correctly. `source.parser:
  sqlparser` loads on 0.13.1, exits 1 on 0.14.0.
  - sqlparser DDL path retired (`3bffa7f`, 2,062 lines) — last regex out of the
    DDL parse path.
  - `parse_sql` (`a20156b`) — statement-level identity for embedders, issue #19.
  - `ALTER TABLE … ADD CONSTRAINT` (`1c73fd1`) — was silently dropped on the
    live apply path.
  - Commit gate fixed (`6b69fa2`) — ran `cargo test` without `--workspace`, so
    it never compiled dbd-core.
- **#20 SQLite round-trip** (`12ea8f2` red, `f7062aa` green), **unreleased**.
  `dialect: sqlite` → `ParserChoice::Verbatim` (file kept in `raw_ddl`,
  applied as written), mirroring what `SqliteAdapter::introspect` already does.
  Verified end-to-end against in-memory SQLite: export → apply → re-introspect →
  compare definitions.
  - Found while verifying: `reconcile`/`diff` reported `added=0 altered=0
    dropped=0` against a **completely empty** database. `Design` now carries its
    `ParserChoice` and both refuse a verbatim project.

- **Uncompilable doc examples fixed** (`c66dba9`) — every documented
  `Design::apply` / `import_data` call showed the three progress callbacks as
  three arguments; both take five, callbacks in one `Progress`. Root cause was
  `apply`'s own doc comment; six surfaces had copied it, including the live
  site. `Design::apply` now carries a doctest, so `cargo test --doc` compiles
  the canonical example — there were no doctests on `Design` at all, which is
  why CI never caught it.

- **`project::survey` + `survey_json`** (`71a5da8` red, `c80295e` green) — is a
  directory a dbd project, and what is in it? Config + layout, no SQL parsed.
  Reports dialect, `ParserChoice`, three file lists, and exclusions with
  reasons. `Ok(None)` for "not a project", `Err` for "broken project".
- **Project-version default named** (`db912dd`) — `ProjectConfig::version()` →
  `DEFAULT_PROJECT_VERSION` (1), used by `release` and `survey`. `dbd merge`
  deliberately keeps its own floor of 0 (a permissive safety gate, not the same
  question); documented both sides and pinned by a test asserting BOTH values.

## Sensei integration — measured, not assumed

`sensei/crates/senseid` **already depends on `dbd-core` at tag v0.14.0**.

- Its **PostgreSQL** half reads through dbd (`the_postgres_corpus_reads_through_dbd`
  — "the reading is dbd's").
- Its **T-SQL** half is its own: `indexer/lang/sql/{lex,tsql,postgres}.rs`,
  ~1,500 lines, with a corpus gate. **dbd should not build T-SQL** — that would
  be a second weaker implementation of something that works, which is the
  mistake this whole release undid.
- `adapters/manifest/dbd.rs` re-implements a partial `design.yaml` reader
  (`stated_dialect -> Option<String>`). `project::survey` replaces it and
  upgrades the answer to a real `ParserChoice` + file list + exclusions.
- **Their measured finding dbd should adopt:** a change script is not a
  declaration. `CREATE` declares; `ALTER`/`DROP` *refer*. Over their corpus
  `ALTER TABLE` outnumbers `CREATE TABLE` 159:101. dbd's `parse_sql` currently
  **drops** an ALTER naming an undeclared table — correct inside a dbd project,
  but on a foreign corpus it discards more edges than it keeps.

## Next

Nothing queued. Candidates:

- Cut **0.14.1** for the SQLite fix + doc fix (`[Unreleased]` has both; no
  breaking changes, so patch).
- `parse_sql` on external files still ignores non-constraint `ALTER`s. Correct
  inside dbd's full-and-final table contract; a real gap for a foreign corpus.
  Needs a scope decision, not a patch.
- Structured SQLite model on both sides, if drift detection on SQLite is ever
  wanted. Starts at `SqliteAdapter::introspect`. Measured coverage if taken up:
  SQLiteDialect 13/13, sqlparser-Pg 9/13, libpg_query 5/13.

      cargo test --workspace --all-features

## Open questions

None blocking.

## Known-broken / carried forward

- `diff`/`reconcile` unavailable on SQLite — now refused explicitly rather than
  falsely reporting clean.
- `ARRAY[col]::t[]` where the column is already type `t` still reads as drift.
- `generate_data_sql` warns "may truncate data" on a *widening* cast.
- `docs/design/architecture.md:360` still lists `is_identity: bool` on
  `ColumnDef`; now `identity: Option<IdentityKind>`, and predates `generated`.
- 31 pre-existing rustdoc intra-doc-link errors (not gated by CI).
- `.cargo/audit.toml` ignores RUSTSEC-2023-0071 (`rsa` via `sqlx-mysql`).
