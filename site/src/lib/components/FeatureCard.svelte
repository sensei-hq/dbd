<script lang="ts">
	let { tag, title, body }: { tag: string; title: string; body: string } = $props();
</script>

<div
	class="group flex flex-col gap-3 rounded-xl border border-surface-z3 bg-surface-z1 p-6 transition-colors hover:border-primary-z4"
>
	<span class="font-mono text-xs text-surface-z5 transition-colors group-hover:text-primary-z5">{tag}</span>
	<h3 class="font-display font-semibold text-xl text-surface-z9">{title}</h3>
	<p class="text-sm text-surface-z7 text-pretty">{body}</p>
</div>
