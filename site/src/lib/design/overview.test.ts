import { it, expect, describe } from 'vitest';
import { DEFAULT_ICONS } from '@rokkit/graph/icons';
import { overviewCounts, schemaRows, OVERVIEW_ICONS } from './overview';
import { sampleModel } from './data';
import type { SchemaModel } from './model';

// The project at a glance (#28). Every number is re-derived from the model here rather than
// copied from the implementation, so a count that drifts from what it names fails.

const counts = (m: SchemaModel) => Object.fromEntries(overviewCounts(m).map((c) => [c.key, c.count]));

describe('overviewCounts', () => {
  it('counts each kind from the model, in a fixed reading order', () => {
    const m = sampleModel;
    const kinds = (k: string) => (m.entities ?? []).filter((e) => e.kind === k).length;
    expect(overviewCounts(m).map((c) => c.key)).toEqual([
      'schemas',
      'tables',
      'view',
      'procedure',
      'enums',
      'references',
    ]);
    expect(counts(m)).toEqual({
      schemas: m.schemas.length,
      tables: m.tables.length,
      view: kinds('view'),
      procedure: kinds('procedure'),
      enums: m.schemas.reduce((a, s) => a + s.enums, 0),
      references: m.refs.length,
    });
  });

  it('omits view and routine counts for a v1 model, rather than reporting 0', () => {
    const { entities: _e, deps: _d, version: _v, ...v1 } = sampleModel;
    expect(overviewCounts(v1).map((c) => c.key)).toEqual(['schemas', 'tables', 'enums', 'references']);
  });

  it('leaves out an entity kind the project has none of', () => {
    expect(overviewCounts(sampleModel).map((c) => c.key)).not.toContain('trigger');
  });

  it('labels a count of one in the singular', () => {
    const byKey = Object.fromEntries(overviewCounts(sampleModel).map((c) => [c.key, c.label]));
    expect(byKey.view).toBe('View');
    expect(byKey.tables).toBe('Tables');
    expect(byKey.enums).toBe('Enum');
  });

  it('gives every kind the icon its diagram card uses', () => {
    const byKey = Object.fromEntries(overviewCounts(sampleModel).map((c) => [c.key, c.icon]));
    expect(byKey.tables).toBe(DEFAULT_ICONS.table);
    expect(byKey.view).toBe(DEFAULT_ICONS.view);
    expect(byKey.procedure).toBe(DEFAULT_ICONS.procedure);
    expect(byKey.enums).toBe(DEFAULT_ICONS.enum);
    expect(byKey.references).toBe(DEFAULT_ICONS.fk);
  });

  it('names only icons the UnoCSS safelist can see', () => {
    for (const c of overviewCounts(sampleModel)) expect(Object.values(OVERVIEW_ICONS)).toContain(c.icon);
  });
});

describe('schemaRows', () => {
  it('breaks the counts down per schema', () => {
    expect(schemaRows(sampleModel)).toEqual([
      { name: 'auth', tables: 2, enums: 0, other: 0 },
      { name: 'shop', tables: 4, enums: 1, other: 2 },
    ]);
  });
});

describe('a project with sequences', () => {
  it('counts them beside the other entity kinds, before the enums', () => {
    const m: SchemaModel = {
      ...sampleModel,
      entities: [...(sampleModel.entities ?? []), { schema: 'shop', name: 'invoice_no', kind: 'sequence' }],
    };
    expect(overviewCounts(m).map((c) => c.key)).toEqual([
      'schemas',
      'tables',
      'view',
      'procedure',
      'sequence',
      'enums',
      'references',
    ]);
    expect(counts(m).sequence).toBe(1);
  });
});
