<script lang="ts">
	import SectionHead from '$lib/components/SectionHead.svelte';
	import FeatureCard from '$lib/components/FeatureCard.svelte';
	import { overview } from '$lib/data';
</script>

<!-- Overview -->
<section id="overview" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={overview.eyebrow} title={overview.title} lede={overview.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
			{#each overview.features as f (f.title)}
				<FeatureCard title={f.title} body={f.body} />
			{/each}
		</div>
	</div>
</section>
