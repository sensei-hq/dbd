/* ============================================================
   dbd designs — Project view
   Routes inside the page:
     { kind: "project" }                    → project root (header + Diagram / Entities tabs)
     { kind: "table" | "enum", key }        → entity page (Details / Diagram tabs)
   ============================================================ */

const PV_APP = window.DBD_APP;
const PV_DATA = window.DBD_SCHEMA;

/* ---------- sidebar ---------- */
function SchemaTree({ data, route, onPick, query, setQuery }) {
  const [open, setOpen] = React.useState(() => {
    const o = {};
    for (const s of data.schemas) o[s.name] = true;
    return o;
  });
  const q = query.trim().toLowerCase();
  const selected = route.kind !== "project" ? route.key : null;

  const groups = data.schemas.map((s) => {
    let tables = data.tables.filter((t) => t.schema === s.name);
    let enums = data.enums.filter((e) => e.schema === s.name);
    if (q) {
      tables = tables.filter((t) => t.name.toLowerCase().includes(q));
      enums = enums.filter((e) => e.name.toLowerCase().includes(q));
    }
    return { schema: s, tables, enums };
  }).filter((g) => !q || g.tables.length || g.enums.length);

  return (
    <div className="flex min-h-0 flex-1 flex-col" data-screen-label="Schema tree sidebar">
      <div className="relative p-3 pb-2">
        <span className="pointer-events-none absolute left-6 top-1/2 -translate-y-1/2 text-faint" style={{ marginTop: "1px" }}>
          <Ic name="search" size={13}></Ic>
        </span>
        <input className="ds-input py-2 text-sm" style={{ paddingLeft: "2rem" }}
          placeholder="Find an entity…" value={query} onChange={(e) => setQuery(e.target.value)} />
      </div>
      <div className="ds-scroll min-h-0 flex-1 overflow-y-auto px-2 pb-4">
        {groups.map(({ schema, tables, enums }) => (
          <div key={schema.name} className="mt-1">
            <button className="tree-group-head" onClick={() => setOpen({ ...open, [schema.name]: !open[schema.name] })}>
              <Ic name={open[schema.name] || q ? "chevD" : "chevR"} size={12} className="text-faint"></Ic>
              <span>{schema.name}</span>
              <span className="ml-auto font-normal text-faint">{tables.length}</span>
            </button>
            {(open[schema.name] || q) && (
              <div className="flex flex-col">
                {tables.map((t) => {
                  const key = t.schema + "." + t.name;
                  return (
                    <button key={key} className={"tree-item" + (selected === key ? " sel" : "")}
                      onClick={() => onPick({ kind: "table", key })}>
                      <Ic name="table" size={12} className="flex-none opacity-60"></Ic>
                      <span className="ti-name">{t.name}</span>
                    </button>
                  );
                })}
                {enums.length > 0 && (
                  <EnumGroup enums={enums} selected={selected} onPick={onPick} forceOpen={!!q}></EnumGroup>
                )}
              </div>
            )}
          </div>
        ))}
        {groups.length === 0 && (
          <p className="px-3 py-6 text-center text-xs text-faint">Nothing matches “{query}”.</p>
        )}
      </div>
    </div>
  );
}

function EnumGroup({ enums, selected, onPick, forceOpen }) {
  const [open, setOpen] = React.useState(false);
  const show = open || forceOpen;
  return (
    <div>
      <button className="tree-group-head" style={{ paddingLeft: "26px", color: "var(--muted)", fontWeight: 500 }}
        onClick={() => setOpen(!open)}>
        <Ic name={show ? "chevD" : "chevR"} size={11} className="text-faint"></Ic>
        <span>enums</span>
        <span className="ml-auto font-normal text-faint">{enums.length}</span>
      </button>
      {show && enums.map((e) => {
        const key = e.schema + "." + e.name;
        return (
          <button key={key} className={"tree-item" + (selected === key ? " sel" : "")}
            style={{ paddingLeft: "42px" }}
            onClick={() => onPick({ kind: "enum", key })}>
            <Ic name="enumI" size={12} className="flex-none opacity-60"></Ic>
            <span className="ti-name">{e.name}</span>
          </button>
        );
      })}
    </div>
  );
}

/* ---------- project root view ---------- */
function ProjectRoot({ data, project, values, onNav, diagramApi }) {
  const [tab, setTab] = React.useState("diagram");
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col" data-screen-label="Project root view">
      <div className="border-b border-line bg-surface">
        <div className="flex flex-wrap items-start gap-x-6 gap-y-2 px-6 pb-3 pt-5">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-3">
              <h1 className="font-display text-h3 font-semibold tracking-tight">{data.project.name}</h1>
              <span className="ds-badge ds-badge-accent">{data.project.db}</span>
              <span className="ds-badge">{project.version}</span>
            </div>
            <p className="mt-1 max-w-2xl text-sm text-muted">{data.project.note}</p>
          </div>
          <div className="ml-auto hidden whitespace-nowrap pt-1 font-mono text-xs text-faint md:block">
            {data.tables.length} tables · {data.enums.length} enums · {data.refs.length} refs
            <span className="text-line"> | </span>
            {project.via} · {project.updated}
          </div>
        </div>
        <DsTabs
          tabs={[["diagram", "Diagram", "grid"], ["entities", "Entities", "rows"]]}
          active={tab} onChange={setTab}
        ></DsTabs>
      </div>

      {tab === "diagram" ? (
        <div className="relative min-h-0 min-w-0 flex-1" style={{ background: "var(--bg-deep)" }}>
          <SchemaDiagram
            data={data}
            density={values.density}
            lineStyle={values.lines}
            arrange={values.arrange}
            tint={values.tint}
            selected={null}
            onSelect={(key) => key && onNav({ kind: "table", key })}
            apiRef={diagramApi}
          ></SchemaDiagram>
          <div className="pointer-events-none absolute bottom-5 left-1/2 z-20 flex -translate-x-1/2 items-center gap-3 whitespace-nowrap rounded-full border border-line bg-surface px-4 py-2 text-xs text-faint shadow-sm">
            drag to pan · ctrl+scroll to zoom · click a table to open it
          </div>
        </div>
      ) : (
        <EntitiesList data={data} onNav={onNav}></EntitiesList>
      )}
    </div>
  );
}

/* ---------- page ---------- */
function ProjectViewPage() {
  const [prefs, setPref] = useAppPrefs();
  const [values, setTweak] = useTweaks({
    density: "keys",        // names | keys | full
    lines: "curved",        // curved | orthogonal
    arrange: "untangle",    // untangle (minimize crossings) | a-z
    tint: true,             // light shade per schema
  });
  const [route, setRoute] = React.useState({ kind: "project" });
  const [query, setQuery] = React.useState("");
  const [copied, setCopied] = React.useState(false);
  const diagramApi = React.useRef(null);

  const project = PV_APP.projects[0]; // sensei

  const share = () => {
    try { navigator.clipboard.writeText("https://dbd.dev/" + PV_APP.user.handle + "/sensei"); } catch (e) {}
    setCopied(true);
    setTimeout(() => setCopied(false), 1600);
  };

  return (
    <div className="flex h-screen flex-col overflow-hidden" data-screen-label="Project view">
      <AppHeader
        prefs={prefs} setPref={setPref} user={PV_APP.user}
        crumbs={[
          { label: "Designs", href: "Projects.html" },
          route.kind === "project"
            ? { label: project.name }
            : { label: project.name, onClick: () => setRoute({ kind: "project" }) },
          ...(route.kind !== "project" ? [{ label: route.key.split(".")[1] }] : []),
        ]}
        right={
          <button className="ds-btn" onClick={share}>
            <Ic name={copied ? "check" : "link"} size={14}></Ic>{copied ? "Copied" : "Share"}
          </button>
        }
      ></AppHeader>

      <div className="relative flex min-h-0 flex-1">
        {/* sidebar */}
        <aside className="flex flex-none flex-col border-r border-line bg-surface" style={{ width: "var(--sb-w)" }}>
          <button
            className={"border-b border-line-soft px-4 py-3 text-left transition-colors hover:bg-surface-2" +
              (route.kind === "project" ? " bg-accent-soft" : "")}
            onClick={() => setRoute({ kind: "project" })}
            title="Project overview">
            <span
              className={"font-display text-sm font-semibold uppercase" + (route.kind === "project" ? " text-accent-2" : "")}
              style={{ letterSpacing: "0.13em" }}
            >{project.name}</span>
          </button>
          <SchemaTree data={PV_DATA} route={route} onPick={setRoute} query={query} setQuery={setQuery}></SchemaTree>
        </aside>

        {/* main */}
        {route.kind === "project"
          ? <ProjectRoot data={PV_DATA} project={project} values={values} onNav={setRoute} diagramApi={diagramApi}></ProjectRoot>
          : <EntityView data={PV_DATA} entity={route} onNav={setRoute}></EntityView>}
      </div>

      <TweaksPanel title="Tweaks">
        <TweakSection title="Look">
          <TweakRadio label="Chrome" value={prefs.look} options={["paper", "tool"]} onChange={(v) => setPref("look", v)}></TweakRadio>
          <TweakRadio label="Theme" value={prefs.theme} options={["light", "dark"]} onChange={(v) => setPref("theme", v)}></TweakRadio>
        </TweakSection>
        <TweakSection title="Overview diagram">
          <TweakRadio label="Card density" value={values.density} options={["names", "keys", "full"]} onChange={(v) => setTweak("density", v)}></TweakRadio>
          <TweakRadio label="Lines" value={values.lines} options={["curved", "orthogonal"]} onChange={(v) => setTweak("lines", v)}></TweakRadio>
          <TweakRadio label="Arrange" value={values.arrange} options={["untangle", "a-z"]} onChange={(v) => setTweak("arrange", v)}></TweakRadio>
          <TweakToggle label="Schema tint" value={values.tint} onChange={(v) => setTweak("tint", v)}></TweakToggle>
        </TweakSection>
      </TweaksPanel>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById("root")).render(<ProjectViewPage></ProjectViewPage>);
