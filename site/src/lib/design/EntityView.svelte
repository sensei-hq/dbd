<script lang="ts">
  import { nodeId, type Column, type Ref, type SchemaModel } from '$lib/design/model';
  import Tabs from './Tabs.svelte';
  import EntityDiagram from './EntityDiagram.svelte';
  import EntityChangelog from './EntityChangelog.svelte';
  import Markdown from './Markdown.svelte';
  import { noteBlocks } from './md';

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

  // `uq` is v2. A v1 payload says the same thing only as a unique index on one plain column,
  // so both count — an expression index like `(lower(email))` names no column and does not.
  const uniqueCols = $derived(
    new Set([
      ...(table?.columns.filter((c) => c.uq).map((c) => c.name) ?? []),
      ...(table?.indexes ?? [])
        .filter((ix) => ix.unique)
        .map((ix) => ix.def.replace(/^\(|\)$/g, '').trim())
        .filter((col) => /^[A-Za-z_][\w$]*$/.test(col)),
    ]),
  );

  type Badge = { label: string; cls: string };
  function propBadges(c: Column): Badge[] {
    const out: Badge[] = [];
    if (c.pk) out.push({ label: 'PK', cls: 'pk' });
    if (c.fk || fkCols.has(c.name)) out.push({ label: 'FK', cls: 'fk' });
    if (c.nn) out.push({ label: 'NN', cls: '' });
    if (uniqueCols.has(c.name) && !c.pk) out.push({ label: 'UNIQUE', cls: '' });
    if (c.en) out.push({ label: 'ENUM', cls: '' });
    return out;
  }

  const comment = $derived(noteBlocks(table?.noteMd ?? table?.note));

  // `deps` is v2: a v1 payload has no dependency graph at all, which is different from a
  // table nothing depends on — the first omits the section, the second says so.
  const hasDeps = $derived(model.deps !== undefined);
  const usedBy = $derived(
    (model.deps ?? []).filter(
      (d) => d.to.s === schema && d.to.n === name && !(d.from.s === schema && d.from.n === name),
    ),
  );
  const kindOf = (s: string, n: string): string =>
    model.entities?.find((e) => e.schema === s && e.name === n)?.kind.replace(/_/g, ' ') ??
    (model.tables.some((t) => t.schema === s && t.name === n) ? 'table' : 'unknown');
</script>


{#if table}
  <div class="flex min-h-0 min-w-0 flex-1 flex-col">
    <!-- header -->
    <div class="border-b border-line bg-paper">
      <div class="px-6 pb-3 pt-5">
        <div class="flex flex-wrap items-center gap-3">
          <!-- One qualified name, `auth.users`: the schema is the name's lead-in, not a line
               above it. The heading stays the bare table name. -->
          <div class="flex items-baseline">
            <span class="font-mono text-sm text-faint">{schema}.</span>
            <h1 class="font-display text-h3 font-semibold tracking-tight">{name}</h1>
          </div>
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
          { id: 'changelog', label: 'Changelog', icon: 'clock' },
        ]}
        active={tab}
        onChange={(id) => (tab = id)}
      />
    </div>

    {#if tab === 'details'}
      <!-- Full width, on the header's left edge: the fields table has six columns and a
           centred measure starved the notes. -->
      <div class="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
        <div class="px-6 py-6">
          <section data-section="info">
            <h2 class="font-mono text-label uppercase text-faint">Table info</h2>
            <div class="mt-3 flex max-w-3xl flex-col gap-2 text-sm leading-relaxed text-muted">
              {#if comment.length}
                <Markdown blocks={comment} />
              {:else}
                <p class="text-faint">No comment on this table — add one with <code
                    class="rounded bg-code-bg px-1 font-mono text-accent-2"
                    style="font-size: 0.85em;">COMMENT ON TABLE</code>.</p>
              {/if}
            </div>
          </section>

          <section data-section="fields" class="mt-8">
            <h2 class="font-mono text-label uppercase text-faint">
              Fields <span class="text-faint">· {table.columns.length}</span>
            </h2>
            <!-- Scrolls sideways below its minimum rather than squeezing Notes to nothing. -->
            <div class="mt-3 overflow-x-auto">
              <table class="w-full table-fixed border-collapse text-left" style="min-width: 56rem;">
                <colgroup>
                  <col style="width: 16%;" />
                  <col style="width: 13%;" />
                  <col style="width: 150px;" />
                  <col style="width: 12%;" />
                  <col style="width: 18%;" />
                  <col />
                </colgroup>
                <thead>
                  <tr class="font-mono text-xs uppercase tracking-wider text-faint">
                    <th class="ds-th py-2.5 pr-4 font-medium">Name</th>
                    <th class="ds-th py-2.5 pr-4 font-medium">Type</th>
                    <th class="ds-th py-2.5 pr-4 font-medium">Settings</th>
                    <th class="ds-th py-2.5 pr-4 font-medium">Default</th>
                    <th class="ds-th py-2.5 pr-4 font-medium">References</th>
                    <th class="ds-th py-2.5 font-medium">Notes</th>
                  </tr>
                </thead>
                <tbody>
                  {#each table.columns as c (c.name)}
                    {@const rr = refsForCol(c.name)}
                    {@const note = noteBlocks(c.note)}
                    <tr data-col-row={c.name} class="border-b border-line-soft align-top">
                      <td
                        data-cell="name"
                        class="py-2.5 pr-4 font-mono text-xs font-semibold text-fg"
                        style="overflow-wrap: anywhere;">{c.name}</td
                      >
                      <td
                        data-cell="type"
                        class="py-2.5 pr-4 font-mono text-xs text-muted"
                        style="overflow-wrap: anywhere;">{c.type}</td
                      >
                      <td data-cell="settings" class="py-2.5 pr-4">
                        <div class="flex flex-wrap gap-1">
                          {#each propBadges(c) as b (b.label)}<span data-badge class="col-badge {b.cls}"
                              >{b.label}</span
                            >{/each}
                        </div>
                      </td>
                      <td
                        data-cell="default"
                        class="py-2.5 pr-4 font-mono text-xs {c.def ? 'text-muted' : 'text-faint'}"
                        style="overflow-wrap: anywhere;">{c.def ?? '—'}</td
                      >
                      <td data-cell="refs" class="py-2.5 pr-4">
                        {#each rr as r (r.to.s + '.' + r.to.t + '.' + r.to.c)}
                          <!-- Wraps rather than truncates: a cut-off target is a broken link label. -->
                          <button
                            type="button"
                            class="block max-w-full text-left font-mono text-xs text-accent-2 hover:underline"
                            style="overflow-wrap: anywhere;"
                            onclick={() => onNav(nodeId(r.to.s, r.to.t))}
                          >
                            → {r.to.s}.{r.to.t}.{r.to.c}
                          </button>
                        {:else}
                          <span class="font-mono text-xs text-faint">—</span>
                        {/each}
                      </td>
                      <td data-cell="notes" class="py-2.5 text-xs leading-snug text-muted">
                        {#if note.length}
                          <div class="flex flex-col gap-1"><Markdown blocks={note} /></div>
                        {:else}
                          <span class="text-faint">—</span>
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          </section>

          <section data-section="references" class="mt-8">
            <h2 class="font-mono text-label uppercase text-faint">
              References <span class="text-faint">· {outRefs.length + inRefs.length}</span>
            </h2>
            {#if outRefs.length || inRefs.length}
              <div class="mt-3 flex flex-col">
                {#each outRefs as r (r.from.c + '>' + r.to.s + '.' + r.to.t + '.' + r.to.c)}
                  <button
                    data-ref="out"
                    type="button"
                    class="flex items-center gap-3 border-b border-line-soft py-2.5 text-left font-mono text-xs text-muted last:border-0 hover:text-fg"
                    onclick={() => onNav(nodeId(r.to.s, r.to.t))}
                  >
                    <span class="col-badge">out</span>
                    <span
                      ><span class="text-fg">{r.from.c}</span> → <span class="font-semibold text-accent-2"
                        >{r.to.s}.{r.to.t}</span
                      >.{r.to.c}</span
                    >
                    {#if r.action}<span class="col-badge ml-auto">{r.action}</span>{/if}
                  </button>
                {/each}
                {#each inRefs as r (r.from.s + '.' + r.from.t + '.' + r.from.c + '>' + r.to.c)}
                  <button
                    data-ref="in"
                    type="button"
                    class="flex items-center gap-3 border-b border-line-soft py-2.5 text-left font-mono text-xs text-muted last:border-0 hover:text-fg"
                    onclick={() => onNav(nodeId(r.from.s, r.from.t))}
                  >
                    <span class="col-badge">in</span>
                    <span
                      ><span class="font-semibold text-accent-2">{r.from.s}.{r.from.t}</span
                      >.{r.from.c} → <span class="text-fg">{r.to.c}</span></span
                    >
                    {#if r.action}<span class="col-badge ml-auto">{r.action}</span>{/if}
                  </button>
                {/each}
              </div>
            {:else}
              <p class="mt-3 text-sm text-faint">No foreign keys in or out.</p>
            {/if}
          </section>

          {#if hasDeps}
            <section data-section="dependencies" class="mt-8">
              <h2 class="font-mono text-label uppercase text-faint">
                Dependencies <span class="text-faint">· {usedBy.length}</span>
              </h2>
              {#if usedBy.length}
                <div class="mt-3 flex flex-col">
                  {#each usedBy as d (d.from.s + '.' + d.from.n + ':' + d.kind)}
                    <div
                      data-dep={nodeId(d.from.s, d.from.n)}
                      class="flex items-center gap-3 border-b border-line-soft py-2.5 font-mono text-xs text-muted last:border-0 {d.unresolved
                        ? 'opacity-60'
                        : ''}"
                      title={d.unresolved ? 'Not defined in this project' : undefined}
                    >
                      <span class="font-semibold text-fg">{d.from.s}.{d.from.n}</span>
                      <span class="col-badge">{kindOf(d.from.s, d.from.n)}</span>
                      <span class="ml-auto text-faint">{d.kind}</span>
                    </div>
                  {/each}
                </div>
              {:else}
                <p class="mt-3 text-sm text-faint">Nothing reads, writes or calls this table.</p>
              {/if}
            </section>
          {/if}

          {#if table.indexes?.length}
            <section data-section="indexes" class="mt-8 pb-6">
              <h2 class="font-mono text-label uppercase text-faint">
                Indexes <span class="text-faint">· {table.indexes.length}</span>
              </h2>
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
        </div>
      </div>
    {:else if tab === 'changelog'}
      <EntityChangelog {model} kind="table" {schema} {name} />
    {:else}
      <EntityDiagram {model} {entityKey} {onNav} />
    {/if}
  </div>
{:else}
  <div class="flex min-h-0 min-w-0 flex-1 items-center justify-center bg-bg text-sm text-muted">
    Entity not found.
  </div>
{/if}
