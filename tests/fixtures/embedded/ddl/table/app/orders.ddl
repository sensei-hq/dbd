set search_path to app;

create table if not exists orders (
  id         uuid primary key default gen_random_uuid()
, item_id    uuid not null references items(id)
, amount     integer not null default 1
, created_at timestamptz not null default now()
);
