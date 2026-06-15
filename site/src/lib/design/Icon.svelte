<script lang="ts" module>
  /* Stroke icon set (24 viewBox) copied verbatim from
     docs/mockup/designs/app-shell.jsx (the ICONS object). Entries that are
     arrays of `d` strings render as multiple <path> elements. */
  export const ICONS: Record<string, string | string[]> = {
    sun: [
      'M12 4V2M12 22v-2M4 12H2M22 12h-2M5.6 5.6 4.2 4.2M19.8 19.8l-1.4-1.4M5.6 18.4l-1.4 1.4M19.8 4.2l-1.4 1.4',
      'M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10Z',
    ],
    moon: 'M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8Z',
    search: ['M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z', 'm21 21-4.3-4.3'],
    x: 'M18 6 6 18M6 6l12 12',
    chevR: 'm9 6 6 6-6 6',
    chevD: 'm6 9 6 6 6-6',
    table: ['M3 5.5h18v13H3z', 'M3 10h18', 'M9.5 10v8.5'],
    enumI: ['M4 6h2M4 12h2M4 18h2', 'M9 6h11M9 12h11M9 18h11'],
    lock: ['M5 11h14v10H5z', 'M8 11V7a4 4 0 0 1 8 0v4'],
    globe: [
      'M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z',
      'M3 12h18',
      'M12 3a13.5 13.5 0 0 1 0 18 13.5 13.5 0 0 1 0-18Z',
    ],
    upload: ['M12 16V4m0 0L7 9m5-5 5 5', 'M4 20h16'],
    terminal: ['m5 8 4 4-4 4', 'M12 17h7'],
    copy: ['M9 9h11v11H9z', 'M5 15H4V4h11v1'],
    check: 'm4.5 12.5 5 5 10-11',
    arrowL: 'M19 12H5m0 0 6 6m-6-6 6-6',
    arrowR: 'M5 12h14m0 0-6-6m6 6-6 6',
    plus: 'M12 5v14M5 12h14',
    minus: 'M5 12h14',
    fit: ['M9 4H4v5', 'M15 4h5v5', 'M9 20H4v-5', 'M15 20h5v-5'],
    key: ['M14 11a4.5 4.5 0 1 0-4.4 4.5L11 14h2v-2h2l1-1Z', 'm11 14 4 4h3v-3'],
    link: [
      'M10 14a5 5 0 0 0 7.5.5l2-2a5 5 0 0 0-7-7l-1 1',
      'M14 10a5 5 0 0 0-7.5-.5l-2 2a5 5 0 0 0 7 7l1-1',
    ],
    mail: ['M3 5.5h18v13H3z', 'm3 7 9 6.5L21 7'],
    doc: ['M6 2.5h8L19 8v13.5H6z', 'M13 3v5h5'],
    grid: ['M4 4h7v7H4z', 'M13 4h7v7h-7z', 'M4 13h7v7H4z', 'M13 13h7v7h-7z'],
    rows: ['M4 5h16M4 12h16M4 19h16'],
    eye: [
      'M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z',
      'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z',
    ],
  };
</script>

<script lang="ts">
  let {
    name,
    size = 16,
    class: cls = '',
  }: { name: string; size?: number; class?: string } = $props();

  // Normalise to an array so single- and multi-path icons share one template.
  const paths = $derived.by(() => {
    const d = ICONS[name];
    if (!d) return [];
    return Array.isArray(d) ? d : [d];
  });
</script>

<svg
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.8"
  stroke-linecap="round"
  stroke-linejoin="round"
  class={cls}
  aria-hidden="true"
>
  {#each paths as d (d)}
    <path {d} />
  {/each}
</svg>
