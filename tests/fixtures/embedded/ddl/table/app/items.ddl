set search_path to app;

create table if not exists items (
  id         uuid primary key default gen_random_uuid()
, name       text not null
, quantity   integer not null default 0
, notes      text
, created_at timestamptz not null default now()
);

create unique index if not exists items_name_ukey on items(name);
