<script lang="ts">
	import '$lib/prose.css';
	import ArrowIcon from './ArrowIcon.svelte';
	import { changelog } from '$lib/data';
	import type { Release } from '$lib/releases';

	let { releases }: { releases: Release[] } = $props();
</script>

<div class="flex flex-col gap-12">
	{#each releases as r (r.version)}
		<section id="v{r.version}" class="flex flex-col gap-2 border-b border-paper-edge pb-10">
			<h2 class="flex flex-wrap items-baseline gap-x-4 gap-y-1 font-display font-semibold text-2xl text-ink">
				<a href={r.url} class="transition-colors hover:text-primary">{r.version}</a>
				{#if r.date}<span class="font-mono text-sm font-normal text-ink-soft">{r.date}</span>{/if}
			</h2>
			<!-- eslint-disable-next-line svelte/no-at-html-tags — trusted, build-time notes from the repo's CHANGELOG.md -->
			<div class="guide-prose text-ink-mute">{@html r.html}</div>
		</section>
	{/each}

	<a
		href={changelog.full}
		class="inline-flex items-center gap-2 self-start text-sm font-medium text-primary hover:underline"
		>{changelog.fullLabel}<ArrowIcon /></a
	>
</div>
