<script lang="ts" module>
  // Type helpers ported from docs/mockup/designs/entity-page.jsx.
  export function typeSize(type: string): string {
    const mm = type.match(/\(([^)]+)\)/);
    if (mm) return mm[1];
    if (type.endsWith('[]')) return '[]';
    return '—';
  }
  export function baseType(type: string): string {
    return type.replace(/\([^)]*\)/, '').replace(/\[\]$/, '');
  }
</script>

<script lang="ts">
  import Icon from '$lib/design/Icon.svelte';
  import { nodeId, type Column, type Ref, type SchemaModel } from '$lib/design/model';
  import Tabs from './Tabs.svelte';
  import EntityDiagram from './EntityDiagram.svelte';
  import { noteBlocks, type Seg } from './md';

  let {
    model,
    entityKey,
    onNav,
  }: { model: SchemaModel; entityKey: string; onNav: (key: string) => void } = $props();

  let tab = $state('details');
  $effect(() => {
    void entityKey;
    tab = 'details';
  });

  const schema = $derived(entityKey.split('.')[0]);
  const name = $derived(entityKey.split('.')[1]);
  const table = $derived(model.tables.find((t) => t.schema === schema && t.name === name) ?? null);

  const outRefs = $derived(model.refs.filter((r) => r.from.s === schema && r.from.t === name));
  const inRefs = $derived(model.refs.filter((r) => r.to.s === schema && r.to.t === name));
  const fkCols = $derived(new Set(outRefs.map((r) => r.from.c)));
  const refsForCol = (col: string): Ref[] => outRefs.filter((r) => r.from.c === col);

  type Badge = { label: string; cls: string };
  function propBadges(c: Column): Badge[] {
    const out: Badge[] = [];
    if (c.pk) out.push({ label: 'PK', cls: 'pk' });
    if (fkCols.has(c.name)) out.push({ label: 'FK', cls: 'fk' });
    if (c.nn) out.push({ label: 'NN', cls: '' });
    if (c.en) out.push({ label: 'ENUM', cls: '' });
    return out;
  }

  const comment = $derived(noteBlocks(table?.noteMd));
</script>

{#snippet segs(parts: Seg[])}
  {#each parts as part (part.text)}
    {#if part.code}
      <code class="rounded bg-code-bg px-1 font-mono text-accent-2" style="font-size: 0.85em;">{part.text}</code>
    {:else}
      {part.text}
    {/if}
  {/each}
{/snippet}

{#if table}
  <div class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- header -->
    <div class="border-b border-line bg-paper">
      <div class="px-6 pb-3 pt-5">
        <div class="font-mono text-xs text-faint">{schema}.</div>
        <div class="flex flex-wrap items-center gap-3">
          <h1 class="font-display text-h3 font-semibold tracking-tight">{name}</h1>
          <span class="ds-badge">{table.columns.length} columns</span>
          {#if inRefs.length || outRefs.length}
            <span class="ds-badge">{outRefs.length} out · {inRefs.length} in</span>
          {/if}
        </div>
      </div>
      <Tabs
        tabs={[
          { id: 'details', label: 'Details', icon: 'rows' },
          { id: 'diagram', label: 'Diagram', icon: 'grid' },
        ]}
        active={tab}
        onChange={(id) => (tab = id)}
      />
    </div>

    {#if tab === 'details'}
      <div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
        <div class="mx-auto max-w-4xl px-6 py-6">
          {#if comment.length}
            <section class="mb-8">
              <h2 class="font-mono text-label uppercase text-faint">Comment</h2>
              <div class="mt-3 flex flex-col gap-2 text-sm leading-relaxed text-muted">
                {#each comment as block (block)}
                  {#if block.type === 'ul'}
                    <ul class="flex list-disc flex-col gap-1 pl-5">
                      {#each block.lines as line (line)}<li>{@render segs(line)}</li>{/each}
                    </ul>
                  {:else}
                    <p>{@render segs(block.lines[0])}</p>
                  {/if}
                {/each}
              </div>
            </section>
          {/if}

          <section>
            <h2 class="font-mono text-label uppercase text-faint">Columns</h2>
            <table class="mt-3 w-full table-fixed border-collapse text-left">
              <colgroup>
                <col style="width: 30%;" />
                <col style="width: 104px;" />
                <col style="width: 130px;" />
                <col style="width: 64px;" />
                <col />
              </colgroup>
              <thead>
                <tr class="font-mono text-xs uppercase tracking-wider text-faint">
                  <th class="ds-th py-2.5 pr-4 font-medium">Column</th>
                  <th class="ds-th py-2.5 pr-4 font-medium">Props</th>
                  <th class="ds-th py-2.5 pr-4 font-medium">Type</th>
                  <th class="ds-th py-2.5 pr-4 font-medium">Size</th>
                  <th class="ds-th py-2.5 font-medium">Refs</th>
                </tr>
              </thead>
              <tbody>
                {#each table.columns as c (c.name)}
                  {@const rr = refsForCol(c.name)}
                  <tr data-col-row={c.name} class="border-b border-line-soft align-top">
                    <td class="py-2.5 pr-4">
                      <div class="font-mono text-xs font-semibold text-fg" style="overflow-wrap: anywhere;">{c.name}</div>
                      {#if c.note}<div class="mt-0.5 text-xs leading-snug text-muted">{c.note}</div>{/if}
                      {#if c.def}<div class="mt-0.5 font-mono text-faint" style="font-size: 0.66rem;">default: {c.def}</div>{/if}
                    </td>
                    <td class="py-2.5 pr-4">
                      <div class="flex flex-wrap gap-1">
                        {#each propBadges(c) as b (b.label)}<span class="col-badge {b.cls}">{b.label}</span>{/each}
                      </div>
                    </td>
                    <td class="py-2.5 pr-4 font-mono text-xs text-muted" style="overflow-wrap: anywhere;">{baseType(c.type)}</td>
                    <td class="whitespace-nowrap py-2.5 pr-4 font-mono text-xs text-faint">{typeSize(c.type)}</td>
                    <td class="py-2.5">
                      {#if rr.length}
                        {#each rr as r (r.to.s + '.' + r.to.t + '.' + r.to.c)}
                          <button
                            type="button"
                            class="block max-w-full truncate font-mono text-xs text-accent-2 hover:underline"
                            title={'→ ' + r.to.s + '.' + r.to.t + '.' + r.to.c}
                            onclick={() => onNav(nodeId(r.to.s, r.to.t))}
                          >
                            → {r.to.s}.{r.to.t}.{r.to.c}
                          </button>
                        {/each}
                      {:else}
                        <span class="font-mono text-xs text-faint">—</span>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </section>

          {#if table.indexes?.length}
            <section class="mt-8">
              <h2 class="font-mono text-label uppercase text-faint">Indexes</h2>
              <div class="mt-3 flex flex-col gap-0">
                {#each table.indexes as ix (ix.def)}
                  <div class="flex items-center gap-3 border-b border-line-soft py-2.5 last:border-0">
                    <span class="font-mono text-xs text-fg">{ix.def}</span>
                    {#if ix.unique}<span class="col-badge pk">UNIQUE</span>{/if}
                    {#if ix.name}<span class="ml-auto font-mono text-faint" style="font-size: 0.66rem;">{ix.name}</span>{/if}
                  </div>
                {/each}
              </div>
            </section>
          {/if}

          {#if inRefs.length}
            <section class="mt-8 pb-6">
              <h2 class="font-mono text-label uppercase text-faint">Referenced by</h2>
              <div class="mt-3 flex flex-col">
                {#each inRefs as r (r.from.s + '.' + r.from.t + '.' + r.from.c)}
                  <button
                    type="button"
                    class="flex items-center gap-2 border-b border-line-soft py-2.5 text-left font-mono text-xs text-muted last:border-0 hover:text-fg"
                    onclick={() => onNav(nodeId(r.from.s, r.from.t))}
                  >
                    <span class="font-semibold text-accent-2">{r.from.s}.{r.from.t}</span>
                    <span class="text-faint">.{r.from.c}</span>
                    <Icon name="arrowR" size={12} class="text-faint" />
                    <span>{r.to.c}</span>
                    {#if r.action}<span class="col-badge ml-auto">{r.action}</span>{/if}
                  </button>
                {/each}
              </div>
            </section>
          {/if}
        </div>
      </div>
    {:else}
      <EntityDiagram {model} {entityKey} {onNav} />
    {/if}
  </div>
{:else}
  <div class="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-bg text-sm text-muted">
    Entity not found.
  </div>
{/if}
