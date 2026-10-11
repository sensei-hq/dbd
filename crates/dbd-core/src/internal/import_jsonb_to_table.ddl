-- internal dbd helper — do not edit, managed by dbd automatically
-- Loaded and applied by dbd before any JSONL import runs.
--
-- Reads each staged record as json (`data::json`): dbd stages records as json, so
-- a json column's value keeps the text it was written with; an older jsonb staging
-- table casts the same way.
--
-- Handles four column categories via pg_catalog:
--   • json / jsonb (and domains over them) — (data->'col')::type: the JSON value
--     itself, so a string scalar keeps its quotes. `null` is SQL NULL, as for
--     every other column; the JSON value null is written {"$json": null}, and
--     any one-key {"$json": …} object is unwrapped one level, so a value that
--     is itself such an object survives as {"$json": {"$json": …}}. An absent
--     key is SQL NULL.
--   • Enums (typtype = 'e')  — (data->>'col')::schema.enumtype
--   • Arrays (typcategory = 'A') — JSON array → PG array via json_array_elements_text
--     (json_array_elements for an array of json/jsonb); NULL-safe (returns SQL NULL
--     when key is absent or JSON null). For arrays of user-defined types (enums,
--     composites) the element type is schema-qualified using the element type's
--     namespace.
--   • Scalars — (data->>'col')::typename; a JSON null is SQL NULL.
-- Skips stored and virtual generated columns: their values are computed.
-- Writes GENERATED ALWAYS identity columns as given (OVERRIDING SYSTEM VALUE), so
-- exported ids import as-is and the rows referencing them still match.
--
-- Only columns present in the staged json are inserted: the INSERT carries an
-- explicit column list restricted to keys that appear in the source data, so a
-- column absent from every row falls back to its table DEFAULT instead of being
-- forced to NULL. Without this, a seed that omits a `not null default now()`
-- column (e.g. `modified_at`) would violate the not-null constraint even though
-- the default would have satisfied it.
create or replace procedure dbd.import_jsonb_to_table(
    p_source_table text
  , p_target_table text
)
language plpgsql
-- Pinned search_path (Supabase lint 0011 "function_search_path_mutable"): the body is fully
-- pg_catalog-qualified and every user type it emits into dynamic SQL is schema-qualified
-- (enums, array elements, and scalars via format_type below), so '' is safe and immune to a
-- caller-hijacked path.
set search_path = ''
as
$$
declare
  v_target_schema text := split_part(p_target_table, '.', 1);
  v_target_name   text := split_part(p_target_table, '.', 2);
  v_present_keys  text[];
  v_col_list      text;
  v_col_exprs     text;
  v_sql           text;
begin
  -- Keys that actually appear in the staged json (union across all rows).
  -- Columns not in this set are omitted from the INSERT so their DEFAULT fires.
  execute format(
    'select coalesce(array_agg(distinct k), array[]::text[]) from %s, lateral pg_catalog.json_object_keys(data::pg_catalog.json) as k'
  , p_source_table
  ) into v_present_keys;

  -- Nothing staged (no rows / no keys) → nothing to import.
  if array_length(v_present_keys, 1) is null then
    return;
  end if;

  select
      string_agg(quote_ident(a.attname), ', ' order by a.attnum),
      string_agg(
        case
          when coalesce(nullif(t.typbasetype, 0), t.oid)
               in ('pg_catalog.json'::pg_catalog.regtype, 'pg_catalog.jsonb'::pg_catalog.regtype)
            -- json/jsonb: the JSON value, not its text — `->>` strips a string's
            -- quotes (so the cast fails) and turns a JSON null into SQL NULL.
            then format(
                   $f$case
                        when pg_catalog.json_typeof(data::pg_catalog.json->%1$L) = 'object'
                         and (data::pg_catalog.json->%1$L->'$json') is not null
                         and (select count(*) from pg_catalog.json_object_keys(data::pg_catalog.json->%1$L)) = 1
                        then (data::pg_catalog.json->%1$L->'$json')::%2$s
                        when pg_catalog.json_typeof(data::pg_catalog.json->%1$L) = 'null' then null
                        else (data::pg_catalog.json->%1$L)::%2$s
                   end$f$,
                   a.attname, format_type(a.atttypid, a.atttypmod))

          when t.typtype = 'e'
            -- Enum scalar: cast text to qualified enum type
            then format('(data::pg_catalog.json->>%L)::%s',
                   a.attname,
                   quote_ident(tn.nspname) || '.' || quote_ident(t.typname))

          when t.typcategory = 'A'
            -- Array: expand JSON array and cast to target type.
            -- json_typeof guard returns SQL NULL for missing/null keys.
            -- Element type is schema-qualified for user-defined types (enums, composites).
            -- An array of json/jsonb takes each element as JSON, as a json column does.
            then format(
                   $f$case when pg_catalog.json_typeof(data::pg_catalog.json->%L) = 'array'
                        then array(select pg_catalog.%s(data::pg_catalog.json->%L))
                        else null
                   end::%s$f$,
                   a.attname,
                   case when et.oid in ('pg_catalog.json'::pg_catalog.regtype, 'pg_catalog.jsonb'::pg_catalog.regtype)
                        then 'json_array_elements'
                        else 'json_array_elements_text'
                   end,
                   a.attname,
                   case when et.typtype in ('e', 'c')
                        then quote_ident(etn.nspname) || '.' || quote_ident(et.typname) || '[]'
                        else format_type(a.atttypid, a.atttypmod)
                   end)

          else
            -- Scalar: cast text representation to the column type. format_type schema-
            -- qualifies a user-defined scalar/domain type (required now search_path is pinned
            -- to '' — an unqualified name would not resolve); built-ins render canonically
            -- (integer, text, …). -1 typmod = no length modifier, matching the prior cast.
            format('(data::pg_catalog.json->>%L)::%s', a.attname, format_type(a.atttypid, -1))
        end
      , ', ' order by a.attnum)
  into v_col_list, v_col_exprs
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
    and a.attgenerated = ''            -- exclude stored and virtual generated columns
    and a.attname::text = any(v_present_keys)  -- only columns present in the source data
  ;

  if v_col_exprs is null then
    raise exception 'import_jsonb_to_table: table %.% has no columns matching the staged data keys (%)',
      v_target_schema, v_target_name, v_present_keys;
  end if;

  -- p_source_table is a dbd-managed, schema-qualified staging table
  -- (e.g. dbd.import_tmp), so substitute it raw with %s — %I would
  -- quote the dot into a single identifier, and an unqualified name would
  -- resolve against the session search_path, which pooled connections don't
  -- share. See sensei-hq/dbd#6.
  --
  -- OVERRIDING SYSTEM VALUE writes a GENERATED ALWAYS identity column as given;
  -- it changes nothing for a table without one.
  v_sql := format(
    'insert into %s (%s) overriding system value select %s from %s'
  , p_target_table
  , v_col_list
  , v_col_exprs
  , p_source_table
  );
  execute v_sql;
end;
$$
