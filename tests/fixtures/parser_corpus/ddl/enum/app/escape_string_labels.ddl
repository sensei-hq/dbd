set search_path to app;

create type escape_string_labels as enum (E'a\tb', 'plain');
