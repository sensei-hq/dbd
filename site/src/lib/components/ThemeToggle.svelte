<script lang="ts">
	import { vibe } from '@rokkit/states';

	// Single-button light↔dark toggle — replaces @rokkit/app's `ThemeSwitcherToggle
	// variant="single"`, which is dropped because @rokkit/app ships untyped .svelte
	// source (no built .d.ts) and trips svelte-check. `vibe.mode` is the source of
	// truth; `use:themable` in +layout writes data-mode + persists + syncs cross-tab,
	// so we only flip the store value here.
	const isDark = $derived(vibe.mode === 'dark');

	function toggle() {
		vibe.mode = isDark ? 'light' : 'dark';
	}
</script>

<button
	type="button"
	onclick={toggle}
	class="inline-flex h-9 w-9 items-center justify-center rounded-md border border-paper-edge text-ink-mute transition-colors hover:bg-paper-mute hover:text-ink"
	aria-label={isDark ? 'Switch to light mode' : 'Switch to dark mode'}
	title={isDark ? 'Switch to light mode' : 'Switch to dark mode'}
>
	{#if isDark}
		<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" aria-hidden="true">
			<circle cx="12" cy="12" r="4" />
			<path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
		</svg>
	{:else}
		<svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
			<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
		</svg>
	{/if}
</button>
