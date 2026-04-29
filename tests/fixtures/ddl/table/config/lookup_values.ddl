set search_path to config, extensions;

create table if not exists lookup_values (
  id                       uuid primary key default uuid_generate_v4()
, lookup_id                uuid references lookups(id)
, value                    varchar(255)
, sequence                 integer
, is_active                boolean default true
, is_hidden                boolean default false
, details                  jsonb
, description              text
, modified_at              timestamp with time zone not null default now()
, modified_by              varchar
, category_id              uuid references categories(id)
, notes                    text
);

create unique index if not exists lookup_values_ukey on lookup_values(lookup_id, value);

comment on table lookup_values IS
'Different values associated with various lookups.
- Used to store predefined values for different lookup categories.
- Each value is associated with a specific lookup.';

comment on column lookup_values.id IS
'Unique identifier for the lookup value. Ensures each value can be uniquely identified.';

comment on column lookup_values.lookup_id IS
'Identifier for the lookup category. References the lookups table to specify the category.';

comment on column lookup_values.value IS
'The actual value associated with the lookup.';

comment on column lookup_values.sequence IS
'Order of the value within the lookup category. Used for sorting purposes.';

comment on column lookup_values.is_active IS
'Flag indicating if the lookup value is active. Used to determine if the value is currently in use.';

comment on column lookup_values.is_hidden IS
'Flag indicating if the lookup value should be hidden. Used to exclude the value from display.';

comment on column lookup_values.details IS
'Additional details associated with the lookup value. Stored as a JSONB field to accommodate various attributes.';

comment on column lookup_values.description IS
'Description of the lookup value. Provides additional context about the value.';

comment on column lookup_values.modified_at IS
'The date the lookup value was last modified. Provides a history of when changes were made.';

comment on column lookup_values.modified_by IS
'The user who last modified the lookup value. Tracks who made the changes for auditing purposes.';
