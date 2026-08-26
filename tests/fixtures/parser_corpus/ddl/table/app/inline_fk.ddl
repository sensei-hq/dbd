set search_path to app;

create table if not exists inline_fk (
  id        uuid primary key
, simple_id uuid not null references simple (id) on delete cascade
, other_id  uuid references other.thing (id) on update set null
, loose_id  uuid references simple
);
