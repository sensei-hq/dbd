<script lang="ts">
  import '$lib/design/styles.css';
  import { onMount } from 'svelte';
  import { vibe } from '@rokkit/states';
  import Header from '$lib/design/Header.svelte';
  import Icon from '$lib/design/Icon.svelte';
  import ProjectsView from '$lib/design/ProjectsView.svelte';
  import { BRAND, sampleUser, type HeaderData } from '$lib/design/data';
  import { listDiagrams, deleteDiagram, type SavedDiagram } from '$lib/design/store';

  const theme = $derived<'light' | 'dark'>(vibe.mode === 'dark' ? 'dark' : 'light');
  const toggleTheme = () => (vibe.mode = vibe.mode === 'dark' ? 'light' : 'dark');

  const headerData: HeaderData = {
    brand: BRAND,
    crumbs: [{ label: 'Designs', current: true }],
    user: sampleUser,
  };

  let list = $state<SavedDiagram[]>([]);
  let loaded = $state(false);
  let layout = $state<'cards' | 'rows' | 'gallery'>('cards');
  let query = $state('');

  onMount(async () => {
    list = await listDiagrams();
    loaded = true;
  });

  function remove(project: string) {
    deleteDiagram(project);
    list = list.filter((d) => d.project !== project);
  }

  const q = $derived(query.trim().toLowerCase());
  const filtered = $derived(
    q
      ? list.filter((d) => (d.project + ' ' + (d.model.project.note ?? '')).toLowerCase().includes(q))
      : list
  );

  const LAYOUTS: { id: 'cards' | 'rows' | 'gallery'; icon: string }[] = [
    { id: 'cards', icon: 'grid' },
    { id: 'rows', icon: 'rows' },
    { id: 'gallery', icon: 'eye' },
  ];
</script>

<svelte:head>
  <title>Your designs — dbd</title>
  <meta
    name="description"
    content="Your saved dbd schema diagrams — open, search, and manage them locally in your browser."
  />
  <!-- Personal workspace (localStorage) — also Disallow:ed in robots.txt. -->
  <meta name="robots" content="noindex" />
</svelte:head>

<div class="dbd-app flex min-h-screen flex-col">
  <Header data={headerData} {theme} brandHref="/" showUser={false} onToggleTheme={toggleTheme} />

  <main class="mx-auto w-full max-w-5xl flex-1 px-5 py-8 lg:py-10">
    <div class="flex flex-wrap items-end gap-4">
      <div>
        <h1 class="font-display text-h3 font-semibold tracking-tight">Your designs</h1>
        <p class="mt-1 text-sm text-muted">
          {#if loaded}{list.length}
            {list.length === 1 ? 'design' : 'designs'} saved in this browser{:else}Loading…{/if}
        </p>
      </div>

      {#if list.length}
        <div class="ml-auto flex items-center gap-2">
          <!-- layout switcher -->
          <div class="flex rounded-app border border-line bg-paper p-0.5">
            {#each LAYOUTS as l (l.id)}
              <button
                type="button"
                title={l.id}
                class="ds-iconbtn {layout === l.id ? 'bg-accent-soft text-accent-2' : ''}"
                aria-pressed={layout === l.id}
                onclick={() => (layout = l.id)}
              >
                <Icon name={l.icon} size={15} />
              </button>
            {/each}
          </div>
          <!-- search -->
          <div class="relative hidden sm:block">
            <span class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-faint">
              <Icon name="search" size={14} />
            </span>
            <input
              class="ds-input w-56"
              style="padding-left: 2.1rem;"
              placeholder="Filter designs…"
              aria-label="Filter designs"
              bind:value={query}
            />
          </div>
        </div>
      {/if}
    </div>

    <div class="mt-7">
      {#if filtered.length}
        <ProjectsView list={filtered} {layout} onDelete={remove} />
      {:else if loaded && list.length}
        <div class="flex flex-col items-center gap-2 rounded-app-lg border border-dashed border-line bg-bg-deep py-16 text-center">
          <p class="text-sm text-muted">No designs match “{query}”.</p>
          <button type="button" class="text-xs text-accent-2" onclick={() => (query = '')}>Clear filter</button>
        </div>
      {:else if loaded}
        <div class="flex flex-col items-center gap-3 rounded-app-lg border border-dashed border-line bg-bg-deep py-16 text-center">
          <p class="text-sm text-muted">No designs in this browser yet.</p>
          <p class="font-mono text-xs text-faint">
            run <span class="text-muted">$ dbd diagram</span> in any project, or open a shared link
          </p>
          <a href="/diagram" class="ds-btn">Open the sample diagram</a>
        </div>
      {/if}
    </div>

    <p class="mt-8 text-center font-mono text-xs text-faint">
      designs are stored locally — no account, nothing uploaded
    </p>
  </main>
</div>
