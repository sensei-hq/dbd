/* Minimal note renderer for DDL comments (noteMd): paragraphs, bullet lists,
   and `inline code`. Shared by EntitiesView + EntityView so they format
   comments identically. Returns a structured tree the components render. */

export type Seg = { code: boolean; text: string };
export type Block = { type: 'p' | 'ul'; lines: Seg[][] };

/** Split a line into plain / `code` segments. */
export function inlineSegs(text: string): Seg[] {
  const segs: Seg[] = [];
  let last = 0;
  for (const m of text.matchAll(/`([^`]+)`/g)) {
    const idx = m.index ?? 0;
    if (idx > last) segs.push({ code: false, text: text.slice(last, idx) });
    segs.push({ code: true, text: m[1] });
    last = idx + m[0].length;
  }
  if (last < text.length) segs.push({ code: false, text: text.slice(last) });
  return segs;
}

/** Parse a noteMd string into paragraph / bullet-list blocks. */
export function noteBlocks(src?: string): Block[] {
  if (!src) return [];
  const out: Block[] = [];
  const lines = src.split('\n');
  let i = 0;
  while (i < lines.length) {
    if (!lines[i].trim()) {
      i++;
      continue;
    }
    if (/^[-•]\s/.test(lines[i].trim())) {
      const items: Seg[][] = [];
      while (i < lines.length && /^[-•]\s/.test(lines[i].trim())) {
        items.push(inlineSegs(lines[i].trim().replace(/^[-•]\s/, '')));
        i++;
      }
      out.push({ type: 'ul', lines: items });
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && lines[i].trim() && !/^[-•]\s/.test(lines[i].trim())) {
      para.push(lines[i].trim());
      i++;
    }
    out.push({ type: 'p', lines: [inlineSegs(para.join(' '))] });
  }
  return out;
}

/**
 * A note split for a header: its first block as plain text (the subtitle), and every block
 * after it (the rest, for a page with room). Split on blocks, not lines, so a paragraph that
 * wraps across lines in the source is never cut in two.
 */
export function splitLead(src?: string): { lead?: string; rest: Block[] } {
  const [first, ...rest] = noteBlocks(src);
  if (!first) return { rest: [] };
  const lead = first.lines[0].map((seg) => seg.text).join('');
  return { lead, rest };
}
