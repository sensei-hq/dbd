set search_path to app;

create materialized view as_in_string_literal as
  select 'x as y' as label, id
  from t
with data;
