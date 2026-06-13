/* ============================================================
   dbd designs — SchemaDiagram (pan/zoom canvas, clusters,
   table cards, relationship edges)
   Exports (window): SchemaDiagram
   ============================================================ */

function diagramRelated(refs, key) {
  const set = new Set();
  for (const r of refs) {
    const f = r.from.s + "." + r.from.t;
    const t = r.to.s + "." + r.to.t;
    if (f === key) set.add(t);
    if (t === key) set.add(f);
  }
  return set;
}

function TableCard({ card, state, onSelect }) {
  const { t, vis, more } = card;
  const cls =
    "dg-card" +
    (vis.length === 0 && more <= 0 ? " headonly" : "") +
    (state === "sel" ? " sel" : state === "rel" ? " rel" : state === "dim" ? " dim" : "");
  return (
    <div
      className={cls}
      style={{ left: card.x, top: card.y, width: card.w, "--cl-h": card.hue }}
      onClick={(e) => { e.stopPropagation(); onSelect(t.schema + "." + t.name); }}
      data-comment-anchor={"dg-" + t.schema + "-" + t.name}
    >
      <div className="dg-card-head">
        <Ic name="table" size={13} className="text-faint"></Ic>
        <span className="dg-card-title">{t.name}</span>
        <span className="ml-auto font-mono text-faint" style={{ fontSize: "0.62rem" }}>{t.columns.length}</span>
      </div>
      {vis.map((c) => (
        <div key={c.name} className={"dg-row" + (c.pk || c.fk ? " iskey" : "")}>
          {c.pk
            ? <Ic name="key" size={11} className="dg-keyicon"></Ic>
            : c.fk
              ? <Ic name="link" size={11} className="dg-fkicon"></Ic>
              : <span style={{ width: 11, flex: "none" }}></span>}
          <span className="cname">{c.name}</span>
          <span className="ctype">{c.type}</span>
        </div>
      ))}
      {more > 0 && <div className="dg-more">+ {more} more</div>}
    </div>
  );
}

function SchemaDiagram({ data, density, lineStyle, arrange, tint, selected, onSelect, apiRef }) {
  const layout = React.useMemo(
    () => window.DiagramLayout.compute(data, density, arrange),
    [data, density, arrange]
  );
  const related = React.useMemo(
    () => (selected ? diagramRelated(data.refs, selected) : null),
    [data, selected]
  );

  const vpRef = React.useRef(null);
  const [view, setView] = React.useState({ scale: 0.5, tx: 0, ty: 0 });
  const viewRef = React.useRef(view);
  viewRef.current = view;
  const [panning, setPanning] = React.useState(false);

  const fit = React.useCallback(() => {
    const el = vpRef.current;
    if (!el) return;
    const { w, h } = layout.size;
    const scale = Math.max(0.12, Math.min(el.clientWidth / w, el.clientHeight / h, 1));
    setView({
      scale,
      tx: (el.clientWidth - w * scale) / 2,
      ty: (el.clientHeight - h * scale) / 2,
    });
  }, [layout]);

  React.useEffect(() => { fit(); }, [fit]);

  const zoomAt = React.useCallback((factor, cx, cy) => {
    setView((v) => {
      const scale = Math.max(0.12, Math.min(2, v.scale * factor));
      const k = scale / v.scale;
      return { scale, tx: cx - k * (cx - v.tx), ty: cy - k * (cy - v.ty) };
    });
  }, []);

  const zoomCenter = (factor) => {
    const el = vpRef.current;
    if (!el) return;
    zoomAt(factor, el.clientWidth / 2, el.clientHeight / 2);
  };

  const panToCard = React.useCallback((key) => {
    const el = vpRef.current;
    const card = layout.cards[key];
    if (!el || !card) return;
    setView((v) => ({
      ...v,
      tx: el.clientWidth / 2 - (card.x + card.w / 2) * v.scale,
      ty: el.clientHeight / 2 - (card.y + card.h / 2) * v.scale,
    }));
  }, [layout]);

  React.useEffect(() => {
    if (apiRef) apiRef.current = { fit, panToCard, zoomCenter };
  });

  // pointer pan
  const drag = React.useRef(null);
  const onPointerDown = (e) => {
    if (e.button !== 0) return;
    drag.current = { x: e.clientX, y: e.clientY, tx: viewRef.current.tx, ty: viewRef.current.ty, moved: false };
    setPanning(true);
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e) => {
    const d = drag.current;
    if (!d) return;
    const dx = e.clientX - d.x, dy = e.clientY - d.y;
    if (Math.abs(dx) + Math.abs(dy) > 3) d.moved = true;
    setView((v) => ({ ...v, tx: d.tx + dx, ty: d.ty + dy }));
  };
  const onPointerUp = (e) => {
    const d = drag.current;
    drag.current = null;
    setPanning(false);
    if (d && !d.moved && e.target === e.currentTarget) onSelect(null); // click empty space
  };

  // wheel: scroll pans, ctrl/cmd+wheel zooms
  React.useEffect(() => {
    const el = vpRef.current;
    if (!el) return;
    const onWheel = (e) => {
      e.preventDefault();
      if (e.ctrlKey || e.metaKey) {
        const rect = el.getBoundingClientRect();
        zoomAt(Math.exp(-e.deltaY * 0.0022), e.clientX - rect.left, e.clientY - rect.top);
      } else {
        setView((v) => ({ ...v, tx: v.tx - e.deltaX, ty: v.ty - e.deltaY }));
      }
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [zoomAt]);

  const cardState = (key) => {
    if (!selected) return "";
    if (key === selected) return "sel";
    if (related && related.has(key)) return "rel";
    return "dim";
  };

  const edgeClass = (e) => {
    if (!selected) return "dg-edge";
    return e.fromKey === selected || e.toKey === selected ? "dg-edge hl" : "dg-edge dim";
  };

  return (
    <div
      ref={vpRef}
      className={"dg-viewport dg-dots" + (tint !== false ? " tinted" : "") + (panning ? " panning" : "")}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      data-screen-label="ER diagram canvas"
    >
      <div
        className="dg-world"
        style={{
          width: layout.size.w, height: layout.size.h,
          transform: `translate(${view.tx}px, ${view.ty}px) scale(${view.scale})`,
        }}
      >
        {layout.clusters.map((c) => (
          <div key={c.name} className="dg-cluster" style={{ left: c.x, top: c.y, width: c.w, height: c.h, "--cl-h": c.hue }}>
            <span className="dg-cluster-label">{c.name} · {c.count}</span>
          </div>
        ))}

        <svg
          width={layout.size.w} height={layout.size.h}
          style={{ position: "absolute", top: 0, left: 0, pointerEvents: "none" }}
        >
          {layout.edges.map((e) => (
            <g key={e.i} className={edgeClass(e)}>
              <path d={window.DiagramLayout.edgePath(e, lineStyle)}></path>
              <circle className="dot-from" cx={e.x1} cy={e.y1} r="3.2"></circle>
              <circle className="dot-to" cx={e.x2} cy={e.y2} r="3.2"></circle>
            </g>
          ))}
        </svg>

        {Object.keys(layout.cards).map((key) => (
          <TableCard key={key} card={layout.cards[key]} state={cardState(key)} onSelect={onSelect}></TableCard>
        ))}
      </div>

      <div className="dg-tools">
        <button className="ds-iconbtn" title="Zoom in" onClick={() => zoomCenter(1.25)}><Ic name="plus" size={15}></Ic></button>
        <button className="ds-iconbtn" title="Zoom out" onClick={() => zoomCenter(0.8)}><Ic name="minus" size={15}></Ic></button>
        <button className="ds-iconbtn" title="Fit to view" onClick={fit}><Ic name="fit" size={15}></Ic></button>
      </div>
    </div>
  );
}

Object.assign(window, { SchemaDiagram });
