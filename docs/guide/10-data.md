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
are staged one at a time as bound `jsonb` values in `dbd.import_tmp` and moved
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
   `TRUNCATE` on PostgreSQL, `DELETE FROM` on SQLite. On Convex the load itself
   replaces the table.
2. **Load** each data file into its table.
3. **Call the import procedures** — each staging table's one loader, once (see
   below). The plan is ordered by dependencies, so a procedure that fills
   `config.lookups` runs before one that fills `config.lookup_values`.
4. **Run `import.after`** scripts.

Before step 1, the run refuses what it cannot do safely, so nothing is
truncated by a run that was going to fail: a `-n` that selects no data file, a
data file outside [`import.staging`](#options), and a staging table whose
loader cannot be decided.

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
does.

## Export

```sh
dbd export                                # every table, or what export: lists
dbd export -f jsonl                       # another format: csv (default), tsv, jsonl
dbd export -n config.lookups -f tsv       # one table
dbd export -o import/staging              # flat into a folder: import/staging/<table>.<fmt>
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
jsonl`) rather than written as CSV under another name. Files land at
`export/<schema>/<table>.<fmt>` (on SQLite, `export/<table>.<fmt>`) — relative to
the current directory, not to `--source`; with `-o <dir>` they land flat in that
directory as `<dir>/<table>.<fmt>`. CSV and TSV carry a header row, and are
written as PostgreSQL's `COPY` writes them, on SQLite too: NULL is an empty
field, `''` is `""`, and a value holding the delimiter, a quote or a line break
is quoted with its quotes doubled.

**A listed view exports like a table** — the rows its query returns, in its
column order. Unlisted, the default stays every table. The next section is what
that is for.

Convex refuses `export`: its CLI dumps whole deployments only (`npx convex
export`).

## Round trips

What `export` writes — CSV, TSV or JSON Lines — `import` reads back. Exporting
straight into the import tree makes the live data the seed data:

```sh
dbd export -n staging.lookups -o import/staging
# writes import/staging/lookups.csv — the file staging.lookups is loaded from
```

`-o` writes `<table>.<fmt>` without the schema, so point it at the folder of
the schema the file should load into.

The values come back as they left. A CSV or TSV export keeps NULL and the empty
string apart and quotes what needs quoting, and the import reads it by its
header — so a file exported from PostgreSQL loads into SQLite, and one exported
from SQLite into PostgreSQL. A JSON Lines export round-trips too: each row is
written as the JSON Postgres renders for it, so a value holding a quote, a
backslash or a newline loads back as it was.

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

`-o` names each file after the view, without its schema, so name each view
after the staging table it feeds, and name its columns as that table's columns
— the import loads a CSV by its header, so the names must match, though the
order need not:

```yaml
export:
  - staging_export.lookups
  - staging_export.lookup_values
```

```sh
dbd export -d $OLD_DATABASE_URL -o import/staging   # import/staging/lookups.csv, lookup_values.csv
dbd deploy -d $NEW_DATABASE_URL
```

None of this is needed when the import files are the well-managed source of
truth — when every change to reference data goes through them.
