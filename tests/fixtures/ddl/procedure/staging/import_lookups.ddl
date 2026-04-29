set search_path to staging;

create or replace procedure import_lookups()
language plpgsql
as
$$
begin
  insert into config.lookups(
     name
   , is_editable
   , description
   , modified_at
   , modified_by)
  select trim(stg.name)
       , stg.is_editable
       , stg.description
       , coalesce(stg.modified_at, now()) as modified_at
       , coalesce(stg.modified_by, current_user)
    from staging.lookups stg
  where not exists (select 1
                       from config.lookups lkp
                      where lkp.name        = trim(stg.name)
                        and lkp.modified_at > stg.modified_at)
      on conflict(name)
      do update
            set is_editable  = excluded.is_editable
              , description  = excluded.description
              , modified_by  = excluded.modified_by
              , modified_at  = excluded.modified_at;
end;
$$
