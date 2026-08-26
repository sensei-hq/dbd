set search_path to app;

create table if not exists named_constraints (
  id        uuid not null
, parent_id uuid not null
, code      text not null
, qty       integer not null
, constraint named_constraints_pk primary key (id)
, constraint named_constraints_code_uk unique (code)
, constraint named_constraints_qty_ck check (qty > 0)
, constraint named_constraints_parent_fk foreign key (parent_id)
    references simple (id) on delete cascade on update cascade
);
