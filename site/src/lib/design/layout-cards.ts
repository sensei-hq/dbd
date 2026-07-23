/* ER diagram layout: table "card" descriptors (size from visible-row count). */

import type { LayoutData, LayoutColumn, LayoutTable, Density, Cards } from './layout-types';
import { CARD_W, ROW_H, HEAD_H, MORE_H, PAD_B } from './layout-types';

/** The columns a card shows at the given density (empty for 'names'). */
function visibleCols(table: LayoutTable, density: Density): LayoutColumn[] {
  if (density === 'names') return [];
  let cols = table.columns;
  if (density === 'keys') cols = cols.filter((c) => c.pk || c.fk);
  return cols.slice(0, density === 'full' ? 14 : 8);
}

/** Build a card descriptor per table; height is derived from its visible rows. */
export function buildCards(data: LayoutData, density: Density): Cards {
  const cards: Cards = {};
  for (const t of data.tables) {
    const vis = visibleCols(t, density);
    const more = t.columns.length - vis.length;
    const h = HEAD_H + vis.length * ROW_H + (more > 0 ? MORE_H : 0) + (vis.length || more > 0 ? PAD_B : 0);
    cards[t.schema + '.' + t.name] = { t, vis, more, w: CARD_W, h, x: 0, y: 0 };
  }
  return cards;
}
