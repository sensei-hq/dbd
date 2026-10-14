/* Every entity in one list (#34): tables, the v2 entities (views, materialized views, routines,
   triggers, and since v3 sequences) and the v3 enums, so the sidebar and the entity pages share one notion of what
   exists. Keyed `schema.name` — the id the diagram gives the same node. */
import { OVERVIEW_ICONS } from './overview';
import type { SchemaModel } from './model';

export type Kind =
  | 'table'
  | 'view'
  | 'materialized_view'
  | 'function'
  | 'procedure'
  | 'trigger'
  | 'sequence'
  | 'enum';

/** Reading order: storage, derived data, behaviour, then the sequences and types tables use. */
export const KIND_ORDER: Kind[] = [
  'table',
  'view',
  'materialized_view',
  'function',
  'procedure',
  'trigger',
  'sequence',
  'enum',
];

const LABELS: Record<Kind, [string, string]> = {
  table: ['Table', 'Tables'],
  view: ['View', 'Views'],
  materialized_view: ['Materialized view', 'Materialized views'],
  function: ['Function', 'Functions'],
  procedure: ['Procedure', 'Procedures'],
  trigger: ['Trigger', 'Triggers'],
  sequence: ['Sequence', 'Sequences'],
  enum: ['Enum', 'Enums'],
};

export const kindLabel = (kind: Kind, count: number) => LABELS[kind][count === 1 ? 0 : 1];

/** The diagram card's icon for each kind — the same glyphs the overview's tiles wear. */
export const KIND_ICON: Record<Kind, string> = {
  table: OVERVIEW_ICONS.tables,
  view: OVERVIEW_ICONS.view,
  materialized_view: OVERVIEW_ICONS.materialized_view,
  function: OVERVIEW_ICONS.function,
  procedure: OVERVIEW_ICONS.procedure,
  trigger: OVERVIEW_ICONS.trigger,
  sequence: OVERVIEW_ICONS.sequence,
  enum: OVERVIEW_ICONS.enums,
};

export type EntityItem = { key: string; kind: Kind; schema: string; name: string; note?: string; module?: string };

const rank = (k: Kind) => KIND_ORDER.indexOf(k);

/** Every entity, ordered by schema, then kind, then name. */
export function allEntities(model: SchemaModel): EntityItem[] {
  const item = (
    kind: Kind,
    e: { schema: string; name: string; note?: string; module?: string },
  ): EntityItem => ({
    key: `${e.schema}.${e.name}`,
    kind,
    schema: e.schema,
    name: e.name,
    note: e.note,
    ...(e.module === undefined ? {} : { module: e.module }),
  });
  const items = [
    ...model.tables.map((t) => item('table', t)),
    ...(model.entities ?? []).filter((e) => KIND_ORDER.includes(e.kind as Kind)).map((e) => item(e.kind as Kind, e)),
    ...(model.enums ?? []).map((e) => item('enum', e)),
  ];
  return items.sort(
    (a, b) => a.schema.localeCompare(b.schema) || rank(a.kind) - rank(b.kind) || a.name.localeCompare(b.name),
  );
}

/**
 * The entity a key names. A table wins over a routine of the same name — Postgres lets a
 * function share a table's name, and the diagram, which ids both `schema.name`, does the same.
 */
export function entityAt(model: SchemaModel, key: string): EntityItem | undefined {
  const matches = allEntities(model).filter((e) => e.key === key);
  return matches.sort((a, b) => rank(a.kind) - rank(b.kind))[0];
}

/** Each kind the model has, with its count, in kind order. */
export function kindCounts(model: SchemaModel): { kind: Kind; count: number }[] {
  const all = allEntities(model);
  return KIND_ORDER.map((kind) => ({ kind, count: all.filter((e) => e.kind === kind).length })).filter(
    (k) => k.count > 0,
  );
}
