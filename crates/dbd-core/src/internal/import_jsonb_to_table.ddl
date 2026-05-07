-- internal dbd helper — do not edit, managed by dbd automatically
-- Loaded and applied by dbd before any JSONL import runs.
--
-- Uses pg_catalog to resolve real column types, including enums (USER-DEFINED),
-- so explicit per-column casting works for any table without a custom procedure.
-- Skips GENERATED ALWAYS AS IDENTITY and stored generated columns automatically.
create or replace procedure staging.import_jsonb_to_table(
    p_source_table text
  , p_target_table text
)
language plpgsql
as
$$
declare
  v_target_schema text := split_part(p_target_table, '.', 1);
  v_target_name   text := split_part(p_target_table, '.', 2);
  v_col_exprs     text;
  v_sql           text;
begin
  -- Build per-column cast expressions using pg_catalog so that enum types
  -- (reported as USER-DEFINED by information_schema) are resolved correctly.
  select string_agg(
    format(
      '(data->>%L)::%s'
    , a.attname
    , case
        when t.typtype = 'e'
          then quote_ident(tn.nspname) || '.' || quote_ident(t.typname)
        else t.typname
      end
    )
  , ', ' order by a.attnum
  )
  into v_col_exprs
  from pg_catalog.pg_attribute  a
  join pg_catalog.pg_class       c  on c.oid  = a.attrelid
  join pg_catalog.pg_namespace   cn on cn.oid = c.relnamespace
  join pg_catalog.pg_type        t  on t.oid  = a.atttypid
  join pg_catalog.pg_namespace   tn on tn.oid = t.typnamespace
  where cn.nspname     = v_target_schema
    and c.relname      = v_target_name
    and a.attnum       > 0
    and not a.attisdropped
    and a.attidentity  <> 'a'   -- exclude GENERATED ALWAYS AS IDENTITY
    and a.attgenerated = ''     -- exclude stored generated columns

  if v_col_exprs is null then
    raise exception 'import_jsonb_to_table: table %.% not found or has no columns',
      v_target_schema, v_target_name;
  end if;

  v_sql := format(
    'insert into %s select %s from %I'
  , p_target_table
  , v_col_exprs
  , p_source_table
  );
  execute v_sql;
end;
$$
