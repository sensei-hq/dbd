export type Column = { name: string; type: string; pk?: boolean; nn?: boolean; en?: boolean; def?: string; note?: string };
export type Index = { def: string; unique?: boolean; name?: string };
export type Table = {
  schema: string;
  name: string;
  kind: string;
  note?: string;
  noteMd?: string;
  columns: Column[];
  indexes?: Index[];
};
export type RefEnd = { s: string; t: string; c: string };
export type Ref = { from: RefEnd; to: RefEnd; action?: string };
export type SchemaModel = {
  project: { name: string; db: string; note?: string };
  schemas: { name: string; tables: number; enums: number }[];
  tables: Table[];
  refs: Ref[];
};

export type ValidationResult = { ok: true; model: SchemaModel } | { ok: false; error: string };

/** Shape-check arbitrary JSON before handing it to the viewer. */
export function validateModel(value: unknown): ValidationResult {
  if (typeof value !== 'object' || value === null) return { ok: false, error: 'not a JSON object' };
  const v = value as Record<string, unknown>;
  const project = v.project as Record<string, unknown> | undefined;
  if (!project || typeof project.name !== 'string') return { ok: false, error: 'missing project.name' };
  if (!Array.isArray(v.schemas)) return { ok: false, error: 'missing schemas[]' };
  if (!Array.isArray(v.tables)) return { ok: false, error: 'missing tables[]' };
  if (!Array.isArray(v.refs)) return { ok: false, error: 'missing refs[]' };
  for (const t of v.tables) {
    const tt = t as Record<string, unknown>;
    if (typeof tt.schema !== 'string' || typeof tt.name !== 'string' || !Array.isArray(tt.columns))
      return { ok: false, error: 'malformed table entry' };
  }
  return { ok: true, model: value as SchemaModel };
}

export const nodeId = (schema: string, name: string) => `${schema}.${name}`;

export type LayoutColumn = Column & { fk: boolean };
export type LayoutData = { tables: { schema: string; name: string; columns: LayoutColumn[] }[]; refs: Ref[] };

/** Map a SchemaModel to the layout's input, deriving a per-column `fk` flag from refs. */
export function toLayoutData(model: SchemaModel): LayoutData {
  const fkCols = new Set<string>();
  for (const r of model.refs) fkCols.add(`${r.from.s}.${r.from.t}.${r.from.c}`);
  return {
    tables: model.tables.map((t) => ({
      schema: t.schema,
      name: t.name,
      columns: t.columns.map((c) => ({ ...c, fk: fkCols.has(`${t.schema}.${t.name}.${c.name}`) })),
    })),
    refs: model.refs,
  };
}

/** Tables connected to `id` (schema.name) via any ref, either direction. */
export function neighborsOf(model: SchemaModel, id: string): Set<string> {
  const out = new Set<string>();
  for (const r of model.refs) {
    const f = nodeId(r.from.s, r.from.t), t = nodeId(r.to.s, r.to.t);
    if (f === id) out.add(t);
    if (t === id) out.add(f);
  }
  return out;
}
