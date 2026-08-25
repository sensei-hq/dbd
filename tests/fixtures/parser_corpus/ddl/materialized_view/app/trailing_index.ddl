set search_path to app;

create materialized view trailing_index as
  select id, total
  from t
  where total > 0
with data;

create unique index trailing_index_id_idx on trailing_index (id);
