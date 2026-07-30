<script lang="ts">
	import SectionHead from '$lib/components/SectionHead.svelte';
	import AudienceCard from '$lib/components/AudienceCard.svelte';
	import { audience } from '$lib/data';
</script>

<!-- Audience -->
<section id="audience" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={audience.eyebrow} title={audience.title} align="center" />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each audience.items as a (a.title)}
				<AudienceCard title={a.title} body={a.body} />
			{/each}
		</div>
	</div>
</section>
