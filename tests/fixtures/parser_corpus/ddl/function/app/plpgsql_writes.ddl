set search_path to app;

create or replace function plpgsql_writes() returns void language plpgsql
as $$
begin
  insert into t(a) values (1);
end
$$;
