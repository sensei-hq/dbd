/* What a changelog card says about a version (#29). Pure, so the counts can be checked
   against the entries they sit above. */
import type { ChangeOp, EntityChange, HistoryEntry } from './model';

export type Summary = { added: number; modified: number; removed: number; fields: number };

/** Entities by what happened to them, and the field edits inside the modified ones. */
export function versionSummary(entry: HistoryEntry): Summary {
  const count = (op: string) => entry.changes.filter((c) => c.op === op).length;
  return {
    added: count('added'),
    modified: count('modified'),
    removed: count('removed'),
    fields: entry.changes.reduce((n, c) => n + c.fields.length, 0),
  };
}

export type SummaryPart = { op: 'added' | 'modified' | 'removed' | 'fields'; text: string };

/**
 * The summary line's parts, naming only what the version did: a card that reads
 * "+0 added · −0 removed" spends half its line on things that did not happen.
 */
export function summaryParts(sum: Summary): SummaryPart[] {
  const parts: SummaryPart[] = [];
  if (sum.added) parts.push({ op: 'added', text: `+${sum.added} added` });
  if (sum.modified) parts.push({ op: 'modified', text: `~${sum.modified} modified` });
  if (sum.removed) parts.push({ op: 'removed', text: `−${sum.removed} removed` });
  if (sum.fields)
    parts.push({ op: 'fields', text: `${sum.fields} ${sum.fields === 1 ? 'field change' : 'field changes'}` });
  return parts;
}

/** The history newest first, as a changelog reads. A copy: the model stays oldest first. */
export function newestFirst(history: HistoryEntry[]): HistoryEntry[] {
  return [...history].sort((a, b) => b.version - a.version);
}

/** `v2`, or `v3–4` for a version cut in stages. */
export function versionLabel(entry: HistoryEntry): string {
  return entry.through ? `v${entry.version}–${entry.through}` : `v${entry.version}`;
}

/**
 * The calendar date a version was cut, in UTC — the snapshot's own clock, not the reader's,
 * so a version cut late in the evening does not land on tomorrow for someone further east.
 * An unreadable timestamp is shown as written rather than as "Invalid Date".
 */
export function versionDate(entry: HistoryEntry): string {
  const date = new Date(entry.timestamp);
  if (Number.isNaN(date.getTime())) return entry.timestamp;
  return date.toLocaleDateString('en-US', { month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' });
}

/** The marker a change wears, by what happened. */
export const OP_MARK: Record<string, string> = { added: '+', removed: '−', modified: '~', renamed: '↻' };

/**
 * Two hues on this site, by design: jade for what arrived, sky for what changed, and a muted
 * struck-through name for what left. Shared by every changelog list so they cannot disagree.
 */
export const OP_CLASS: Record<ChangeOp, string> = {
  added: 'text-success',
  modified: 'text-accent-2',
  renamed: 'text-accent-2',
  removed: 'text-faint',
};

export type EntityHistory = {
  /** Newest first: each version that touched the entity, with the entity's own change. */
  rows: { entry: HistoryEntry; change: EntityChange }[];
  /** When no version added the entity, the version it has been there since — the baseline. */
  sinceBaseline?: number;
};

/**
 * One table's or enum's changelog (#33): the project history narrowed to it. Matched on kind
 * as well as name, so a table and an enum that share a name are two histories.
 */
export function entityHistory(
  history: HistoryEntry[],
  kind: EntityChange['kind'],
  schema: string,
  name: string,
): EntityHistory {
  const rows = newestFirst(history).flatMap((entry) => {
    const change = entry.changes.find((c) => c.kind === kind && c.schema === schema && c.name === name);
    return change ? [{ entry, change }] : [];
  });
  if (history.length === 0 || rows.some((r) => r.change.op === 'added')) return { rows };
  const baseline = history.find((h) => h.baseline) ?? history[0];
  return { rows, sinceBaseline: baseline.version };
}
