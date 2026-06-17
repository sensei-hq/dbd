import { sveltekit } from '@sveltejs/kit/vite';
import UnoCSS from 'unocss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [UnoCSS(), sveltekit()],
	// Rokkit packages ship Svelte source (.svelte / .svelte.ts runes modules);
	// exclude them from dep pre-bundling so vite-plugin-svelte preprocesses them
	// instead of the optimizer choking on the TS syntax.
	optimizeDeps: {
		exclude: ['@rokkit/ui', '@rokkit/states', '@rokkit/core']
	}
});
