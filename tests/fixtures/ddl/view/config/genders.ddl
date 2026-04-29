set search_path to config;

create or replace view genders
as
select lv.id
     , lv.value
     , lv.is_active
  from lookups  lkp
 inner join lookup_values lv
    on lv.lookup_id = lkp.id
 where lkp.name = 'Gender';
