/* ============================================================
   dbd designs — ER diagram layout: shared types + geometry constants.
   ============================================================ */

import type { LayoutData, LayoutColumn, Ref } from './model';
export type { LayoutData, LayoutColumn, Ref };

// ---- public types ----
export type Density = 'names' | 'keys' | 'full';
export type Arrange = 'untangle' | 'a-z';
export type EdgeStyle = 'curved' | 'orthogonal';

/** One table's layout descriptor (a "LayoutTable" is just the LayoutData table row) */
export type LayoutTable = LayoutData['tables'][number];

export type Card = {
  t: LayoutTable;
  vis: LayoutColumn[];
  more: number;
  w: number;
  h: number;
  x: number;
  y: number;
  hue?: number;
};

export type Cluster = {
  name: string;
  list: LayoutTable[];
  count: number;
  hue: number;
  x: number;
  y: number;
  w?: number;
  h?: number;
  pos?: { key: string; dx: number; dy: number }[];
};

export type Edge = {
  i: number;
  ref: Ref;
  fromKey: string;
  toKey: string;
  self: boolean;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  s1: number;
  s2: number;
};

export type Layout = {
  clusters: Cluster[];
  cards: Record<string, Card>;
  edges: Edge[];
  size: { w: number; h: number };
  consts: { CARD_W: number; ROW_H: number; HEAD_H: number };
};

/** Internal aliases shared across the layout modules. */
export type Cards = Record<string, Card>;
export type Size = { w: number; h: number };

// ---- geometry constants ----
export const CARD_W = 248, ROW_H = 24, HEAD_H = 40, MORE_H = 22, PAD_B = 6;
export const GAP_X = 36, GAP_Y = 30;
export const CL_PAD = 26, CL_TITLE = 16, CL_GAP_X = 110, CL_GAP_Y = 110;
export const MAX_ROW_W = 2750;
/* per-schema tint hues (oklch hue angles), assigned a-z */
export const HUES = [245, 160, 70, 330, 200, 25, 120, 285];
