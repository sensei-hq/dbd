# The schema viewer

`dbd diagram` opens the project in a hosted, interactive viewer: an overview, an
ER diagram, a page for every entity, and a changelog built from your snapshots.
Nothing is installed and nothing is uploaded — the schema travels inside the
link.

```sh
dbd diagram                                  # build the model and open it in your browser
dbd diagram --print-url                      # print the link instead (headless machines, CI)
dbd diagram --scope hub                      # only the hub scope's working set and its history
dbd diagram --site http://localhost:5173     # another viewer (or set $DBD_DIAGRAM_URL)
dbd diagram --json -f schema.json            # write the model as JSON instead
```

The viewer is at `https://dbd.sensei-hq.com/diagram` unless `--site` or
`$DBD_DIAGRAM_URL` says otherwise.

## Privacy: the model lives in the URL

`dbd diagram` builds the schema model, gzips it, base64url-encodes it and puts it
in the URL **fragment** — `…/diagram#1.<payload>`. Browsers never send the
fragment to a server, and the viewer decodes it in the page, so the schema stays
on your machine. The link is self-contained: anyone you send it to sees the
same schema, and no one else can.

For a schema too large for a comfortable link, write the JSON with
`--json -f schema.json` and **drop the file onto the `/diagram` page**. There is
no file picker or paste; drag and drop is the way in.

### What the browser remembers

The viewer keeps a list of the designs you have opened, at `/projects` ("Your
designs"). Opening a link or dropping a file saves that design in the browser's
`localStorage`, under `dbd:diagram:<project name>`, as the same encoded payload
the link carries — so a later link for the same project replaces the earlier
one. Each card on `/projects` has a remove button ("Remove from this browser").
The only other thing stored is your light/dark choice (`dbd-theme`). Nothing is
sent anywhere, and `/projects` is excluded from crawling (robots.txt) and marked
`noindex`.

## The root view

The viewer opens on the project, with four tabs:

| Tab | Shows |
|---|---|
| **Overview** | The database, the model version, counts — schemas, tables, each kind of view, routine and sequence present, enums, references — the rest of the project note as markdown, the three most recent changes, and a table of schemas |
| **Diagram** | The ER diagram: tables and their foreign keys, tinted by schema, with a schema legend. A foreign key into a table the model does not carry lands on that table's stub |
| **Entities** | Every table, with its column and reference counts and its comment |
| **Changelog** | What each snapshot changed — see [below](#the-changelog) |

The project note is `project.note` in `design.yaml`; its first paragraph heads
every root tab, and the Overview shows the rest.

On the **Diagram** tab, drag the background to pan and use ctrl/⌘ + scroll to
zoom. The controls choose the detail level (names, keys, or all columns) and
the edge style (curved or angled). Click a table to open it.

## The sidebar

Every entity is listed in the sidebar, grouped by schema: tables, views,
materialized views, functions, procedures, triggers, sequences and enums. A box at the top
finds an entity by name, and — when the project has more than one kind — a row
of toggles filters by kind, each showing its count. The project name at the top
returns to the root view.

## Entity pages

Selecting an entity opens its own page.

**A table** has three tabs. **Details** shows its comment, its fields (name,
type, settings — `PK`, `FK`, `NN`, `UNIQUE`, `ENUM`, `IDENTITY`, `GENERATED` —
default, references and notes; an identity or generated column shows how it is
generated where a default would be), the foreign keys in and out — one that
lands on a stub says which kind — its dependencies (what reads, writes or calls
it), its indexes with a partial index's `where` predicate, and its **Checks**,
each CHECK constraint's expression. **Diagram** draws the table's
neighbourhood, one to three levels out. **Changelog** lists the versions that
changed it.

**A stub** — a table a foreign key lands on that the model does not carry —
opens the same way, from the key or the diagram. Its page says why it is a
stub: declared under `external:`, a table of the project that the scope leaves
out, or referenced and defined nowhere in the project. It shows only the
columns the keys land on. Stubs are drawn in the ER diagram, not listed in the
sidebar.

**A view, materialized view, function, procedure, trigger or sequence** has the
same three tabs. Details shows its comment, what it **uses** and what it is
**used by** — a sequence is used by the tables whose defaults draw from it; a
dependency on something the project does not define (a built-in like `now()`)
is shown dimmed. Its diagram is its neighbourhood in the dependency graph.

**An enum** has Details — its values, in declaration order, and the columns
that use it — and Changelog.

Triggers appear only for projects read by the T-SQL or MySQL reader, and only
when the trigger is the first statement of its own file (say, under
`ddl/procedure/`). There is no `ddl/trigger/` folder — dbd refuses one — and a
`CREATE TRIGGER` that follows a table in the same file stays part of that file,
as it always does in a PostgreSQL project.

The viewer keeps no URL per page: the address only ever holds the model, so
reloading a link returns to its overview. A design you dropped as a file is not
in the address at all — reopen it from `/projects`.

## The changelog

The changelog is built from `snapshots/` when `dbd diagram` runs:

- The first snapshot is the **baseline** — "History begins here", with its
  table and enum counts.
- Each later version lists the tables and enums it added, removed or modified,
  and expands to the column, index, constraint and enum-value edits.
- A change `dbd snapshot` cut in stages — a rename, a type change — is one
  entry spanning its versions (`v3–4`), read as the rename or type change.
- A type spelled two ways (`varchar(32)`, `character varying(32)`) is not a
  change.

Snapshots record **tables and enums only**, so views, routines, triggers and
sequences have no history; their changelog tab says so. A project has history
once it has snapshots — written by `dbd release`, `dbd snapshot`,
`dbd init --from-db` and `dbd merge` — and with `--scope` the history covers the
scope's working set, the `deps: include` closure too, so it matches what the
diagram draws. If a snapshot cannot be read, `dbd diagram` warns and opens the viewer
without a changelog rather than show one with a hole in it.

## The model

`dbd diagram --json` writes the model the viewer reads — `SchemaModel`, from
`dbd_core::schema_model`. Being JSON rather than DBML, it covers more than
tables, and it states its own `version`, currently `3`:

| Field | What it holds |
| --- | --- |
| `version` | The model version — `3` |
| `project` | Name, dialect and note |
| `schemas` | Each schema the model draws something in — a table, an enum, a view, a routine or a sequence — with its table and enum counts |
| `tables` | Tables, with columns (`pk`, `nn`, `en`, `fk`, `uq`, type, default, comment, and `identity` — `always` or `by default` — or `generated`, the expression), indexes (`def`, `unique`, `name`, and `where` for a partial index) and `checks` (`{name?, expression}`) |
| `refs` | Foreign keys — the ER edges, column-level, as `{s, t, c}` — every one, landing on a table or a stub |
| `stubs` | The tables `refs` land on that `tables` does not carry: table-shaped, only the referenced columns, `kind` `external`, `out_of_scope` or `unresolved` — omitted when there are none |
| `entities` | Views, materialized views, functions, procedures, triggers and sequences |
| `deps` | `{from, to, kind}` — what reads, writes, calls, uses (a column's enum or sequence) or is a member of what |
| `history` | What each snapshot changed, oldest first — omitted when there are no snapshots |
| `enums` | Enum types with their values in order and their comment — omitted when there are none |

`entities` and `deps` are separate from `tables` and `refs` on purpose: an ER
renderer wants foreign keys, a call-graph renderer wants dependencies, and
folding them together would change what every existing consumer of `tables`
receives. Stubs are kept apart from `tables` for the same reason: a consumer
counting the model's tables is not handed tables it does not own. Each table and
stub carries a `kind` — `table`, or the stub's reason. A dependency whose target
is not a project entity is marked `"unresolved": true` rather than dropped.
Comments reach every kind: for tables, views, routines, sequences and enums
`note` is the first line and `noteMd` the full text; a column's `note` is its
whole comment.

The fields 0.25 added — `stubs`, sequences, `checks`, `where`, `identity`,
`generated` — are optional, so every earlier version-3 model still reads. Models
of version 1 and 2 open too, with the panels their fields allow. A model of a
version newer than the viewer reads is refused, with a message to open it on the
current site, rather than shown with whatever the new version added silently
missing.
