# Reverse-engineering & workflows

dbd supports two ways of starting and evolving a schema. **Greenfield** — you author the
DDL from scratch and dbd applies it. **Brownfield** — you generate a whole project from an
existing database or a DBML diagram, then evolve it the same way. Both paths converge on the
same `design.yaml` + `ddl/` tree, snapshots, and `dbd apply`, so nothing about the day-to-day
workflow depends on where the project came from.

<svg viewBox="0 0 820 470" role="img" aria-label="dbd greenfield and brownfield workflows. Greenfield: dbd init scaffolds a sample, you edit the ddl folder, then dbd apply, iterating with dbd snapshot and dbd apply. Brownfield: an existing Postgres or SQLite database, or a DBML file, is reverse-engineered with dbd init --from-db or dbd init --from-dbml into a generated project with a baseline snapshot, which you review and format before dbd apply; ongoing database drift is synced back with dbd merge, which is guarded and refuses if the database is behind the project version." xmlns="http://www.w3.org/2000/svg" style="max-width:820px;width:100%;height:auto;font-family:system-ui,-apple-system,Segoe UI,Roboto,sans-serif;color:currentColor">
  <title>dbd workflows: greenfield (author from scratch) and brownfield (reverse-engineer)</title>
  <defs>
    <marker id="dbd-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
      <path d="M0,0 L10,5 L0,10 z" fill="currentColor" />
    </marker>
  </defs>
  <style>
    .lane { fill: none; stroke: currentColor; stroke-opacity: .25; stroke-width: 1; rx: 10; }
    .lane-label { font-size: 13px; font-weight: 700; letter-spacing: .04em; fill: currentColor; }
    .box { fill: none; stroke: currentColor; stroke-width: 1.5; }
    .box-accent { fill: currentColor; fill-opacity: .07; stroke: currentColor; stroke-width: 1.5; }
    .box-text { font-size: 12.5px; fill: currentColor; }
    .mono { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; fill: currentColor; }
    .edge { fill: none; stroke: currentColor; stroke-width: 1.5; }
    .edge-label { font-size: 10.5px; fill: currentColor; fill-opacity: .8; }
    .edge-label-bg { fill: currentColor; fill-opacity: .07; }
  </style>

  <!-- ===== Greenfield lane ===== -->
  <rect class="lane" x="8" y="8" width="804" height="190" rx="10" />
  <text class="lane-label" x="28" y="34">GREENFIELD — author DDL from scratch</text>

  <rect class="box-accent" x="28" y="58" width="150" height="56" rx="10" />
  <text class="box-text" x="103" y="82" text-anchor="middle">scaffold a sample</text>
  <text class="mono" x="103" y="100" text-anchor="middle">dbd init</text>

  <rect class="box" x="238" y="58" width="150" height="56" rx="10" />
  <text class="box-text" x="313" y="82" text-anchor="middle">edit your DDL</text>
  <text class="mono" x="313" y="100" text-anchor="middle">ddl/</text>

  <rect class="box-accent" x="448" y="58" width="150" height="56" rx="10" />
  <text class="box-text" x="523" y="82" text-anchor="middle">apply to the DB</text>
  <text class="mono" x="523" y="100" text-anchor="middle">dbd apply</text>

  <!-- greenfield forward arrows -->
  <path class="edge" d="M178,86 L232,86" marker-end="url(#dbd-arrow)" />
  <path class="edge" d="M388,86 L442,86" marker-end="url(#dbd-arrow)" />

  <!-- iterate loop: snapshot -> apply -->
  <rect class="box" x="448" y="140" width="150" height="40" rx="10" />
  <text class="mono" x="523" y="165" text-anchor="middle">dbd snapshot</text>
  <path class="edge" d="M523,114 L523,136" marker-end="url(#dbd-arrow)" />
  <path class="edge" d="M448,160 H313 V114" marker-end="url(#dbd-arrow)" />
  <rect class="edge-label-bg" x="330" y="123" width="78" height="17" rx="4" />
  <text class="edge-label" x="369" y="135" text-anchor="middle">iterate: re-apply</text>

  <!-- ===== Brownfield lane ===== -->
  <rect class="lane" x="8" y="220" width="804" height="240" rx="10" />
  <text class="lane-label" x="28" y="246">BROWNFIELD — reverse-engineer an existing source</text>

  <!-- sources -->
  <rect class="box-accent" x="28" y="276" width="160" height="50" rx="10" />
  <text class="box-text" x="108" y="299" text-anchor="middle">existing database</text>
  <text class="box-text" x="108" y="315" text-anchor="middle" font-size="11">Postgres · SQLite</text>

  <rect class="box-accent" x="28" y="356" width="160" height="50" rx="10" />
  <text class="box-text" x="108" y="385" text-anchor="middle">DBML file</text>

  <!-- generated project -->
  <rect class="box" x="358" y="316" width="180" height="56" rx="10" />
  <text class="box-text" x="448" y="340" text-anchor="middle">generated project</text>
  <text class="box-text" x="448" y="358" text-anchor="middle">+ baseline snapshot</text>

  <!-- review / format -->
  <rect class="box" x="588" y="276" width="160" height="50" rx="10" />
  <text class="box-text" x="668" y="300" text-anchor="middle">review /</text>
  <text class="mono" x="668" y="316" text-anchor="middle">dbd format</text>

  <!-- apply (brownfield) -->
  <rect class="box-accent" x="588" y="356" width="160" height="50" rx="10" />
  <text class="mono" x="668" y="385" text-anchor="middle">dbd apply</text>

  <!-- source -> generated arrows with command labels -->
  <path class="edge" d="M188,301 C260,301 280,344 352,344" marker-end="url(#dbd-arrow)" />
  <rect class="edge-label-bg" x="206" y="286" width="138" height="18" rx="4" />
  <text class="edge-label mono" x="275" y="299" text-anchor="middle">init --from-db &lt;conn&gt;</text>

  <path class="edge" d="M188,381 C260,381 280,344 352,344" marker-end="url(#dbd-arrow)" />
  <rect class="edge-label-bg" x="206" y="384" width="146" height="18" rx="4" />
  <text class="edge-label mono" x="279" y="397" text-anchor="middle">init --from-dbml &lt;file&gt;</text>

  <!-- generated -> review -> apply -->
  <path class="edge" d="M538,336 C566,330 566,310 584,303" marker-end="url(#dbd-arrow)" />
  <path class="edge" d="M668,326 L668,352" marker-end="url(#dbd-arrow)" />

  <!-- ongoing drift loop: DB drift -> merge -> snapshot -->
  <rect class="box" x="358" y="406" width="180" height="42" rx="10" />
  <text class="box-text" x="448" y="424" text-anchor="middle">DB drift → snapshot</text>
  <text class="edge-label" x="448" y="440" text-anchor="middle">guarded: refuses if DB &lt; project version</text>
  <!-- merge loop: the Postgres DB drifts → dbd merge pulls it back as a snapshot -->
  <path class="edge" d="M358,427 C280,432 210,400 192,330" marker-end="url(#dbd-arrow)" />
  <path class="edge" d="M538,420 C600,415 668,412 668,408" marker-end="url(#dbd-arrow)" />
  <rect class="edge-label-bg" x="214" y="334" width="74" height="17" rx="4" />
  <text class="edge-label mono" x="251" y="346" text-anchor="middle">dbd merge</text>
</svg>

## Greenfield

`dbd init` scaffolds a `design.yaml`, the `ddl/` folder structure, and a sample table so you
have something that applies immediately. From there you edit the DDL by hand, run `dbd apply`
to push it to the database, and iterate: when you change a table, `dbd snapshot` captures the
diff as a migration and the next `dbd apply` runs it. See
[Snapshots and migrations](05-snapshots-migrations.md) for the version/migration mechanics.

## Brownfield — from a database

`dbd init --from-db <conn>` (or `$DATABASE_URL` / `-d` when given no value) introspects an
existing database and generates the whole project — schemas, extensions, enums, tables (with
constraints, indexes, and comments), views, functions, procedures, and standalone sequences —
plus a **baseline snapshot** so the project starts version-tracked. Review the result, run
`dbd format` to normalize it to your conventions (the introspected column order follows the
database's physical order, which a formatter tidies up), then `dbd apply` elsewhere as usual.

`init --from-db` is for databases **not** already managed by dbd. If it finds a `_dbd_meta`
table it **refuses** and points you at [`dbd merge`](04-commands.md#dbd-merge) — the command
for reconciling a managed database into its own project.

For ongoing sync, `dbd merge` pulls database drift back into the current project: it
re-introspects, overwrites the on-disk DDL, and captures the delta as a new snapshot version.
It never touches `design.yaml` and never deletes files (orphaned files are reported, not
removed). The full flag set and classification rules live in the
[commands guide](04-commands.md#dbd-merge).

## Brownfield — from DBML

`dbd init --from-dbml <file>` (and `dbd merge --from-dbml <file>`) take a DBML diagram as the
source instead of a live connection — handy when the schema lives in dbdiagram.io / dbdocs.io
rather than a running database. It is mutually exclusive with `--from-db` and needs no
connection.

DBML can only express a subset of a real schema, so a DBML source produces **schemas + enums +
tables + foreign keys only**. Functions, procedures, views, standalone sequences, roles, and
check constraints are not in DBML and therefore aren't generated. `serial`/identity columns do
survive, because dbd's own DBML carries `bigserial`/`[increment]` on the column.

## Brownfield — from SQLite

`dbd init --from-db sqlite://app.db` (also `file:` / `sqlite::memory:`) reverse-engineers a
SQLite database — the dialect is taken from the URL scheme, so the generated `design.yaml`
gets a `sqlite` target. SQLite objects are captured **verbatim** from `sqlite_master`, so
tables (including their CHECK constraints, type affinity, `AUTOINCREMENT`, and `WITHOUT
ROWID`), user indexes, and views all survive losslessly. SQLite has no schemas, enums,
functions, sequences, or roles, so those don't apply; **triggers are skipped** for now. Files
are written flat (`ddl/table/<name>.ddl`) and `design.yaml` carries an empty `schemas:` list.

## Version safety

`dbd merge` against a **dbd-managed** database compares the database's applied version **D**
against the project's `project.version` **Y**:

- **D < Y → refuse.** The project is ahead of a stale database; overwriting project DDL from it
  would discard newer work. Bring the database up to date with `dbd apply`, or revert the
  project to version `D` through version control if you really mean to discard it. There is no
  override flag.
- **D ≥ Y, or any foreign / DBML source → proceed.** dbd overwrites the introspected DDL and
  auto-snapshots the diff. (A DBML or foreign source has no `_dbd_meta`, so the gate never
  engages.)

Secrets never reach the repository: reverse-engineering writes the connection into `design.yaml`
as the literal `$DATABASE_URL` env reference, never the connection string you passed, and
`dbd merge` doesn't persist the connection at all.
