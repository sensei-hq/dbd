set search_path to app;

create view calls_a_function as select app.myfn(id) as v from t;
