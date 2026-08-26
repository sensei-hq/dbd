set search_path to app;

create table if not exists indexed (
  id     uuid primary key
, name   text not null
, tags   text[]
, ctx    jsonb
, folder uuid
, qty    integer not null default 0
);

create unique index if not exists indexed_name_uk on indexed (name);
create index indexed_tags_gin on indexed using gin (tags);
create index indexed_name_pat on indexed (name text_pattern_ops);
create index indexed_lower_name on indexed (lower(name));
create unique index indexed_folder_uk on indexed (folder)
  nulls not distinct
 where folder is not null;
create index indexed_qty_desc on indexed (qty desc nulls last)
  include (name)
  with (fillfactor = 70);
