set search_path to app;

create table if not exists commented (
  id   uuid primary key
, code text not null
);

comment on table commented is 'A commented table';

comment on column commented.id is 'The identifier';

comment on column commented.code is 'The code, which may contain an '' escaped quote';
