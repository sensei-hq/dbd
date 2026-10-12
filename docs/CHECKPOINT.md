# Checkpoint

**Slice:** 0.25.0 shipped; #41 merged to `develop` for 0.25.1.

## Done

- v0.25.0: crates.io (`release.yml` green), PR #45 merged to `main`
  (`b4f075e3`); CI and the Workers production deploy green; the site serves
  0.25.0. The develop preview's 504 was Cloudflare's API; a retry deployed it.
- #41 (and duplicate #39): PR #46 merged to `develop` (`7795094b`) — extension
  types spelled as the database spells them in `reconcile` and `diff`, reviewed
  twice, mutation-checked; embedded suite 128/128.

## Next

    # scoped reset refused by a policy calling an in-scope function (below)

## Open

- **Scoped reset vs policies.** `build_scoped_reset_script` (`script.rs`
  ~767) drops in reverse apply order, so in-scope functions go before in-scope
  tables; a policy on such a table calling such a function (sensei `dojo`: six
  files, five functions) makes Postgres refuse the function's drop, and the
  message blames dependents outside the scope. Fix: drop the policies of the
  scope's tables first — they go with their tables anyway.
- `6fccb0ff` (#44) and `b4f075e3` (#45) are authored by GitHub's noreply
  identity (`gh pr merge`); not rewritten, as `6fccb0ff` is an ancestor of the
  published `v0.25.0` tag. Merges are now done locally.
- #41 closes when 0.25.1 reaches `main`.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
- CHANGELOG Known limitations; `docs/BACKLOG.md` (0.25.1).
