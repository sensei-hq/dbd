set search_path to app;

create table if not exists simple (
  id         uuid primary key default gen_random_uuid()
, name       text not null
, quantity   integer not null default 0
, active     boolean default true
, notes      text
, created_at timestamp with time zone not null default now()
);
