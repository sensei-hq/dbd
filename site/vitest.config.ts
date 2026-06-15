import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { fileURLToPath } from 'node:url';

export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: 'jsdom',
    include: ['src/lib/**/*.test.ts'],
    // Polyfill jsdom gaps (matchMedia/scrollIntoView) Rokkit touches when the
    // Viewer mounts, so page tests that render it don't crash.
    setupFiles: ['src/lib/design/test-setup.ts'],
  },
  resolve: {
    // Use the `browser` package entry points so component mounting resolves the
    // client runtime (not Svelte's server build) when Vitest runs in Node.
    conditions: ['browser'],
    // Mirror SvelteKit's `$lib` alias here (this config uses the standalone
    // `svelte()` plugin, not `sveltekit()`, so the route components the page
    // tests import resolve their `$lib/...` imports).
    alias: { $lib: fileURLToPath(new URL('./src/lib', import.meta.url)) },
  },
});
