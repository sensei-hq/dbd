create materialized view config.genders_mv as
select id, value from config.genders
with data;

create unique index genders_mv_id_uidx on config.genders_mv(id);
