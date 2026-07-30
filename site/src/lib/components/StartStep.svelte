<script lang="ts">
	let { n, text, code }: { n: string; text: string; code: string } = $props();
</script>

<div class="flex items-center gap-4 rounded-lg border border-paper-edge bg-paper px-5 py-4">
	<span
		class="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-accent-soft font-mono text-sm text-primary"
	>
		{n}
	</span>
	<div class="flex flex-col">
		<span class="text-xs text-ink-soft">{text}</span>
		<code class="font-mono text-sm text-ink"><span class="text-primary">$ </span>{code}</code>
	</div>
</div>
