set search_path to staging;

create or replace procedure import_lookup_values()
language plpgsql
as
$$
begin
  insert into config.lookup_values(
     lookup_id
   , value
   , sequence
   , is_active
   , modified_at
   , modified_by)
  select lkp.id
       , stg.value
       , stg.sequence
       , stg.is_active
       , now()
       , current_user
    from staging.lookup_values stg
   inner join config.lookups lkp
      on lkp.name = stg.lookup_name
  where not exists (select 1
                       from config.lookup_values lv
                      where lv.lookup_id = lkp.id
                        and lv.value     = stg.value)
      on conflict(lookup_id, value)
      do update
            set sequence    = excluded.sequence
              , is_active   = excluded.is_active
              , modified_at = excluded.modified_at
              , modified_by = excluded.modified_by;
end;
$$
