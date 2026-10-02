<script lang="ts">
  /**
   * An enum (#34): its values in declaration order, the columns of its type, and its own
   * changelog — enums are in snapshots, so unlike views and routines they have history.
   */
  import Tabs from './Tabs.svelte';
  import Markdown from './Markdown.svelte';
  import EntityChangelog from './EntityChangelog.svelte';
  import { noteBlocks } from './md';
  import { KIND_ICON } from './entities';
  import type { SchemaModel } from './model';

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

  const en = $derived(model.enums?.find((e) => `${e.schema}.${e.name}` === entityKey));
  const comment = $derived(noteBlocks(en?.noteMd ?? en?.note));
  // A column names its type bare or schema-qualified; either is this enum.
  const columns = $derived(
    en
      ? model.tables.flatMap((t) =>
          t.columns
            .filter((c) => c.type === en.name || c.type === `${en.schema}.${en.name}`)
            .map((c) => ({ table: `${t.schema}.${t.name}`, column: c.name })),
        )
      : [],
  );
</script>

{#if en}
  <div data-enum-view class="flex min-h-0 min-w-0 flex-1 flex-col">
    <div class="border-b border-line bg-paper">
      <div class="px-6 pb-3 pt-5">
        <div class="flex flex-wrap items-center gap-3">
          <div class="flex items-baseline">
            <span class="font-mono text-sm text-faint">{en.schema}.</span>
            <h1 class="font-display text-h3 font-semibold tracking-tight">{en.name}</h1>
          </div>
          <span data-kind-badge class="ds-badge">enum</span>
          <span class="ds-badge">{en.values.length} values</span>
        </div>
      </div>
      <Tabs
        tabs={[
          { id: 'details', label: 'Details', icon: 'rows' },
          { id: 'changelog', label: 'Changelog', icon: 'clock' },
        ]}
        active={tab}
        onChange={(id) => (tab = id)}
      />
    </div>

    {#if tab === 'details'}
      <div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
        <div class="px-6 py-6">
          <section data-section="info">
            <h2 class="font-mono text-label uppercase text-faint">Info</h2>
            <div class="mt-3 flex max-w-3xl flex-col gap-2 text-sm leading-relaxed text-muted">
              {#if comment.length}
                <Markdown blocks={comment} />
              {:else}
                <p class="text-faint">No comment on this enum.</p>
              {/if}
            </div>
          </section>
          <section data-section="values" class="mt-8">
            <h2 class="font-mono text-label uppercase text-faint">
              Values <span class="text-faint">· {en.values.length}</span>
            </h2>
            <ol class="mt-3 flex max-w-3xl flex-col">
              {#each en.values as value, i (value)}
                <li data-value={value} class="flex items-center gap-3 border-b border-line-soft py-2 font-mono text-xs last:border-0">
                  <span class="w-6 text-right text-faint">{i + 1}</span>
                  <span class="text-fg">{value}</span>
                </li>
              {/each}
            </ol>
          </section>
          <section data-section="used-by" class="mt-8 pb-6">
            <h2 class="font-mono text-label uppercase text-faint">
              Used by <span class="text-faint">· {columns.length}</span>
            </h2>
            {#if columns.length}
              <div class="mt-3 flex max-w-3xl flex-col">
                {#each columns as c (c.table + '.' + c.column)}
                  <div data-column="{c.table}.{c.column}" class="flex items-center gap-3 border-b border-line-soft py-2.5 font-mono text-xs text-muted last:border-0">
                    <span class="{KIND_ICON.table} text-faint" aria-hidden="true"></span>
                    <!-- One qualified name: the flex gap must not split the table from its column. -->
                    <span
                      ><button type="button" class="font-semibold text-accent-2 hover:underline" onclick={() => onNav(c.table)}
                        >{c.table}</button
                      >.{c.column}</span
                    >
                  </div>
                {/each}
              </div>
            {:else}
              <p class="mt-3 text-sm text-faint">No column in this project is of this type.</p>
            {/if}
          </section>
        </div>
      </div>
    {:else}
      <EntityChangelog {model} kind="enum" schema={en.schema} name={en.name} />
    {/if}
  </div>
{:else}
  <div class="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-bg text-sm text-muted">Entity not found.</div>
{/if}
