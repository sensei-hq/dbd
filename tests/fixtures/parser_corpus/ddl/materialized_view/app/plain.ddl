set search_path to app;

create materialized view plain as select id, name from t where id > 0 with data;
