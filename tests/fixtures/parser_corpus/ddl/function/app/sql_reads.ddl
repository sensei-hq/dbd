set search_path to app;

create or replace function sql_reads() returns bigint language sql stable
as $$ select count(*) from t $$;
