do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'bare_grant') then
    create role bare_grant;
  end if;
end $$;
grant parent to bare_grant;
