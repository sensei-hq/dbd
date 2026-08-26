set search_path to app;

create view trim_syntax as select trim(both from name) as n from t;
