# Backlog

Pending and future work only. **Shipped features are documented in the
[guides](guide/)** — the [commands reference](guide/04-commands.md), the
[design.yaml reference](guide/03-design-yaml.md), and
[snapshots & migrations](guide/05-snapshots-migrations.md) — with internals in
the [architecture notes](design/architecture.md). They are not tracked here.

## Now

### RLS policy files are applied alphabetically, ignoring their dependencies

Reported from a live project: a policy on `repository_metrics` whose body
references `team_projects` was applied first and failed.

`apply_policies` (`design/mod.rs:252`) executes `policies/**` in **alphabetical
order**. Policy files are never parsed into entities — they have no
`EntityType`, no `refers`, and never enter the dependency graph — so nothing can
see that one policy body depends on another object. `repository_metrics` sorts
before `team_projects`, which is the entire mechanism.

Open question before designing a fix: policies run *after* all DDL, so a
referenced **table** should already exist. Establish what the failing body
actually referenced — a helper function or view created by another policy file,
a table outside the design, or something else — because that determines whether
the fix is ordering within `policies/`, or a missing dependency edge from a
policy to a DDL entity. Do not assume; get the failing file.

Candidate directions once that is known:

- Parse policy bodies for references (`pg_query` already extracts these — see
  `parser::pg::common`) and order `policies/` topologically like DDL entities.
- Or keep alphabetical execution but fail with a message naming the missing
  object and the file, instead of the raw Postgres error.

Note the current behaviour also *skips* failed policy files and logs them
(`PolicyReport.failed`) rather than aborting — worth checking whether that
produced a partial policy state in the reporting project, and whether it should
refuse like `ensure_fully_parsed` does for DDL.

## Future

### Convex

- Per-table `export_data` via the Convex CLI — currently errors with a clear
  message because `npx convex export` only supports whole-deployment dumps.
  Revisit if the CLI grows a `--table` flag, or extract the table from the
  export zip.
