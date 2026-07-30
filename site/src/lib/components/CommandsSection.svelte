<script lang="ts">
	import SectionHead from '$lib/components/SectionHead.svelte';
	import CommandCard from '$lib/components/CommandCard.svelte';
	import { commands } from '$lib/data';
</script>

<!-- Commands -->
<section id="commands">
	<div class="mx-auto max-w-content px-6 py-section">
		<SectionHead eyebrow={commands.eyebrow} title={commands.title} lede={commands.lede} />
		<div class="mt-12 grid gap-4 sm:grid-cols-2">
			{#each commands.items as c (c.cmd)}
				<CommandCard cmd={c.cmd} body={c.body} />
			{/each}
		</div>
	</div>
</section>
