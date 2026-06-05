import { defineConfig } from 'unocss';
import presetWind4 from '@unocss/preset-wind4';
import {
	themeColors,
	semanticShortcuts,
	contrastShortcuts
} from '@rokkit/themes';

// Semantic roles the site uses. Rokkit generates z-scale utilities
// (bg-surface-z0, text-surface-z9, text-primary-z5, text-on-primary, …)
// for each, with automatic light/dark flipping via [data-mode].
const ROLES = ['surface', 'primary', 'secondary', 'success', 'info'];

export default defineConfig({
	presets: [presetWind4()],
	theme: {
		colors: themeColors(),
		fontFamily: {
			display: ['"Space Grotesk"', 'system-ui', 'sans-serif'],
			sans: ['"IBM Plex Sans"', 'system-ui', 'sans-serif'],
			mono: ['"IBM Plex Mono"', 'ui-monospace', 'monospace']
		},
		maxWidth: { content: '76rem' }
	},
	shortcuts: [
		...ROLES.flatMap((r) => semanticShortcuts(r)),
		...ROLES.flatMap((r) => contrastShortcuts(r))
	]
});
