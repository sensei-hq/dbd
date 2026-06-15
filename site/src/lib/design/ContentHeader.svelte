<script lang="ts">
  import Tabs from './Tabs.svelte';
  import type { ContentHeaderData } from './data';

  let {
    data,
    activeTab = $bindable(data.activeTab),
    onTab,
  }: {
    data: ContentHeaderData;
    activeTab?: string;
    onTab?: (id: string) => void;
  } = $props();

  function pick(id: string) {
    activeTab = id;
    onTab?.(id);
  }
</script>

<div class="border-b border-line bg-paper">
  <div class="flex flex-wrap items-start gap-x-6 gap-y-2 px-6 pb-3 pt-5">
    <div class="min-w-0">
      <div class="flex flex-wrap items-center gap-3">
        <h1 class="font-display text-h3 font-semibold tracking-tight">{data.project.name}</h1>
        <span class="ds-badge ds-badge-accent">{data.project.db}</span>
        {#if data.project.version}<span class="ds-badge">{data.project.version}</span>{/if}
      </div>
      {#if data.project.note}<p class="mt-1 max-w-2xl text-sm text-muted">{data.project.note}</p>{/if}
    </div>
    <div class="ml-auto hidden whitespace-nowrap pt-1 font-mono text-xs text-faint md:block">
      {#each data.stats as stat, i (stat.label)}{i > 0 ? ' · ' : ''}{stat.value}
        {stat.label}{/each}{#if data.project.via && data.project.updated}<span class="text-line"> | </span>{data.project.via} · {data.project.updated}{/if}
    </div>
  </div>

  <Tabs tabs={data.tabs} active={activeTab} onChange={pick} />
</div>
