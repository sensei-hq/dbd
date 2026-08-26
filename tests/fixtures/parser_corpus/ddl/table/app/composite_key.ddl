set search_path to app;

-- A composite key must land BOTH as a table-level PRIMARY KEY constraint and as
-- `is_pk` on each member column; reconcile's lift_pk_unique_keep_others and
-- pk_unique_col_sets both read that shape.
create table if not exists composite_key (
  tenant_id uuid not null
, item_id   uuid not null
, qty       integer not null default 1
, primary key (tenant_id, item_id)
);
