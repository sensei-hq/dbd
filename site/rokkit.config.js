/**
 * Rokkit token configuration for the dbd site.
 *
 * Consumed by presetRokkit() in uno.config.ts. Roles map to palettes; the
 * z-scale utilities (bg-surface-z0, text-primary-z5, text-on-primary, …) are
 * generated from these and flip automatically under [data-mode="dark"].
 *
 * The design is "ink on warm paper" with a sky-blue accent:
 *   surface → stone (warm neutral) in light, zinc in dark
 *   primary → sky   (matches the brand hex #38BDF8 and the hero accent line)
 */
export default {
	colors: {
		surface: { light: 'stone', dark: 'zinc' },
		primary: 'sky',
		secondary: 'cyan',
		success: 'emerald',
		info: 'sky'
	}
};
