import { defineConfig } from 'unocss';
import { presetRokkit } from '@rokkit/unocss';
import { DEFAULT_ICONS } from '@rokkit/graph/icons';
import rokkitConfig from './rokkit.config.js';

// presetRokkit bundles presetWind3 + presetIcons + presetTypography + the
// Svelte extractor, generates the z-scale semantic utilities from
// rokkit.config.js, and wires dark mode to [data-mode="dark"].
export default defineConfig({
	presets: [presetRokkit(rokkitConfig)],
	// @rokkit/graph picks a node's icon at RUNTIME from its kind, so these class names never
	// appear in source and UnoCSS's extractor purges them — the icons silently vanish and the
	// cards render a blank box. presetRokkit already exposes the `glyph` collection; only the
	// safelist is needed, and the package exports the exact list to avoid hand-maintaining it.
	safelist: Object.values(DEFAULT_ICONS),
	theme: {
		fontFamily: {
			display: ['"Space Grotesk"', 'system-ui', 'sans-serif'],
			sans: ['"IBM Plex Sans"', 'system-ui', 'sans-serif'],
			mono: ['"IBM Plex Mono"', 'ui-monospace', 'monospace']
		},
		maxWidth: { content: '76rem' }
	}
});
