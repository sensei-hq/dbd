set search_path to app;

create table if not exists "MixedCase" (
  "Id"        uuid primary key
, "FirstName" text not null
, "createdAt" timestamp with time zone not null default now()
);

create unique index if not exists "MixedCase_FirstName_uk" on "MixedCase" ("FirstName");
