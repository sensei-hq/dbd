set search_path to app;

create materialized view with_no_data as select id, name from t with no data;
