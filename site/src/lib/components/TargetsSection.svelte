<script lang="ts">
	import SectionHead from '$lib/components/SectionHead.svelte';
	import TargetCard from '$lib/components/TargetCard.svelte';
	import { targets } from '$lib/data';
</script>

<!-- Targets -->
<section id="targets" class="tint-soft">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={targets.eyebrow} title={targets.title} lede={targets.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
			{#each targets.items as t (t.name)}
				<TargetCard name={t.name} scheme={t.scheme} body={t.body} notes={t.notes} />
			{/each}
		</div>
	</div>
</section>
