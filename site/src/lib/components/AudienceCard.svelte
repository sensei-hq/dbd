<script lang="ts">
	let { title, body }: { title: string; body: string } = $props();
</script>

<div class="flex flex-col gap-2 rounded-xl border border-paper-edge bg-paper-mute p-6">
	<h3 class="font-display font-medium text-lg text-ink">{title}</h3>
	<p class="text-sm text-ink-mute text-pretty">{body}</p>
</div>
