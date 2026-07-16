<script lang="ts">
	let { cmd, body }: { cmd: string; body: string } = $props();
</script>

<div
	class="flex flex-col gap-2 rounded-xl border border-paper-edge bg-paper-mute p-5 transition-colors hover:border-accent"
>
	<code class="w-fit rounded-md bg-paper-soft px-2.5 py-1 font-mono text-sm text-primary">{cmd}</code>
	<p class="text-sm text-ink-mute text-pretty">{body}</p>
</div>
