import { mount } from 'svelte';
import Viewer from './Viewer.svelte';
import type { SchemaModel } from './model';

export { default as Viewer } from './Viewer.svelte';
export { type SchemaModel } from './model';

/** Mount the schema viewer into `target`, rendering `model`. */
export function mountViewer(target: HTMLElement, model: SchemaModel) {
  return mount(Viewer, { target, props: { model } });
}
