import type { Density, Arrange, EdgeStyle } from './layout';

export type ViewerState = {
  selected: string | null;
  mode: 'overview' | 'focus';
  density: Density;
  arrange: Arrange;
  filter: string;
  /** Per-schema hue tint on clusters/cards (mockup `tint`, default on). */
  tint: boolean;
  /** Relationship edge routing (mockup `lineStyle`, default curved). */
  lines: EdgeStyle;
};

export const createViewerState = (): ViewerState => {
  const state = $state<ViewerState>({
    selected: null,
    mode: 'overview',
    density: 'keys',
    arrange: 'untangle',
    filter: '',
    tint: true,
    lines: 'curved',
  });
  return state;
};
