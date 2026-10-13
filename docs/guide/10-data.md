# Data: import and export

`dbd import` loads data files into staging tables and then runs the procedures
that move the rows into place; `dbd export` writes table data back out to
files. Both are driven by folder conventions, so most projects need no
configuration at all.

## The `import/` convention

```
import/
  staging/
    lookups.csv               → staging.lookups, every environment
    lookup_values.jsonl       → staging.lookup_values, every environment
  dev/
    staging/
      fixtures.csv            → staging.fixtures, only under -e dev
```

A file at `import/<schema>/<table>.<ext>` loads into `<schema>.<table>`. The
extension picks the reader:

| Extension | Read as |
|---|---|
| `.csv` | CSV with a header row that names the columns the values load into |
| `.tsv` | The same, tab-separated |
| `.jsonl` | JSON Lines — one JSON object per line |
| `.json` | JSON — a JSON array of records, or JSON Lines; a file whose first character is `[` is an array. On Convex, an array only |

Anything else under `import/` — a `notes.txt`, a `README.md` — is ignored, and
so is anything outside `import/`: data under `data/` or `seeds/` is never
scanned.

On PostgreSQL, CSV and TSV stream through `COPY`. JSON and JSON Lines records
are staged one at a time as bound `json` values in the project's own
`dbd."import_tmp:<project>"` (a name past 63 bytes ends in a hash) and moved
into the table by `dbd.import_jsonb_to_table` — a procedure the load installs
itself, in dbd's own `dbd` schema beside the bookkeeping, so a JSON import works
on a database no import has touched and creates no schema your design does not
declare. (Before 0.25 both lived in a schema called `staging`, whatever your
staging schemas were called. A database an earlier dbd imported JSON into keeps
its `staging.import_jsonb_to_table`; drop it if nothing else uses it.) On SQLite
every format loads through batched `INSERT`s. Data files are read as UTF-8; a
UTF-16 CSV is not decoded.

### CSV and TSV

Both engines read a CSV or TSV file the way PostgreSQL's `COPY … (FORMAT csv)`
reads one — RFC 4180:

- The **header row names the columns** the values load into, matched as
  PostgreSQL resolves a name: exactly, or folded to lower case, so `Name` loads
  `name`. The file's column order need not be the table's, and a column the
  header leaves out gets its default.
- A header naming a column the table does not have, or naming one twice, is
  refused, and that file loads nothing:
  `the header names 'nick' but staging.people has no such column — its columns are name, age, feeling`.
- A **quoted** field may hold the delimiter, a line break and a doubled quote
  (`"Smith, ""Jo"""`).
- An unquoted field equal to `null_value` — by default an empty one — is NULL.
  A quoted field never is: `""` is the empty string.
- Nothing is trimmed, values or header — a header written `id, name` names a
  column ` name`, and is refused.
- CRLF line ends and a byte-order mark (spreadsheet exports start with one) are
  handled.

Before 0.25 a PostgreSQL load skipped the header and loaded by position, so a
file whose columns were in another order put values in the wrong columns, and
SQLite split on commas and trimmed, so a quoted comma was refused and quotes
were kept.

### JSON and JSON Lines

A record's keys name the columns its values load into, as a CSV header does —
so a key that names no column is refused rather than dropped, which is what
happened to it before 0.25:

```
the key 'nick' (record 1) names no column of app.items — its columns are id, name, qty, doubled, meta. A JSON record's keys name the columns its values load into
```

- A key no record carries leaves the column to its default. A key some records
  carry and another lacks loads SQL NULL, on PostgreSQL, in the record that
  lacks it; SQLite inserts record by record, so there the record that lacks it
  gets the default.
- `null` is SQL NULL, in every column. To load the JSON value `null` into a
  `json` or `jsonb` column, write `{"$json": null}`: a one-key `{"$json": …}`
  object is unwrapped one level, so a value that is itself such an object is
  written `{"$json": {"$json": …}}`. This is how `dbd export` writes them (see
  [export](#export)).

### What a data file cannot set

A **generated column** is computed from the row, so a file that sets one — a CSV
header or a JSON key naming it — is refused, and `dbd export` never writes one:

```
doubled of app.items is generated — computed from the row, so a data file cannot set it. Leave it out of the file; dbd export writes no generated column
```

**Ids load as given**, an identity column's included — `GENERATED ALWAYS` too —
so a file exported from one database keeps its keys in another. On PostgreSQL
the load then moves each identity or `serial` sequence past the largest id it
loaded, so the next insert does not collide with a loaded row. That is a
`setval`, so the importing role needs `UPDATE` on the sequence; without it the
run fails, naming the table and the sequence. SQLite's `AUTOINCREMENT` continues
past the loaded ids by itself.

### Environment folders

A file directly under `import/<schema>/` loads in every environment. A file one
level deeper, under `import/<env>/<schema>/`, loads only when the run's
environment is `<env>`:

```sh
dbd import            # -e prod is the default: import/prod/… loads, import/dev/… does not
dbd import -e dev     # import/dev/… loads as well as the shared files
```

The environment is `dev` or `prod`, so the folders are `import/dev/` and
`import/prod/` (`-e development` and `-e production` are accepted and select
those same folders). A run that leaves files out says so —
`3 data file(s) under import/dev/ skipped — this run's env is 'prod'` — so a
deploy that seeded nothing is never mistaken for one that seeded everything.

`dev` and `prod` at the top of `import/` are always environment folders, so a
file directly in `import/dev/` loads only under `-e dev`, on every engine. That
is where a project without schemas — SQLite's — keeps its environment-only
data, beside the shared files directly in `import/`; before 0.25 such a file
loaded in prod. It follows that a schema named `dev` or `prod` cannot keep its
files directly there.

Those are the only layouts an import reads. A file in a folder deeper than
them — `import/staging/old/x.csv`, `import/dev/staging/old/y.csv` — never
loads, under any env, and an import names it:

```
⚠ import/staging/old/x.csv not loaded — an import reads import/<schema>/<table>.<ext>, or import/<env>/<schema>/<table>.<ext> for env dev or prod, and no folder deeper
```

Before 0.25 such a file was taken for an environment's: the first was reported
as skipped for env `staging`, as though some `-e` could load it, and the second
loaded into `staging.y` under `-e dev`, its `old` folder ignored.

**A dev import into a production database is refused.** The database records
the env it was built for, and `dbd import -e dev` against one marked prod stops
before anything is truncated — it would load `import/dev/` seed data into
production:

```
Error: Safety guard: environment guard: this database is marked prod, but this run is -e dev.
```

The rest of that message says how to rebuild a database that is not production
but was marked prod by a run without `-e` — see
[the environment](04-commands.md#global-options). `dbd import -n X -f file`, which
loads one file you name rather than an environment's folder, is refused the
same way: it still loads data into production. A project whose `project.name`
was renamed is held to the mark its old name left, as every guard holds it
(see [the scope guard](07-scopes.md#the-scope-guard)).

## What a run does

```sh
dbd import --dry-run
```
```
  import staging.lookups (csv) ← ./import/staging/lookups.csv

  call staging.import_lookups()
1 entities — no issues
```

In order:

1. **Truncate** every staging table in the plan (unless `truncate: false`) —
   on PostgreSQL in one `TRUNCATE`, since it empties tables linked by a foreign
   key only together; on SQLite with `DELETE FROM`, one table at a time, a
   table that references another first. A table outside the import that holds
   a key to one of them fails the run, named, before anything is emptied. On
   Convex the load itself replaces the table.
2. **Load** each data file into its table — a staging table another one
   references before the table that references it. On SQLite, whose DDL dbd
   reads verbatim and so knows no references from, that order — and the
   truncate's — comes from the foreign keys the database reports; `import
   --dry-run` lists it when the database exists, and alphabetically when there
   is none to ask.
3. **Call the import procedures** — each staging table's one loader, once (see
   below). The plan is ordered by dependencies, so a procedure that fills
   `config.lookups` runs before one that fills `config.lookup_values`.
4. **Run `import.after`** scripts.

Before step 1, the run refuses what it cannot do safely, so nothing is
truncated by a run that was going to fail: a `-n` that selects no data file, a
data file outside [`import.staging`](#options), a data file whose target is a
view, a materialized view or a sequence, a staging table whose loader
cannot be decided, and — under `--scope` — a staging table the scope keeps whose
loader procedure it leaves out (see [below](#nothing-is-skipped-silently)).

It refuses the database, too, when it is the wrong one: a `-e dev` import into a
database marked prod ([above](#environment-folders)), and a database
[pinned](07-scopes.md#the-scope-guard) to a scope other than the run's —

```
Error: scope guard: this database is pinned to scope 'hub', but you requested 'all'.
Importing under another scope would load tables this database was not built with.
→ re-run with --scope hub, or re-point the database with `dbd apply --scope all --allow-scope-change` first.
```

— because the tables it would load are another scope's to build, and a run on
such a database used to fail partway, with staging already truncated and some
loaders called. `import` never re-pins a database, so it has no
`--allow-scope-change`; `import -n X -f file` is checked the same way.
`import --dry-run` checks both whenever a database is configured, so the
preview refuses where the run would; with none it stays offline.

### Which procedure loads a table

The candidates for a staging table are the procedures whose body *reads* it,
found by analysing the body — which tables it reads and which it writes. Each
table gets exactly one, chosen in this order:

1. the procedure named `import_<table>` — the convention, and the way to settle
   anything the rules below cannot;
2. else the only candidate that reads no other staging table;
3. else the only candidate no other table claimed by rules 1 and 2 — and, when
   that leaves several, the one that writes a table of the same name.

A loader that joins a second staging table — cities looking up their country in
`staging.countries` — is therefore the cities loader, not the countries one,
and `staging.countries` keeps its own. A procedure loads at most one table and
is called once per import, however many data files its table has (a shared one
and an environment's own). The choice is made over every data file, so
`dbd import -n staging.cities` calls the loader a full run would.

Two candidates still equal at the deciding rule is refused before anything
runs, naming them:

```
cannot tell which procedure loads staging.items: staging.load_a, staging.load_b have an equal claim. An import calls one loader per staging table — name it import_items, and call the others from it or from import.after
```

Only a **procedure** is a loader — the step is `CALL`, which PostgreSQL refuses
for a function — so a function is never run. A function that reads a staging
table no procedure loads is usually a loader written as a function, and the run
names it in a warning: make it a procedure, or run it from `import.after`. A
procedure that reads a staging table without being its loader is named too
(`P reads X but is not called — L is its loader`): call it from the loader, or
from `import.after`.

Dynamic SQL (`EXECUTE format(…)`) is invisible to that analysis; a procedure
whose staging read happens only inside dynamic SQL is not matched.

### `import.after`

Scripts listed under `import.after` run once the data is loaded and the
procedures have run. They run on **`dbd import` and `dbd deploy`** — the two
commands that run the import phase — and never on `dbd apply`, which does not
import. Post-DDL setup that should happen whenever the schema is applied
(publications, grants) belongs in `apply.after` instead. See
[Hook scripts](03-design-yaml.md#hook-scripts).

## Options

```yaml
import:
  staging: [staging]       # the schemas an import may load; omit for no restriction
  options:
    truncate: true         # default: empty each staging table before loading it
    null_value: ''         # default: an empty CSV cell is NULL
  tables:
    - staging.legacy_export:
        null_value: '\N'   # this table's files mark NULL as \N
        truncate: false
        format: tsv        # parse as TSV whatever the extension says
  after:
    - import/loader.sql
```

- **`staging`** lists the schemas an import may load into. A load truncates its
  table first, so a data file whose target schema the list leaves out is
  refused — every such file named in one error — before anything is truncated,
  on `import`, its dry run, `import -n X -f file`, `deploy` and its dry run.
  Leave it out, or empty, for no restriction. Before 0.25 it was parsed and
  ignored, and a file under `import/app/` emptied and reloaded the live `app`
  table.
- **`null_value`** is the string that means SQL NULL in an unquoted CSV or TSV
  cell. The default `''` makes an empty cell NULL. Set a sentinel such as `\N`
  when your files must tell an empty string from NULL: then only the sentinel is
  NULL and an empty cell is an empty string. A quoted cell is never NULL, so
  `""` is the empty string under any setting.
- **`tables:`** holds per-table overrides of `truncate`, `null_value` and
  `format`. It is not a list of what to import — it does not filter which files
  load.
- **`truncate`** (default `true`) empties each staging table before it loads:
  `TRUNCATE` on PostgreSQL, `DELETE FROM` on SQLite, which has no `TRUNCATE`.
  Convex imports with `npx convex import --replace`, which always replaces the
  table, so there `truncate: false` has no effect (see [targets](08-targets.md)).

Two keys are parsed but not read, so they change nothing:
`import.options.format` (the format comes from the file extension, or a
per-table `format`), and a per-table `env` (the environment is the folder).

## Loading one file

```sh
dbd import -n staging.lookups                                # one table, from import/
dbd import -n activity.hook_events -f ~/data/events.jsonl    # one table, from any file
```

`-n` narrows the run to one table but otherwise runs it as usual —
`import.after` included, and the table's loader the one a full run would call.
It must select a data file this run loads; otherwise the import is refused
before anything runs, saying which way it missed:

```
no entity named 'staging.nope'
staging.batches has no data file under import/ — nothing to import (a table's data file is import/<schema>/<table>.csv, .tsv, .json or .jsonl)
no data file for staging.batches in this run: import/dev/staging/batches.csv belongs to another env, and this run's env is 'prod'
```

Before 0.25 that was a warning, and the run exited 0 having still run
`import.after`.

`-f` loads exactly the file you name into the `-n` table, and nothing else: no
truncate, no procedures, no `import.after`. Its format comes from the extension
— `.jsonl`, `.json` (an array of records or JSON Lines), `.tsv`, and anything
else is read as CSV. `import.staging` applies to it as to every load. (On
`export`, `-f` is the *format*.)

Under `--scope`, a table the design declares must be one the scope builds —
otherwise `-f` refuses with `X is outside scope 'S'`. A table the design does not
declare is an ad-hoc target that no scope can contain, so it loads, with a
warning that the scope was not applied to it.

## Nothing is skipped silently

The summary always states the table count, including `0`, followed by the
reason for anything left out:

| Reason | Warning |
| --- | --- |
| No `import/` directory | `no import/ directory — nothing to import` |
| File belongs to another environment | `N data file(s) under import/<env>/ skipped — this run's env is '<env>'` |
| File in a folder no import reads | `F not loaded — an import reads import/<schema>/<table>.<ext>, …, and no folder deeper` |
| A file failed to parse | `staging table X not imported — data file failed to parse` |
| Entry cut by `--scope` | `X not imported — T is outside scope '<name>'` |
| A function reads a staging table no procedure loads | `X has no loader procedure — function F reads it but is not run` |
| A procedure reads a staging table it is not the loader of | `P reads X but is not called — L is its loader` |

Under `--scope`, an entry runs only where both of its halves can: every table
its procedure writes must be in the scope, and so must its staging table when
the design declares one (a staging table with no DDL of its own is not the
scope's to judge). An entry with no procedure needs only its own table. The
warning names whichever table is outside — the staging table or a target — and
`import --dry-run` and `deploy --dry-run` print the same lines the real run
does, `import.after` hooks the scope skips included.

An entry the scope keeps but whose **loader** it leaves out is refused instead,
before anything is truncated — and, on `deploy`, before anything is applied. A
database built with that scope has no such procedure, so the run would truncate
and load staging and only then fail on the `CALL`:

```
import refused: scope 'imp' leaves out the loader of these staging tables, so a database built with it has no such procedure:
  staging.items → call staging.import_items()
Add each loader to the scope, or leave its staging table out of the scope too.
```

The summary counts tables, not files — a table with a shared data file and an
environment's own is one table loaded:
`Import complete — 1 table(s) loaded, 1 procedure(s) called, 1 after script(s) run.`

## Export

```sh
dbd export                                # every table, or what export: lists
dbd export -f jsonl                       # another format: csv (default), tsv, jsonl
dbd export -n config.lookups -f tsv       # one table
dbd export -o import                      # into the import tree: import/<schema>/<table>.<fmt>
dbd export -n core.accounts -o import/staging  # into one schema folder: import/staging/accounts.csv
dbd export --scope hub                    # only what the scope builds
```

What is written:

- **No `export:` list** — every table in the design (within the scope).
- **An `export:` list** — only what it names: tables, and views or materialized
  views. `-n` then narrows that list.
- **A `-n` that selects nothing is an error**, not an empty export:
  `X is not listed under export: in design.yaml, so it is not exported`, or,
  without a list, `X is not a table — without an export: list, only tables are
  exported`. A name the design does not have is `no entity named 'X'`, and one
  the scope leaves out `X is outside scope 'S'`.
- **An entry that names nothing exportable is reported** —
  `export: X is not a table or view in this design — nothing exported for it` —
  and the rest of the list still exports.
- **An entry the scope leaves out is named** —
  `export: X is outside scope 'hub' — not exported` — so a file you expected is
  never missing without a word.

The run counts what it writes by kind — `Exporting 2 table(s) and 1 view(s)...`
— rather than calling a view a table.

```yaml
export:
  - config.lookups
  - config.lookup_values:
      format: jsonl        # this table always exports as JSON Lines
```

A per-table `format` wins over `-f`. The formats are `csv`, `tsv` and `jsonl`;
any other is refused (`cannot export X as json: Postgres exports csv, tsv or
jsonl`) rather than written as CSV under another name.

Files land at `export/<schema>/<table>.<fmt>` in the project (`-s`), wherever
you run from — on SQLite, which has no schemas, `export/<table>.<fmt>`. `-o <dir>`
puts the same layout under `<dir>` instead, a relative `<dir>` taken from the
current directory, so `-o import` writes straight into the
[import tree](#the-import-convention). The layout is the one `import/` reads, and
the schema folder is what keeps two same-named tables apart: before 0.25 `-o`
wrote every file flat into `<dir>`, and the second of two `lookups` tables
overwrote the first while the summary counted both.

The exception is a `<dir>` that is itself a schema folder of the project's
import tree — `import/<schema>`, or `import/<env>/<schema>`. There each file is
written `<table>.<fmt>`, as that import loads it: `-n core.accounts -o
import/staging` writes `import/staging/accounts.csv`, which an import loads
into `staging.accounts`. Under the usual layout it would have landed in
`import/staging/core/`, a folder no import reads. Two tables that would be the
same file there — `core.accounts` and `staging.accounts` — are refused before
anything is written. An import loads every file there into that one schema, so
export into the folder, with `-n`, the one table whose rows the file should
hold — a second export there overwrites the first.

Generated columns are left out of every export: the import recomputes them, and
refuses a file that sets one. CSV and TSV carry a header row — on SQLite even for
a table with no rows — and are written as PostgreSQL's `COPY` writes them, on
SQLite too: NULL is an empty field, `''` is `""`, and a value holding the
delimiter, a quote or a line break is quoted with its quotes doubled. A SQLite
BLOB is written as `\x` and its bytes in hex, as PostgreSQL writes a `bytea`,
in every format — so it loads into a `bytea` column, and back into a BLOB
column; before 0.25 every BLOB was exported as NULL.

A JSON Lines export writes every column's key in every record. `null` is SQL
NULL, whatever the column's type, and a `json` or `jsonb` column holding the
JSON value `null` is written `{"$json": null}` — the spelling the import
unwraps (see [JSON and JSON Lines](#json-and-json-lines)) — so SQL NULL and the
JSON value `null` both come back as they left, and no column is left to its
default because its key went missing.

`export` refuses a database [pinned](07-scopes.md#the-scope-guard) to a scope
other than the run's, before it writes a file — `Exporting under another scope
would read tables this database was not built with.` — since it would otherwise
fail on the first table that scope never built, with some files already written.
Like `import`, it never re-pins, so it has no `--allow-scope-change`.

**A listed view exports like a table** — the rows its query returns, in its
column order. Unlisted, the default stays every table. The next section is what
that is for.

Convex refuses `export`: its CLI dumps whole deployments only (`npx convex
export`).

## Round trips

What `export` writes — CSV, TSV or JSON Lines — `import` reads back. Exporting
straight into the import tree makes the live data the seed data:

```sh
dbd export -n staging.lookups -o import
# writes import/staging/lookups.csv — the file staging.lookups is loaded from
```

Point `-o` at `import/` itself and each table lands in its own schema's folder;
point it at one schema's folder, `import/staging`, and every table exported
lands there, as the files that schema's tables load from.

The values come back as they left. A CSV or TSV export keeps NULL and the empty
string apart and quotes what needs quoting, and the import reads it by its
header — so a file exported from PostgreSQL loads into SQLite, and one exported
from SQLite into PostgreSQL. A JSON Lines export round-trips too: each row is
written as the JSON Postgres renders for it, so a value holding a quote, a
backslash or a newline loads back as it was; a `json` column keeps its exact
text, a JSON string scalar stays a string, and the JSON value `null` stays apart
from SQL NULL. Ids, `GENERATED ALWAYS` identities included, import as given,
and generated columns are recomputed.

### Moving a long-lived system to a new database

A database that has run for years has data its seed files never knew about:
rows edited by hand, by the application, by support. To move such a system to a
new database with dbd, the import side is already the right shape — staging
tables that carry natural keys, and import procedures that resolve them into
foreign keys. What is needed is an export in that same shape.

The pattern is a set of views that dereference each foreign key back to the
natural key the staging table expects — `lookup_values` joined to `lookups` so
each row carries the lookup's *name* rather than its id — listed under
`export:` so that exporting from the old database produces files that match the
staging tables column for column. Importing those files into the new database
then runs the same procedures a fresh seed does.

An import loads a file into the table its folder and name give, so export the
views straight into the staging schema's folder: `-o import/staging` writes
each one there as `<view>.<fmt>`. Name each view after the staging table it
feeds, and its columns after that table's columns: the import loads a CSV by
its header, so the names must match, though the order need not.

```yaml
export:
  - staging_export.lookups
  - staging_export.lookup_values
```

```sh
dbd export -d $OLD_DATABASE_URL -o import/staging   # import/staging/lookups.csv, lookup_values.csv
dbd deploy -d $NEW_DATABASE_URL                     # staging.lookups and staging.lookup_values load from them
```

Not `-o import`: that keeps each view under its own schema's folder,
`import/staging_export/`, and an import refuses a file whose target is a view
before it truncates anything. Listing `import.staging: [staging]` refuses it
too.

None of this is needed when the import files are the well-managed source of
truth — when every change to reference data goes through them.
