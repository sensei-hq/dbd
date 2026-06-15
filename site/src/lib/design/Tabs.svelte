<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import type { Tab } from './data';

  let {
    tabs,
    active,
    onChange,
  }: { tabs: Tab[]; active: string; onChange?: (id: string) => void } = $props();
</script>

<div class="flex gap-1 border-b border-line px-6">
  {#each tabs as tab (tab.id)}
    <button
      type="button"
      class="flex items-center gap-2 border-b-2 px-3.5 py-2.5 text-sm font-medium transition-colors {active ===
      tab.id
        ? 'border-accent text-fg'
        : 'border-transparent text-muted hover:text-fg'}"
      onclick={() => onChange?.(tab.id)}
    >
      <Icon name={tab.icon} size={14} />
      {tab.label}
    </button>
  {/each}
</div>
