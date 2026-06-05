<script lang="ts">
	let {
		name,
		scheme,
		body,
		notes
	}: { name: string; scheme: string; body: string; notes: string[] } = $props();
</script>

<div
	class="flex flex-col gap-4 rounded-xl border border-surface-z3 bg-surface-z2 p-6 transition-colors hover:border-primary-z4"
>
	<h3 class="font-display font-semibold text-xl text-surface-z9">{name}</h3>
	<code class="w-fit rounded-md bg-surface-z1 px-2.5 py-1 font-mono text-xs text-primary-z5">{scheme}</code>
	<p class="text-sm text-surface-z7 text-pretty">{body}</p>
	<ul class="mt-1 flex flex-col gap-2 border-t border-surface-z2 pt-4">
		{#each notes as n (n)}
			<li class="flex items-center gap-2.5 text-sm text-surface-z7">
				<span class="h-1 w-1 rounded-full bg-primary-z5"></span>
				{n}
			</li>
		{/each}
	</ul>
</div>
