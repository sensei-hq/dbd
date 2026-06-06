<script lang="ts">
	let {
		name,
		scheme,
		body,
		notes
	}: { name: string; scheme: string; body: string; notes: string[] } = $props();
</script>

<div
	class="flex flex-col gap-4 rounded-xl border border-paper-edge bg-paper-mute p-6 transition-colors hover:border-accent"
>
	<h3 class="font-display font-semibold text-xl text-ink">{name}</h3>
	<code class="w-fit rounded-md bg-paper-soft px-2.5 py-1 font-mono text-xs text-primary">{scheme}</code>
	<p class="text-sm text-ink-mute text-pretty">{body}</p>
	<ul class="mt-1 flex flex-col gap-2 border-t border-paper-edge pt-4">
		{#each notes as n (n)}
			<li class="flex items-center gap-2.5 text-sm text-ink-mute">
				<span class="h-1 w-1 rounded-full bg-primary"></span>
				{n}
			</li>
		{/each}
	</ul>
</div>
