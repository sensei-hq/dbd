# Adapter Catalog Queries Design

**Date:** 2026-05-04  |  **Status:** Draft
**Scope:** Enhanced `load_catalog()`, namespace-aware classification, catalog cache

---

## Overview

Replace static pattern matching in `classify_reference()` with live catalog queries. The current implementation uses hardcoded lists and regex prefixes (`pg_`, `json_`, etc.) which misclassifies extension-provided functions/types. By querying `pg_proc`, `pg_type`, and `pg_extension` with namespace info, the adapter accurately classifies references as internal, extension-provided, or user-defined.

## Current State

`PostgresAdapter.load_catalog()` queries `pg_proc`/`pg_type` but stores only bare names in `HashSet<String>` -- no namespace distinction. Extension functions come from `pg_depend` join. `classify_reference()` checks these sets then falls back to `matches_static_pattern()`. Problem: functions like `st_distance` (PostGIS) can produce false "unresolved reference" warnings.

## New Data Structure

```rust
pub struct CatalogData {
    pub functions: HashSet<String>,              // "pg_catalog.array_agg", "extensions.st_distance"
    pub types: HashSet<String>,                  // "pg_catalog.int4", "public.geometry"
    pub extension_objects: HashMap<String, String>, // bare name -> extension name
    pub extension_schemas: HashSet<String>,      // schemas owned by extensions
}
```

Replaces the three separate fields (`builtin_functions`, `builtin_types`, `extension_objects`) in `PostgresAdapter`.

## Enhanced Queries

```sql
-- Functions with namespace
SELECT n.nspname, p.proname FROM pg_proc p
JOIN pg_namespace n ON p.pronamespace = n.oid;

-- Types with namespace
SELECT n.nspname, t.typname FROM pg_type t
JOIN pg_namespace n ON t.typnamespace = n.oid;

-- Extension functions + types (existing query extended with namespace)
SELECT p.proname, e.extname, n.nspname FROM pg_proc p
JOIN pg_depend d ON d.objid = p.oid AND d.deptype = 'e'
JOIN pg_extension e ON e.oid = d.refobjid
JOIN pg_namespace n ON p.pronamespace = n.oid;

-- Extension schemas
SELECT n.nspname FROM pg_extension e
JOIN pg_namespace n ON e.extnamespace = n.oid;
```

## Classification Logic

`classify_from_catalog(name, catalog, installed_extensions) -> ReferenceClass`

1. `is_sql_noise(name)` -> `Internal` (unchanged, fast path)
2. `catalog.extension_objects.get(name)` -> `Extension(ext_name)`
3. Qualified name (contains `.`): check `catalog.functions`/`catalog.types` directly
4. Bare name: check if `pg_catalog.<name>` exists in catalog -> `Internal`
5. Name in `extension_schemas` namespace -> `Extension("unknown")`
6. `matches_static_pattern(name)` -> `Internal` (offline fallback)
7. Default -> `UserDefined`

## Catalog Cache

**Location:** `~/.cache/dbd/catalog/<sha256(url)>.json`

```rust
struct CatalogCache {
    url_hash: String,
    created_at: String,   // ISO 8601
    ttl_hours: u32,       // default: 24, configurable via DBD_CATALOG_TTL env var
    data: CatalogData,
}
```

On `load_catalog()`: read cache if present and within TTL, otherwise query DB and write cache. `--no-cache` global flag forces refresh.

## Test Scenarios

| ID | Scenario | Assert |
|----|----------|--------|
| C1 | pg_catalog function | `classify("array_agg")` -> `Internal` |
| C2 | Extension function | `classify("st_distance")` -> `Extension("postgis")` |
| C3 | Unknown function | `classify("my_custom_func")` -> `UserDefined` |
| C4 | SQL noise | `classify("varchar")` -> `Internal` (no catalog needed) |
| C5 | Qualified name | `classify("extensions.st_distance")` -> `Extension("postgis")` |
| C6 | Cache written | After first `load_catalog()`, cache file exists |
| C7 | Cache read | Within TTL: no DB queries, cache used |
| C8 | Stale cache | Past TTL: DB queried, cache overwritten |
| C9 | --no-cache | Cache bypassed, DB queried |
| C10 | No extensions | pg_catalog functions still classified correctly |
| C11 | Empty catalog | Static pattern fallback works |
| C12 | Extension schema | Function in extension schema -> `Extension("unknown")` |

## Files

| File | Action |
|------|--------|
| `crates/dbd-core/src/adapter/mod.rs` | Modify -- add `CatalogData` struct |
| `crates/dbd-core/src/adapter/postgres.rs` | Modify -- enhanced `load_catalog()`, consolidated fields, cache |
| `crates/dbd-cli/src/cli.rs` | Modify -- `--no-cache` global flag |

## Future Work

- Incremental catalog refresh on extension install/uninstall
- Multi-database cache entries
- Catalog-driven DDL autocompletion for editor integrations
