/* The project at a glance (#28): what the overview counts, and the icon each count wears.
   ============================================================================
   Pure, so every number can be checked against the model it claims to count. No Svelte
   and no `$lib` at runtime: `uno.config.ts` imports `OVERVIEW_ICONS` for its safelist, and
   Node loads that config directly. */
import { DEFAULT_ICONS } from '@rokkit/graph/icons';
import type { SchemaModel } from './model';

/**
 * Every icon the overview can name. The tiles pick them at runtime from a count's kind, so
 * UnoCSS's extractor never sees them in source and would purge them — the safelist spreads
 * this. Entity kinds wear their diagram card's icon, so a view looks the same here as there.
 */
export const OVERVIEW_ICONS = {
  schemas: 'i-glyph:folder',
  tables: DEFAULT_ICONS.table,
  view: DEFAULT_ICONS.view,
  materialized_view: DEFAULT_ICONS.materialized_view,
  function: DEFAULT_ICONS.function,
  procedure: DEFAULT_ICONS.procedure,
  trigger: DEFAULT_ICONS.trigger,
  enums: DEFAULT_ICONS.enum,
  /** The diagram's foreign-key row badge. */
  references: DEFAULT_ICONS.fk,
  database: 'i-glyph:database',
} as const;

/** v2 entity kinds, in reading order: derived data, then behaviour. */
const ENTITY_KINDS = [
  ['view', 'View', 'Views'],
  ['materialized_view', 'Materialized view', 'Materialized views'],
  ['function', 'Function', 'Functions'],
  ['procedure', 'Procedure', 'Procedures'],
  ['trigger', 'Trigger', 'Triggers'],
] as const;

export type Count = { key: string; count: number; label: string; icon: string };

const counted = (key: string, count: number, one: string, many: string, icon: string): Count => ({
  key,
  count,
  label: count === 1 ? one : many,
  icon,
});

/**
 * Schemas, tables, each entity kind the project has, enums and references — in that order.
 *
 * An entity kind with none is left out rather than shown as 0: on a v1 payload there is no
 * `entities` half at all, and "0 views" would claim a fact the model never carried.
 */
export function overviewCounts(model: SchemaModel): Count[] {
  const enums = model.schemas.reduce((total, s) => total + s.enums, 0);
  const entities = model.entities ?? [];
  const kinds = ENTITY_KINDS.map(([kind, one, many]) =>
    counted(kind, entities.filter((e) => e.kind === kind).length, one, many, OVERVIEW_ICONS[kind]),
  ).filter((c) => c.count > 0);
  return [
    counted('schemas', model.schemas.length, 'Schema', 'Schemas', OVERVIEW_ICONS.schemas),
    counted('tables', model.tables.length, 'Table', 'Tables', OVERVIEW_ICONS.tables),
    ...kinds,
    counted('enums', enums, 'Enum', 'Enums', OVERVIEW_ICONS.enums),
    counted('references', model.refs.length, 'Reference', 'References', OVERVIEW_ICONS.references),
  ];
}

export type SchemaRow = { name: string; tables: number; enums: number; other: number };

/**
 * The same counts, per schema. Tables are counted from the model rather than read from the
 * schema entry, so the two halves of the page cannot disagree; enums have no list of their
 * own in the model, so they are the entry's number. `other` is views and routines.
 */
export function schemaRows(model: SchemaModel): SchemaRow[] {
  return model.schemas.map((s) => ({
    name: s.name,
    tables: model.tables.filter((t) => t.schema === s.name).length,
    enums: s.enums,
    other: (model.entities ?? []).filter((e) => e.schema === s.name).length,
  }));
}

/** How the database type reads in prose. */
export function databaseLabel(db: string): string {
  const names: Record<string, string> = {
    postgresql: 'PostgreSQL',
    postgres: 'PostgreSQL',
    supabase: 'Supabase',
    sqlite: 'SQLite',
    convex: 'Convex',
  };
  return names[db.toLowerCase()] ?? db;
}
