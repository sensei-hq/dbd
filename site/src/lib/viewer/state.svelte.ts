import type { Density, Arrange } from './layout';

export type ViewerState = {
  selected: string | null;
  mode: 'overview' | 'focus';
  density: Density;
  arrange: Arrange;
  filter: string;
};

export const createViewerState = (): ViewerState => {
  const state = $state<ViewerState>({
    selected: null,
    mode: 'overview',
    density: 'keys',
    arrange: 'untangle',
    filter: '',
  });
  return state;
};
