import { afterEach, beforeAll } from 'vitest';
import { cleanup } from '@testing-library/svelte';

// `@testing-library/svelte` only auto-registers cleanup when Vitest `globals`
// are enabled (they aren't here). Without it, a rendered component stays
// mounted across tests in the same file, leaking DOM/effects into the next
// test. Unmount + clear the document after every test so each render starts
// from a clean tree.
afterEach(() => cleanup());

// jsdom lacks these; Rokkit calls them when the Viewer mounts (Sidebar's
// List→Navigator uses scrollIntoView on select, ThemeSwitcherToggle resolves
// the `system` color mode via matchMedia). Polyfill them once for every viewer
// test so any test that renders the Viewer (directly or via the /diagram page)
// doesn't crash.
beforeAll(() => {
  Element.prototype.scrollIntoView ??= () => {};
  if (!window.matchMedia) {
    window.matchMedia = (query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent() {
        return false;
      },
    });
  }
});
