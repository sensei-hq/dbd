set search_path to app;

create sequence full_options
  as bigint
  increment by 2
  minvalue 10
  maxvalue 1000
  start with 10
  cache 5
  cycle;
