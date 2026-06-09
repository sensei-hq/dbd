# Backlog

Pending and future work only. **Shipped features are documented in the
[guides](guide/)** — the [commands reference](guide/04-commands.md), the
[design.yaml reference](guide/03-design-yaml.md), and
[snapshots & migrations](guide/05-snapshots-migrations.md) — with internals in
the [architecture notes](design/architecture.md). They are not tracked here.

## Now

_(nothing queued — the scopes work completed in v0.4.4)_

## Future

### Convex

- Per-table `export_data` via the Convex CLI — currently errors with a clear
  message because `npx convex export` only supports whole-deployment dumps.
  Revisit if the CLI grows a `--table` flag, or extract the table from the
  export zip.
