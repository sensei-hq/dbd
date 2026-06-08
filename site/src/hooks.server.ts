import { themeHook } from '@rokkit/unocss/hooks';

// Injects a pre-paint script that applies the persisted Rokkit theme
// (data-mode / data-style / data-density) before first paint — no flash.
// Shares the 'dbd-theme' storage key with the vibe store (see +layout.svelte).
export const handle = themeHook({
	storageKey: 'dbd-theme',
	// No defaultMode → default to the OS preference (system); the pre-paint script
	// resolves prefers-color-scheme. ThemeSwitcherToggle's single variant reflects
	// the resolved light/dark, so its first click always flips.
	defaultStyle: 'zen-sumi',
	defaultDensity: 'comfortable'
});
