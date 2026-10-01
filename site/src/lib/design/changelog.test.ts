import { it, expect, describe } from 'vitest';
import { newestFirst, summaryParts, versionLabel, versionSummary, versionDate } from './changelog';
import { sampleModel } from './data';
import type { HistoryEntry } from './model';

// What a changelog card says about a version (#29). The counts are re-derived from the entries
// themselves, so a tally that drifts from the list it sits above fails here.

const entry = (over: Partial<HistoryEntry>): HistoryEntry => ({
  version: 2,
  description: 'd',
  timestamp: '2026-09-03T10:00:00Z',
  changes: [],
  ...over,
});

describe('versionSummary', () => {
  it('counts entities by what happened to them, and the field edits inside', () => {
    const e = entry({
      changes: [
        { kind: 'table', schema: 'app', name: 'a', op: 'added', fields: [] },
        { kind: 'enum', schema: 'app', name: 'b', op: 'added', fields: [] },
        { kind: 'table', schema: 'app', name: 'c', op: 'removed', fields: [] },
        {
          kind: 'table',
          schema: 'app',
          name: 'd',
          op: 'modified',
          fields: [
            { kind: 'column', name: 'x', op: 'added', to: 'text' },
            { kind: 'column', name: 'y', op: 'renamed', from: 'z', to: 'y' },
          ],
        },
      ],
    });
    const byOp = (op: string) => e.changes.filter((c) => c.op === op).length;
    expect(versionSummary(e)).toEqual({
      added: byOp('added'),
      modified: byOp('modified'),
      removed: byOp('removed'),
      fields: e.changes.reduce((n, c) => n + c.fields.length, 0),
    });
  });

  it('is all zeros for a version that changed no table or enum', () => {
    expect(versionSummary(entry({}))).toEqual({ added: 0, modified: 0, removed: 0, fields: 0 });
  });
});

describe('newestFirst', () => {
  it('orders the history newest version first, without reordering the model', () => {
    const history = sampleModel.history!;
    const versions = newestFirst(history).map((h) => h.version);
    expect(versions).toEqual([...history.map((h) => h.version)].sort((a, b) => b - a));
    expect(history[0].version).toBeLessThan(history[history.length - 1].version);
  });
});

describe('versionLabel', () => {
  it('names a single-stage version by its number, and a multi-stage one by its span', () => {
    expect(versionLabel(entry({ version: 2 }))).toBe('v2');
    expect(versionLabel(entry({ version: 3, through: 4 }))).toBe('v3–4');
  });
});

describe('versionDate', () => {
  it('reads the timestamp as a calendar date in UTC', () => {
    expect(versionDate(entry({ timestamp: '2026-09-03T23:30:00Z' }))).toBe('Sep 3, 2026');
  });

  it('shows an unreadable timestamp as it was written rather than as Invalid Date', () => {
    expect(versionDate(entry({ timestamp: 'yesterday' }))).toBe('yesterday');
  });
});

describe('summaryParts', () => {
  it('names only what the version did, so a card does not read "+0 added · −0 removed"', () => {
    expect(summaryParts({ added: 0, modified: 2, removed: 0, fields: 3 })).toEqual([
      { op: 'modified', text: '~2 modified' },
      { op: 'fields', text: '3 field changes' },
    ]);
  });

  it('says each part in the singular when there is one', () => {
    expect(summaryParts({ added: 1, modified: 0, removed: 1, fields: 1 }).map((p) => p.text)).toEqual([
      '+1 added',
      '−1 removed',
      '1 field change',
    ]);
  });

  it('is empty for a version that changed nothing', () => {
    expect(summaryParts({ added: 0, modified: 0, removed: 0, fields: 0 })).toEqual([]);
  });
});
