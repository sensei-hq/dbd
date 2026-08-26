set search_path to app;

create table if not exists types (
  a integer
, b varchar(30)
, c char(2)
, d numeric(10,2)
, e boolean
, f timestamp with time zone
, g text[]
, h uuid
, i jsonb
, j status_t
, k bigint
, l double precision
, m timestamptz
, n varchar
);
