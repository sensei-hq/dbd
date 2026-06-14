import type { Density, Arrange, EdgeStyle } from './layout';

export type ViewerState = {
  selected: string | null;
  mode: 'overview' | 'focus';
  /** Project-root tab (mockup `ProjectRoot` Diagram/Entities tabs). */
  tab: 'diagram' | 'entities';
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
    tab: 'diagram',
    density: 'keys',
    arrange: 'untangle',
    filter: '',
    tint: true,
    lines: 'curved',
  });
  return state;
};
