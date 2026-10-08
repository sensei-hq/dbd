<script lang="ts">
	import Eyebrow from '$lib/components/Eyebrow.svelte';
	import Seo from '$lib/components/Seo.svelte';
	import ReleaseNotes from '$lib/components/ReleaseNotes.svelte';
	import { changelog } from '$lib/data';
	import type { Release } from '$lib/releases';

	let { data }: { data: { releases: Release[] } } = $props();
</script>

<Seo title="Changelog — dbd" description={changelog.description} />

<section class="mx-auto max-w-content px-6 py-section">
	<div class="flex flex-col gap-4">
		<Eyebrow>{changelog.eyebrow}</Eyebrow>
		<h1 class="font-display font-bold text-display text-ink text-balance">{changelog.title}</h1>
		<p class="max-w-2xl text-lg text-ink-mute text-pretty">{changelog.lede}</p>
	</div>

	<div class="mt-12 max-w-3xl">
		<ReleaseNotes releases={data.releases} />
	</div>
</section>
