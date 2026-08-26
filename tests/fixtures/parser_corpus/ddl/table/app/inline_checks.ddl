set search_path to app;

-- Postgres promotes a column-level CHECK to a table constraint and introspection
-- reports it as one, so the parser lifts it here too.
create table if not exists inline_checks (
  singleton boolean primary key default true check (singleton)
, source    text not null check (source in ('mcp', 'builtin'))
, qty       integer constraint inline_checks_qty_ck check (qty > 0)
);
