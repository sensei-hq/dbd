set search_path to app;

create or replace procedure proc_writes() language plpgsql
as $$
begin
  insert into t(a) values (1);
end
$$;
