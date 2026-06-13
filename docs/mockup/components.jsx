/* ============================================================
   dbd website — MODULAR COMPONENTS
   Pure presentation. All copy comes from props (sourced from
   window.DBD_DATA in app.jsx). Color only via semantic utilities.
   ============================================================ */

/* ---- syntax token class map (semantic utilities only) ---- */
const TOK_CLASS = {
  ws: "",
  punct: "text-tok-punct",
  key: "text-tok-key",
  str: "text-tok-str",
  num: "text-tok-num",
  comment: "text-tok-comment",
  arrow: "text-tok-arrow",
};

/* ---------- Primitives ---------- */

function Eyebrow({ children }) {
  return (
    <div className="flex items-center gap-2.5 text-label font-mono font-medium uppercase text-accent">
      <span className="inline-block h-1.5 w-1.5 rounded-full bg-accent" />
      {children}
    </div>
  );
}

function Button({ href, children, variant = "primary", size = "md" }) {
  const base =
    "inline-flex items-center justify-center gap-2 rounded-lg font-medium transition-colors duration-150 whitespace-nowrap";
  const sizes = { md: "px-5 py-2.5 text-sm", lg: "px-6 py-3 text-base" };
  const variants = {
    primary: "bg-accent text-on-accent hover:bg-accent-2",
    ghost: "border border-line text-fg hover:bg-surface hover:border-accent-line",
    soft: "bg-surface text-fg border border-line hover:border-accent-line",
  };
  return (
    <a href={href} className={`${base} ${sizes[size]} ${variants[variant]}`}>
      {children}
    </a>
  );
}

function ArrowIcon() {
  return (
    <svg viewBox="0 0 16 16" className="h-3.5 w-3.5" fill="none" stroke="currentColor" strokeWidth="1.6">
      <path d="M3 8h10M9 4l4 4-4 4" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

function SectionHead({ eyebrow, title, lede, align = "left", className = "" }) {
  const alignCls = align === "center" ? "items-center text-center mx-auto" : "items-start";
  return (
    <div className={`flex flex-col gap-4 ${alignCls} ${className}`}>
      <Eyebrow>{eyebrow}</Eyebrow>
      <h2 className="font-display font-semibold text-h2 text-fg max-w-2xl text-balance">{title}</h2>
      {lede && <p className="text-lg text-muted max-w-2xl text-pretty">{lede}</p>}
    </div>
  );
}

/* ---------- Code block ---------- */

function CodeBlock({ code, className = "" }) {
  const lines = window.highlightCode(code.source, code.lang);
  return (
    <div className={`overflow-hidden rounded-xl2 border border-line bg-code-bg ${className}`}>
      <div className="flex items-center justify-between border-b border-line-soft px-4 py-2.5">
        <div className="flex items-center gap-1.5">
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
        </div>
        <span className="font-mono text-xs text-faint">{code.label}</span>
      </div>
      <pre className="overflow-x-auto px-4 py-4 font-mono text-[0.82rem] leading-relaxed">
        <code>
          {lines.map((toks, i) => (
            <div key={i} className="whitespace-pre">
              {toks.map((tk, j) => (
                <span key={j} className={TOK_CLASS[tk.c] || ""}>{tk.t}</span>
              ))}
              {toks.length === 0 ? "\n" : ""}
            </div>
          ))}
        </code>
      </pre>
    </div>
  );
}

/* ---------- Brand / Nav ---------- */

/* dbd logo (uploads/dbd.svg) tinted with the site accent via currentColor */
function BrandMark({ className = "h-8 w-8" }) {
  return (
    <span
      className={"inline-flex flex-none " + className}
      aria-hidden="true"
      style={{ color: "var(--accent)" }}
      dangerouslySetInnerHTML={{ __html: window.DBD_LOGO_SVG }}
    ></span>
  );
}

function Wordmark({ name, markClass = "h-8 w-8" }) {
  return (
    <a href="#top" className="group inline-flex items-center gap-2.5">
      <BrandMark className={markClass} />
      <span className="font-display font-semibold text-lg tracking-tight text-fg">{name}</span>
    </a>
  );
}

function Nav({ brand, nav, controls }) {
  return (
    <header className="sticky top-0 z-40 bg-bg/80 backdrop-blur-md">
      <div className="mx-auto flex max-w-content items-center justify-between gap-6 px-6 py-3.5">
        <Wordmark name={brand.name} />
        <nav className="hidden items-center gap-7 md:flex">
          {nav.links.map((l) => (
            <a key={l.href} href={l.href} className="whitespace-nowrap text-sm text-muted transition-colors hover:text-fg">
              {l.label}
            </a>
          ))}
        </nav>
        <div className="flex items-center gap-3">
          {controls}
          <a href="designs/Sign In.html" className="hidden whitespace-nowrap text-sm text-muted transition-colors hover:text-fg sm:block">
            Sign in
          </a>
          <div className="hidden sm:block">
            <Button href={nav.cta.href} size="md">
              {nav.cta.label} <ArrowIcon />
            </Button>
          </div>
        </div>
      </div>
    </header>
  );
}

/* ---------- Hero ---------- */

function Terminal({ data }) {
  const lineColor = {
    cmd: "text-fg",
    out: "text-muted",
    ok: "text-tok-str",
  };
  return (
    <div className="overflow-hidden rounded-xl2 border border-line bg-code-bg shadow-2xl shadow-bg-deep/40">
      <div className="flex items-center justify-between border-b border-line-soft px-4 py-2.5">
        <div className="flex items-center gap-1.5">
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
          <span className="h-2.5 w-2.5 rounded-full bg-line" />
        </div>
        <span className="font-mono text-xs text-faint">{data.file}</span>
      </div>
      <div className="px-4 py-4 font-mono text-[0.82rem] leading-relaxed">
        {data.lines.map((ln, i) => (
          <div key={i} className="flex gap-2 whitespace-pre">
            {ln.type === "cmd" ? <span className="text-accent">$</span> : <span className="text-transparent">$</span>}
            <span className={lineColor[ln.type]}>{ln.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function Hero({ data }) {
  return (
    <section id="top" className="relative overflow-hidden">
      <div className="pointer-events-none absolute inset-0 bg-grid mask-fade-b opacity-60" />
      <div className="relative mx-auto grid max-w-content items-center gap-12 px-6 pb-section pt-16 lg:grid-cols-[1.05fr_0.95fr] lg:pt-24">
        <div className="anim-rise flex flex-col items-start gap-6">
          <Eyebrow>{data.eyebrow}</Eyebrow>
          <h1 className="font-display font-bold text-display text-fg text-balance">
            {data.title[0]}
            <br />
            <span className="text-accent">{data.title[1]}</span>
          </h1>
          <p className="max-w-xl text-lg text-muted text-pretty">{data.lede}</p>
          <div className="flex flex-wrap items-center gap-3 pt-1">
            <Button href={data.primaryCta.href} size="lg">
              {data.primaryCta.label} <ArrowIcon />
            </Button>
            <Button href={data.secondaryCta.href} size="lg" variant="ghost">
              {data.secondaryCta.label}
            </Button>
          </div>
          <div className="mt-2 inline-flex items-center gap-3 rounded-lg border border-line bg-surface px-4 py-2.5 font-mono text-sm">
            <span className="text-accent">$</span>
            <span className="text-fg">{data.install}</span>
          </div>
        </div>
        <div className="anim-rise lg:pl-4" style={{ animationDelay: "0.12s" }}>
          <Terminal data={data.terminal} />
        </div>
      </div>
    </section>
  );
}

/* ---------- Overview / Features ---------- */

function FeatureCard({ tag, title, body }) {
  return (
    <div className="group flex flex-col gap-3 rounded-xl2 border border-line bg-surface p-6 transition-colors hover:border-accent-line">
      <span className="font-mono text-xs text-faint transition-colors group-hover:text-accent">{tag}</span>
      <h3 className="font-display font-semibold text-xl text-fg">{title}</h3>
      <p className="text-sm text-muted text-pretty">{body}</p>
    </div>
  );
}

function Overview({ data }) {
  return (
    <section id="overview" className="grid-section bg-bg-deep">
      <div className="mx-auto max-w-content px-6 py-section">
        <SectionHead eyebrow={data.eyebrow} title={data.title} lede={data.lede} />
        <div className="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {data.features.map((f) => (
            <FeatureCard key={f.tag} {...f} />
          ))}
        </div>
      </div>
    </section>
  );
}

/* ---------- Concepts ---------- */

function ConceptRow({ item, index }) {
  const flip = index % 2 === 1;
  return (
    <div className="grid items-center gap-8 lg:grid-cols-2 lg:gap-14">
      <div className={`flex flex-col gap-4 ${flip ? "lg:order-2" : ""}`}>
        <div className="flex items-center gap-3">
          <span className="grid h-7 w-7 place-items-center rounded-md border border-accent-line bg-accent-soft font-mono text-xs text-accent">
            {String(index + 1).padStart(2, "0")}
          </span>
          <span className="font-mono text-label uppercase text-faint">{item.kicker}</span>
        </div>
        <h3 className="font-display font-semibold text-h3 text-fg text-balance">{item.title}</h3>
        <p className="text-base text-muted text-pretty">{item.body}</p>
      </div>
      <div className={flip ? "lg:order-1" : ""}>
        <CodeBlock code={item.code} />
      </div>
    </div>
  );
}

function Concepts({ data }) {
  return (
    <section id="concepts" className="grid-section">
      <div className="mx-auto max-w-content px-6 py-section">
        <SectionHead eyebrow={data.eyebrow} title={data.title} align="center" />
        <div className="mt-14 flex flex-col gap-16">
          {data.items.map((item, i) => (
            <ConceptRow key={item.id} item={item} index={i} />
          ))}
        </div>
      </div>
    </section>
  );
}

/* ---------- Targets ---------- */

function TargetCard({ name, scheme, body, notes }) {
  return (
    <div className="flex flex-col gap-4 rounded-xl2 border border-line bg-surface p-6 transition-colors hover:border-accent-line">
      <div className="flex items-center justify-between gap-3">
        <h3 className="font-display font-semibold text-xl text-fg">{name}</h3>
      </div>
      <code className="w-fit rounded-md bg-code-bg px-2.5 py-1 font-mono text-xs text-accent">{scheme}</code>
      <p className="text-sm text-muted text-pretty">{body}</p>
      <ul className="mt-1 flex flex-col gap-2 border-t border-line-soft pt-4">
        {notes.map((n) => (
          <li key={n} className="flex items-center gap-2.5 text-sm text-muted">
            <span className="h-1 w-1 rounded-full bg-accent" />
            {n}
          </li>
        ))}
      </ul>
    </div>
  );
}

function Targets({ data }) {
  return (
    <section id="targets" className="grid-section bg-bg-deep">
      <div className="mx-auto max-w-content px-6 py-section">
        <SectionHead eyebrow={data.eyebrow} title={data.title} lede={data.lede} />
        <div className="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {data.items.map((t) => (
            <TargetCard key={t.name} {...t} />
          ))}
        </div>
      </div>
    </section>
  );
}

/* ---------- Audience ---------- */

function Audience({ data }) {
  return (
    <section id="audience" className="grid-section">
      <div className="mx-auto max-w-content px-6 py-section">
        <SectionHead eyebrow={data.eyebrow} title={data.title} align="center" />
        <div className="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {data.items.map((a) => (
            <div key={a.title} className="flex flex-col gap-2 rounded-xl2 border border-line bg-surface p-6">
              <h3 className="font-display font-medium text-lg text-fg">{a.title}</h3>
              <p className="text-sm text-muted text-pretty">{a.body}</p>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

/* ---------- Get started ---------- */

function Start({ data }) {
  return (
    <section id="start" className="grid-section bg-bg-deep">
      <div className="mx-auto max-w-content px-6 py-section">
        <div className="overflow-hidden rounded-xl2 border border-accent-line bg-surface">
          <div className="grid gap-10 p-8 lg:grid-cols-[1fr_1.1fr] lg:p-12">
            <div className="flex flex-col gap-4">
              <SectionHead eyebrow={data.eyebrow} title={data.title} lede={data.lede} />
              <div className="pt-2">
                <Button href={data.cta.href} size="lg">
                  {data.cta.label} <ArrowIcon />
                </Button>
              </div>
            </div>
            <div className="flex flex-col gap-3">
              {data.steps.map((s) => (
                <div key={s.n} className="flex items-center gap-4 rounded-lg border border-line bg-code-bg px-5 py-4">
                  <span className="grid h-7 w-7 shrink-0 place-items-center rounded-md bg-accent-soft font-mono text-sm text-accent">
                    {s.n}
                  </span>
                  <div className="flex flex-col">
                    <span className="text-xs text-faint">{s.text}</span>
                    <code className="font-mono text-sm text-fg">
                      <span className="text-accent">$ </span>{s.code}
                    </code>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}

/* ---------- Footer ---------- */

function Footer({ brand, footer }) {
  return (
    <footer className="bg-bg">
      <div className="mx-auto grid max-w-content gap-10 px-6 py-14 md:grid-cols-[1.4fr_1fr_1fr]">
        <div className="flex flex-col gap-3">
          <Wordmark name={brand.name} markClass="h-16 w-16" />
          <p className="max-w-xs text-sm text-muted">{footer.tagline}</p>
        </div>
        {footer.columns.map((col) => (
          <div key={col.title} className="flex flex-col gap-3">
            <span className="font-mono text-label uppercase text-faint">{col.title}</span>
            <ul className="flex flex-col gap-2">
              {col.links.map((l) => (
                <li key={l.label}>
                  <a href={l.href} className="text-sm text-muted transition-colors hover:text-fg">{l.label}</a>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
      <div className="border-t border-line">
        <div className="mx-auto flex max-w-content flex-col gap-2 px-6 py-5 sm:flex-row sm:items-center sm:justify-between">
          <span className="font-mono text-xs text-faint">{brand.name} — {brand.full}</span>
          <span className="text-xs text-faint">{footer.legal}</span>
        </div>
      </div>
    </footer>
  );
}

/* ---- export to window for cross-file access ---- */
Object.assign(window, {
  Eyebrow, Button, ArrowIcon, SectionHead, CodeBlock, Wordmark, Nav,
  Terminal, Hero, FeatureCard, Overview, ConceptRow, Concepts,
  TargetCard, Targets, Audience, Start, Footer,
});
