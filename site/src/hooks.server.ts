import { themeHook } from '@rokkit/unocss/hooks';

// Injects a pre-paint script that applies the persisted Rokkit theme
// (data-mode / data-style / data-density) before first paint — no flash.
// Shares the 'dbd-theme' storage key with the vibe store (see +layout.svelte).
export const handle = themeHook({
	storageKey: 'dbd-theme',
	// defaultMode: 'system' → the pre-paint script resolves prefers-color-scheme to
	// light/dark (≥ @rokkit/unocss 1.1.12). ThemeSwitcherToggle's single variant
	// reflects the resolved light/dark, so its first click always flips.
	defaultMode: 'system',
	defaultStyle: 'zen-sumi',
	defaultDensity: 'comfortable'
});
