# Other dialects

dbd is built around PostgreSQL, but it reads T-SQL, MySQL and SQLite DDL too, and
`dbd emit` writes a PostgreSQL schema out as MySQL, T-SQL or SQLite. Reading,
connecting and emitting are separate abilities, and each dialect has a
different set of them:

| Dialect | Read its DDL | Apply to a live database | `dbd emit` writes it |
|---|---|---|---|
| PostgreSQL / Supabase | yes — full structure: columns, constraints, indexes | yes | — (it is the source) |
| SQLite | yes — verbatim: the file *is* the model | yes | yes |
| T-SQL (SQL Server) | identity and references, **no columns** | **no adapter** | yes |
| MySQL / MariaDB | identity and references, **no columns** | **no adapter** | yes |
| Convex | — (a code-generation target) | writes `schema.ts` | — |

## Readers

`source.dialect` picks the reader; `source.parser` overrides it, and you should
rarely need it.

```yaml
source:
  dialect: tsql          # or: postgresql, sqlite, mysql
  # parser: tsql         # pg_query | tsql | mysql | verbatim
```

| `dialect` | reader | structured? |
|---|---|---|
| `postgresql`, `postgres`, `supabase` | `pg_query` | yes |
| `tsql`, `t-sql`, `mssql`, `sqlserver`, `sql_server` | `tsql` | no |
| `mysql`, `mariadb` | `mysql` | no |
| `sqlite`, `sqlite3` | `verbatim` | no |
| anything else | `pg_query` | yes |

- **`pg_query`** is libpg_query — PostgreSQL's own grammar — and builds the
  structured model the rest of dbd runs on: columns, constraints, indexes.
- **`tsql`** reads SQL Server statement *heads* — what each file declares and
  what it references — with a lexer rather than a grammar, and reads no column
  lists. Over 2,154 real T-SQL files, `sqlparser`'s SQL Server dialect lost 99%
  of `CREATE PROCEDURE` and 95% of `CREATE TABLE`; Microsoft's complete parser
  is .NET.
- **`mysql`** reads MySQL statement heads the same way, with MySQL's rules:
  `db.table` names a *database* (MySQL has no schemas), `ALTER` never defines an
  object, backticks quote and `#` starts a comment. It is **fixture-verified
  only** — no MySQL corpus was available.
- **`verbatim`** takes each file as written. `dialect: sqlite` selects it, and
  that is a match for SQLite rather than a fallback: `sqlite_master.sql` *is*
  SQLite's schema, and a PostgreSQL parse would reject `AUTOINCREMENT`,
  `WITHOUT ROWID` and `STRICT`.

The `tsql` and `mysql` readers are the only ones that report **triggers**, as
entities of their own — when the trigger is the first statement of its file
(say, under `ddl/procedure/`; there is no `ddl/trigger/` folder). Inside a
project, a file is one entity: a trigger written after its table stays part of
the table's file. Those triggers are the ones the
[schema viewer](11-schema-viewer.md) shows.

`parser: sqlparser`, an earlier PostgreSQL parser, was removed in 0.14.0; a
project still naming it fails to load and says why.

### What works on a project without structure

A T-SQL, MySQL or SQLite project gives dbd identity and references but no
columns, so:

| Works | Refused or empty |
|---|---|
| `inspect`, `graph`, `combine` — the files concatenated, after a PostgreSQL `CREATE SCHEMA IF NOT EXISTS` per schema, which you remove for SQL Server or SQLite | `diff`, `reconcile` — nothing to compare; they refuse and say why |
| `doctor` | `emit` — nothing to translate |
| `apply`/`deploy` on SQLite | `dbml`, `diagram` — they run, but with no tables to draw |
| | `snapshot` — it records tables from their structure, so it records none |
| | `apply`/`deploy` for T-SQL or MySQL — there is no adapter; a `mysql://` or `sqlserver://` URL is refused by name |

The refusal names the reason. For a SQLite project:

```
diff needs a structured model, and this project's DDL is not read into one (source.dialect: sqlite) — its files give identity and references but no columns or constraints, so there is nothing to compare. `apply`, `deploy`, `import` and `export` work normally.
```

For T-SQL and MySQL the CLI connects first, so the refusal you see is the
missing adapter — `Error: Failed to connect to database: Config error: no
adapter for SQL Server: …` — and the last sentence above does not apply.

From Rust, `parser::parse_sql_as(Dialect::TSql, sql)` reads one file in a stated
dialect, and `project::survey` describes a directory without parsing it — see
[Embedding dbd-core](12-embedding.md#parse-sql-without-a-project).

## Encodings

DDL files are decoded before they are parsed. UTF-8 is read as is; a file with
a byte-order mark — UTF-8, UTF-16LE or UTF-16BE, which is what SSMS writes by
default — is decoded by its mark. A file with no mark that is not valid UTF-8
fails the load and names the file:

```
read ./ddl/table/dbo/Orders.sql: not UTF-8, and it carries no byte-order mark saying what it is. Re-encode it as UTF-8 (or UTF-16 with a BOM, which SSMS writes by default).
```

A file with null bytes and no mark is reported as binary — which is also what a
UTF-16 file saved *without* a byte-order mark looks like.

> In 0.24.1, `apply`, `deploy` and `combine` read the SQL they execute without
> that decoding, so a UTF-16 DDL file loads and validates but is **skipped when
> applied**. Re-encode such files as UTF-8 before applying them.

## `dbd emit`

`dbd emit` translates a PostgreSQL project's schema into another engine's DDL.
It is not `combine`, which concatenates this project's own DDL for its own
target.

```sh
dbd emit --dialect mysql -f schema.mysql.sql
dbd emit --dialect tsql -f schema.tsql.sql --report downgrades.json
dbd emit --dialect sqlite --scope hub -f hub.sqlite.sql
```

| Flag | Meaning |
|---|---|
| `--dialect` | `mysql`, `tsql` or `sqlite` (any label `source.dialect` accepts for those) |
| `-f`, `--file` | Destination, default `schema.sql` |
| `--report` | Also write every downgrade as a JSON array |

The source must be a PostgreSQL project — the only dialect dbd reads into
columns. A T-SQL, MySQL or SQLite project is refused, and so is
`--dialect postgresql` (use `combine`). `--scope` limits the output to the
scope's entities.

### What is emitted

- **Tables**, with their columns, `NOT NULL`, defaults, primary keys, unique
  constraints, foreign keys, `CHECK` constraints and indexes (`CREATE INDEX`).
- **Views** and **materialized views**, as `CREATE VIEW` with the `SELECT`
  passed through untranslated — and reported, since anything
  PostgreSQL-specific inside it has to be checked by hand. A materialized view
  becomes a plain view, without its indexes.
- **Functions, procedures and triggers** are skipped and reported: their bodies
  do not translate.
- Schemas, extensions, roles, sequences and enum *types* produce no statement
  of their own.
- A file dbd could not read is left out without a report, so run
  `dbd inspect` first — it exits 1 when something cannot be read.

A `CHECK` expression is passed through untranslated and reported, the same way
a view body is. An index the target cannot express as written — an expression
index, a partial (`WHERE`) index, or one on a column type the target cannot
index — is reported, and passed through, built without its predicate, or left
out, as the target allows.

### Downgrades

A construct the target cannot express is emitted as the nearest thing it can,
and the loss is reported: as a comment above the line it affects, in the run's
summary, and with `--report` as JSON — except the losses listed below. A faithful mapping is not reported — a report padded with
`integer → INT` is one nobody reads.

| PostgreSQL | MySQL | T-SQL | SQLite |
|---|---|---|---|
| `integer` / `bigint` / `smallint` | `INT` / `BIGINT` / `SMALLINT` | `int` / `bigint` / `smallint` | `INTEGER` |
| `text` | `TEXT` | `nvarchar(max)` | `TEXT` |
| `varchar(n)` | `VARCHAR(n)` | `nvarchar(n)` | `TEXT` |
| `boolean` | `TINYINT(1)` | `bit` | `INTEGER` |
| `numeric(p,s)` | `DECIMAL(p,s)` | `decimal(p,s)` | `NUMERIC` |
| `double precision` | `DOUBLE` | `float` | `REAL` |
| `timestamptz` | `DATETIME` | `datetimeoffset` | `TEXT` |
| `timestamp` | `DATETIME` | `datetime2` | `TEXT` |
| `date` | `DATE` | `date` | `TEXT` |
| `bytea` | `BLOB` | `varbinary(max)` | `BLOB` |
| `jsonb`, `json` | `JSON` | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `uuid` | `CHAR(36)` ⚠ | `uniqueidentifier` | `TEXT` ⚠ |
| `text[]` (any array) | `JSON` ⚠ | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| an enum, or any other type | `VARCHAR(255)` ⚠ | `nvarchar(255)` ⚠ | `TEXT` ⚠ |
| `schema.table` | `` `schema_table` `` ⚠ | `[schema].[table]` | `"schema_table"` ⚠ |

⚠ = reported. An **enum** column becomes plain text on every target — MySQL's
`ENUM` is not used and no `CHECK` is added — and is reported as "if it is an
enum, the allowed values are no longer enforced". MySQL and SQLite have no
schemas, so the schema is folded into the name with an underscore, and every
reference to the table has to be updated to match.

```sql
-- dbd: `id` was `uuid` — emitted as CHAR(36); MySQL has no UUID column type, so it is stored as its text form
`id` CHAR(36) NOT NULL,
```

Some losses are **not** reported in 0.24.1, so check for them by hand:

- **Defaults** — a literal is kept as written, including any PostgreSQL cast
  (`'x'::text` stays, and is invalid on the target); `now()`/`current_timestamp`
  and `true`/`false` are mapped; any other default (`gen_random_uuid()`,
  `nextval(…)`) is left out.
- **Identity** — `serial`, `bigserial` and `GENERATED … AS IDENTITY` columns
  become plain integers, without `AUTO_INCREMENT` or `IDENTITY`; a `serial`
  column is also added to the primary key. Generated columns lose their
  expression.
- **Precision and zones** — `timestamptz` loses its time zone on MySQL and
  SQLite, `varchar(n)` its length on SQLite; a bare `varchar` becomes
  `VARCHAR(255)`/`nvarchar(255)` and a bare `numeric` `DECIMAL(65,30)`/
  `decimal(38,10)`.
- **Materialized views** — the materialization and the view's indexes.

The schema-name folds and the skipped routines appear in the summary and the
report but not as comments in the file — they have no single line to sit on.

### The report

`--report` writes a JSON array, `[]` when nothing was downgraded:

```json
[
  {
    "entity": "config.lookups",
    "column": "id",
    "from": "`uuid`",
    "to": "CHAR(36)",
    "reason": "MySQL has no UUID column type, so it is stored as its text form"
  }
]
```

`column` is `null` for a loss that belongs to the whole entity. Downgrades do
not fail the run — `emit` exits 0 — so a pipeline should assert on the
report's length rather than the exit code.
