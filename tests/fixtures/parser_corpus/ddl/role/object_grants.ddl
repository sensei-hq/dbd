do $$ begin
  if not exists (select from pg_catalog.pg_roles where rolname = 'object_grants') then
    create role object_grants;
  end if;
end $$;
grant membership to object_grants;
grant select on table t to object_grants;
grant insert, update on all tables in schema app to object_grants;
