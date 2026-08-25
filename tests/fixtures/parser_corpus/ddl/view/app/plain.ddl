set search_path to app;

create view plain as select id, name from t where id > 0;
