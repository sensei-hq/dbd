<script lang="ts" module>
  // Helpers ported verbatim from docs/mockup/designs/entity-page.jsx.
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
  import './styles.css';
  import { marked } from 'marked';
  import Icon from './Icon.svelte';
  import Tabs from './Tabs.svelte';
  import EntityDiagram from './EntityDiagram.svelte';
  import { nodeId, type Column, type Ref, type SchemaModel } from './model';

  // Ports docs/mockup/designs/entity-page.jsx `EntityView` (table kind only —
  // our model has no enum entities, so the enum branch + enum nav are omitted).
  let {
    model,
    entityKey,
    onNav,
  }: {
    model: SchemaModel;
    entityKey: string;
    onNav: (key: string) => void;
  } = $props();

  let tab = $state<'details' | 'diagram'>('details');
  // Reset to Details whenever the entity changes (depends only on entityKey).
  $effect(() => {
    void entityKey;
    tab = 'details';
  });

  const parts = $derived(entityKey.split('.'));
  const schema = $derived(parts[0]);
  const name = $derived(parts[1]);
  const table = $derived(model.tables.find((t) => t.schema === schema && t.name === name) ?? null);

  const outRefs = $derived(model.refs.filter((r) => r.from.s === schema && r.from.t === name));
  const inRefs = $derived(model.refs.filter((r) => r.to.s === schema && r.to.t === name));
  // FK columns of this entity = columns appearing as `from.c` in its outgoing refs.
  const fkCols = $derived(new Set(outRefs.map((r) => r.from.c)));

  function refsForCol(col: string): Ref[] {
    return outRefs.filter((r) => r.from.c === col);
  }

  type Badge = { label: string; cls: string };
  // PK (c.pk), FK (derived), NN (c.nn), ENUM (c.en). UQ omitted (no field).
  function propBadges(c: Column): Badge[] {
    const out: Badge[] = [];
    if (c.pk) out.push({ label: 'PK', cls: 'pk' });
    if (fkCols.has(c.name)) out.push({ label: 'FK', cls: 'fk' });
    if (c.nn) out.push({ label: 'NN', cls: '' });
    if (c.en) out.push({ label: 'ENUM', cls: '' });
    return out;
  }

  // The comment is the user's own local DDL comment — trusted input for this
  // offline tool — so rendering it via {@html} is acceptable. marked.parse is
  // synchronous (no async extensions configured); cast to string.
  const noteHtml = $derived(table?.noteMd ? (marked.parse(table.noteMd) as string) : '');
</script>

{#if table}
  <div class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- header -->
    <div class="border-b border-paper-edge bg-paper-soft">
      <div class="px-6 pb-3 pt-5">
        <div class="text-xs font-mono text-ink-soft">{schema}.</div>
        <div class="flex flex-wrap items-center gap-3">
          <h1 class="font-display text-lg font-semibold tracking-tight text-ink">{name}</h1>
          <span class="ds-badge text-xs font-mono">{table.columns.length} columns</span>
          {#if inRefs.length > 0 || outRefs.length > 0}
            <span class="ds-badge text-xs font-mono">{outRefs.length} out · {inRefs.length} in</span>
          {/if}
        </div>
      </div>
      <Tabs
        tabs={[
          ['details', 'Details', 'rows'],
          ['diagram', 'Diagram', 'grid'],
        ]}
        active={tab}
        onChange={(id) => (tab = id as 'details' | 'diagram')}
      />
    </div>

    {#if tab === 'details'}
      <div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-paper">
        <div class="mx-auto max-w-4xl px-6 py-6">
          {#if noteHtml}
            <section class="mb-8">
              <h2 class="text-xs font-mono uppercase text-ink-soft">Comment</h2>
              <!-- trusted local DDL comment, see noteHtml above -->
              <div class="vw-md mt-3 text-sm text-ink-mute [&_code]:font-mono">
                {@html noteHtml}
              </div>
            </section>
          {/if}

          <section>
            <h2 class="text-xs font-mono uppercase text-ink-soft">Columns</h2>
            <table class="mt-3 w-full table-fixed border-collapse text-left">
              <colgroup>
                <col style="width:30%" />
                <col style="width:104px" />
                <col style="width:130px" />
                <col style="width:64px" />
                <col />
              </colgroup>
              <thead>
                <tr class="text-xs font-mono uppercase tracking-wider text-ink-soft">
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
                  <tr data-col-row={c.name} class="border-b border-paper-edge align-top">
                    <td class="py-2.5 pr-4">
                      <div
                        class="text-sm font-mono font-semibold text-ink"
                        style="overflow-wrap:anywhere"
                      >
                        {c.name}
                      </div>
                      {#if c.note}
                        <div class="mt-0.5 text-xs leading-snug text-ink-mute">{c.note}</div>
                      {/if}
                      {#if c.def}
                        <div class="mt-0.5 text-xs font-mono text-ink-soft">default: {c.def}</div>
                      {/if}
                    </td>
                    <td class="py-2.5 pr-4">
                      <div class="flex flex-wrap gap-1">
                        {#each propBadges(c) as b (b.label)}
                          <span class="col-badge {b.cls}">{b.label}</span>
                        {/each}
                      </div>
                    </td>
                    <td
                      class="py-2.5 pr-4 text-xs font-mono text-ink-mute"
                      style="overflow-wrap:anywhere"
                    >
                      {baseType(c.type)}
                    </td>
                    <td class="whitespace-nowrap py-2.5 pr-4 text-xs font-mono text-ink-soft">
                      {typeSize(c.type)}
                    </td>
                    <td class="py-2.5">
                      {#if rr.length}
                        {#each rr as r (r.to.s + '.' + r.to.t + '.' + r.to.c)}
                          <button
                            type="button"
                            class="block max-w-full truncate text-xs font-mono text-primary hover:underline"
                            title={'→ ' + r.to.s + '.' + r.to.t + '.' + r.to.c}
                            onclick={() => onNav(nodeId(r.to.s, r.to.t))}
                          >
                            → {r.to.s}.{r.to.t}.{r.to.c}
                          </button>
                        {/each}
                      {:else}
                        <span class="text-xs font-mono text-ink-soft">—</span>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </section>

          {#if inRefs.length > 0}
            <section class="mt-8 pb-6">
              <h2 class="text-xs font-mono uppercase text-ink-soft">Referenced by</h2>
              <div class="mt-3 flex flex-col">
                {#each inRefs as r (r.from.s + '.' + r.from.t + '.' + r.from.c)}
                  <button
                    type="button"
                    class="flex items-center gap-2 border-b border-paper-edge py-2.5 text-left text-xs font-mono text-ink-mute last:border-0 hover:text-ink"
                    onclick={() => onNav(nodeId(r.from.s, r.from.t))}
                  >
                    <span class="font-semibold text-primary">{r.from.s}.{r.from.t}</span>
                    <span class="text-ink-soft">.{r.from.c}</span>
                    <Icon name="arrowR" size={12} class="text-ink-soft" />
                    <span>{r.to.c}</span>
                    {#if r.action}
                      <span class="col-badge ml-auto">{r.action}</span>
                    {/if}
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
  <div class="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-paper text-sm text-ink-mute">
    Entity not found.
  </div>
{/if}

<style>
  /* structural-only; colors come from Rokkit tokens in markup */
  .vw-md :global(strong) {
    font-weight: 600;
    color: var(--ink);
  }
  .vw-md :global(p) {
    margin: 0.25rem 0;
  }
  .vw-md :global(code) {
    font-size: 0.85em;
  }
  .vw-md :global(ul) {
    list-style: disc;
    padding-left: 1.25rem;
  }
  .vw-md :global(pre) {
    overflow-x: auto;
  }
</style>
