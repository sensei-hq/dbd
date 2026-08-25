do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'no_grants') then
    create role no_grants;
  end if;
end $$;
