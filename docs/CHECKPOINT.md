# Checkpoint

**Slice:** ER diagram rendered by `@rokkit/graph` 1.7.0. Merged to `develop`
and `main`, **not yet pushed** (both branches ahead of origin).

## Done

- **Diagram renders from `@rokkit/graph`** — `DiagramView`/`EntityDiagram` call
  `Graph` + `toGraphInput`; dbd's own `layout*.ts` (clusters, edges, types) and
  their tests are deleted. Net −921 lines.
- **`SchemaModel` site mirror upgraded to v2** — matches the v0.22.0 crate.
- **rokkit 1.7.0, all ten packages from the registry** — `@rokkit/graph` was a
  `link:` to `~/Developer/rokkit`, a path that exists on one machine; CI and the
  Cloudflare build would have resolved nothing. `@rokkit/themes` was a second
  stray symlink that package.json never declared (asked ^1.4.1, served 1.7.0
  locally), so local and CI were building different code silently. Both gone.

## Verified on merged `main`

    bun install --frozen-lockfile   exit 0   # the CI gate
    bun run test                    29/29
    bun run check                   0 errors 0 warnings
    CF_PAGES=1 bun run build        adapter-cloudflare, exit 0
    cargo test / clippy -D warnings / fmt --check   all green (pre-commit hook)

Browser against the registry copy: `/diagram` renders 6 tables across both
schemas with FK edges and pk/fk icons, `/`, `/guide` render, 0 console errors.

## Next

    git push origin develop main    # 3 and 5 commits ahead respectively

Then decide whether this warrants a release — see the open question below.

## Open questions

- **`CHANGELOG.md [Unreleased]` is empty** and covers none of this slice: the
  graph refactor, the v2 mirror, or the rokkit upgrade. It needs writing before
  any release is cut. No version bump has been made; site/package.json is still
  0.22.0.
- Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
  residual difference is not a defect.

## Known-broken / carried forward

- `shiki` peer warning on install: `@rokkit/ui` declares `^3.23.0` at both
  1.4.1 and 1.7.0, installed `shiki@4.4.3` comes from `@devframes/service-shiki`.
  Unchanged by the upgrade, and the site's `CodeBlock` does not use it.
- `emit` covers tables and views only; routines are skipped and reported.
- `CREATE ROLE … IN ROLE …` is not read as a membership; `GRANT … TO …` is.
- `cmd_format --check` calls `std::process::exit(1)`, so that path cannot be
  tested in-process.
- The commit gate tests the **worktree**, not the index — a staged-only change
  is not what it verifies.
