/* Shared Tailwind CDN config for the dbd designs app — mirrors the marketing site. */
tailwind.config = {
  theme: {
    extend: {
      colors: {
        bg: "var(--bg)",
        "bg-deep": "var(--bg-deep)",
        surface: "var(--surface)",
        "surface-2": "var(--surface-2)",
        line: "var(--line)",
        "line-soft": "var(--line-soft)",
        fg: "var(--fg)",
        muted: "var(--muted)",
        faint: "var(--faint)",
        accent: "var(--accent)",
        "accent-2": "var(--accent-2)",
        "on-accent": "var(--on-accent)",
        "accent-soft": "var(--accent-soft)",
        "accent-line": "var(--accent-line)",
        "code-bg": "var(--code-bg)",
      },
      fontFamily: {
        display: ["var(--font-display)"],
        sans: ['"IBM Plex Sans"', "system-ui", "sans-serif"],
        mono: ['"IBM Plex Mono"', "ui-monospace", "monospace"],
      },
      fontSize: {
        label: ["0.72rem", { lineHeight: "1.1", letterSpacing: "0.14em" }],
        xs: ["0.8rem", { lineHeight: "1.5" }],
        sm: ["0.9rem", { lineHeight: "1.6" }],
        base: ["1rem", { lineHeight: "1.65" }],
        lg: ["1.15rem", { lineHeight: "1.6" }],
        xl: ["1.4rem", { lineHeight: "1.4", letterSpacing: "-0.01em" }],
        h3: ["1.55rem", { lineHeight: "1.25", letterSpacing: "-0.015em" }],
        h2: ["2.1rem", { lineHeight: "1.1", letterSpacing: "-0.02em" }],
      },
      borderRadius: {
        app: "var(--radius)",
        "app-lg": "var(--radius-lg)",
        "app-sm": "var(--radius-sm)",
      },
    },
  },
};
