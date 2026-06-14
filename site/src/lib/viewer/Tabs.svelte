<script lang="ts">
  import Icon from './Icon.svelte';

  // Shared DsTabs (ports docs/mockup/designs/entity-page.jsx `DsTabs`).
  // `tabs` is a list of [id, label, icon?] triples; `active` is the current id;
  // `onChange` is called with the clicked tab's id.
  let {
    tabs,
    active,
    onChange,
  }: {
    tabs: [id: string, label: string, icon?: string][];
    active: string;
    onChange: (id: string) => void;
  } = $props();
</script>

<div class="flex gap-1 border-b border-paper-edge px-6">
  {#each tabs as [id, label, icon] (id)}
    <button
      type="button"
      class="flex items-center gap-2 border-b-2 px-3.5 py-2.5 text-sm font-medium transition-colors {active ===
      id
        ? 'border-primary text-ink'
        : 'border-transparent text-ink-mute hover:text-ink'}"
      onclick={() => onChange(id)}
    >
      {#if icon}
        <Icon name={icon} size={14} />
      {/if}
      {label}
    </button>
  {/each}
</div>
