/* The TypeScript mirror of `crates/dbd-core/src/schema_model.rs`.
   ============================================================================
   Hand-maintained, and deliberately so: the viewer that reads it moved to
   `@rokkit/graph`, which does NOT import this type. Keeping the mirror here
   means the package never becomes a third definition to keep in step.

   v2 (2026-09-27) added `version`, `entities`, `deps`, and `fk`/`uq` on
   `Column`; v3 (2026-10-01) added `history` and `enums`, and later `stubs`, sequences in
   `entities`, `Table.checks`, `Index.where` and `Column.identity`/`generated`. Everything
   past v1 is optional here, so a share link encoded before an upgrade still validates and
   still renders. */

export type Column = {
  name: string;
  type: string;
  pk?: boolean;
  nn?: boolean;
  en?: boolean;
  def?: string;
  note?: string;
  /** v2. Declared by dbd, which resolved the constraint against the whole database. */
  fk?: boolean;
  /** v2. Previously only derivable from `Index.unique`. */
  uq?: boolean;
  /** v3. The column is `GENERATED … AS IDENTITY`; its values come from a sequence, not `def`. */
  identity?: 'always' | 'by default';
  /** v3. The expression of a `GENERATED ALWAYS AS (…) STORED` column — not a default. */
  generated?: string;
};
export type Index = {
  def: string;
  unique?: boolean;
  name?: string;
  /** v3. A partial index's `WHERE` predicate, as authored. */
  where?: string;
};
/** v3. A CHECK constraint, named when it was given a name. */
export type Check = { name?: string; expression: string };
export type Table = {
  schema: string;
  name: string;
  /** `table` in `SchemaModel.tables`; a `StubKind` in `SchemaModel.stubs`. */
  kind: string;
  note?: string;
  noteMd?: string;
  columns: Column[];
  indexes?: Index[];
  /** v3. CHECK constraints, inline ones first. */
  checks?: Check[];
};
export type RefEnd = { s: string; t: string; c: string };
export type Ref = { from: RefEnd; to: RefEnd; action?: string };

/**
 * v3. Why a table a foreign key lands on is a stub rather than one of the model's tables:
 * declared under `external:` in design.yaml, a table of the project the scope leaves out, or
 * defined nowhere in the project.
 */
export type StubKind = 'external' | 'out_of_scope' | 'unresolved';

/** v2. One end of a dependency: schema and name, no column. */
export type NodeRef = { s: string; n: string };

/**
 * v2. A non-table entity: a view, materialized view, function, procedure or trigger — and
 * since v3 a sequence.
 *
 * No columns — a parsed routine has none and a view's are not read. What it has is a body
 * and the things it depends on, which are in `SchemaModel.deps`.
 */
export type EntityNode = {
  schema: string;
  name: string;
  /** `view` | `materialized_view` | `function` | `procedure` | `trigger` | `sequence` */
  kind: string;
  note?: string;
  noteMd?: string;
};

/** v2. One dependency edge: a view reading a table, a routine calling a routine. */
export type DepEdge = {
  from: NodeRef;
  to: NodeRef;
  /** `reads` | `writes` | `calls` | `member` */
  kind: string;
  /**
   * The target did not resolve to anything in the project — a built-in, or a genuine
   * dangling reference. A renderer should DIM rather than drop it: the edge is real, the
   * endpoint is not placeable.
   */
  unresolved?: boolean;
};

/** v3. An enum type and its values, in declaration order. */
export type EnumNode = {
  schema: string;
  name: string;
  values: string[];
  note?: string;
  noteMd?: string;
};

/** v3. What happened to an entity or a field in one version. */
export type ChangeOp = 'added' | 'removed' | 'modified' | 'renamed';

/** v3. A column, constraint, index or enum value inside a modified entity. */
export type FieldEdit = {
  kind: 'column' | 'constraint' | 'index' | 'value';
  name: string;
  op: ChangeOp;
  /** Before: the definition, or for a rename the old name. */
  from?: string;
  /** After: the definition, or for a rename the new name. */
  to?: string;
  /** What changed when the definition reads the same either side — `comment`. */
  note?: string;
};

/** v3. A table or enum one version added, removed or modified. */
export type EntityChange = {
  kind: 'table' | 'enum';
  schema: string;
  name: string;
  op: ChangeOp;
  fields: FieldEdit[];
};

/**
 * v3. One version of the schema, from `snapshots/NNN.json` (`crates/dbd-core/src/history.rs`).
 * The first carries `baseline` counts instead of changes; a multi-stage version spans
 * `version`…`through`.
 */
export type HistoryEntry = {
  version: number;
  through?: number;
  description: string;
  timestamp: string;
  baseline?: { tables: number; enums: number };
  changes: EntityChange[];
};

/**
 * A schema is two graphs, and this type keeps them apart on purpose.
 *
 * `tables` + `refs` is the ER diagram: entities and their foreign keys. `entities` + `deps`
 * is the dependency graph: what reads, writes or calls what. An ER renderer wants the first
 * pair and a call-graph renderer wants the second — see `toGraphInput` in
 * `@rokkit/graph/schema`, which takes the scope as an argument.
 */
export type SchemaModel = {
  /** v2 onward. Absent on a v1 payload. */
  version?: number;
  project: { name: string; db: string; note?: string };
  schemas: { name: string; tables: number; enums: number }[];
  /** Tables only. Every other kind is in `entities`. */
  tables: Table[];
  /** Foreign keys only. The dependency graph is `deps`. */
  refs: Ref[];
  /**
   * v3. The tables `refs` land on that `tables` does not carry, each with only the columns the
   * refs land on and a `StubKind` as its `kind`. Apart from `tables`, so nothing counting or
   * listing the model's tables is handed one it does not own. Absent when every ref lands
   * inside the model.
   */
  stubs?: Table[];
  /** v2. Views, materialized views, functions, procedures and triggers; v3 adds sequences. */
  entities?: EntityNode[];
  /** v2. What reads, writes or calls what. */
  deps?: DepEdge[];
  /** v3. What each snapshot changed, oldest first. Absent when there are no snapshots. */
  history?: HistoryEntry[];
  /** v3. Enum types with their values. Absent when the project has none. */
  enums?: EnumNode[];
};

export type ValidationResult = { ok: true; model: SchemaModel } | { ok: false; error: string };

/** The newest model version this viewer reads — `default_version()` in `schema_model.rs`. */
export const MODEL_VERSION = 3;

/** Shape-check arbitrary JSON before handing it to the viewer. */
export function validateModel(value: unknown): ValidationResult {
  if (typeof value !== 'object' || value === null) return { ok: false, error: 'not a JSON object' };
  const v = value as Record<string, unknown>;
  const project = v.project as Record<string, unknown> | undefined;
  if (!project || typeof project.name !== 'string') return { ok: false, error: 'missing project.name' };
  // Absent means v1. A version past this viewer's would render with whatever it added
  // silently dropped, so it is refused with the reason instead.
  if (v.version !== undefined) {
    if (typeof v.version !== 'number' || !Number.isInteger(v.version))
      return { ok: false, error: 'version must be an integer' };
    if (v.version > MODEL_VERSION)
      return {
        ok: false,
        error: `this model is version ${v.version}; this viewer reads up to version ${MODEL_VERSION} — open it on the current dbd site`,
      };
  }
  if (!Array.isArray(v.schemas)) return { ok: false, error: 'missing schemas[]' };
  if (!Array.isArray(v.tables)) return { ok: false, error: 'missing tables[]' };
  if (!Array.isArray(v.refs)) return { ok: false, error: 'missing refs[]' };
  if (!v.tables.every(isTableShaped)) return { ok: false, error: 'malformed table entry' };
  // Stubs become graph nodes beside the tables, so each is held to the same shape.
  if (v.stubs !== undefined && !(Array.isArray(v.stubs) && v.stubs.every(isTableShaped)))
    return { ok: false, error: 'stubs must be an array of table entries' };
  // v2 halves are optional, but a PRESENT one has to be the right shape — a payload carrying
  // `entities: {}` would otherwise reach the renderer and fail there instead of here.
  if (v.entities !== undefined && !Array.isArray(v.entities))
    return { ok: false, error: 'entities must be an array' };
  if (v.deps !== undefined && !Array.isArray(v.deps))
    return { ok: false, error: 'deps must be an array' };
  if (v.history !== undefined && !Array.isArray(v.history))
    return { ok: false, error: 'history must be an array' };
  if (v.enums !== undefined && !Array.isArray(v.enums))
    return { ok: false, error: 'enums must be an array' };
  return { ok: true, model: value as SchemaModel };
}

function isTableShaped(t: unknown): boolean {
  const tt = t as Record<string, unknown> | null;
  return (
    typeof tt === 'object' &&
    tt !== null &&
    typeof tt.schema === 'string' &&
    typeof tt.name === 'string' &&
    Array.isArray(tt.columns)
  );
}

export const nodeId = (schema: string, name: string) => `${schema}.${name}`;

/**
 * The model the ER graph draws: the stubs beside the tables. `toGraphInput` makes nodes of
 * `tables` alone, so without this every foreign key into a stub would be an unplaced edge.
 */
export const withStubs = (model: SchemaModel): SchemaModel =>
  model.stubs?.length ? { ...model, tables: [...model.tables, ...model.stubs] } : model;

/** The stub a `schema.name` key names, if it names one. */
export const stubAt = (model: SchemaModel, schema: string, name: string): Table | undefined =>
  model.stubs?.find((t) => t.schema === schema && t.name === name);
