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
| `inspect`, `graph`, `combine` — the files concatenated, with the schema statements in the project's dialect: for T-SQL a guarded `CREATE SCHEMA`, and every entity's DDL closed with `GO` (SQL Server needs `CREATE VIEW`, `PROCEDURE` and `TRIGGER` first in their batch; a file that already ends in `GO` keeps its own); none for MySQL and SQLite | `diff`, `reconcile` — nothing to compare; they refuse and say why |
| `doctor` | `emit` — nothing to translate |
| `apply`/`deploy` on SQLite | `dbml`, `diagram` — they run, but with no tables to draw |
| | `snapshot` — it records tables from their structure, so it records none |
| | `apply`/`deploy` for T-SQL or MySQL — there is no adapter; a `mysql://` or `sqlserver://` URL is refused by name |

The refusal names the reason. For a SQLite project:

```
diff needs a structured model, and this project's DDL is not read into one (source.dialect: sqlite) — its files give identity and references but no columns or constraints, so there is nothing to compare. `apply`, `deploy`, `import` and `export` work normally.
```

For a T-SQL or MySQL project the last sentence says what works instead —
there is no adapter for SQL Server or MySQL, so it names `inspect`, `graph`,
`combine`, `dbml` and `diagram`, which work offline. The CLI connects first,
though, so with a `sqlserver://` or `mysql://` URL the refusal you see is the
missing adapter: `Error: Failed to connect to database: Config error: no
adapter for SQL Server: …`.

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

Every reader of a DDL file decodes it this way — `apply`, `deploy`, `combine`,
`format`, `doctor`, `merge` and the migration readers as well as the scan — so a
UTF-16 file applies like any other. `dbd format` and `dbd inspect --fix` write a
file back in the encoding they read it in: an SSMS file stays UTF-16 behind its
byte-order mark, and a UTF-8 byte-order mark is kept. Data files under `import/`
are read as UTF-8 only.

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

`-f` and `--report` must resolve inside the project — a bare name resolves
against the project root — and a path outside it is refused rather than
overwriting a file dbd does not own.

The source must be a PostgreSQL project — the only dialect dbd reads into
columns. A T-SQL, MySQL or SQLite project is refused, and so is
`--dialect postgresql` (use `combine`). `--scope` limits the output to the
scope's entities.

### What is emitted

- **Tables**, with their columns, `NOT NULL`, defaults, primary keys, unique
  constraints, foreign keys, `CHECK` constraints, indexes (`CREATE INDEX`) and
  comments where the target keeps them. A foreign key and a `CHECK` keep their
  names (`CONSTRAINT [people_amt_ck] CHECK (…)`); a primary key and a unique
  constraint are written unnamed. Identity and `serial` columns are numbered by
  the target; generated columns are computed by it.
- **Views** and **materialized views**, as `CREATE VIEW`. A materialized view
  becomes a plain view, which is reported, and each of its indexes is reported
  and left out. On MySQL and SQL Server the `SELECT` is passed through
  untranslated — and reported, since anything PostgreSQL-specific inside it has
  to be checked by hand. On SQLite it is written again from PostgreSQL's parse
  tree (see [below](#sqlite-views-and-checks)).
- **Sequences.** SQL Server gets `CREATE SEQUENCE`, every bound resolved to
  PostgreSQL's and spelled out (SQL Server's own defaults start at the type's
  minimum), and a `nextval('s')` default becomes `NEXT VALUE FOR`. MySQL and
  SQLite have no sequences: the sequence is reported, an integer key that drew
  from it is numbered by the table instead, and any other column loses the
  default — each reported.
- **Schemas.** SQL Server gets a `CREATE SCHEMA` for every schema the script
  uses, `dbo` aside; MySQL and SQLite fold the schema into the name.
- **Functions, procedures and triggers** are skipped and reported: their bodies
  do not translate. That includes a trigger written in a table's file, after its
  `CREATE TABLE`.
- **Extensions, roles, grants and RLS policies** have no equivalent the script
  could carry, so each is reported as not emitted: an extension or a role by
  name, grants once per schema (with the roles and privileges), and policies
  once per `policies/` file. Enum *types* produce no statement of their own;
  their columns become text (below).
- A file dbd could not parse is reported as missing — `a table whose file could
  not be parsed -> nothing` — rather than left out without a word.

A generated column's expression is passed through untranslated and reported on
every target, a partial index's predicate on SQL Server and SQLite, and a
`CHECK` expression on MySQL and SQL Server — the same way a view body is there. An index
the target would reject outright is left out and reported, so
the script still applies: an expression key on SQL Server, and on MySQL or SQL
Server a key on a column the target holds as unbounded text or binary — `text`,
a `varchar` with no length or one past the target's limit, `bytea`, `json`. MySQL has no
partial indexes, so there one becomes an index over every row (reported). A
foreign key to a table the script does not create — declared `external:`, or
left out by `--scope` — is left out and reported; the script has to apply on its
own.

**T-SQL batches.** SQL Server needs `CREATE SCHEMA` and `CREATE VIEW` each alone
in a batch, so the T-SQL script ends every statement with `GO` — the batch
separator `sqlcmd` and SSMS read. `GO` is not T-SQL: a driver that sends the
script in one call must split it on the `GO` lines first.

### SQLite views and CHECKs

Copied verbatim, a PostgreSQL view or `CHECK` breaks a SQLite script: SQLite
has no schemas, so `core.cities` names a database that is not attached;
`POSITION('-' IN code)` and `slug ~ '…'` are syntax errors; and a `CREATE TABLE`
that fails takes the whole table with it. So on SQLite dbd writes each view's
query and each `CHECK` expression again from PostgreSQL's parse tree, node by
node, from what SQLite has with the same meaning:

- table names become the names the script creates — `core.cities` reads
  `"core_cities" AS "cities"`;
- `POSITION(a IN b)` becomes `instr(b, a)`;
- PostgreSQL's `NULLS FIRST`/`LAST` order and `LIKE` escape (`ESCAPE '\'`) are
  spelled out, and every result column is named as PostgreSQL names it.

```sql
CREATE VIEW "app_v1" AS SELECT "id" AS "id", instr("name", 'a') AS "p" FROM "app_city" AS "city" WHERE "name" LIKE 'x\_%' ESCAPE '\' ORDER BY "name" NULLS FIRST;
```

The first construct outside that set refuses the whole statement: the view or
`CHECK` is left out and reported with the reason (for a window
`array_agg()`: SQLite has no `array_agg()` dbd can map PostgreSQL's to), and a
view that reads a left-out view is left out with it. Nothing
PostgreSQL-only can reach the script, so it applies; a refusal costs only what
SQLite could not have run. A rewritten statement is still reported, because the
engines differ even where the SQL reads the same (`LIKE` ignores ASCII case,
values compare by column affinity). The output needs SQLite 3.39 or later, for
`RIGHT`/`FULL JOIN`, `NULLS FIRST`/`LAST`, `FILTER` and window functions.

### Downgrades

A construct the target cannot express is emitted as the nearest thing it can,
and the loss is reported: as a one-line comment above the line it affects (one
line even when what it quotes — a comment, an expression — spans several), in
the run's summary, and with `--report` as JSON — except the losses listed below.
A faithful mapping is not reported — a report padded with `integer → INT` is one
nobody reads.

| PostgreSQL | MySQL | T-SQL | SQLite |
|---|---|---|---|
| `integer` / `bigint` / `smallint` | `INT` / `BIGINT` / `SMALLINT` | `int` / `bigint` / `smallint` | `INTEGER` |
| `text`, and `varchar` with no length | `LONGTEXT` | `nvarchar(max)` | `TEXT` |
| `varchar(n)` | `VARCHAR(n)`; past 16,383, `MEDIUMTEXT` or `LONGTEXT` ⚠ | `nvarchar(n)`; past 4,000, `nvarchar(max)` ⚠ | `TEXT` |
| `boolean` | `TINYINT(1)` | `bit` | `INTEGER` |
| `numeric(p,s)` | `DECIMAL(p,s)`, at most 65 digits with 30 after the point ⚠ | `decimal(p,s)`, at most 38 digits ⚠ | `NUMERIC` (⚠ past 15 digits) |
| `numeric` with no precision | `DECIMAL(65,30)` ⚠ | `decimal(38,10)` ⚠ | `NUMERIC` ⚠ |
| `double precision` | `DOUBLE` | `float` | `REAL` |
| `real` / `float4` | `FLOAT` | `real` | `REAL` |
| `char(n)` | `CHAR(n)`; past 255, `VARCHAR(n)` ⚠; past 16,383, text ⚠ | `nchar(n)`; past 4,000, `nvarchar(max)` ⚠ | `TEXT` |
| `timestamptz` | `DATETIME(6)` | `datetimeoffset` | `TEXT` |
| `timestamp` | `DATETIME(6)` | `datetime2` | `TEXT` |
| `date` | `DATE` | `date` | `TEXT` |
| `time` | `TIME(6)` | `time` | `TEXT` |
| `timetz` | `TIME(6)` ⚠ — the offset is not kept | `time` ⚠ — the same | `TEXT` |
| `interval` | `VARCHAR(255)` ⚠ | `nvarchar(255)` ⚠ | `TEXT` ⚠ |
| `inet`, `cidr` | `VARCHAR(49)` ⚠ | `nvarchar(49)` ⚠ | `TEXT` ⚠ |
| `bytea` | `LONGBLOB` | `varbinary(max)` | `BLOB` |
| `jsonb` | `JSON` | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `json` | `JSON` ⚠ — MySQL normalises the document | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| `uuid` | `CHAR(36)` ⚠ | `uniqueidentifier` | `TEXT` ⚠ |
| `text[]` (any array) | `JSON` ⚠ | `nvarchar(max)` ⚠ | `TEXT` ⚠ |
| an enum, or any other type | `VARCHAR(255)` ⚠ | `nvarchar(255)` ⚠ | `TEXT` ⚠ |
| `schema.table` | `` `schema_table` `` ⚠ | `[schema].[table]` | `"schema_table"` ⚠ |
| `text` or a `varchar` with no length, in a key (primary, unique or foreign) | `VARCHAR(255)` ⚠ | `nvarchar(450)` ⚠ | `TEXT` |
| a row wider than the target stores | the widest non-key `VARCHAR`/`CHAR` columns become text ⚠ — 65,535 bytes a row | the widest `nchar` columns become `nvarchar` ⚠ — 8,060 bytes | — |
| identity, `serial` | `AUTO_INCREMENT` (⚠ for `ALWAYS`) | `IDENTITY(1,1)` (⚠ for `BY DEFAULT` and `serial`) | `INTEGER PRIMARY KEY AUTOINCREMENT` (⚠ for `ALWAYS`) |
| a sequence | nothing ⚠ | `CREATE SEQUENCE` | nothing ⚠ |
| `DEFAULT nextval('s')` | the table's `AUTO_INCREMENT` on its numbered key ⚠, else no default ⚠ | `NEXT VALUE FOR` (no default ⚠ when the sequence is not in the script) | `AUTOINCREMENT` on its integer key ⚠, else no default ⚠ |
| a generated column | `GENERATED ALWAYS AS (…) STORED` ⚠ | `AS CAST((…) AS type) PERSISTED` ⚠ | `GENERATED ALWAYS AS (…) STORED` ⚠ |
| `now()`, `current_date` | `CURRENT_TIMESTAMP(n)` at the column's precision, `(CURRENT_DATE)` | `SYSDATETIMEOFFSET()`, `CONVERT(date, SYSDATETIME())` | `CURRENT_TIMESTAMP`, `CURRENT_DATE` |
| `gen_random_uuid()` | `(UUID())` ⚠ — version 1, not random | `NEWID()` | a random v4 expression over `randomblob` |
| a literal default | the literal, without its `::cast` | the same | the same |
| any other default | left out ⚠ | left out ⚠ | left out ⚠ |
| a materialized view | a view ⚠ | a view ⚠ | a view ⚠ |
| a view's query, a `CHECK` | untranslated ⚠ | untranslated ⚠ | written again ⚠, or left out ⚠ |
| comments | table and column `COMMENT`; a view's ⚠ | `MS_Description` on tables, columns and views | none ⚠ |
| a foreign key to a table not in the script | left out ⚠ | left out ⚠ | left out ⚠ |
| extensions, roles, grants, RLS policies | nothing ⚠ | nothing ⚠ | nothing ⚠ |

⚠ = reported. An **enum** column becomes plain text on every target — MySQL's
`ENUM` is not used and no `CHECK` is added — and is reported as an enum whose
allowed values are no longer enforced. That reason is given only for a type the
design declares as an enum; any other type dbd has no mapping for (`point`,
`money`) becomes text too, reported as unmapped. MySQL and SQLite have no
schemas, so the schema is folded into the name with an underscore, and every
reference to the table has to be updated to match.

**Limits.** emit knows each target's documented limits, so a type the target
would refuse is switched or clamped where the target would fail, and reported:
`varchar(5000)` is `nvarchar(max)` on SQL Server, `varchar(70000)` `MEDIUMTEXT`
on MySQL (counted at four bytes a character), `numeric(70,40)` `DECIMAL(60,30)`
on MySQL — the integer digits kept — and `decimal(38,8)` on SQL Server. A row
whose columns add up past the target's row size has its widest non-key columns
moved out of the row. `text` is `LONGTEXT` and `bytea` `LONGBLOB` on MySQL,
because MySQL's `TEXT` and `BLOB` hold 64 KB; date-times keep fractional
seconds as `DATETIME(6)` and `TIME(6)`, a declared precision kept as written.

```sql
-- dbd: `id` was `uuid` — emitted as CHAR(36); MySQL has no UUID column type, so it is stored as its text form
`id` CHAR(36) NOT NULL,
```

Each target numbers one column per table. MySQL numbers only a column that
leads the primary key or a `UNIQUE`, and SQLite only a single-column `INTEGER
PRIMARY KEY`; an identity or `serial` column they cannot number becomes a plain
column, reported — inserts must then supply it.

Some losses are still **not** reported, so check for them by hand:

- **Identity options** — an identity column's `START WITH` and `INCREMENT BY`;
  the target's numbering counts from 1 by 1.
- **Precision and zones** — `timestamptz` loses its time zone on MySQL and
  SQLite, and `varchar(n)` its length on SQLite.
- **Index-key length** — MySQL refuses an index key past 3,072 bytes and SQL
  Server one past 900, and neither limit is checked: a `UNIQUE` on a
  `varchar(3000)` is 12,000 bytes on MySQL.
- **A MySQL `time DEFAULT now()`** is still written `DEFAULT CURRENT_TIMESTAMP`,
  which MySQL refuses on a `TIME` column.
- **A view's column list** — `CREATE VIEW v (a, b) AS …` comes out with the
  query's own column names on every target.

Copied verbatim but reported: a view's query and a `CHECK` on MySQL and SQL
Server, a generated column's expression on every target, and a partial index's
predicate on SQL Server and SQLite.

The MySQL and SQL Server output is written from those engines' documented
syntax and has not been run against a server, so apply it to a scratch
database before trusting it.

The schema-name folds appear in the summary and the report but not as comments
in the file — they have no single line to sit on. The skipped routines and
triggers, and the extensions, roles, grants and policies left out, are each
noted in the file as well, where the statement would have been.

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
