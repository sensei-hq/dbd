import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  test: { environment: 'jsdom', include: ['src/lib/viewer/**/*.test.ts'] },
  // Use the `browser` package entry points so component mounting resolves the
  // client runtime (not Svelte's server build) when Vitest runs in Node.
  resolve: { conditions: ['browser'] },
});
