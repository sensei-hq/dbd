import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import UnoCSS from 'unocss/vite';
import cssInjectedByJsPlugin from 'vite-plugin-css-injected-by-js';
import { fileURLToPath } from 'node:url';

export default defineConfig({
	plugins: [UnoCSS(), svelte(), cssInjectedByJsPlugin()],
	optimizeDeps: { exclude: ['@rokkit/app', '@rokkit/ui', '@rokkit/states', '@rokkit/core'] },
	build: {
		outDir: fileURLToPath(new URL('../crates/dbd-core/assets', import.meta.url)),
		emptyOutDir: false, // don't wipe diagram.html (added in the next task)
		cssCodeSplit: false,
		lib: {
			entry: fileURLToPath(new URL('./src/lib/viewer/standalone.ts', import.meta.url)),
			formats: ['iife'],
			name: 'DbdDiagram',
			fileName: () => 'diagram_viewer.js'
		}
	}
});
