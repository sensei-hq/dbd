<script lang="ts">
  import { onMount } from 'svelte';
  import Viewer from '$lib/viewer/Viewer.svelte';
  import { decodeFragment } from '$lib/viewer/fragment';
  import { validateModel } from '$lib/viewer/model';
  import { SAMPLE_MODEL } from '$lib/viewer/sample';
  import type { SchemaModel } from '$lib/viewer/model';

  let model = $state<SchemaModel | null>(null);
  let error = $state<string | null>(null);
  let dragging = $state(false);

  function accept(value: unknown) {
    const res = validateModel(value);
    if (res.ok) {
      model = res.model;
      error = null;
    } else {
      error = `Not a valid schema model: ${res.error}`;
    }
  }

  async function loadFile(file: File) {
    try {
      accept(JSON.parse(await file.text()));
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

  function onPick(e: Event) {
    const file = (e.target as HTMLInputElement).files?.[0];
    if (file) loadFile(file);
  }

  onMount(async () => {
    const hash = window.location.hash;
    if (hash.length > 1) {
      try {
        accept(await decodeFragment(hash));
      } catch (e) {
        error = `Could not read the diagram link: ${(e as Error).message}`;
      }
    }
  });
</script>

<svelte:head><title>dbd — diagram viewer</title></svelte:head>

{#if model}
  <div class="h-screen w-screen">
    <Viewer {model} />
  </div>
{:else}
  <main class="grid min-h-screen place-items-center bg-paper p-6 text-ink">
    <div
      role="region"
      aria-label="Upload schema"
      class="w-full max-w-lg rounded-lg border border-dashed p-10 text-center {dragging
        ? 'border-primary bg-accent-soft'
        : 'border-paper-edge'}"
      ondragover={(e) => {
        e.preventDefault();
        dragging = true;
      }}
      ondragleave={() => (dragging = false)}
      ondrop={onDrop}
    >
      <h1 class="font-display text-xl font-semibold">Open a schema diagram</h1>
      <p class="mt-2 text-sm text-ink-soft">
        Drop a <code class="font-mono">schema.json</code> here (from
        <code class="font-mono">dbd diagram --json</code>), or
      </p>
      <div class="mt-4 flex items-center justify-center gap-3">
        <label class="cursor-pointer rounded-md bg-primary px-3 py-2 text-sm text-on-primary">
          Choose file
          <input type="file" accept="application/json,.json" class="hidden" onchange={onPick} />
        </label>
        <button
          type="button"
          class="rounded-md border border-paper-edge px-3 py-2 text-sm"
          onclick={() => accept(SAMPLE_MODEL)}
        >Load example</button>
      </div>
      {#if error}<p class="mt-4 text-sm text-danger" data-error>{error}</p>{/if}
    </div>
  </main>
{/if}
