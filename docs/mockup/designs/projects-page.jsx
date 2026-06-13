/* ============================================================
   dbd designs — Projects landing page
   Three layout variations (Tweaks): cards / rows / gallery
   Publish flow modal: dbd push (CLI) or upload .dbml
   ============================================================ */

const APP = window.DBD_APP;

/* ---------- publish modal ---------- */
function PublishModal({ onClose }) {
  const [tab, setTab] = React.useState("cli");
  const [copied, setCopied] = React.useState(false);
  const [drag, setDrag] = React.useState(false);
  const [uploaded, setUploaded] = React.useState(false);

  React.useEffect(() => {
    const onKey = (e) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const copy = () => {
    try { navigator.clipboard.writeText("dbd push"); } catch (e) {}
    setCopied(true);
    setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="ds-overlay" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="ds-modal" data-screen-label="Publish flow">
        <div className="flex items-center gap-3 border-b border-line px-6 py-4">
          <h2 className="font-display text-lg font-semibold tracking-tight">Publish a design</h2>
          <button className="ds-iconbtn ml-auto" onClick={onClose} title="Close"><Ic name="x" size={16}></Ic></button>
        </div>

        {/* tabs */}
        <div className="flex gap-1 border-b border-line px-6 pt-3">
          {[["cli", "terminal", "From the CLI"], ["upload", "upload", "Upload DBML"]].map(([id, icon, label]) => (
            <button key={id}
              className={"flex items-center gap-2 rounded-t-md px-4 py-2.5 text-sm font-medium " +
                (tab === id ? "border border-b-0 border-line bg-surface text-fg" : "text-muted hover:text-fg")}
              style={tab === id ? { marginBottom: "-1px" } : {}}
              onClick={() => { setTab(id); setUploaded(false); }}>
              <Ic name={icon} size={14}></Ic>{label}
            </button>
          ))}
        </div>

        {tab === "cli" ? (
          <div className="flex flex-col gap-4 p-6">
            <p className="text-sm text-muted">
              Run <code className="font-mono text-xs text-fg">dbd push</code> in any dbd project.
              It generates DBML from your DDL files and publishes it here.
            </p>
            <div className="ds-term">
              {APP.publish.cli.map((l, i) => (
                <div key={i} className={"t-" + l.type}>{l.text}</div>
              ))}
            </div>
            <div className="flex items-center gap-2">
              <button className="ds-btn" onClick={copy}>
                <Ic name={copied ? "check" : "copy"} size={14}></Ic>{copied ? "Copied" : "Copy command"}
              </button>
              <span className="text-xs text-faint">Pushes are versioned — every publish keeps history.</span>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-4 p-6">
            {!uploaded ? (
              <button
                className="flex flex-col items-center justify-center gap-3 rounded-app-lg border-2 border-dashed px-6 py-12 text-center"
                style={{ borderColor: drag ? "var(--accent)" : "var(--line)", background: drag ? "var(--accent-soft)" : "var(--bg-deep)" }}
                onDragOver={(e) => { e.preventDefault(); setDrag(true); }}
                onDragLeave={() => setDrag(false)}
                onDrop={(e) => { e.preventDefault(); setDrag(false); setUploaded(true); }}
                onClick={() => setUploaded(true)}>
                <Ic name="doc" size={28} className="text-faint"></Ic>
                <div className="text-sm font-medium">{APP.publish.uploadHint}</div>
                <div className="font-mono text-xs text-faint">.dbml · max 5 MB</div>
              </button>
            ) : (
              <div className="flex flex-col gap-4">
                <div className="flex items-center gap-3 rounded-app border border-line bg-bg-deep px-4 py-3">
                  <Ic name="doc" size={18} className="text-faint"></Ic>
                  <div className="min-w-0">
                    <div className="truncate font-mono text-xs">sensei.dbml</div>
                    <div className="text-xs text-faint">66 tables · 40 enums · 62 refs parsed</div>
                  </div>
                  <span className="ds-badge ds-badge-accent ml-auto"><Ic name="check" size={11}></Ic> valid</span>
                </div>
                <a href="Project View.html" className="ds-btn ds-btn-primary justify-center py-3">
                  Publish as “sensei” <Ic name="arrowR" size={15}></Ic>
                </a>
              </div>
            )}
            <p className="text-xs text-faint">
              Uploads create the same versioned design as <code className="font-mono">dbd push</code> — just without the terminal.
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

/* ---------- shared project meta line ---------- */
function ProjectMeta({ p }) {
  const pl = (n, w) => n + " " + w + (n === 1 ? "" : "s");
  return (
    <div className="flex flex-wrap items-center gap-1.5">
      <EngineBadge engine={p.engine}></EngineBadge>
      <span className="ds-badge">{pl(p.schemas, "schema")}</span>
      <span className="ds-badge">{pl(p.tables, "table")}</span>
      {p.enums > 0 && <span className="ds-badge">{pl(p.enums, "enum")}</span>}
    </div>
  );
}

/* ---------- layout A: cards ---------- */
function CardsLayout({ projects }) {
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      {projects.map((p) => (
        <a key={p.id} href="Project View.html" className="ds-card p-5" data-comment-anchor={"project-" + p.id}>
          <div className="flex items-start gap-3">
            <div className="flex h-10 w-10 flex-none items-center justify-center rounded-app border border-line bg-bg-deep font-display text-sm font-semibold text-accent-2">
              {p.name.slice(0, 2)}
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="truncate font-display text-base font-semibold">{p.name}</span>
                <span className="font-mono text-xs text-faint">{p.version}</span>
              </div>
              <p className="mt-0.5 truncate text-sm text-muted">{p.desc}</p>
            </div>
          </div>
          <div className="mt-4"><ProjectMeta p={p}></ProjectMeta></div>
          <div className="mt-4 flex items-center gap-2 border-t border-line-soft pt-3 text-xs text-faint">
            <VisBadge visibility={p.visibility}></VisBadge>
            <span className="ml-auto whitespace-nowrap">{p.via} · {p.updated}</span>
          </div>
        </a>
      ))}
    </div>
  );
}

/* ---------- layout B: rows ---------- */
function RowsLayout({ projects }) {
  return (
    <div className="ds-card overflow-hidden">
      <div className="grid grid-cols-[1fr_auto] items-center gap-3 border-b border-line bg-surface-2 px-5 py-2.5 font-mono text-xs uppercase tracking-wider text-faint sm:grid-cols-[2fr_1fr_1fr_auto]">
        <span>Design</span>
        <span className="hidden sm:block">Entities</span>
        <span className="hidden sm:block">Published</span>
        <span>Access</span>
      </div>
      {projects.map((p, i) => (
        <a key={p.id} href="Project View.html"
          className={"grid grid-cols-[1fr_auto] items-center gap-3 px-5 py-3.5 hover:bg-surface-2 sm:grid-cols-[2fr_1fr_1fr_auto] " + (i > 0 ? "border-t border-line-soft" : "")}
          data-comment-anchor={"project-row-" + p.id}>
          <div className="flex min-w-0 items-center gap-3">
            <div className="flex h-8 w-8 flex-none items-center justify-center rounded-app-sm border border-line bg-bg-deep font-display text-xs font-semibold text-accent-2">
              {p.name.slice(0, 2)}
            </div>
            <div className="min-w-0">
              <div className="flex items-center gap-2">
                <span className="truncate text-sm font-semibold">{p.name}</span>
                <span className="ds-badge hidden md:inline-flex">{p.engine}</span>
              </div>
              <div className="truncate text-xs text-muted">{p.desc}</div>
            </div>
          </div>
          <div className="hidden font-mono text-xs text-muted sm:block">
            {p.tables} tables · {p.schemas} schemas
          </div>
          <div className="hidden text-xs text-muted sm:block">{p.updated} <span className="text-faint">via {p.via}</span></div>
          <VisBadge visibility={p.visibility}></VisBadge>
        </a>
      ))}
    </div>
  );
}

/* ---------- layout C: gallery (thumbnail-first) ---------- */
function GalleryLayout({ projects }) {
  return (
    <div className="grid gap-5 sm:grid-cols-2">
      {projects.map((p) => (
        <a key={p.id} href="Project View.html" className="ds-card overflow-hidden" data-comment-anchor={"project-thumb-" + p.id}>
          <div className="dg-dots border-b border-line" style={{ background: "var(--bg-deep)" }}>
            <SchemaSnapshotThumb project={p} className="block h-36 w-full"></SchemaSnapshotThumb>
          </div>
          <div className="p-5">
            <div className="flex items-center gap-2">
              <span className="font-display text-base font-semibold">{p.name}</span>
              <span className="font-mono text-xs text-faint">{p.version}</span>
              <span className="ml-auto"><VisBadge visibility={p.visibility}></VisBadge></span>
            </div>
            <p className="mt-1 text-sm text-muted">{p.desc}</p>
            <div className="mt-3.5 flex items-center gap-1.5">
              <ProjectMeta p={p}></ProjectMeta>
              <span className="ml-auto whitespace-nowrap text-xs text-faint">{p.updated}</span>
            </div>
          </div>
        </a>
      ))}
    </div>
  );
}

/* ---------- page ---------- */
function ProjectsPage() {
  const [prefs, setPref] = useAppPrefs();
  const [values, setTweak] = useTweaks({ layout: "cards" });
  const [publishOpen, setPublishOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");

  const projects = APP.projects.filter((p) =>
    (p.name + " " + p.desc).toLowerCase().includes(query.toLowerCase())
  );

  const Layout = { cards: CardsLayout, rows: RowsLayout, gallery: GalleryLayout }[values.layout] || CardsLayout;

  return (
    <div className="flex min-h-screen flex-col" data-screen-label="Projects landing">
      <AppHeader
        prefs={prefs} setPref={setPref} user={APP.user}
        crumbs={[{ label: "Designs" }]}
        right={
          <button className="ds-btn ds-btn-primary" onClick={() => setPublishOpen(true)}>
            <Ic name="upload" size={14}></Ic> Publish
          </button>
        }
      ></AppHeader>

      <main className="mx-auto w-full max-w-5xl flex-1 px-5 py-8 lg:py-10">
        <div className="flex flex-wrap items-end gap-4">
          <div>
            <h1 className="font-display text-h2 font-semibold tracking-tight">Your designs</h1>
            <p className="mt-1 text-sm text-muted">
              {APP.projects.length} published designs · signed in as <span className="font-mono text-xs">{APP.user.email}</span>
            </p>
          </div>
          <div className="ml-auto flex items-center gap-2">
            {/* layout switcher mirrors the Tweak */}
            <div className="flex rounded-app border border-line bg-surface p-0.5">
              {[["cards", "grid"], ["rows", "rows"], ["gallery", "eye"]].map(([id, icon]) => (
                <button key={id} title={id}
                  className="ds-iconbtn"
                  style={values.layout === id ? { background: "var(--accent-soft)", color: "var(--accent-2)" } : {}}
                  onClick={() => setTweak("layout", id)}>
                  <Ic name={icon} size={15}></Ic>
                </button>
              ))}
            </div>
            <div className="relative hidden sm:block">
              <span className="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-faint"><Ic name="search" size={14}></Ic></span>
              <input className="ds-input w-56" style={{ paddingLeft: "2.1rem" }} placeholder="Filter designs…"
                value={query} onChange={(e) => setQuery(e.target.value)} />
            </div>
          </div>
        </div>

        <div className="mt-7">
          {projects.length > 0
            ? <Layout projects={projects}></Layout>
            : <div className="ds-card flex flex-col items-center gap-2 py-16 text-center">
                <span className="text-sm text-muted">No designs match “{query}”.</span>
                <button className="text-xs text-accent-2" onClick={() => setQuery("")}>Clear filter</button>
              </div>}
        </div>

        <p className="mt-8 text-center font-mono text-xs text-faint">
          publish from any project with <span className="text-muted">$ dbd push</span>
        </p>
      </main>

      {publishOpen && <PublishModal onClose={() => setPublishOpen(false)}></PublishModal>}

      <TweaksPanel title="Tweaks">
        <TweakSection title="Look">
          <TweakRadio label="Chrome" value={prefs.look} options={["paper", "tool"]} onChange={(v) => setPref("look", v)}></TweakRadio>
          <TweakRadio label="Theme" value={prefs.theme} options={["light", "dark"]} onChange={(v) => setPref("theme", v)}></TweakRadio>
        </TweakSection>
        <TweakSection title="Landing layout">
          <TweakRadio label="Layout" value={values.layout} options={["cards", "rows", "gallery"]} onChange={(v) => setTweak("layout", v)}></TweakRadio>
        </TweakSection>
      </TweaksPanel>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<ProjectsPage></ProjectsPage>);
