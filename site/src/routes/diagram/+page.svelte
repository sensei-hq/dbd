<script lang="ts">
  import '$lib/design/styles.css';
  import { onMount } from 'svelte';
  import { vibe } from '@rokkit/states';
  import Header from '$lib/design/Header.svelte';
  import Sidebar from '$lib/design/Sidebar.svelte';
  import ContentHeader from '$lib/design/ContentHeader.svelte';
  import DiagramView from '$lib/design/DiagramView.svelte';
  import EntitiesView from '$lib/design/EntitiesView.svelte';
  import EntityView from '$lib/design/EntityView.svelte';
  import { buildHeaderData, buildContentHeaderData, sampleModel } from '$lib/design/data';
  import { saveDiagram } from '$lib/design/store';
  import { decodeFragment } from '$lib/design/fragment';
  import { validateModel, type SchemaModel } from '$lib/design/model';

  // Dark mode is the site-wide rokkit [data-mode]; drive the shared store.
  const theme = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
  const toggleTheme = () => (vibe.mode = vibe.mode === 'dark' ? 'light' : 'dark');

  // The model arrives via the `#1.<payload>` share link (decoded client-side) or
  // an uploaded .json (the large-schema fallback). The bundled sample is the
  // empty-state shown when neither is present.
  let model = $state<SchemaModel>(sampleModel);
  let error = $state<string | null>(null);
  let dragging = $state(false);
  let selected = $state<string | null>(null);
  let rootTab = $state('diagram');

  function accept(value: unknown): SchemaModel | null {
    const res = validateModel(value);
    if (res.ok) {
      model = res.model;
      selected = null;
      error = null;
      return res.model;
    }
    error = `Not a valid schema model: ${res.error}`;
    return null;
  }
  async function loadFile(file: File) {
    try {
      const m = accept(JSON.parse(await file.text()));
      if (m) await saveDiagram(m); // persist uploaded design to the local projects list
    } catch {
      error = 'Could not parse that file as JSON.';
    }
  }
  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    const file = e.dataTransfer?.files?.[0];
    if (file) loadFile(file);
  }

  onMount(async () => {
    const hash = window.location.hash;
    if (hash.length > 1) {
      try {
        const payload = hash.slice(1);
        const m = accept(await decodeFragment(hash));
        if (m) await saveDiagram(m, payload); // a visited share link joins the local projects list
      } catch (e) {
        error = `Could not read the diagram link: ${(e as Error).message}`;
      }
    }
  });

  const entityName = $derived(selected ? selected.split('.')[1] : undefined);
  const headerData = $derived(buildHeaderData(model.project.name, entityName));
  const contentHeaderData = $derived(buildContentHeaderData(model, rootTab));
  const sidebarData = $derived({ project: { name: model.project.name }, model, enums: [] });

  // string → select a table/entity; '' (sidebar project button) / null → root.
  const pick = (key: string | null) => (selected = key || null);
</script>

<svelte:head>
  <title>{model.project.name} — dbd</title>
  <meta
    name="description"
    content="Interactive ER diagram viewer — pan, zoom, and inspect tables, columns, and relationships."
  />
</svelte:head>

<div
  class="dbd-app relative flex h-screen flex-col overflow-hidden"
  role="application"
  aria-label="dbd schema diagram"
  ondragover={(e) => {
    e.preventDefault();
    dragging = true;
  }}
  ondragleave={() => (dragging = false)}
  ondrop={onDrop}
>
  <Header
    data={headerData}
    {theme}
    brandHref="/projects"
    showUser={false}
    onCrumb={() => (selected = null)}
    onToggleTheme={toggleTheme}
  />

  <div class="flex min-h-0 flex-1">
    <Sidebar data={sidebarData} selectedKey={selected} onPick={pick} />

    {#if selected}
      <EntityView {model} entityKey={selected} onNav={pick} />
    {:else}
      <div class="flex min-h-0 min-w-0 flex-1 flex-col">
        <ContentHeader data={contentHeaderData} bind:activeTab={rootTab} />
        {#if rootTab === 'entities'}
          <EntitiesView {model} onNav={pick} />
        {:else}
          <div class="relative min-h-0 min-w-0 flex-1 bg-bg-deep">
            <DiagramView {model} {selected} onSelect={pick} />
            <div
              class="pointer-events-none absolute bottom-5 left-1/2 z-20 -translate-x-1/2 whitespace-nowrap rounded-full border border-line bg-paper px-4 py-2 text-xs text-faint"
            >
              click a table to open it · drop a schema .json to load your own
            </div>
          </div>
        {/if}
      </div>
    {/if}
  </div>

  {#if dragging}
    <div
      class="pointer-events-none absolute inset-3 z-40 flex items-center justify-center rounded-app-lg border-2 border-dashed border-accent text-sm font-medium text-accent-2"
      style="background: color-mix(in oklch, var(--accent-soft) 55%, transparent);"
    >
      Drop a schema .json to load it
    </div>
  {/if}
  {#if error}
    <div
      data-error
      class="absolute bottom-4 left-1/2 z-50 -translate-x-1/2 rounded-app border border-line bg-paper px-4 py-2 text-sm text-fg shadow-sm"
    >
      {error}
    </div>
  {/if}
</div>
