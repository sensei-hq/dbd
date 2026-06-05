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
		<h1 class="font-display font-bold text-display text-surface-z9 text-balance">Guide</h1>
		<p class="max-w-2xl text-lg text-surface-z7 text-pretty">
			Everything you need to go from a folder of SQL to a deployed, versioned schema.
		</p>
	</div>

	<div class="mt-12 grid gap-4 sm:grid-cols-2">
		{#each pages as p (p.slug)}
			<a
				href="/guide/{p.slug}"
				class="group flex items-center justify-between gap-4 rounded-xl border border-surface-z3 bg-surface-z1 p-6 transition-colors hover:border-primary-z4"
			>
				<div class="flex flex-col gap-1">
					<span class="font-mono text-xs text-surface-z5">{String(p.order).padStart(2, '0')}</span>
					<span class="font-display font-semibold text-xl text-surface-z9">{p.title}</span>
				</div>
				<span class="text-primary-z5 transition-transform group-hover:translate-x-1">→</span>
			</a>
		{/each}
	</div>
</section>
