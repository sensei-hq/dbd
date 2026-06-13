import type { SchemaModel } from './model';

/** A small example schema so the empty /diagram page can demo the viewer. */
export const SAMPLE_MODEL: SchemaModel = {
  project: { name: 'example', db: 'postgresql' },
  schemas: [{ name: 'shop', tables: 2, enums: 0 }],
  tables: [
    {
      schema: 'shop', name: 'customers', kind: 'table',
      note: 'People who place orders.',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true },
        { name: 'email', type: 'text', nn: true },
        { name: 'name', type: 'text' },
      ],
    },
    {
      schema: 'shop', name: 'orders', kind: 'table',
      columns: [
        { name: 'id', type: 'uuid', pk: true, nn: true },
        { name: 'customer_id', type: 'uuid', nn: true },
        { name: 'total', type: 'numeric' },
      ],
    },
  ],
  refs: [{ from: { s: 'shop', t: 'orders', c: 'customer_id' }, to: { s: 'shop', t: 'customers', c: 'id' } }],
};
