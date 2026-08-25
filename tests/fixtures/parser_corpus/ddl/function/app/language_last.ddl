set search_path to app;

create or replace function language_last() returns bigint
as $$ select count(*) from t $$ language sql;
