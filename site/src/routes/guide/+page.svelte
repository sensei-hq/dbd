<script lang="ts">
	import Eyebrow from '$lib/components/Eyebrow.svelte';
	import { pages } from '$lib/guide';
</script>

<svelte:head>
	<title>Guide — dbd</title>
	<meta name="description" content="Guides for dbd: getting started, the design.yaml reference, commands, and schema evolution." />
</svelte:head>

<section class="mx-auto max-w-content px-6 py-section">
	<div class="flex flex-col gap-4">
		<Eyebrow>Documentation</Eyebrow>
		<h1 class="font-display font-bold text-display text-ink text-balance">Guide</h1>
		<p class="max-w-2xl text-lg text-ink-mute text-pretty">
			Everything you need to go from a folder of SQL to a deployed, versioned schema.
		</p>
	</div>

	<div class="mt-12 grid gap-4 sm:grid-cols-2">
		{#each pages as p (p.slug)}
			<a
				href="/guide/{p.slug}"
				class="group flex items-center justify-between gap-4 rounded-xl border border-paper-edge bg-paper-mute p-6 transition-colors hover:border-accent"
			>
				<div class="flex flex-col gap-1">
					<span class="font-mono text-xs text-ink-soft">{String(p.order).padStart(2, '0')}</span>
					<span class="font-display font-semibold text-xl text-ink">{p.title}</span>
				</div>
				<span class="text-primary transition-transform group-hover:translate-x-1">→</span>
			</a>
		{/each}
	</div>
</section>
