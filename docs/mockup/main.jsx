/* ============================================================
   dbd website — APP
   Composes sections from window.DBD_DATA and wires the semantic
   theme tokens (data-theme / data-accent / data-density) to Tweaks.
   ============================================================ */
const { useEffect } = React;
const D = window.DBD_DATA;

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "theme": "light",
  "accent": "sky",
  "density": "comfortable",
  "heroTitleA": "Your database schema,",
  "heroTitleB": "managed like source code."
}/*EDITMODE-END*/;

function SunMoonIcon({ theme }) {
  return theme === "dark" ? (
    <svg viewBox="0 0 20 20" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="1.5">
      <circle cx="10" cy="10" r="4" />
      <path d="M10 2v2M10 16v2M2 10h2M16 10h2M4.2 4.2l1.4 1.4M14.4 14.4l1.4 1.4M15.8 4.2l-1.4 1.4M5.6 14.4l-1.4 1.4" strokeLinecap="round" />
    </svg>
  ) : (
    <svg viewBox="0 0 20 20" className="h-4 w-4" fill="none" stroke="currentColor" strokeWidth="1.5">
      <path d="M16 11.5A6.5 6.5 0 0 1 8.5 4a6.5 6.5 0 1 0 7.5 7.5z" strokeLinejoin="round" />
    </svg>
  );
}

function ThemeToggle({ theme, onToggle }) {
  return (
    <button
      onClick={onToggle}
      aria-label="Toggle color theme"
      className="grid h-9 w-9 place-items-center rounded-lg border border-line text-muted transition-colors hover:border-accent-line hover:text-fg"
    >
      <SunMoonIcon theme={theme} />
    </button>
  );
}

function App() {
  const [t, setTweak] = useTweaks(TWEAK_DEFAULTS);

  useEffect(() => {
    const root = document.documentElement;
    root.setAttribute("data-theme", t.theme);
    root.setAttribute("data-accent", t.accent);
    root.setAttribute("data-density", t.density);
  }, [t.theme, t.accent, t.density]);

  // merge tweakable hero copy into the data-sourced hero
  const hero = { ...D.hero, title: [t.heroTitleA, t.heroTitleB] };

  return (
    <div id="top">
      <Nav
        brand={D.brand}
        nav={D.nav}
        controls={<ThemeToggle theme={t.theme} onToggle={() => setTweak("theme", t.theme === "dark" ? "light" : "dark")} />}
      />
      <main>
        <Hero data={hero} />
        <Overview data={D.overview} />
        <Concepts data={D.concepts} />
        <Targets data={D.targets} />
        <Audience data={D.audience} />
        <Start data={D.start} />
      </main>
      <Footer brand={D.brand} footer={D.footer} />

      <TweaksPanel>
        <TweakSection label="Theme" />
        <TweakRadio label="Mode" value={t.theme} options={["dark", "light"]} onChange={(v) => setTweak("theme", v)} />
        <TweakColor
          label="Accent"
          value={ACCENT_SWATCH[t.accent]}
          options={Object.values(ACCENT_SWATCH)}
          onChange={(hex) => setTweak("accent", Object.keys(ACCENT_SWATCH).find((k) => ACCENT_SWATCH[k] === hex) || "sky")}
        />
        <TweakSection label="Layout" />
        <TweakRadio label="Density" value={t.density} options={["compact", "comfortable", "spacious"]} onChange={(v) => setTweak("density", v)} />
        <TweakSection label="Hero copy" />
        <TweakText label="Line 1" value={t.heroTitleA} onChange={(v) => setTweak("heroTitleA", v)} />
        <TweakText label="Line 2" value={t.heroTitleB} onChange={(v) => setTweak("heroTitleB", v)} />
      </TweaksPanel>
    </div>
  );
}

// accent swatch hex (display only — real colors come from semantic tokens)
const ACCENT_SWATCH = {
  sky: "#38BDF8",
  rust: "#E0A33B",
  green: "#3FD168",
  violet: "#A98Cff",
};

ReactDOM.createRoot(document.getElementById("root")).render(<App />);
