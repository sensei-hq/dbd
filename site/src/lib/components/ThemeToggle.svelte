<script lang="ts">
	import { vibe } from '@rokkit/states';

	// vibe.mode is reactive ($state); flipping it drives the $effect in
	// +layout.svelte that updates [data-mode] and persists to storage.
	function toggle() {
		vibe.mode = vibe.mode === 'dark' ? 'light' : 'dark';
	}
</script>

<button
	type="button"
	onclick={toggle}
	aria-label="Toggle colour theme"
	title="Toggle colour theme"
	class="grid h-9 w-9 place-items-center rounded-lg border border-surface-z3 text-surface-z7 transition-colors hover:border-primary-z4 hover:text-surface-z9"
>
	{#if vibe.mode === 'dark'}
		<svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.8">
			<circle cx="12" cy="12" r="4" />
			<path
				d="M12 2v2M12 20v2M4 12H2M22 12h-2M5 5l1.5 1.5M17.5 17.5L19 19M19 5l-1.5 1.5M6.5 17.5L5 19"
				stroke-linecap="round"
			/>
		</svg>
	{:else}
		<svg viewBox="0 0 24 24" class="h-4 w-4" fill="none" stroke="currentColor" stroke-width="1.8">
			<path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" stroke-linejoin="round" />
		</svg>
	{/if}
</button>
