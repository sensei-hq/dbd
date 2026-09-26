# Checkpoint

**Slice:** v0.19.0 shipped and verified. `develop` and `main` level.

## Done

- **v0.19.0 released** — both crates on crates.io, merged to `main`, CI +
  CodeQL green. Minor, because it breaks `Entity`'s reference API.
- **One list of references.** `Entity::refs: Vec<Ref>` replaces `refers`,
  `references`, `reads`, `writes`. `Ref { name, kind, schema_source,
  unresolved }` puts on one row what four fields held between them and none
  held alone. `RefKind` retires the stringly `ref_type`; `REF_TYPE_FUNCTION`
  and `recover_bare_target_by_proxy` are gone.
- **`Entity::body`** — `writes` held table names from a parser and DDL body
  text from the introspector. A trap, not a live bug, but it had to split.
- **No meaning changed** — the 2,154-file corpus reports identically before
  and after: 2,737 entities, 8,558/1,828/1,216 edges, 14,754 references.
- **Verified from the registry** — a crate on `dbd-core = "0.19.0"` ran 12
  repros green and confirmed `entity.refers`/`.reads` no longer compile.

## Next

    cargo test --workspace --all-features   # 1592 pass, clippy + fmt + doc clean

Nothing queued. Open: #18 (reconcile), #11 (CLI coverage), #7 (tenancy).

## Open question

Does sensei's 43,737 count occurrences or unique pairs? Until settled, a
residual difference is not a defect.

## Known-broken / carried forward

- `CREATE ROLE … IN ROLE …` is not read as a membership; `GRANT … TO …` is.
- `schema_model::Ref` (a DBML graph edge) shares a name with `entity::Ref`.
  Different modules, type-checked, so not the silent-confusion shape.
- `diff`/`reconcile`/snapshots refused on SQLite (verbatim, no structure).
- `ARRAY[col]::t[]` where the column is already `t` reads as drift;
  `generate_data_sql` warns "may truncate" on a widening cast.
