create materialized view if not exists config.genders_mv as
select id, value from config.genders
with data;

create unique index if not exists genders_mv_id_uidx on config.genders_mv(id);
