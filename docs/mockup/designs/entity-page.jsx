/* ============================================================
   dbd designs — Entity page + entities list
   Exports (window): EntityView, EntitiesList, Md, DsTabs
   ============================================================ */

/* ---------- tiny markdown renderer (notes are plain text +
   bullets + indented code + `inline code` + **bold**) ---------- */
function mdInline(text, keyBase) {
  const parts = [];
  const re = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let last = 0, mm, k = 0;
  while ((mm = re.exec(text))) {
    if (mm.index > last) parts.push(text.slice(last, mm.index));
    const tok = mm[0];
    if (tok.startsWith("`")) {
      parts.push(<code key={keyBase + "-c" + k++} className="rounded bg-code-bg px-1 font-mono text-[0.85em] text-accent-2">{tok.slice(1, -1)}</code>);
    } else {
      parts.push(<strong key={keyBase + "-b" + k++}>{tok.slice(2, -2)}</strong>);
    }
    last = mm.index + tok.length;
  }
  if (last < text.length) parts.push(text.slice(last));
  return parts;
}

function Md({ src, className = "" }) {
  if (!src) return null;
  const blocks = [];
  const lines = src.split("\n");
  let i = 0, k = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) { i++; continue; }
    // indented code block (2+ leading spaces)
    if (/^\s{2,}\S/.test(line)) {
      const code = [];
      while (i < lines.length && (/^\s{2,}\S/.test(lines[i]) || !lines[i].trim())) {
        if (!lines[i].trim() && !(i + 1 < lines.length && /^\s{2,}\S/.test(lines[i + 1]))) break;
        code.push(lines[i].replace(/^\s{2}/, ""));
        i++;
      }
      blocks.push(<pre key={"pre" + k++} className="ds-scroll overflow-x-auto rounded-app border border-line bg-code-bg px-3.5 py-2.5 font-mono text-xs leading-relaxed text-muted">{code.join("\n")}</pre>);
      continue;
    }
    // bullet list
    if (/^[-•]\s/.test(line.trim())) {
      const items = [];
      while (i < lines.length && /^[-•]\s/.test(lines[i].trim())) {
        items.push(lines[i].trim().replace(/^[-•]\s/, ""));
        i++;
      }
      blocks.push(
        <ul key={"ul" + k++} className="flex list-disc flex-col gap-1 pl-5">
          {items.map((it, j) => <li key={j}>{mdInline(it, "ul" + k + j)}</li>)}
        </ul>
      );
      continue;
    }
    // paragraph: join until blank / bullet / code
    const para = [];
    while (i < lines.length && lines[i].trim() && !/^[-•]\s/.test(lines[i].trim()) && !/^\s{2,}\S/.test(lines[i])) {
      para.push(lines[i].trim());
      i++;
    }
    blocks.push(<p key={"p" + k++}>{mdInline(para.join(" "), "p" + k)}</p>);
  }
  return <div className={"flex flex-col gap-2.5 text-sm leading-relaxed text-muted " + className}>{blocks}</div>;
}

/* ---------- tabs ---------- */
function DsTabs({ tabs, active, onChange }) {
  return (
    <div className="flex gap-1 border-b border-line px-6">
      {tabs.map(([id, label, icon]) => (
        <button key={id}
          className={"flex items-center gap-2 border-b-2 px-3.5 py-2.5 text-sm font-medium transition-colors " +
            (active === id ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")}
          onClick={() => onChange(id)}>
          {icon && <Ic name={icon} size={14}></Ic>}{label}
        </button>
      ))}
    </div>
  );
}

/* ---------- helpers ---------- */
function typeSize(type) {
  const mm = type.match(/\(([^)]+)\)/);
  if (mm) return mm[1];
  if (type.endsWith("[]")) return "[]";
  return "—";
}
function baseType(type) {
  return type.replace(/\([^)]*\)/, "").replace(/\[\]$/, "");
}
function entPropBadges(c) {
  const out = [];
  if (c.pk) out.push(["PK", "pk"]);
  if (c.fk) out.push(["FK", "fk"]);
  if (c.nn) out.push(["NN", ""]);
  if (c.uq) out.push(["UQ", ""]);
  if (c.en) out.push(["ENUM", ""]);
  return out;
}

/* ============================================================
   ENTITIES LIST (project root tab) — md comments embedded
   ============================================================ */
function EntitiesList({ data, onNav }) {
  return (
    <div className="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg">
      <div className="mx-auto max-w-5xl px-6 pb-6">
        <table className="w-full table-fixed border-collapse text-left">
          <colgroup>
            <col style={{ width: "200px" }}></col>
            <col style={{ width: "58px" }}></col>
            <col style={{ width: "58px" }}></col>
            <col></col>
          </colgroup>
          <thead>
            <tr className="font-mono text-xs uppercase tracking-wider text-faint">
              <th className="ds-th py-3 pr-4 font-medium">Entity</th>
              <th className="ds-th py-3 pr-4 font-medium">Cols</th>
              <th className="ds-th py-3 pr-4 font-medium">Refs</th>
              <th className="ds-th py-3 font-medium">Comment</th>
            </tr>
          </thead>
          <tbody>
            {data.tables.map((t) => {
              const key = t.schema + "." + t.name;
              const refCount = data.refs.filter((r) =>
                (r.from.s === t.schema && r.from.t === t.name) || (r.to.s === t.schema && r.to.t === t.name)
              ).length;
              return (
                <tr key={key} className="group cursor-pointer border-b border-line-soft align-top hover:bg-surface-2"
                  onClick={() => onNav({ kind: "table", key })}>
                  <td className="py-3.5 pr-4">
                    <div className="font-mono text-xs text-faint" style={{ overflowWrap: "anywhere" }}>{t.schema}.</div>
                    <div className="font-display text-sm font-semibold text-fg group-hover:text-accent-2" style={{ overflowWrap: "anywhere" }}>{t.name}</div>
                  </td>
                  <td className="py-3.5 pr-4 font-mono text-xs text-muted">{t.columns.length}</td>
                  <td className="py-3.5 pr-4 font-mono text-xs text-muted">{refCount || "—"}</td>
                  <td className="py-3.5 text-sm">
                    <Md src={t.noteMd} className="max-w-2xl"></Md>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/* ============================================================
   ENTITY-CENTRIC DIAGRAM — entity central, neighbors around
   ============================================================ */
const ED = { CARD_W: 248, ROW_H: 24, HEAD_H: 40, MORE_H: 22, PAD_B: 6, GAP_Y: 26, COL_GAP: 170 };

function edBuildCard(t, mode, focusCols) {
  let vis;
  if (mode === "center") vis = t.columns.slice(0, 16);
  else {
    const want = new Set(focusCols);
    vis = t.columns.filter((c) => c.pk || want.has(c.name)).slice(0, 8);
  }
  const more = t.columns.length - vis.length;
  const h = ED.HEAD_H + vis.length * ED.ROW_H + (more > 0 ? ED.MORE_H : 0) + (vis.length || more > 0 ? ED.PAD_B : 0);
  return { t, vis, more, w: ED.CARD_W, h };
}
function edAnchorY(card, top, colName) {
  const idx = card.vis.findIndex((c) => c.name === colName);
  return idx >= 0 ? top + ED.HEAD_H + idx * ED.ROW_H + ED.ROW_H / 2 : top + ED.HEAD_H / 2;
}

function EdCard({ card, x, y, center, onNav }) {
  return (
    <div className={"dg-card" + (center ? " sel" : "")} style={{ left: x, top: y, width: card.w, position: "absolute", cursor: center ? "default" : "pointer" }}
      onClick={() => !center && onNav({ kind: "table", key: card.t.schema + "." + card.t.name })}>
      <div className="dg-card-head">
        <Ic name="table" size={13} className="text-faint"></Ic>
        <span className="dg-card-title">{center ? card.t.name : card.t.schema + "." + card.t.name}</span>
      </div>
      {card.vis.map((c) => (
        <div key={c.name} className={"dg-row" + (c.pk || c.fk ? " iskey" : "")}>
          {c.pk ? <Ic name="key" size={11} className="dg-keyicon"></Ic>
            : c.fk ? <Ic name="link" size={11} className="dg-fkicon"></Ic>
            : <span style={{ width: 11, flex: "none" }}></span>}
          <span className="cname">{c.name}</span>
          <span className="ctype">{c.type}</span>
        </div>
      ))}
      {card.more > 0 && <div className="dg-more">+ {card.more} more</div>}
    </div>
  );
}

function EntityDiagram({ data, entityKey, onNav }) {
  const [schema, name] = entityKey.split(".");
  const t = data.tables.find((x) => x.schema === schema && x.name === name);
  const wrapRef = React.useRef(null);
  const [scale, setScale] = React.useState(1);

  const model = React.useMemo(() => {
    const selfRefs = [], neighbors = new Map();
    for (const r of data.refs) {
      const fk = r.from.s + "." + r.from.t, tk = r.to.s + "." + r.to.t;
      if (fk === entityKey && tk === entityKey) { selfRefs.push(r); continue; }
      if (fk === entityKey) {
        const nb = neighbors.get(tk) || { key: tk, in: [], out: [] };
        nb.out.push(r); neighbors.set(tk, nb);
      } else if (tk === entityKey) {
        const nb = neighbors.get(fk) || { key: fk, in: [], out: [] };
        nb.in.push(r); neighbors.set(fk, nb);
      }
    }
    // outgoing (and mixed) → right; pure incoming → left
    const right = [], left = [];
    for (const nb of neighbors.values()) (nb.out.length ? right : left).push(nb);
    const center = edBuildCard(t, "center");
    const mk = (nb) => {
      const [s2, n2] = nb.key.split(".");
      const t2 = data.tables.find((x) => x.schema === s2 && x.name === n2);
      const focus = [];
      nb.in.forEach((r) => focus.push(r.from.c));
      nb.out.forEach((r) => focus.push(r.to.c));
      return { nb, card: edBuildCard(t2, "nb", focus) };
    };
    const L = left.map(mk), R = right.map(mk);
    const stackH = (arr) => arr.reduce((a, x) => a + x.card.h + ED.GAP_Y, 0) - (arr.length ? ED.GAP_Y : 0);
    const H = Math.max(center.h, stackH(L), stackH(R), 120) + 20;
    const hasL = L.length > 0, hasR = R.length > 0 || selfRefs.length > 0;
    const cx = hasL ? ED.CARD_W + ED.COL_GAP : 0;
    const W = cx + ED.CARD_W + (hasR ? ED.COL_GAP + ED.CARD_W : 0) + (selfRefs.length ? 60 : 0) + 4;
    const cy = (H - center.h) / 2;
    let yy = (H - stackH(L)) / 2;
    const lPos = L.map((x) => { const p = { ...x, x: 0, y: yy }; yy += x.card.h + ED.GAP_Y; return p; });
    yy = (H - stackH(R)) / 2;
    const rPos = R.map((x) => { const p = { ...x, x: cx + ED.CARD_W + ED.COL_GAP, y: yy }; yy += x.card.h + ED.GAP_Y; return p; });

    const edges = [];
    for (const p of lPos) for (const r of p.nb.in)
      edges.push({ x1: p.x + ED.CARD_W, y1: edAnchorY(p.card, p.y, r.from.c), x2: cx, y2: edAnchorY(center, cy, r.to.c), out: false });
    for (const p of rPos) {
      for (const r of p.nb.out)
        edges.push({ x1: cx + ED.CARD_W, y1: edAnchorY(center, cy, r.from.c), x2: p.x, y2: edAnchorY(p.card, p.y, r.to.c), out: true });
      for (const r of p.nb.in)
        edges.push({ x1: p.x, y1: edAnchorY(p.card, p.y, r.from.c), x2: cx + ED.CARD_W, y2: edAnchorY(center, cy, r.to.c), out: false });
    }
    const loops = selfRefs.map((r) => ({
      x1: cx + ED.CARD_W, y1: edAnchorY(center, cy, r.from.c),
      x2: cx + ED.CARD_W, y2: edAnchorY(center, cy, r.to.c) + (r.from.c === r.to.c ? 16 : 0),
    }));
    return { center, cx, cy, lPos, rPos, edges, loops, W, H };
  }, [data, entityKey, t]);

  React.useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const update = () => setScale(Math.min(1, (el.clientWidth - 16) / model.W));
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [model]);

  const path = (e) => {
    const dx = Math.max(40, Math.min(150, Math.abs(e.x2 - e.x1) / 2));
    const s1 = e.x2 >= e.x1 ? 1 : -1;
    return `M ${e.x1} ${e.y1} C ${e.x1 + dx * s1} ${e.y1}, ${e.x2 - dx * s1} ${e.y2}, ${e.x2} ${e.y2}`;
  };

  const empty = model.lPos.length === 0 && model.rPos.length === 0 && model.loops.length === 0;

  return (
    <div ref={wrapRef} className="ds-scroll dg-dots min-h-0 min-w-0 flex-1 overflow-auto" style={{ background: "var(--bg-deep)" }}>
      {empty ? (
        <div className="flex h-full flex-col items-center justify-center gap-4 py-20 text-center">
          <div style={{ width: ED.CARD_W, height: model.center.h, position: "relative" }}>
            <EdCard card={model.center} x={0} y={0} center onNav={onNav}></EdCard>
          </div>
          <p className="text-sm text-faint">No relationships reference this table.</p>
        </div>
      ) : (
        <div className="flex justify-center px-6 py-8">
          <div style={{ width: model.W * scale, height: model.H * scale }}>
            <div style={{ width: model.W, height: model.H, transform: `scale(${scale})`, transformOrigin: "0 0", position: "relative" }}>
              <svg width={model.W} height={model.H} style={{ position: "absolute", inset: 0, pointerEvents: "none" }}>
                {model.edges.map((e, i) => (
                  <g key={i} className={"dg-edge" + (e.out ? " hl" : "")}>
                    <path d={path(e)}></path>
                    <circle className="dot-from" cx={e.x1} cy={e.y1} r="3.2"></circle>
                    <circle className="dot-to" cx={e.x2} cy={e.y2} r="3.2"></circle>
                  </g>
                ))}
                {model.loops.map((e, i) => (
                  <g key={"l" + i} className="dg-edge hl">
                    <path d={`M ${e.x1} ${e.y1} C ${e.x1 + 52} ${e.y1}, ${e.x2 + 52} ${e.y2}, ${e.x2} ${e.y2}`}></path>
                    <circle className="dot-from" cx={e.x1} cy={e.y1} r="3.2"></circle>
                    <circle className="dot-to" cx={e.x2} cy={e.y2} r="3.2"></circle>
                  </g>
                ))}
              </svg>
              {model.lPos.map((p) => <EdCard key={p.nb.key} card={p.card} x={p.x} y={p.y} onNav={onNav}></EdCard>)}
              <EdCard card={model.center} x={model.cx} y={model.cy} center onNav={onNav}></EdCard>
              {model.rPos.map((p) => <EdCard key={p.nb.key} card={p.card} x={p.x} y={p.y} onNav={onNav}></EdCard>)}
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ============================================================
   ENTITY VIEW — header + tabs (Details / Diagram)
   ============================================================ */
function EntityView({ data, entity, onNav }) {
  const [tab, setTab] = React.useState("details");
  const [schema, name] = entity.key.split(".");
  React.useEffect(() => { setTab("details"); }, [entity.key]);

  if (entity.kind === "enum") {
    const en = data.enums.find((e) => e.schema === schema && e.name === name);
    if (!en) return null;
    return (
      <div className="flex min-h-0 min-w-0 flex-1 flex-col" data-screen-label="Enum detail">
        <div className="border-b border-line bg-surface px-6 pb-4 pt-5">
          <div className="font-mono text-xs text-faint">{schema}.</div>
          <div className="flex items-center gap-3">
            <h1 className="font-display text-h3 font-semibold tracking-tight">{name}</h1>
            <span className="ds-badge">enum · {en.values.length} values</span>
          </div>
        </div>
        <div className="ds-scroll min-h-0 flex-1 overflow-y-auto px-6 py-6">
          <div className="flex max-w-2xl flex-wrap gap-1.5">
            {en.values.map((v) => <span key={v} className="ds-badge">{v}</span>)}
          </div>
        </div>
      </div>
    );
  }

  const t = data.tables.find((x) => x.schema === schema && x.name === name);
  if (!t) return null;
  const outRefs = data.refs.filter((r) => r.from.s === schema && r.from.t === name);
  const inRefs = data.refs.filter((r) => r.to.s === schema && r.to.t === name);
  const refsFor = (col) => outRefs.filter((r) => r.from.c === col);

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col" data-screen-label="Entity detail page">
      <div className="border-b border-line bg-surface">
        <div className="px-6 pb-3 pt-5">
          <div className="font-mono text-xs text-faint">{schema}.</div>
          <div className="flex flex-wrap items-center gap-3">
            <h1 className="font-display text-h3 font-semibold tracking-tight">{name}</h1>
            <span className="ds-badge">{t.columns.length} columns</span>
            {(inRefs.length > 0 || outRefs.length > 0) && (
              <span className="ds-badge">{outRefs.length} out · {inRefs.length} in</span>
            )}
          </div>
        </div>
        <DsTabs
          tabs={[["details", "Details", "rows"], ["diagram", "Diagram", "grid"]]}
          active={tab} onChange={setTab}
        ></DsTabs>
      </div>

      {tab === "details" ? (
        <div className="ds-scroll min-h-0 min-w-0 flex-1 overflow-y-auto bg-bg" data-screen-label="Entity details tab">
          <div className="mx-auto max-w-4xl px-6 py-6">
            {t.noteMd && (
              <section className="mb-8">
                <h2 className="text-label font-mono uppercase text-faint">Comment</h2>
                <div className="mt-3">
                  <Md src={t.noteMd}></Md>
                </div>
              </section>
            )}

            <section>
              <h2 className="text-label font-mono uppercase text-faint">Columns</h2>
              <table className="mt-3 w-full table-fixed border-collapse text-left">
                <colgroup>
                  <col style={{ width: "30%" }}></col>
                  <col style={{ width: "104px" }}></col>
                  <col style={{ width: "130px" }}></col>
                  <col style={{ width: "64px" }}></col>
                  <col></col>
                </colgroup>
                <thead>
                  <tr className="font-mono text-xs uppercase tracking-wider text-faint">
                    <th className="ds-th py-2.5 pr-4 font-medium">Column</th>
                    <th className="ds-th py-2.5 pr-4 font-medium">Props</th>
                    <th className="ds-th py-2.5 pr-4 font-medium">Type</th>
                    <th className="ds-th py-2.5 pr-4 font-medium">Size</th>
                    <th className="ds-th py-2.5 font-medium">Refs</th>
                  </tr>
                </thead>
                <tbody>
                  {t.columns.map((c) => {
                    const rr = refsFor(c.name);
                    return (
                      <tr key={c.name} className="border-b border-line-soft align-top">
                        <td className="py-2.5 pr-4">
                          <div className="font-mono text-xs font-semibold text-fg" style={{ overflowWrap: "anywhere" }}>{c.name}</div>
                          {c.note && <div className="mt-0.5 max-w-xs text-xs leading-snug text-muted">{c.note}</div>}
                          {c.def && <div className="mt-0.5 font-mono text-[0.66rem] text-faint">default: {c.def}</div>}
                        </td>
                        <td className="py-2.5 pr-4">
                          <div className="flex flex-wrap gap-1">
                            {entPropBadges(c).map(([label, cls]) => <span key={label} className={"col-badge " + cls}>{label}</span>)}
                          </div>
                        </td>
                        <td className="py-2.5 pr-4 font-mono text-xs text-muted" style={{ overflowWrap: "anywhere" }}>
                          {c.en
                            ? <button className="text-accent-2 hover:underline" onClick={() => onNav({ kind: "enum", key: schema + "." + baseType(c.type) })}>{baseType(c.type)}</button>
                            : baseType(c.type)}
                        </td>
                        <td className="whitespace-nowrap py-2.5 pr-4 font-mono text-xs text-faint">{typeSize(c.type)}</td>
                        <td className="py-2.5">
                          {rr.length
                            ? rr.map((r, i) => (
                                <button key={i} className="block max-w-full truncate font-mono text-xs text-accent-2 hover:underline"
                                  title={"\u2192 " + r.to.s + "." + r.to.t + "." + r.to.c}
                                  onClick={() => onNav({ kind: "table", key: r.to.s + "." + r.to.t })}>
                                  → {r.to.s}.{r.to.t}.{r.to.c}
                                </button>
                              ))
                            : <span className="font-mono text-xs text-faint">—</span>}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </section>

            {t.indexes.length > 0 && (
              <section className="mt-8">
                <h2 className="text-label font-mono uppercase text-faint">Indexes</h2>
                <div className="mt-3 flex flex-col gap-0">
                  {t.indexes.map((ix, i) => (
                    <div key={i} className="flex items-center gap-3 border-b border-line-soft py-2.5 last:border-0">
                      <span className="font-mono text-xs text-fg">{ix.def}</span>
                      {ix.unique && <span className="col-badge pk">UNIQUE</span>}
                      {ix.name && <span className="ml-auto font-mono text-[0.66rem] text-faint">{ix.name}</span>}
                    </div>
                  ))}
                </div>
              </section>
            )}

            {inRefs.length > 0 && (
              <section className="mt-8 pb-6">
                <h2 className="text-label font-mono uppercase text-faint">Referenced by</h2>
                <div className="mt-3 flex flex-col">
                  {inRefs.map((r, i) => (
                    <button key={i}
                      className="flex items-center gap-2 border-b border-line-soft py-2.5 text-left font-mono text-xs text-muted last:border-0 hover:text-fg"
                      onClick={() => onNav({ kind: "table", key: r.from.s + "." + r.from.t })}>
                      <span className="font-semibold text-accent-2">{r.from.s}.{r.from.t}</span>
                      <span className="text-faint">.{r.from.c}</span>
                      <Ic name="arrowR" size={12} className="text-faint"></Ic>
                      <span>{r.to.c}</span>
                      {r.action && <span className="col-badge ml-auto">{r.action}</span>}
                    </button>
                  ))}
                </div>
              </section>
            )}
          </div>
        </div>
      ) : (
        <EntityDiagram data={data} entityKey={entity.key} onNav={onNav}></EntityDiagram>
      )}
    </div>
  );
}

Object.assign(window, { EntityView, EntitiesList, Md, DsTabs });
