/* ============================================================
   dbd designs — deterministic ER diagram layout.
   Pure function: schema data + density + arrange -> geometry.
   No DOM measuring; card heights are computed from row counts.

   arrange = "untangle" (default): connectivity-aware ordering —
     clusters are chained by inter-schema link weight and tables
     inside each cluster are reordered with barycenter passes so
     related tables sit near each other and edges cross less.
   arrange = "a-z": plain alphabetical, biggest cluster first.

   The algorithm is split across sibling modules (framework-agnostic,
   unit-tested via layout.test.ts): types/consts, card sizing, cluster
   building+ordering+packing, and edge routing. This file is the public
   entry point + orchestration.
   ============================================================ */

import type { LayoutData, Density, Arrange, Layout } from './layout-types';
import { CARD_W, ROW_H, HEAD_H } from './layout-types';
import { buildCards } from './layout-cards';
import {
  groupBySchema,
  buildAdjacency,
  buildClusters,
  pack,
  orderClusters,
  flow,
  barycenterPasses,
} from './layout-clusters';
import { buildEdges } from './layout-edges';

// Public surface — keep the stable `$lib/design/layout` import path for consumers.
export type {
  LayoutData,
  Density,
  Arrange,
  EdgeStyle,
  LayoutTable,
  Card,
  Cluster,
  Edge,
  Layout,
} from './layout-types';
export { edgePath } from './layout-edges';

export function compute(data: LayoutData, density: Density, arrange: Arrange = 'untangle'): Layout {
  const cards = buildCards(data, density);
  const nbrs = buildAdjacency(data);
  let clusters = buildClusters(groupBySchema(data));
  clusters.forEach((c) => pack(c, cards));

  clusters = orderClusters(clusters, data, arrange);

  // Initial flow, then (for untangle) barycenter passes that reorder + re-flow.
  let size = flow(clusters, cards);
  if (arrange === 'untangle') size = barycenterPasses(clusters, cards, nbrs);

  const edges = buildEdges(data, cards);
  return { clusters, cards, edges, size, consts: { CARD_W, ROW_H, HEAD_H } };
}
