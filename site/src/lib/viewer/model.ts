export type Column = { name: string; type: string; pk?: boolean; nn?: boolean; en?: boolean; def?: string; note?: string };
export type Table = { schema: string; name: string; kind: string; note?: string; noteMd?: string; columns: Column[] };
export type RefEnd = { s: string; t: string; c: string };
export type Ref = { from: RefEnd; to: RefEnd; action?: string };
export type SchemaModel = {
  project: { name: string; db: string; note?: string };
  schemas: { name: string; tables: number; enums: number }[];
  tables: Table[];
  refs: Ref[];
};

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
