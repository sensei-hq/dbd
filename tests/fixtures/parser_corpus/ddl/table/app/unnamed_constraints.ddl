set search_path to app;

create table if not exists unnamed_constraints (
  a    integer not null
, b    integer not null
, code text not null
, unique (code)
, check (a < b)
, foreign key (a, b) references composite_key (tenant_id, item_id)
);
