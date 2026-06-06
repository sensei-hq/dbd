/**
 * Rokkit token configuration for the dbd site (named-token model).
 *
 * presetRokkit (uno.config.ts) turns this into the named-token utilities the
 * components use — bg-paper / bg-paper-soft / bg-paper-mute / border-paper-edge,
 * text-ink / text-ink-mute / text-ink-soft, text-primary / bg-accent-soft,
 * text-success — all flipping under [data-mode="dark"].
 *
 * Design = "ink on warm paper" with a sky accent:
 *   surface/ink → kami (warm paper, light) ↔ sumi (cool ink, authored inverted)
 *   primary/accent → sky · success → jade
 */
export default {
	palettes: {
		// Light surface — warm paper (50 lightest → 950 darkest).
		kami: {
			50: '0.987 0.006 85',
			100: '0.967 0.007 85',
			200: '0.94 0.008 85',
			300: '0.905 0.009 85',
			400: '0.79 0.01 82',
			500: '0.63 0.012 82',
			600: '0.5 0.013 80',
			700: '0.4 0.013 80',
			800: '0.32 0.012 80',
			900: '0.25 0.012 80',
			950: '0.18 0.01 80'
		},
		// Dark surface — cool ink, authored INVERTED (50 = darkest bg → 950 = lightest text)
		// so the same shade index reads correctly in dark mode.
		sumi: {
			50: '0.185 0.035 245',
			100: '0.225 0.037 245',
			200: '0.27 0.038 245',
			300: '0.325 0.04 245',
			400: '0.45 0.02 245',
			500: '0.58 0.016 245',
			600: '0.66 0.016 245',
			700: '0.74 0.014 245',
			800: '0.86 0.012 245',
			900: '0.93 0.008 245',
			950: '0.985 0.006 245'
		},
		// Accent — sky.
		sky: {
			50: '0.97 0.02 245',
			100: '0.93 0.04 245',
			200: '0.88 0.06 245',
			300: '0.8 0.09 245',
			400: '0.68 0.12 245',
			500: '0.55 0.13 245',
			600: '0.49 0.135 245',
			700: '0.42 0.115 245',
			800: '0.35 0.09 245',
			900: '0.28 0.07 245',
			950: '0.2 0.05 245'
		},
		// Success — jade/emerald.
		jade: {
			50: '0.96 0.03 160',
			100: '0.92 0.06 160',
			200: '0.86 0.1 160',
			300: '0.78 0.13 158',
			400: '0.7 0.15 156',
			500: '0.62 0.14 155',
			600: '0.52 0.12 155',
			700: '0.44 0.1 155',
			800: '0.36 0.08 155',
			900: '0.3 0.06 155',
			950: '0.22 0.05 155'
		}
	},
	colorSpace: 'oklch',
	tokens: 'core',
	skin: {
		surface: { light: 'kami', dark: 'sumi' },
		ink: { light: 'kami', dark: 'sumi' },
		primary: 'sky',
		accent: 'sky',
		success: 'jade',
		warning: 'jade',
		danger: 'sky',
		error: 'sky',
		info: 'sky'
	},
	shape: { radius: 'soft' }
};
