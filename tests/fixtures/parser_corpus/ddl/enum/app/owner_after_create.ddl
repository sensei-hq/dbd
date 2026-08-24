set search_path to app;

create type owner_after_create as enum ('x', 'y');
alter type owner_after_create owner to current_user;
