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
- A module's design has no `scopes:`: scopes select over the workspace
  (slice 3). Until then `inspect` refuses `--scope` and `--deps` on a
  workspace, and every command but `inspect` and `install` refuses a
  workspace until slice 2 runs it.
- `Workspace::config` is the manifest as written — its `apply:` hooks, which
  run around the whole workspace, and its `scopes:`.

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
   row (T21). `--module <name>` selects a module and its dependencies.
3. **Scopes over modules.** `scopes.<name>.modules: [...]` selects modules (and
   their dependencies), alongside entity selection. Running inside a module
   folder finds the workspace root by walking up and selects that module.
4. **Per-module history.** `snapshot`, `release` and `apply`'s migrations per
   module: each module's own `snapshots/`, `migrations/` and version. A
   version bump or a release writes `modules.<name>.version` / `released`,
   never `project.*` — which a workspace refuses.
5. **The viewer.** The diagram model carries each entity's module; the viewer
   groups the diagram and sidebar by module.
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
