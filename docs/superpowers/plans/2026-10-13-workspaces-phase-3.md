# #7 Phase 3 — workspaces and modules

Spec: [multi-project isolation](../specs/2026-10-06-multi-project-isolation-design.md),
section 2 (Workspaces and module roots), decisions 2, 3 and 5. Phases 1 and 2
shipped in 0.26.0.

## The shape

A **module is a `Design` of its own**. Everything Phase 2 built is keyed on the
owner name and the project root: claims, uses, the record in `dbd.meta`,
`snapshots/`, `migrations/`, the version. So a module is loaded as a `Design`
whose owner is `<project>/<module>`, rooted at its folder, with the manifest's
shared configuration (targets, schemas, import options, format) and its own
version. Every per-project mechanism then works per module unchanged, and the
registry keeps modules apart exactly as it keeps projects apart.

A **workspace** is the manifest plus its modules in `depends_on` order. A
`design.yaml` without `modules:` is a workspace of one module rooted at the
project directory, owned by `project.name` — today's behaviour, untouched.

A module references its dependencies' entities without `external:` entries:
they are added to its design as externals, which resolve its references and
are never its to build, claim or own. Their definitions stay in their module —
so what reads a module's definitions of another's (the viewer, DBML) reads them
from the owning module (slice 5).

## Decisions taken here (the spec leaves them open)

- A module's version and release state live in its block:
  `modules.<name>.version`, `modules.<name>.released`. `project.version` is the
  workspace's only when it has no `modules:`.
- A module's folder is `modules.<name>.path`, default `<name>/`, relative to
  the project directory (`--source`, as `ddl/` is; design.yaml's folder when
  loaded without one); it holds `ddl/`, `import/`, `policies/`, `snapshots/`,
  `migrations/`, and its hook scripts. It is a folder of its own — not the
  project's (`path: .`), not inside another module's — since a module that
  moves to a repository of its own takes it.
- `modules.<name>.apply` holds that module's `apply.before`/`apply.after`;
  the manifest's top-level `apply:` runs once around the whole workspace run.
- A run without `--module` or a module-selecting scope runs every module, in
  `depends_on` order (reset in reverse): of the modules whose dependencies are
  in place, the one `modules:` lists first.

Taken in slice 1:

- `Workspace::load` refuses, before any database work and so for every
  command: a module name outside `[A-Za-z0-9_][A-Za-z0-9_-]*` (it names an
  owner and a folder); unknown keys in a module block; `project.version` or
  `project.released` (each module has its own); an unknown dependency or a
  cycle, named; a folder outside the project, the project's own, inside
  another's, shared or missing; files in a root `ddl/`, `import/`, `policies/`,
  `snapshots/` or `migrations/` (dotfiles such as `.gitkeep` aside) — they
  belong to no module; two modules declaring the same entity, keyed as the
  registry keys it (T19); and a reference into a module outside the
  `depends_on` closure (T20) — to any entity another module's file declares,
  roles included, or creates beside it.
- A module's design has no `scopes:` of its own: scopes select over the
  workspace (slice 3).
- `Workspace::config` is the manifest as written — its `apply:` hooks, which
  run around the whole workspace, and its `scopes:`.

Taken in slice 2:

- `Workspace::select(module)` is what a run that builds or reads goes over:
  every module, or the module and every module it depends on, in load order;
  the manifest's `apply.before` runs ahead of the first module's own, its
  `apply.after` and `import.after` behind the last's, read from the project
  folder, in that module's run. `Workspace::select_declaring(module, name)`
  is `-n`'s: the module declaring the entity, the hooks around it; one
  `--module` leaves out is refused naming its module. `select_alone(module)`
  is `export`'s: the module alone — an export only reads.
  `Workspace::selected_to_reset(module)` is every module in reverse, or the
  module alone — never its dependencies, which are other owners. Each module
  connects as its owner (`Design::owner`, now public); `--adopt-from` and
  `--allow-ownership-change` apply to each.
- `--module` is a global flag; on a design without `modules:` it is refused.
- A run over more than one module checks every module's run as its dry run
  checks it — scope pin, guards, claims, migrations, hook files, imports —
  before the first module writes, so a refusal any module would meet comes
  first. A reset previews every module, each beside the modules reset
  before it (`Design::resetting_beside`: their uses and holdings do not hold
  it back, their prod marks still do; the modules after it still hold what
  they hold), and is refused whole or run whole; its `--dry-run` passes
  where it passes. An error past those checks — a statement the database
  refuses — still stops the run with the modules before it applied, as one
  project's run stops with its earlier statements committed.
- A workspace's import reloads as one project's does: its first module
  empties, together, every table the run reloads that exists by then, in
  load order by the database's keys (`Design::emptying_for_import`), the
  others none — so `shared`'s reference data reloads with the tables that
  hold keys to it, and `--module shared` alone is refused naming them. An
  import that fails names what it emptied and did not reload.
- A workspace's deploy goes over the modules twice: every module's schema
  (`Design::deploy_schema`: apply, grants), then every module's data
  (`Design::deploy_data`: import, policies). No module migrates after the
  import has emptied the tables it reloads.
- Each `export:` entry is the module's that declares its entity; one no
  module declares stays with the first module, which warns about it once.
- Each module's run is headed `module <name> (<owner>)`. `inspect --shared`
  lists what shares the database once, then each module's conflicts; `diff`
  answers for every module — `--exit-code` over all, `--json` one document,
  `{"modules": [{"module", "owner", "diff"}]}` (a project's is its diff, as
  before); a policy failing in one module is reported, the modules after it
  still run, and the run fails at the end.
- Still refusing a workspace then: `format`, `doctor`, `snapshot` and
  `release` (slice 4), `combine`, `emit`, `graph`, `dbml` and `diagram`
  (slice 5) — since slice 5 every command runs one but `init` and `merge`.
  `init` and `merge` onboard a project into one design and refuse a
  workspace, pointing at `dbd split` (slice 6).

Taken in slice 3:

- `scopes.<name>.modules: [...]` selects modules, each with every module it
  depends on, whole; `includes:` selects entities of modules the scope does
  not select whole (those modules' dependencies whole, as `--module` takes
  them); `excludes:` leaves entities of selected modules out; a scope naming
  no module and including nothing selects every module. `extensions:` and
  `deps:` are each module's.
- Each manifest scope is projected onto each module it selects as the
  workspace loads, set in the module's config under the scope's name — so
  every command resolves, guards and pins the scope per module unchanged.
  `Workspace::under_scope(scope)` narrows the run to the scope's modules;
  no `--scope` is the `default` scope when the manifest has one, else every
  module; `all` is every module. `--module` within a scope must be one of
  its modules. Refused at load: a scope naming an undeclared module, an item
  no module declares, and `modules:` in a design without modules.
- An include selects the modules with entities it names (a bare schema alone
  selects none: every module has the manifest's schemas; one no module knows
  is refused). What one module of a run requires and the run's scope leaves
  out of another is refused as a gap (`check_scope_gaps`), over the modules
  the run goes over, each resolving the scope and `--deps` as the commands
  do — checked when the run starts, not when the workspace loads, so other
  scopes and runs not going over that module still run.
- With no `--scope`, a module asked for — `--module`, or the folder dbd runs
  in — that the `default` scope leaves out runs on its own with its
  dependencies (`Workspace::under_scope_of`), each module resolving its own
  `default`, else `all`; the scope guard still holds each to its pin.
- Run inside a module's folder — no design.yaml of its own — dbd walks up to
  the nearest design.yaml declaring `modules:` (`Workspace::locate`), along
  the path the run was started in (a module folder may be a link), and runs
  that module, unless `--module` says otherwise. Run in a folder of the
  workspace no module holds, it runs the workspace; `-s <module folder>`
  runs that module. `init` and `install` do not walk up, and a design
  without modules is not looked for above.

Taken in slice 4:

- A module's version and release state are written in its block,
  `modules.<name>.version` / `released`, edited in place as `project.*` is
  (comments and layout kept; a module written `{}` becomes a block; a
  flow-style block with keys falls back to rewriting the file):
  `config::update_version_of` / `set_released_of`. Its snapshots and
  migrations are in its folder: `snapshot::create_snapshot_of` /
  `create_baseline_snapshot_of`, which name the module (`Design::module`).
- An edit in place must read as the file did with that one key set — every
  other value as it was — or the file is rewritten whole.
- `snapshot` and `release` go over every module, or `--module` alone, never
  a scope (a snapshot records the design). A `release` checks every module
  it releases before writing any; a whole-workspace `release` skips the
  modules already released and refuses only when none is left.
  `snapshot --list`, `format` and `doctor` go over each module's folder —
  or `--module`'s, or the folder dbd runs in — without loading designs
  (`Workspace::folders`, which refuses a folder that is not there); `doctor`
  audits a design.yaml it cannot parse as a project's, as before.
- A released module changes by snapshot and `dbd apply`: `reconcile` refuses
  the module asked for if it is released, and leaves out — saying so — a
  released module the run takes in otherwise, a dependency included. It is
  left out before the manifest's hooks attach (`Workspace::select_except`,
  `Order::Reconcile`), so they run around the modules reconciled.
- A module's name must read as a string to YAML 1.2 and 1.1 alike: `2024`,
  `true`, `null`, `no`, `on`, `y`, `1_000` and `0x1f` are refused.
- `doctor` does not migrate a design.yaml that declares `modules:` — the
  migration would rebuild it without them; its old entries are fixed by hand.
  A design.yaml it cannot read is named as an issue, with the error.

Taken in slice 5:

- `dbml`, `diagram`, `graph`, `combine` and `emit` go over the modules a run
  selects (`--module`, `--scope`, the folder dbd runs in), in `depends_on`
  order, each through its own scope, as one design: what each module keeps
  in view, together — so a key, a type or a read into a dependency lands on
  its entity, drawn and written once, in its module, from its module's
  definition. Without the gap check: a table a scope leaves out of a
  dependency is a stub (`out_of_scope`), as in one design.
- The schema model (still v3, additive) carries `modules` — each module's
  `name`, `owner`, `depends_on` and `note`, in load order — and `module` on
  each table, entity, enum and stub a module declares; each history entry
  carries its module, the modules' histories one after another in load
  order. A project's model is as it was: no `modules`, no `module`. Its
  `project` is the manifest's.
- `graph`'s nodes carry `module` in a workspace; `-n` is the entity's
  subgraph across the modules in view, refused for one none declares.
- `dbml` documents the workspace in one document (or one per `dbml:` doc); a
  document with no groups of its own (no `groups`, no
  `auto_group_by_schema`) gets one `TableGroup` per module — DBML puts a
  table in one group.
- `combine` and `emit` write one script; a comment heads each module's
  statements, `module <name> (<owner>)`.
- The viewer groups the sidebar by module, then schema; the diagram is tinted
  by module or by schema — a switch, module first for a workspace's model;
  an entity's page and each changelog entry name their module.
- What these commands read is `Workspace::reading` — a `Reading`, the
  selected modules each with its resolved scope; a project is a reading of
  one, its output as before. `schema_model::build_reading`,
  `dbml::generate_reading` and `emit_dialect::emit_reading` read it;
  `DbmlMultiParams`, `generate_all` and `emit_schema` are unchanged for an
  embedder.
- The changelog lists each module's versions in turn, in `depends_on`
  order, newest first; an entity's changelog is its module's, dated from
  that module's baseline, and says when its module has none.
- Review: an entity is its module's by kind and name (a function and a
  view may share a name across modules), a graph node's included; the scope
  line counts what the run writes, each entity once; `emit` notes a schema's
  grants once, each module's policies its own; a module's refusal reads
  `module "<name>": …`, as the gap check's does, and `-n` of an entity a
  module the run does not select declares names that module. The overview's
  recent changes are the newest across the modules, by when each was cut.
  An unknown scope reads the same in all five commands — `diagram` no longer
  says "Failed to resolve scope:" first.

Taken in slice 6, the workspace transfer rule (its first PR; `dbd split`
follows in a second):

- A project's own owners hand over to each other. A module of a workspace
  takes over, as it claims, what it declares that the project's other owners
  hold: the project before it had modules (`<project>`, the predecessor) and
  the workspace's other modules (`<project>/<module>` for each module in
  `modules:`, run or not). As `--adopt-from` does — in the claim's
  transaction, previewed and said the same way (`adopting … from
  "<owner>"`), its held entities the run's own to plan against. Nothing else
  changes hands: an owner by any other name (`shopx`, `billing`) still
  refuses the run. A project without modules takes over from nobody.
- A module does not prune what another module of the workspace declares: it
  is moving there, and that module's run takes it over (T22). Moved either
  way, nothing is dropped or created afresh: the module running first either
  takes it over or leaves it for the one that does.
- The predecessor's record is retired once nothing of it is left: when a
  module's run leaves the predecessor holding nothing that still exists, its
  `dbd.meta` row, its `dbd.uses`, its claims of what is gone and its
  materialized-view stamps go, in the run's claim — its `dbd.migrations`
  stay, as history. Until then it is an owner like any other.
- What another module declares is no sign of another project under a
  module's name either: a module whose entities all moved holds none it
  declares, and still runs.
- Review: what an entity uses goes with it when it changes hands — in the
  take-over's transaction its giver's `dbd.uses` rows through it become the
  taker's — and a module's run keeps the uses of what it still holds and
  hands over until the taker records its own: until then they are all that
  guards what the entity reads. A reset keeps a schema holding what another
  module declares. A retire is housekeeping: in a savepoint, bounded as a
  claim is, and left to a later run when another session holds its rows.
- The adapter takes over from several owners at once through
  `claim_objects_from(owner, objects, write, from)` — by default
  `claim_objects`, taking over from the one of them that holds anything, so
  an adapter written before, or keeping no registry, keeps working; refused
  only when several hold what the claim takes — and retires an owner through
  `retire_owner(owner)`.

Taken in slice 6, `dbd split --from-scopes` (its second PR):

- The regions are the scopes' Venn diagram over each scope's entities with
  their dependencies (`deps: include`, whatever the scope says): an entity's
  region is the scopes holding it. What every scope deploys — schemas,
  extensions, roles — is in every scope. A region in every scope (of two or
  more) is `shared`; in one, that scope's name (`default` becomes `core`);
  in several, their names joined with `-`; in none, `unscoped`. Empty regions
  are dropped. `--rename <module>=<name>` renames one, each a module name.
- Each module `depends_on` the modules its entities reference; a cycle is
  shown and refuses `--write`. Each scope becomes `modules:` — the modules of
  the regions it holds, less those another of them depends on — keeping its
  `extensions:`, and `excludes:` the manifest's schemas it did not deploy,
  which every module has; an `all` scope stays `all`.
- What moves, each to the folder of the module owning it: each DDL file —
  by its entity, refused if it holds entities of two regions — each import
  data file and each policy file, by its table. The manifest's
  `import.after` scripts under `import/` move to `scripts/`, their paths
  rewritten. A file of `ddl/`, `import/` or `policies/` that maps to no
  module refuses the split, named: a workspace refuses a root folder left
  holding files.
- A project with snapshots or migrations is refused: a migration is SQL over
  several tables, and does not divide by module, and a module's snapshots
  without its migrations would leave its history inconsistent — so a
  project splits before its first release (sensei has neither). The plan
  said they would be split by owning module; this is why they are not.
- Without `--write` it prints the plan and changes nothing. `--write` moves
  the files — `git mv` in a git work tree — and writes `modules:` and the new
  `scopes:` into design.yaml in place, keeping the rest as it was, then loads
  the workspace to check it. A module folder that already holds files, an
  invalid name, a workspace and a design without scopes are refused before
  anything moves.
- Review: `--write` is whole or not at all. It first makes what the split
  would make in a scratch folder — import data as empty files — and refuses
  unless that workspace loads and deploys, under each scope and with none,
  what the project does. It writes the new manifest aside, records each
  move and on any failure puts every one back — a file git tracks moved by
  `git mv` or not at all — then puts the manifest in place. Refused before
  anything moves, besides: an earlier split's leftovers, a plan of no
  modules, two files onto one path or a taken one, names equal but for case,
  a folder it cannot read, a file that does not parse, a schema only a file's
  path names (add it to `schemas:`), and a scope that leaves out what its
  entities need (as modules it would deploy them). What a file creates
  beside its entity is its module's, as the gap check reads it (T20).

## Slices

Each is red tests first, then the implementation, a focused review, CI, and a
PR into the integration branch `feat/workspaces`.

1. **Manifest and load.** The `modules:` block (`note`, `depends_on`, `path`,
   `version`, `released`, `apply`), validated: unknown dependency, a cycle, a
   path outside the project. `Workspace::load` builds a `Design` per module.
   `dbd inspect` validates every module and refuses two modules declaring the
   same entity (T19) and a reference into a module that is not a dependency, as
   a gap (T20).
2. **Running a workspace.** `apply`, `deploy`, `reconcile`, `diff`, `import`,
   `export`, `policies`, `refresh`, `migrate --status` run each selected module
   in order; `reset` in reverse. Each module claims, applies and records its own
   row (T21). `--module <name>` selects a module and its dependencies —
   `reset` and `export`, the module alone.
3. **Scopes over modules.** `scopes.<name>.modules: [...]` selects modules (and
   their dependencies), alongside entity selection. Running inside a module
   folder finds the workspace root by walking up and selects that module.
4. **Per-module history.** `snapshot`, `release` and `apply`'s migrations per
   module: each module's own `snapshots/`, `migrations/` and version. A
   version bump or a release writes `modules.<name>.version` / `released`,
   never `project.*` — which a workspace refuses. `format` and `doctor` go
   over the modules too.
5. **The viewer.** The diagram model carries each entity's module; the viewer
   groups the diagram and sidebar by module, reading each entity's definition
   from the module declaring it (a dependency is an external in its
   dependents). `dbml`, `diagram`, `graph`, `combine` and `emit` go over the
   workspace.
6. **Converting a project: `dbd split --from-scopes`.** The plan (regions of the
   scopes' Venn diagram after `deps: include`, entity counts, inferred
   `depends_on`, rewritten `scopes:`), and `--write` (moves files with `git mv`,
   splits `import/`, `policies/`, `snapshots/` and `migrations/` by owning
   module — a root folder left holding files refuses the workspace — writes
   `modules:` and `scopes:`). The
   **workspace transfer rule**: a module's first run, where the predecessor
   owner is the same project (`sensei` before `sensei/core`), takes over what
   it declares from the predecessor and retires the predecessor's record once
   nothing of it is left (T22–T24).
7. **Docs.** Guide 03 (`modules:`, module-selecting scopes), guide 04 (flags),
   the sharing guide's modules section, llms, both SKILL.md, the site.

Converting sensei itself is the proving case (T24). It is done in sensei's
repository, with its owner, once slices 1–6 are released.

## Test scenarios (from the spec)

| # | Scenario | Asserts |
|---|---|---|
| T19 | Two modules declare the same entity | `dbd inspect` fails before any database work |
| T20 | `core` references `dojo` without `depends_on` | gap error |
| T21 | Workspace deploy of shared, core and dojo into one database | three meta rows; ownership by module |
| T22 | An entity file moves from `core/` to `shared/` | ownership transfers; nothing is pruned |
| T23 | `dbd split --from-scopes` on a fixture shaped like sensei | three modules; `shared` is the intersection; inferred `depends_on`; imports split by owning module |
| T24 | Sensei converted; `--scope dojo` and default deploys | the same entities deploy to each database as with today's scopes; the `sensei` meta row is retired; module rows exist |
