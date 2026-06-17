// Ambient type shims for @rokkit/* entry points that ship untyped source (no .d.ts).
//
// Upstream packaging gap in @rokkit 1.1.18 surfaced by `svelte-check`:
// `@rokkit/unocss/hooks` exposes only `src/hooks.js` with no `types` export condition,
// so the import resolves as implicitly `any`. This minimal declaration types exactly
// what the site uses; runtime resolution is unaffected (Vite loads the real package).
// Remove once @rokkit/unocss ships a typed `./hooks` export.
//
// (@rokkit/app is intentionally NOT used — it ships untyped .svelte source as its types
// entry; the site uses a local ThemeToggle instead, see lib/components/ThemeToggle.svelte.)

declare module '@rokkit/unocss/hooks' {
	import type { Handle } from '@sveltejs/kit';
	/** SSR hook that injects a pre-paint script applying the persisted Rokkit theme. */
	export function themeHook(opts?: {
		storageKey?: string;
		defaultMode?: 'system' | 'light' | 'dark' | 'auto';
		defaultStyle?: string;
		defaultDensity?: 'compact' | 'comfortable' | 'cozy';
	}): Handle;
}
