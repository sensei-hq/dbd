set search_path to app;

create or replace function sql_calls(n int) returns int language sql immutable
as $$ select app.myfn(n) $$;
