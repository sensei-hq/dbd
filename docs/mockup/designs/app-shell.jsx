/* ============================================================
   dbd designs — shared app shell
   Exports (window): useAppPrefs, AppHeader, BrandMark, Avatar,
   EngineBadge, VisBadge, SchemaSnapshotThumb, AppIcon
   ============================================================ */

const PREFS_KEY = "dbd-designs-prefs";

function readPrefs() {
  try {
    const p = JSON.parse(localStorage.getItem(PREFS_KEY) || "{}");
    return { theme: p.theme === "dark" ? "dark" : "light", look: p.look === "tool" ? "tool" : "paper" };
  } catch (e) {
    return { theme: "light", look: "paper" };
  }
}

function applyPrefs(p) {
  const el = document.documentElement;
  el.setAttribute("data-theme", p.theme);
  el.setAttribute("data-look", p.look);
  el.setAttribute("data-accent", "sky");
}

/* Shared light/dark + paper/tool prefs, persisted across the app pages. */
function useAppPrefs() {
  const [prefs, setPrefs] = React.useState(readPrefs);
  React.useEffect(() => { applyPrefs(prefs); }, [prefs]);
  const setPref = React.useCallback((key, value) => {
    setPrefs((prev) => {
      const next = { ...prev, [key]: value };
      try { localStorage.setItem(PREFS_KEY, JSON.stringify(next)); } catch (e) {}
      return next;
    });
  }, []);
  return [prefs, setPref];
}

/* ---------- icons (stroke, 24 viewBox) ---------- */
function AppIcon({ d, size = 16, className = "", filled = false }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill={filled ? "currentColor" : "none"}
      stroke={filled ? "none" : "currentColor"} strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"
      className={className} aria-hidden="true">
      {Array.isArray(d) ? d.map((p, i) => <path key={i} d={p}></path>) : <path d={d}></path>}
    </svg>
  );
}

const ICONS = {
  sun: ["M12 4V2M12 22v-2M4 12H2M22 12h-2M5.6 5.6 4.2 4.2M19.8 19.8l-1.4-1.4M5.6 18.4l-1.4 1.4M19.8 4.2l-1.4 1.4", "M12 17a5 5 0 1 0 0-10 5 5 0 0 0 0 10Z"],
  moon: "M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8Z",
  search: ["M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16Z", "m21 21-4.3-4.3"],
  x: "M18 6 6 18M6 6l12 12",
  chevR: "m9 6 6 6-6 6",
  chevD: "m6 9 6 6 6-6",
  table: ["M3 5.5h18v13H3z", "M3 10h18", "M9.5 10v8.5"],
  enumI: ["M4 6h2M4 12h2M4 18h2", "M9 6h11M9 12h11M9 18h11"],
  lock: ["M5 11h14v10H5z", "M8 11V7a4 4 0 0 1 8 0v4"],
  globe: ["M12 21a9 9 0 1 0 0-18 9 9 0 0 0 0 18Z", "M3 12h18", "M12 3a13.5 13.5 0 0 1 0 18 13.5 13.5 0 0 1 0-18Z"],
  upload: ["M12 16V4m0 0L7 9m5-5 5 5", "M4 20h16"],
  terminal: ["m5 8 4 4-4 4", "M12 17h7"],
  copy: ["M9 9h11v11H9z", "M5 15H4V4h11v1"],
  check: "m4.5 12.5 5 5 10-11",
  arrowL: "M19 12H5m0 0 6 6m-6-6 6-6",
  arrowR: "M5 12h14m0 0-6-6m6 6-6 6",
  plus: "M12 5v14M5 12h14",
  minus: "M5 12h14",
  fit: ["M9 4H4v5", "M15 4h5v5", "M9 20H4v-5", "M15 20h5v-5"],
  key: ["M14 11a4.5 4.5 0 1 0-4.4 4.5L11 14h2v-2h2l1-1Z", "m11 14 4 4h3v-3"],
  link: ["M10 14a5 5 0 0 0 7.5.5l2-2a5 5 0 0 0-7-7l-1 1", "M14 10a5 5 0 0 0-7.5-.5l-2 2a5 5 0 0 0 7 7l1-1"],
  mail: ["M3 5.5h18v13H3z", "m3 7 9 6.5L21 7"],
  doc: ["M6 2.5h8L19 8v13.5H6z", "M13 3v5h5"],
  grid: ["M4 4h7v7H4z", "M13 4h7v7h-7z", "M4 13h7v7H4z", "M13 13h7v7h-7z"],
  rows: ["M4 5h16M4 12h16M4 19h16"],
  eye: ["M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z", "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z"],
};

function Ic({ name, size = 16, className = "" }) {
  return <AppIcon d={ICONS[name]} size={size} className={className}></AppIcon>;
}

/* ---------- brand ---------- */
/* dbd logo (uploads/dbd.svg) tinted with the app accent via currentColor */
function BrandMark({ size = 26 }) {
  return (
    <span
      aria-hidden="true"
      style={{ display: "inline-flex", width: size, height: size, flex: "none", color: "var(--accent)" }}
      dangerouslySetInnerHTML={{ __html: window.DBD_LOGO_SVG }}
    ></span>
  );
}

function Avatar({ user, size = 32 }) {
  return (
    <div
      className="flex items-center justify-center rounded-full font-display font-semibold select-none"
      style={{
        width: size, height: size, fontSize: size * 0.36,
        background: "var(--accent-soft)", color: "var(--accent-2)",
        border: "1px solid var(--accent-line)",
      }}
      title={user.email}
    >{user.initials}</div>
  );
}

/* ---------- badges ---------- */
function EngineBadge({ engine }) {
  return <span className="ds-badge">{engine}</span>;
}

function VisBadge({ visibility }) {
  const pub = visibility === "public";
  return (
    <span className={"ds-badge" + (pub ? " ds-badge-accent" : "")}>
      <Ic name={pub ? "globe" : "lock"} size={11}></Ic>
      {visibility}
    </span>
  );
}

/* ---------- schema snapshot thumbnail ----------
   Zoomed-out map of the design: one tinted tile per schema with its
   entity count and a tidy grid of entity blocks. No edges — reads
   cleanly at gallery size. Hues match the diagram's schema tints. */
const SN_HUES = [245, 160, 70, 330, 200, 25, 120, 285];

function SchemaSnapshotThumb({ project, className = "" }) {
  const layout = React.useMemo(() => {
    const list = (project.schemaList || [{ name: "public", tables: project.tables }]).slice();
    const CELL_W = 26, CELL_H = 14, GAP = 5, PAD = 10, LABEL = 15, TILE_GAP = 18;
    const tiles = list.map((s, i) => {
      const n = Math.max(1, s.tables);
      const ncols = Math.max(1, Math.min(7, Math.round(Math.sqrt(n * 1.7))));
      const nrows = Math.ceil(n / ncols);
      return {
        name: s.name, n, ncols, hue: SN_HUES[i % SN_HUES.length],
        w: ncols * CELL_W + (ncols - 1) * GAP + PAD * 2,
        h: LABEL + nrows * CELL_H + (nrows - 1) * GAP + PAD * 2,
        x: 0, y: 0,
      };
    });
    const totalArea = tiles.reduce((a, t) => a + (t.w + TILE_GAP) * (t.h + TILE_GAP), 0);
    const maxW = Math.max(Math.sqrt(totalArea * 2.6), ...tiles.map((t) => t.w));
    let x = 0, y = 0, rowH = 0;
    for (const t of tiles) {
      if (x > 0 && x + t.w > maxW) { x = 0; y += rowH + TILE_GAP; rowH = 0; }
      t.x = x; t.y = y;
      x += t.w + TILE_GAP;
      rowH = Math.max(rowH, t.h);
    }
    const W = Math.max(...tiles.map((t) => t.x + t.w));
    const H = y + rowH;
    return { tiles, W, H, CELL_W, CELL_H, GAP, PAD, LABEL };
  }, [project]);

  const { tiles, W, H, CELL_W, CELL_H, GAP, PAD, LABEL } = layout;
  const M = 16;
  return (
    <svg viewBox={`${-M} ${-M} ${W + M * 2} ${H + M * 2}`} className={className}
      preserveAspectRatio="xMidYMid meet" aria-hidden="true">
      {tiles.map((t) => (
        <g key={t.name} style={{ "--cl-h": t.hue }}>
          <rect className="sn-tile" x={t.x} y={t.y} width={t.w} height={t.h} rx="6"
            strokeDasharray="5 4" strokeWidth="1.2"></rect>
          <text className="sn-label" x={t.x + PAD} y={t.y + PAD + 6} fontSize="10">{t.name} · {t.n}</text>
          {Array.from({ length: t.n }).map((_, i) => {
            const cx = t.x + PAD + (i % t.ncols) * (CELL_W + GAP);
            const cy = t.y + PAD + LABEL + Math.floor(i / t.ncols) * (CELL_H + GAP);
            return (
              <g key={i}>
                <rect className="sn-cell" x={cx} y={cy} width={CELL_W} height={CELL_H} rx="2.5" strokeWidth="1"></rect>
                <rect className="sn-bar" x={cx} y={cy} width={CELL_W} height="4.5" rx="2.5"></rect>
              </g>
            );
          })}
        </g>
      ))}
    </svg>
  );
}

/* ---------- app header ---------- */
/* crumbs: [{label, href?}] — last crumb rendered as current page */
function AppHeader({ prefs, setPref, crumbs = [], right = null, user }) {
  return (
    <header
      className="flex items-center gap-3 border-b border-line bg-surface px-4 lg:px-6"
      style={{ height: "var(--header-h)", flex: "none" }}
    >
      <a href="Projects.html" className="flex items-center gap-2.5" title="Your designs">
        <BrandMark size={prefs.look === "tool" ? 22 : 26}></BrandMark>
        <span className="font-display text-base font-700 font-semibold tracking-tight">dbd</span>
        <span className="ds-badge" style={{ transform: "translateY(1px)" }}>designs</span>
      </a>

      {crumbs.length > 0 && (
        <nav className="ml-2 hidden items-center gap-1.5 text-sm text-muted sm:flex">
          {crumbs.map((c, i) => (
            <React.Fragment key={i}>
              <span className="text-faint">/</span>
              {c.href
                ? <a className="hover:text-fg" href={c.href}>{c.label}</a>
                : c.onClick
                  ? <button className="hover:text-fg" onClick={c.onClick}>{c.label}</button>
                  : <span className="font-medium text-fg">{c.label}</span>}
            </React.Fragment>
          ))}
        </nav>
      )}

      <div className="ml-auto flex items-center gap-2">
        {right}
        <button
          className="ds-iconbtn"
          title={prefs.theme === "light" ? "Switch to dark" : "Switch to light"}
          onClick={() => setPref("theme", prefs.theme === "light" ? "dark" : "light")}
        >
          <Ic name={prefs.theme === "light" ? "moon" : "sun"} size={17}></Ic>
        </button>
        <Avatar user={user} size={prefs.look === "tool" ? 28 : 32}></Avatar>
      </div>
    </header>
  );
}

Object.assign(window, {
  useAppPrefs, AppHeader, BrandMark, Avatar, EngineBadge, VisBadge,
  SchemaSnapshotThumb, AppIcon, Ic,
});
