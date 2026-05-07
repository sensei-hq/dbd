-- internal dbd helper — do not edit, managed by dbd automatically
-- Loaded and applied by dbd before any JSONL import runs.
--
-- Handles three column categories via pg_catalog:
--   • Enums (typtype = 'e')  — (data->>'col')::schema.enumtype
--   • Arrays (typcategory = 'A') — JSON array → PG array via jsonb_array_elements_text;
--     NULL-safe (returns SQL NULL when key is absent or JSON null).
--     For arrays of user-defined types (enums, composites) the element type is
--     schema-qualified using the element type's namespace.
--   • Scalars — (data->>'col')::typename
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
  select string_agg(
    case
      when t.typtype = 'e'
        -- Enum scalar: cast text to qualified enum type
        then format('(data->>%L)::%s',
               a.attname,
               quote_ident(tn.nspname) || '.' || quote_ident(t.typname))

      when t.typcategory = 'A'
        -- Array: expand JSON array and cast to target type.
        -- jsonb_typeof guard returns SQL NULL for missing/null keys.
        -- Element type is schema-qualified for user-defined types (enums, composites).
        then format(
               $f$case when jsonb_typeof(data->%L) = 'array'
                    then array(select jsonb_array_elements_text(data->%L))
                    else null
               end::%s$f$,
               a.attname,
               a.attname,
               case when et.typtype in ('e', 'c')
                    then quote_ident(etn.nspname) || '.' || quote_ident(et.typname) || '[]'
                    else format_type(a.atttypid, a.atttypmod)
               end)

      else
        -- Scalar: cast text representation to the column type
        format('(data->>%L)::%s', a.attname, t.typname)
    end
  , ', ' order by a.attnum
  )
  into v_col_exprs
  from pg_catalog.pg_attribute  a
  join pg_catalog.pg_class       c   on c.oid   = a.attrelid
  join pg_catalog.pg_namespace   cn  on cn.oid  = c.relnamespace
  join pg_catalog.pg_type        t   on t.oid   = a.atttypid
  join pg_catalog.pg_namespace   tn  on tn.oid  = t.typnamespace
  -- element type joins (used only for array columns)
  left join pg_catalog.pg_type      et  on et.oid  = t.typelem
  left join pg_catalog.pg_namespace etn on etn.oid = et.typnamespace
  where cn.nspname     = v_target_schema
    and c.relname      = v_target_name
    and a.attnum       > 0
    and not a.attisdropped
    and a.attidentity  <> 'a'   -- exclude GENERATED ALWAYS AS IDENTITY
    and a.attgenerated = ''     -- exclude stored generated columns
  ;

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
