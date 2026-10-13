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
- Still refusing a workspace: `format`, `doctor`, `snapshot` and `release`
  (slice 4), `combine`, `emit`, `graph`, `dbml` and `diagram` (slice 5).
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
- Run inside a module's folder — no design.yaml of its own — dbd walks up to
  the nearest design.yaml declaring `modules:` (`Workspace::locate`) and runs
  that module, unless `--module` says otherwise; `init` and `install` do not
  walk up, and a design without modules is not looked for above.

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
