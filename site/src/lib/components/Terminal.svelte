<script lang="ts">
	import type { TerminalLine } from '$lib/data';

	let { file, lines }: { file: string; lines: TerminalLine[] } = $props();

	const lineColor: Record<TerminalLine['type'], string> = {
		cmd: 'text-surface-z9',
		out: 'text-surface-z7',
		ok: 'text-success-z6'
	};
</script>

<div class="overflow-hidden rounded-xl border border-surface-z3 bg-surface-z1 shadow-2xl">
	<div class="flex items-center justify-between border-b border-surface-z2 px-4 py-2.5">
		<div class="flex items-center gap-1.5">
			<span class="h-2.5 w-2.5 rounded-full bg-surface-z3"></span>
			<span class="h-2.5 w-2.5 rounded-full bg-surface-z3"></span>
			<span class="h-2.5 w-2.5 rounded-full bg-surface-z3"></span>
		</div>
		<span class="font-mono text-xs text-surface-z5">{file}</span>
	</div>
	<div class="px-4 py-4 font-mono text-[0.82rem] leading-relaxed">
		{#each lines as ln, i (i)}
			<div class="flex gap-2 whitespace-pre">
				<span class={ln.type === 'cmd' ? 'text-primary-z5' : 'text-transparent'}>$</span>
				<span class={lineColor[ln.type]}>{ln.text}</span>
			</div>
		{/each}
	</div>
</div>
