# Checkpoint

**Slice:** #41 — an extension type outside the `search_path` read as drift
(`fix/extension-type-reconcile`, off `develop`). #39 closed as its duplicate.

## Done

- Red e2e `4c95bc3b` reproduced #41; fix `213fac6d`
  (`DatabaseAdapter::extension_types()`, `reconcile::qualify_extension_types`
  on both snapshots in `reconcile` and `diff_live`). Red units `64e1f0e9`, fix
  `9b0d505c` (modifier kept; a name an enum or two extensions share left).
- Review hardening `1d28de74`: both live spellings pinned (fresh connections,
  then `ALTER DATABASE … SET search_path` reaching `extensions`), the real
  change's `TYPE extensions.citext` asserted with `extensions` outside the
  managed schemas. Each surviving mutation now kills a test.
- Review fix: red `fd06a080`, fix `0d4ea5c3` — only a name an extension alone
  owns (`date` stays `pg_catalog.date` beside orafce's `oracle.date`), and an
  extension's domain over an array counts.
- Docs `18827714` + this commit: CHANGELOG `[Unreleased]`, guide 04,
  `llms-full.txt`, trait doc; quoted-schema type spelling added to the backlog.
- Embedded suite 128/128; CLI repro: 0.25.0 fails, the branch reconciles
  `0 altered` and `diff` is in sync.

## Next

    gh pr create --base develop --head fix/extension-type-reconcile

## Open

- PR #45 (v0.25.0 develop → main) waits on a Cloudflare "Retry build" of
  `4ae031f1`; every GitHub check is green. The red check is Cloudflare's API
  returning 504 to `wrangler versions upload`, not the code.
- Keep #41 off `develop` until #45 merges, so the release PR stays 0.25.0.

## Known limits

- A type in a schema that needs quoting (`"Ext"`) still reads as drift — backlog.
- Domains and composite types are not dbd entities.
