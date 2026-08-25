set search_path to app;

create materialized view multiline_indented as
  select a,
         b,
         c
  from t
  where a > 0
    and b < 10
with data;
