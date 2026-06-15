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
  // jsdom has no ResizeObserver; Svelte's bind:clientWidth/clientHeight (the
  // design DiagramView's fit-to-container) needs it.
  if (!window.ResizeObserver) {
    window.ResizeObserver = class {
      observe() {}
      unobserve() {}
      disconnect() {}
    };
  }
  // Bun injects a partial `localStorage` global (missing `clear()`) that shadows
  // jsdom's; install a clean in-memory Storage so app code (bare `localStorage`)
  // and tests share one that works.
  {
    const mem = new Map<string, string>();
    const memStorage = {
      getItem: (k: string) => (mem.has(k) ? mem.get(k)! : null),
      setItem: (k: string, v: string) => void mem.set(k, String(v)),
      removeItem: (k: string) => void mem.delete(k),
      clear: () => mem.clear(),
      key: (i: number) => [...mem.keys()][i] ?? null,
      get length() {
        return mem.size;
      },
    };
    try {
      Object.defineProperty(globalThis, 'localStorage', { value: memStorage, configurable: true });
      Object.defineProperty(window, 'localStorage', { value: memStorage, configurable: true });
    } catch {
      // a non-configurable global — leave it; jsdom's may still work
    }
  }
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
