set search_path to app;

do $$ begin
  create type guarded as enum ('active', 'archived');
exception when duplicate_object then null;
end $$;
