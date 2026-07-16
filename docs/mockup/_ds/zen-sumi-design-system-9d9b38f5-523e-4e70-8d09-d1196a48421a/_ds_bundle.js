/* @ds-bundle: {"format":4,"namespace":"ZenSumiDesignSystemSensei_9d9b38","components":[{"name":"WindowChrome","sourcePath":"components/app/WindowChrome.jsx"},{"name":"Badge","sourcePath":"components/core/Badge.jsx"},{"name":"Button","sourcePath":"components/core/Button.jsx"},{"name":"Eyebrow","sourcePath":"components/core/Eyebrow.jsx"},{"name":"IconButton","sourcePath":"components/core/IconButton.jsx"},{"name":"Kanji","sourcePath":"components/core/Kanji.jsx"},{"name":"StatusDot","sourcePath":"components/core/StatusDot.jsx"},{"name":"Insight","sourcePath":"components/data/Insight.jsx"},{"name":"Sparkline","sourcePath":"components/data/Sparkline.jsx"},{"name":"Input","sourcePath":"components/forms/Input.jsx"},{"name":"Card","sourcePath":"components/surfaces/Card.jsx"}],"sourceHashes":{"components/app/WindowChrome.jsx":"8660c73e263f","components/core/Badge.jsx":"6c4f839565da","components/core/Button.jsx":"7a4ab1ecd3a5","components/core/Eyebrow.jsx":"349fb7536ea8","components/core/IconButton.jsx":"8a14dc8edb11","components/core/Kanji.jsx":"2168c4f4703c","components/core/StatusDot.jsx":"3c14079ac84a","components/data/Insight.jsx":"61b78945b5d1","components/data/Sparkline.jsx":"900b3239d138","components/forms/Input.jsx":"c3940021dbc3","components/surfaces/Card.jsx":"e6039f49d4ce","ui_kits/observatory/Chrome.jsx":"16786aca3aa5","ui_kits/observatory/FtrPanel.jsx":"138140509144","ui_kits/observatory/Hero.jsx":"b81ca906d3bc","ui_kits/observatory/Rows.jsx":"f7b0e8e911fa","ui_kits/observatory/Sidebar.jsx":"af20591185ee","ui_kits/site/Footer.jsx":"5546b1d982c6","ui_kits/site/Mock.jsx":"0c24f337b336","ui_kits/site/Nav.jsx":"e6d1e6241528","ui_kits/site/Sections.jsx":"19db3d90469d","ui_kits/site/SiteHero.jsx":"6337d0b67775"},"inlinedExternals":[],"unexposedExports":[]} */

(() => {

const __ds_ns = (window.ZenSumiDesignSystemSensei_9d9b38 = window.ZenSumiDesignSystemSensei_9d9b38 || {});

const __ds_scope = {};

(__ds_ns.__errors = __ds_ns.__errors || []);

// components/app/WindowChrome.jsx
try { (() => {
/**
 * WindowChrome — the 38px Tauri title bar: traffic lights left, a centered
 * subtitle, nothing else. The desktop app's every screen sits under this.
 */
function WindowChrome({
  title = 'Sensei  先生  ·  observatory'
}) {
  return /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome"
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-traffic"
  }, /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null)), /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome-title"
  }, title), /*#__PURE__*/React.createElement("div", {
    style: {
      width: 54
    }
  }));
}
Object.assign(__ds_scope, { WindowChrome });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/app/WindowChrome.jsx", error: String((e && e.message) || e) }); }

// components/core/Badge.jsx
try { (() => {
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
/**
 * Badge — a small mono-type tag. Prefer a meaningful phrase ("3rd time")
 * over a raw count. Tones: default (neutral), success, warning, accent.
 */
function Badge({
  tone = 'default',
  className = '',
  children,
  ...rest
}) {
  const cls = ['zs-badge'];
  if (tone === 'success') cls.push('zs-badge-success');else if (tone === 'warning') cls.push('zs-badge-warning');else if (tone === 'accent') cls.push('zs-badge-accent');
  if (className) cls.push(className);
  return /*#__PURE__*/React.createElement("span", _extends({
    className: cls.join(' ')
  }, rest), children);
}
Object.assign(__ds_scope, { Badge });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/Badge.jsx", error: String((e && e.message) || e) }); }

// components/core/Button.jsx
try { (() => {
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
/**
 * Button — the brand's primary action. Ink-on-paper primary, hairline
 * secondary, quiet ghost. Press dips 0.5px; no shadow, no glow.
 */
function Button({
  variant = 'primary',
  size = 'md',
  disabled = false,
  kanji,
  onClick,
  type = 'button',
  className = '',
  children,
  ...rest
}) {
  const cls = ['zs-btn'];
  if (variant === 'primary') cls.push('zs-btn-primary');else if (variant === 'secondary') cls.push('zs-btn-secondary');else if (variant === 'ghost') cls.push('zs-btn-ghost');
  if (size === 'sm') cls.push('zs-btn-sm');else if (size === 'lg') cls.push('zs-btn-lg');
  if (className) cls.push(className);
  return /*#__PURE__*/React.createElement("button", _extends({
    type: type,
    className: cls.join(' '),
    disabled: disabled,
    onClick: onClick
  }, rest), kanji ? /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      color: 'var(--accent)',
      fontSize: 14,
      lineHeight: 1
    }
  }, kanji) : null, children);
}
Object.assign(__ds_scope, { Button });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/Button.jsx", error: String((e && e.message) || e) }); }

// components/core/Eyebrow.jsx
try { (() => {
/**
 * Eyebrow — an uppercase, letter-spaced label that sits above a heading.
 * Only the eyebrow is uppercased; the heading beneath stays sentence case.
 * An optional leading kanji anchors it.
 */
function Eyebrow({
  kanji,
  style,
  children
}) {
  return /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      display: 'inline-flex',
      alignItems: 'center',
      gap: 6,
      ...style
    }
  }, kanji ? /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 12,
      color: 'var(--accent)'
    }
  }, kanji) : null, children);
}
Object.assign(__ds_scope, { Eyebrow });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/Eyebrow.jsx", error: String((e && e.message) || e) }); }

// components/core/IconButton.jsx
try { (() => {
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
/**
 * IconButton — a 32×32 square affordance for a single glyph (a Solar duotone
 * icon <img>, a kanji, or an inline SVG). Hover lifts to paper-3.
 */
function IconButton({
  label,
  onClick,
  className = '',
  children,
  ...rest
}) {
  const cls = ['zs-icon-btn'];
  if (className) cls.push(className);
  return /*#__PURE__*/React.createElement("button", _extends({
    className: cls.join(' '),
    "aria-label": label,
    title: label,
    onClick: onClick
  }, rest), children);
}
Object.assign(__ds_scope, { IconButton });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/IconButton.jsx", error: String((e && e.message) || e) }); }

// components/core/Kanji.jsx
try { (() => {
/**
 * Kanji — a single functional Japanese character in Mincho serif. Kanji are
 * the brand's icon layer; each has a fixed meaning (see assets/kanji.md).
 * Always keep kanji in their own span — never mixed with a Latin glyph run.
 * Never render below ~14px; the brush detail dies.
 *
 * tone: accent (functional / do-this) · ink (documentary) ·
 *       decor (--ink-3) · disabled (--ink-4)
 */
function Kanji({
  tone = 'accent',
  size = 18,
  title,
  style,
  children
}) {
  const color = tone === 'ink' ? 'var(--ink)' : tone === 'decor' ? 'var(--ink-3)' : tone === 'disabled' ? 'var(--ink-4)' : 'var(--accent)';
  return /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    title: title,
    style: {
      color,
      fontSize: size,
      lineHeight: 1,
      ...style
    }
  }, children);
}
Object.assign(__ds_scope, { Kanji });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/Kanji.jsx", error: String((e && e.message) || e) }); }

// components/core/StatusDot.jsx
try { (() => {
/**
 * StatusDot — a 7px dot. accent = live/active, success = clean/FTR,
 * warning = needs attention, neutral = idle.
 */
function StatusDot({
  tone = 'neutral',
  style
}) {
  const cls = ['zs-dot'];
  if (tone === 'accent') cls.push('zs-dot-accent');else if (tone === 'success') cls.push('zs-dot-success');else if (tone === 'warning') cls.push('zs-dot-warning');
  return /*#__PURE__*/React.createElement("span", {
    className: cls.join(' '),
    style: style
  });
}
Object.assign(__ds_scope, { StatusDot });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/core/StatusDot.jsx", error: String((e && e.message) || e) }); }

// components/data/Insight.jsx
try { (() => {
/**
 * Insight — the signature Zen-Sumi row: a functional kanji, an eyebrow label,
 * a plain-language line, and a *meaningful* mono badge ("3rd time", "+7% FTR")
 * instead of a big number. tone tints the kanji and the badge.
 */
function Insight({
  kanji,
  label,
  text,
  tag,
  tone = 'neutral'
}) {
  const toneColor = tone === 'warning' ? 'var(--warning)' : tone === 'success' ? 'var(--success)' : tone === 'accent' ? 'var(--accent)' : 'var(--ink-3)';
  const tagBg = tone === 'warning' ? 'var(--warning-soft)' : tone === 'success' ? 'var(--success-soft)' : tone === 'accent' ? 'var(--accent-soft)' : 'var(--paper-3)';
  return /*#__PURE__*/React.createElement("div", {
    className: "border-b py-3 flex items-baseline gap-4"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 18,
      color: toneColor,
      width: 24,
      flexShrink: 0
    }
  }, kanji), /*#__PURE__*/React.createElement("div", {
    style: {
      flex: 1
    }
  }, label ? /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      marginBottom: 2,
      fontSize: 10
    }
  }, label) : null, /*#__PURE__*/React.createElement("div", {
    className: "zs-body-sm",
    style: {
      color: 'var(--ink-2)'
    }
  }, text)), tag ? /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs",
    style: {
      color: toneColor,
      padding: '3px 8px',
      borderRadius: 'var(--radius-sm)',
      background: tagBg,
      whiteSpace: 'nowrap'
    }
  }, tag) : null);
}
Object.assign(__ds_scope, { Insight });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/data/Insight.jsx", error: String((e && e.message) || e) }); }

// components/data/Sparkline.jsx
try { (() => {
/**
 * Sparkline — a tiny inline trend line. Ink line, vermillion endpoint dot.
 * No axes, no grid, no fill. Pass a plain array of numbers.
 */
function Sparkline({
  data = [],
  width = 120,
  height = 32,
  accent = false
}) {
  if (!data.length) return null;
  const max = Math.max(...data);
  const min = Math.min(...data);
  const rng = max - min || 1;
  const x = i => data.length === 1 ? width : i / (data.length - 1) * width;
  const y = v => height - (v - min) / rng * (height - 2) - 1;
  const points = data.map((v, i) => `${x(i).toFixed(1)},${y(v).toFixed(1)}`).join(' ');
  const last = data[data.length - 1];
  return /*#__PURE__*/React.createElement("svg", {
    width: width,
    height: height,
    style: {
      display: 'block',
      color: accent ? 'var(--accent)' : 'var(--ink-3)',
      overflow: 'visible'
    }
  }, /*#__PURE__*/React.createElement("polyline", {
    className: "zs-sparkline",
    points: points
  }), /*#__PURE__*/React.createElement("circle", {
    cx: x(data.length - 1),
    cy: y(last),
    r: "2",
    fill: "var(--accent)"
  }));
}
Object.assign(__ds_scope, { Sparkline });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/data/Sparkline.jsx", error: String((e && e.message) || e) }); }

// components/forms/Input.jsx
try { (() => {
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
/**
 * Input — a hairline text field on raised paper. Focus deepens the border to
 * ink (no glow). An optional leading glyph (search kanji, icon) sits inline.
 */
function Input({
  value,
  onChange,
  placeholder,
  glyph,
  type = 'text',
  className = '',
  ...rest
}) {
  const cls = ['zs-input'];
  if (className) cls.push(className);
  return /*#__PURE__*/React.createElement("label", {
    className: cls.join(' ')
  }, glyph ? /*#__PURE__*/React.createElement("span", {
    style: {
      color: 'var(--ink-3)',
      display: 'inline-flex',
      alignItems: 'center'
    }
  }, glyph) : null, /*#__PURE__*/React.createElement("input", _extends({
    type: type,
    value: value,
    onChange: onChange,
    placeholder: placeholder
  }, rest)));
}
Object.assign(__ds_scope, { Input });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/forms/Input.jsx", error: String((e && e.message) || e) }); }

// components/surfaces/Card.jsx
try { (() => {
function _extends() { return _extends = Object.assign ? Object.assign.bind() : function (n) { for (var e = 1; e < arguments.length; e++) { var t = arguments[e]; for (var r in t) ({}).hasOwnProperty.call(t, r) && (n[r] = t[r]); } return n; }, _extends.apply(null, arguments); }
/**
 * Card — the system's container: paper-2 + hairline + radius-lg + space-5.
 * No shadow, no colored left-border, no header fill. Set flush to drop the
 * padding when the card wraps its own edge-to-edge rows.
 */
function Card({
  flush = false,
  className = '',
  style,
  children,
  ...rest
}) {
  const cls = [flush ? 'zs-card-flush' : 'zs-card'];
  if (className) cls.push(className);
  return /*#__PURE__*/React.createElement("div", _extends({
    className: cls.join(' '),
    style: style
  }, rest), children);
}
Object.assign(__ds_scope, { Card });
})(); } catch (e) { __ds_ns.__errors.push({ path: "components/surfaces/Card.jsx", error: String((e && e.message) || e) }); }

// ui_kits/observatory/Chrome.jsx
try { (() => {
// Observatory · 38px window chrome
// Reused from the source prototype, simplified to pure design-system classes.

function Chrome({
  title = "Sensei  先生  ·  observatory"
}) {
  return /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome"
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-traffic"
  }, /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null)), /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome-title"
  }, title), /*#__PURE__*/React.createElement("div", {
    style: {
      width: 54
    }
  }));
}
window.Chrome = Chrome;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/observatory/Chrome.jsx", error: String((e && e.message) || e) }); }

// ui_kits/observatory/FtrPanel.jsx
try { (() => {
// FtrPanel — the 14-day "First-Try-Right" bar strip in the top right of Today.

function FtrPanel({
  value = 78,
  delta = 6,
  data
}) {
  const d = data || [0.71, 0.69, 0.74, 0.72, 0.68, 0.70, 0.73, 0.75, 0.72, 0.78, 0.74, 0.79, 0.76, 0.78];
  const w = 168,
    h = 56,
    gap = 2;
  const barW = (w - gap * (d.length - 1)) / d.length;
  return /*#__PURE__*/React.createElement("div", {
    className: "flex items-center gap-4",
    style: {
      paddingTop: 4
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "text-right"
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      fontSize: 10
    }
  }, "First-try-right \xB7 14d"), /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline justify-end gap-1",
    style: {
      marginTop: 4
    }
  }, /*#__PURE__*/React.createElement("span", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-3xl)',
      fontWeight: 400,
      lineHeight: 1
    }
  }, value), /*#__PURE__*/React.createElement("span", {
    className: "text-xs text-ink-3"
  }, "%"), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs",
    style: {
      marginLeft: 4,
      color: delta >= 0 ? 'var(--success)' : 'var(--warning)'
    }
  }, delta >= 0 ? "↑" : "↓", " ", Math.abs(delta), "%"))), /*#__PURE__*/React.createElement("svg", {
    width: w,
    height: h + 14,
    style: {
      display: 'block',
      overflow: 'visible'
    }
  }, /*#__PURE__*/React.createElement("line", {
    x1: "0",
    x2: w,
    y1: h * 0.5,
    y2: h * 0.5,
    stroke: "var(--edge)",
    strokeDasharray: "2 3"
  }), d.map((v, i) => {
    const bh = Math.max(3, v * h);
    const isLast = i === d.length - 1;
    return /*#__PURE__*/React.createElement("rect", {
      key: i,
      x: i * (barW + gap),
      y: h - bh,
      width: barW,
      height: bh,
      fill: isLast ? 'var(--accent)' : 'var(--ink-3)',
      opacity: isLast ? 1 : 0.45
    });
  }), /*#__PURE__*/React.createElement("text", {
    x: 0,
    y: h + 11,
    fontSize: "9",
    fill: "var(--ink-3)",
    fontFamily: "var(--font-ui)",
    letterSpacing: "0.08em"
  }, "14d ago"), /*#__PURE__*/React.createElement("text", {
    x: w,
    y: h + 11,
    fontSize: "9",
    fill: "var(--ink-3)",
    textAnchor: "end",
    fontFamily: "var(--font-ui)",
    letterSpacing: "0.08em"
  }, "today")));
}
window.FtrPanel = FtrPanel;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/observatory/FtrPanel.jsx", error: String((e && e.message) || e) }); }

// ui_kits/observatory/Hero.jsx
try { (() => {
// The hero koan — the single daily teaching.
// Big kanji on the left, koan + body + action on the right.

function HeroKoan({
  kanji = "聴",
  phase = "Listen",
  koan,
  body,
  action,
  source
}) {
  return /*#__PURE__*/React.createElement("section", {
    style: {
      display: 'grid',
      gridTemplateColumns: '128px 1fr',
      gap: 32,
      padding: 'var(--space-7) 0',
      borderTop: 'var(--hairline)',
      borderBottom: 'var(--hairline)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      position: 'relative'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-kanji",
    style: {
      fontSize: 96,
      color: 'var(--accent)',
      lineHeight: 1
    }
  }, kanji), /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      position: 'absolute',
      left: -4,
      top: -2,
      writingMode: 'vertical-rl',
      transform: 'rotate(180deg)',
      fontSize: 9,
      height: 96
    }
  }, phase)), /*#__PURE__*/React.createElement("div", {
    className: "flex flex-col"
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-2xl)',
      fontWeight: 400,
      letterSpacing: '-0.01em',
      lineHeight: 1.2,
      marginBottom: 'var(--space-3)'
    }
  }, koan), /*#__PURE__*/React.createElement("p", {
    className: "zs-body",
    style: {
      margin: 0,
      marginBottom: 'var(--space-4)',
      maxWidth: 620
    }
  }, body), /*#__PURE__*/React.createElement("div", {
    className: "flex items-center gap-4",
    style: {
      marginTop: 'var(--space-2)'
    }
  }, action && /*#__PURE__*/React.createElement("button", {
    className: "zs-btn zs-btn-primary"
  }, action, " ", /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      color: 'var(--accent)',
      fontSize: 14
    }
  }, "\u2192")), /*#__PURE__*/React.createElement("div", {
    className: "flex items-center gap-2",
    style: {
      color: 'var(--accent)'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-ink-dot"
  }), /*#__PURE__*/React.createElement("span", {
    style: {
      fontSize: 'var(--text-sm)'
    }
  }, "Projected FTR + 14% in Lumen Cloud")), /*#__PURE__*/React.createElement("span", {
    style: {
      flex: 1
    }
  }), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-3"
  }, source))));
}
window.HeroKoan = HeroKoan;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/observatory/Hero.jsx", error: String((e && e.message) || e) }); }

// ui_kits/observatory/Rows.jsx
try { (() => {
// InsightRow / SessionRow / LearnedRow — the small list rows below the hero.

function InsightRow({
  kanji,
  label,
  text,
  tag,
  tone = "mute"
}) {
  const toneColor = tone === "warn" ? 'var(--warning)' : tone === "good" ? 'var(--success)' : tone === "accent" ? 'var(--accent)' : 'var(--ink-3)';
  const tagBg = tone === "warn" ? 'var(--warning-soft)' : tone === "good" ? 'var(--success-soft)' : tone === "accent" ? 'var(--accent-soft)' : 'var(--paper-3)';
  return /*#__PURE__*/React.createElement("div", {
    className: "border-b py-3 flex items-baseline gap-4"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 18,
      color: toneColor,
      width: 24,
      flexShrink: 0
    }
  }, kanji), /*#__PURE__*/React.createElement("div", {
    style: {
      flex: 1
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      marginBottom: 2,
      fontSize: 10
    }
  }, label), /*#__PURE__*/React.createElement("div", {
    className: "zs-body-sm",
    style: {
      color: 'var(--ink-2)'
    }
  }, text)), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs",
    style: {
      color: toneColor,
      padding: '3px 8px',
      borderRadius: 'var(--radius-sm)',
      background: tagBg
    }
  }, tag));
}
function SessionRow({
  project,
  title,
  time,
  duration,
  ftr
}) {
  return /*#__PURE__*/React.createElement("div", {
    className: "border-b py-3 flex items-center gap-3"
  }, /*#__PURE__*/React.createElement("span", {
    className: `zs-dot ${ftr ? 'zs-dot-success' : 'zs-dot-warning'}`
  }), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-3",
    style: {
      width: 100
    }
  }, project), /*#__PURE__*/React.createElement("span", {
    className: "text-sm text-ink",
    style: {
      flex: 1
    }
  }, title), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-3"
  }, time, " \xB7 ", duration));
}
function LearnedRow({
  when,
  scope,
  what,
  source
}) {
  return /*#__PURE__*/React.createElement("div", {
    className: "border-b",
    style: {
      padding: '14px 0'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-2",
    style: {
      marginBottom: 4
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-3"
  }, when), /*#__PURE__*/React.createElement("span", {
    className: "text-xs text-ink-4"
  }, "\xB7"), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-accent"
  }, scope)), /*#__PURE__*/React.createElement("div", {
    className: "text-sm text-ink"
  }, what), /*#__PURE__*/React.createElement("div", {
    className: "text-xs text-ink-4",
    style: {
      marginTop: 4
    }
  }, source));
}
window.InsightRow = InsightRow;
window.SessionRow = SessionRow;
window.LearnedRow = LearnedRow;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/observatory/Rows.jsx", error: String((e && e.message) || e) }); }

// ui_kits/observatory/Sidebar.jsx
try { (() => {
// Observatory · left rail
// Sections (Today, Projects, Sessions, Insights, Memories, Instruments, Libraries)
// + Active projects list + Dormant.

const NAV = [{
  id: "home",
  label: "Today",
  kanji: "今"
}, {
  id: "projects",
  label: "Projects",
  kanji: "場"
}, {
  id: "sessions",
  label: "Sessions",
  kanji: "刻"
}, {
  id: "insights",
  label: "Insights",
  kanji: "察"
}, {
  id: "memories",
  label: "Memories",
  kanji: "覚",
  badge: 24
}, {
  id: "instruments",
  label: "Instruments",
  kanji: "具",
  badge: 7
}, {
  id: "libraries",
  label: "Libraries",
  kanji: "庫"
}, {
  id: "config",
  label: "Configure",
  kanji: "設"
}];
const ACTIVE = [{
  id: "lumen-studio",
  kanji: "工",
  name: "Lumen Studio",
  ftr: 82
}, {
  id: "lumen-cloud",
  kanji: "雲",
  name: "Lumen Cloud",
  ftr: 64,
  warn: true
}, {
  id: "brand-kit",
  kanji: "紋",
  name: "Brand Kit",
  ftr: 91
}];
const DORMANT = [{
  kanji: "筆",
  name: "Sketch tool",
  last: "3w"
}, {
  kanji: "巻",
  name: "Docs site",
  last: "2mo"
}];
function NavItem({
  item,
  active,
  onClick
}) {
  return /*#__PURE__*/React.createElement("button", {
    onClick: onClick,
    className: "flex items-center gap-3 w-full",
    style: {
      padding: '7px 10px',
      borderRadius: 6,
      textAlign: 'left',
      background: active ? 'var(--paper-3)' : 'transparent',
      color: active ? 'var(--ink)' : 'var(--ink-2)',
      fontSize: 'var(--text-sm)'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 13,
      width: 14,
      color: active ? 'var(--accent)' : 'var(--ink-3)'
    }
  }, item.kanji), /*#__PURE__*/React.createElement("span", {
    style: {
      flex: 1
    }
  }, item.label), item.badge != null && /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-3"
  }, item.badge));
}
function ProjectItem({
  p,
  active,
  onClick
}) {
  return /*#__PURE__*/React.createElement("button", {
    onClick: onClick,
    className: "flex items-center gap-3 w-full",
    style: {
      padding: '8px 10px',
      borderRadius: 6,
      textAlign: 'left',
      background: active ? 'var(--paper-3)' : 'transparent',
      color: active ? 'var(--ink)' : 'var(--ink-2)',
      fontSize: 'var(--text-sm)'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 13,
      width: 14,
      color: p.warn ? 'var(--warning)' : 'var(--accent)'
    }
  }, p.kanji), /*#__PURE__*/React.createElement("span", {
    style: {
      flex: 1
    }
  }, p.name), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs",
    style: {
      color: p.warn ? 'var(--warning)' : 'var(--ink-3)'
    }
  }, p.ftr));
}
function Sidebar({
  section,
  setSection
}) {
  return /*#__PURE__*/React.createElement("aside", {
    className: "border-r",
    style: {
      width: 240,
      padding: '20px 12px',
      display: 'flex',
      flexDirection: 'column',
      gap: 24,
      overflow: 'auto',
      background: 'var(--paper)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-2",
    style: {
      padding: '0 6px'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 20,
      color: 'var(--accent)'
    }
  }, "\u5148"), /*#__PURE__*/React.createElement("span", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 16
    }
  }, "Sensei")), /*#__PURE__*/React.createElement("div", null, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      padding: '0 10px',
      marginBottom: 8,
      fontSize: 10
    }
  }, "Observatory"), /*#__PURE__*/React.createElement("div", {
    className: "flex flex-col",
    style: {
      gap: 1
    }
  }, NAV.map(item => /*#__PURE__*/React.createElement(NavItem, {
    key: item.id,
    item: item,
    active: section === item.id,
    onClick: () => setSection(item.id)
  })))), /*#__PURE__*/React.createElement("div", null, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline justify-between",
    style: {
      padding: '0 10px',
      marginBottom: 8
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-eyebrow",
    style: {
      fontSize: 10
    }
  }, "Active"), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-4"
  }, ACTIVE.length)), /*#__PURE__*/React.createElement("div", {
    className: "flex flex-col",
    style: {
      gap: 1
    }
  }, ACTIVE.map(p => /*#__PURE__*/React.createElement(ProjectItem, {
    key: p.id,
    p: p
  })))), /*#__PURE__*/React.createElement("div", null, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      padding: '0 10px',
      marginBottom: 8,
      fontSize: 10
    }
  }, "Dormant"), /*#__PURE__*/React.createElement("div", {
    className: "flex flex-col",
    style: {
      gap: 1
    }
  }, DORMANT.map((p, i) => /*#__PURE__*/React.createElement("button", {
    key: i,
    className: "flex items-center gap-3 w-full",
    style: {
      padding: '7px 10px',
      borderRadius: 6,
      textAlign: 'left',
      color: 'var(--ink-3)',
      fontSize: 'var(--text-sm)',
      opacity: 0.82
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 12,
      width: 14,
      opacity: 0.6
    }
  }, p.kanji), /*#__PURE__*/React.createElement("span", {
    style: {
      flex: 1
    }
  }, p.name), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono text-xs text-ink-4"
  }, p.last))))), /*#__PURE__*/React.createElement("div", {
    style: {
      flex: 1
    }
  }), /*#__PURE__*/React.createElement("div", {
    className: "border-t",
    style: {
      padding: '12px 10px 0',
      fontSize: 10,
      color: 'var(--ink-3)',
      lineHeight: 1.6
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-mono"
  }, "daemon \xB7 running"), /*#__PURE__*/React.createElement("br", null), /*#__PURE__*/React.createElement("span", {
    style: {
      color: 'var(--ink-4)'
    }
  }, "last heartbeat 2s ago")));
}
window.Sidebar = Sidebar;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/observatory/Sidebar.jsx", error: String((e && e.message) || e) }); }

// ui_kits/site/Footer.jsx
try { (() => {
// FAQ — expandable hairline-divided rows.

function Faq() {
  const qs = [{
    q: "Which AI assistants does it observe?",
    a: "Any AI assistant that speaks the Model Context Protocol. The list grows as MCP grows."
  }, {
    q: "Does sensei see my code?",
    a: "Only what passes through your AI tool's session. It runs locally and stores everything in a SQLite file you can inspect or delete at any time."
  }, {
    q: "Will it slow down my machine?",
    a: "Sensei is a Tauri app — small binary, low memory. The observer is event-driven; it only does work when a session happens."
  }, {
    q: "Can I export my memories?",
    a: "Yes. Settings → Export gives you a JSON dump of every pattern, memory, and adopted teaching. Import is also supported."
  }, {
    q: "What's the long-term plan?",
    a: "Sensei stays local-first and free. We may add an optional paid tier later for cross-machine sync, but the core promise — quiet, local, observant — never changes."
  }];
  return /*#__PURE__*/React.createElement("section", {
    id: "faq",
    style: {
      borderTop: 'var(--hairline)',
      padding: 'var(--space-8) var(--space-7)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      maxWidth: 880,
      margin: '0 auto'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mb-3"
  }, "Frequently asked"), /*#__PURE__*/React.createElement("h2", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-2xl)',
      fontWeight: 400,
      letterSpacing: '-0.015em',
      margin: '0 0 var(--space-6)'
    }
  }, "Common questions, plain answers."), /*#__PURE__*/React.createElement("div", null, qs.map((it, i) => /*#__PURE__*/React.createElement("details", {
    key: i,
    style: {
      borderTop: 'var(--hairline)',
      padding: '20px 0',
      ...(i === qs.length - 1 ? {
        borderBottom: 'var(--hairline)'
      } : {})
    }
  }, /*#__PURE__*/React.createElement("summary", {
    style: {
      cursor: 'pointer',
      listStyle: 'none',
      display: 'flex',
      justifyContent: 'space-between',
      fontSize: 'var(--text-base)',
      color: 'var(--ink)'
    }
  }, /*#__PURE__*/React.createElement("span", null, it.q), /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji text-ink-3"
  }, "+")), /*#__PURE__*/React.createElement("div", {
    className: "text-sm text-ink-2",
    style: {
      lineHeight: 1.7,
      marginTop: 'var(--space-3)',
      maxWidth: 640
    }
  }, it.a))))));
}
function Footer() {
  return /*#__PURE__*/React.createElement("footer", {
    style: {
      borderTop: 'var(--hairline)',
      padding: 'var(--space-6) var(--space-7)',
      fontSize: 'var(--text-xs)',
      color: 'var(--ink-3)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      maxWidth: 1100,
      margin: '0 auto',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-2"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 13,
      letterSpacing: '-0.04em'
    }
  }, "\u5148\u751F"), /*#__PURE__*/React.createElement("span", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 13,
      color: 'var(--ink-2)'
    }
  }, "Sensei"), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono",
    style: {
      marginLeft: 12
    }
  }, "v0.4.2")), /*#__PURE__*/React.createElement("div", {
    className: "flex gap-5"
  }, /*#__PURE__*/React.createElement("a", {
    href: "#privacy"
  }, "Privacy"), /*#__PURE__*/React.createElement("a", {
    href: "#faq"
  }, "FAQ"), /*#__PURE__*/React.createElement("a", {
    href: "#github"
  }, "GitHub"), /*#__PURE__*/React.createElement("a", {
    href: "#twitter"
  }, "Twitter"))));
}
window.Faq = Faq;
window.Footer = Footer;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/site/Footer.jsx", error: String((e && e.message) || e) }); }

// ui_kits/site/Mock.jsx
try { (() => {
// A miniature product screenshot — used in the hero and gallery.
// Not interactive; it's marketing.

function MockToday({
  width = 900,
  height = 560
}) {
  return /*#__PURE__*/React.createElement("div", {
    style: {
      width,
      maxWidth: '100%',
      height,
      background: 'var(--paper)',
      border: 'var(--hairline)',
      borderRadius: 'var(--radius-lg)',
      overflow: 'hidden',
      display: 'flex',
      flexDirection: 'column',
      boxShadow: 'var(--shadow-sm)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome"
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-traffic"
  }, /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null), /*#__PURE__*/React.createElement("span", null)), /*#__PURE__*/React.createElement("div", {
    className: "zs-chrome-title"
  }, "Sensei  \u5148\u751F  \xB7  today"), /*#__PURE__*/React.createElement("div", {
    style: {
      width: 54
    }
  })), /*#__PURE__*/React.createElement("div", {
    style: {
      flex: 1,
      display: 'flex',
      minHeight: 0
    }
  }, /*#__PURE__*/React.createElement("aside", {
    style: {
      width: 180,
      borderRight: 'var(--hairline)',
      padding: 16,
      fontSize: 12
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-2 mb-4"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 16,
      color: 'var(--accent)'
    }
  }, "\u5148"), /*#__PURE__*/React.createElement("span", {
    style: {
      fontFamily: 'var(--font-display)'
    }
  }, "Sensei")), /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mb-2",
    style: {
      fontSize: 9
    }
  }, "Observatory"), [["今", "Today", true], ["場", "Projects"], ["刻", "Sessions"], ["察", "Insights"], ["覚", "Memories"]].map(([k, l, active], i) => /*#__PURE__*/React.createElement("div", {
    key: i,
    style: {
      padding: '5px 8px',
      borderRadius: 4,
      background: active ? 'var(--paper-3)' : 'transparent',
      color: active ? 'var(--ink)' : 'var(--ink-2)',
      display: 'flex',
      gap: 8,
      alignItems: 'center',
      marginBottom: 1
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 11,
      color: active ? 'var(--accent)' : 'var(--ink-3)'
    }
  }, k), /*#__PURE__*/React.createElement("span", null, l)))), /*#__PURE__*/React.createElement("main", {
    style: {
      flex: 1,
      padding: 24,
      overflow: 'hidden'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow",
    style: {
      fontSize: 10
    }
  }, "Wed \xB7 22 Apr"), /*#__PURE__*/React.createElement("div", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 22,
      fontWeight: 400,
      marginTop: 6,
      marginBottom: 24
    }
  }, "Good morning, Aiko."), /*#__PURE__*/React.createElement("div", {
    style: {
      display: 'grid',
      gridTemplateColumns: '72px 1fr',
      gap: 20,
      paddingTop: 16,
      borderTop: 'var(--hairline)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-kanji",
    style: {
      fontSize: 56,
      color: 'var(--accent)',
      lineHeight: 1
    }
  }, "\u8074"), /*#__PURE__*/React.createElement("div", null, /*#__PURE__*/React.createElement("div", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 18,
      marginBottom: 8
    }
  }, "The AI does not know your auth."), /*#__PURE__*/React.createElement("div", {
    style: {
      fontSize: 11.5,
      color: 'var(--ink-2)',
      lineHeight: 1.55
    }
  }, "Three sessions corrected this week in lumen-auth \u2014 all touched refresh or device flow."))), /*#__PURE__*/React.createElement("div", {
    style: {
      marginTop: 20,
      paddingTop: 12,
      borderTop: 'var(--hairline)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mb-2",
    style: {
      fontSize: 9
    }
  }, "Also worth noticing"), [["繰", "Cache invalidation missed again in s-2891.", "3rd time", "warn"], ["昇", "Canvas smoothing pattern promoted to rule.", "+7%", "good"]].map(([k, t, tag, tone], i) => /*#__PURE__*/React.createElement("div", {
    key: i,
    className: "flex items-baseline gap-3",
    style: {
      padding: '8px 0',
      borderBottom: 'var(--hairline)'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 14,
      width: 20,
      color: tone === "warn" ? 'var(--warning)' : 'var(--success)'
    }
  }, k), /*#__PURE__*/React.createElement("span", {
    style: {
      flex: 1,
      fontSize: 11.5,
      color: 'var(--ink-2)'
    }
  }, t), /*#__PURE__*/React.createElement("span", {
    className: "zs-mono",
    style: {
      fontSize: 9.5,
      padding: '2px 6px',
      borderRadius: 3,
      color: tone === "warn" ? 'var(--warning)' : 'var(--success)',
      background: tone === "warn" ? 'var(--warning-soft)' : 'var(--success-soft)'
    }
  }, tag)))))));
}
window.MockToday = MockToday;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/site/Mock.jsx", error: String((e && e.message) || e) }); }

// ui_kits/site/Nav.jsx
try { (() => {
// Site nav — thin, brand mark on the left, links on the right.

function Nav() {
  const links = [["#how", "How it works"], ["#screens", "Screens"], ["#philosophy", "Philosophy"], ["#privacy", "Privacy"], ["#faq", "FAQ"]];
  return /*#__PURE__*/React.createElement("nav", {
    style: {
      maxWidth: 1100,
      margin: '0 auto',
      padding: 'var(--space-6) var(--space-7)',
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'space-between'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-2"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 20,
      letterSpacing: '-0.04em'
    }
  }, "\u5148\u751F"), /*#__PURE__*/React.createElement("span", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 17
    }
  }, "Sensei")), /*#__PURE__*/React.createElement("div", {
    className: "flex gap-7",
    style: {
      fontSize: 'var(--text-xs)'
    }
  }, links.map(([href, label]) => /*#__PURE__*/React.createElement("a", {
    key: href,
    href: href,
    className: "text-ink-2",
    style: {
      transition: 'color var(--dur-fast) var(--ease)'
    },
    onMouseEnter: e => e.currentTarget.style.color = 'var(--ink)',
    onMouseLeave: e => e.currentTarget.style.color = 'var(--ink-2)'
  }, label))));
}

// Auto-detected OS download CTA
function DownloadCTA({
  size = "lg"
}) {
  const [os, setOs] = React.useState("macOS");
  React.useEffect(() => {
    const ua = navigator.userAgent || "";
    if (/Win/.test(ua)) setOs("Windows");else if (/Linux/.test(ua)) setOs("Linux");else if (/Mac/.test(ua)) setOs("macOS");
  }, []);
  return /*#__PURE__*/React.createElement("a", {
    href: `#download-${os.toLowerCase()}`,
    className: `zs-btn zs-btn-primary ${size === "lg" ? "zs-btn-lg" : ""}`,
    style: {
      textDecoration: 'none'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      color: 'var(--accent)',
      fontSize: 16
    }
  }, "\u4E0B"), "Download for ", os);
}
window.Nav = Nav;
window.DownloadCTA = DownloadCTA;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/site/Nav.jsx", error: String((e && e.message) || e) }); }

// ui_kits/site/Sections.jsx
try { (() => {
// Section — generic eyebrow + title (left) and body (right) layout.

function Section({
  id,
  eyebrow,
  title,
  children,
  background,
  narrow
}) {
  return /*#__PURE__*/React.createElement("section", {
    id: id,
    style: {
      borderTop: 'var(--hairline)',
      background: background || 'var(--paper)',
      padding: 'var(--space-8) var(--space-7)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      maxWidth: narrow ? 760 : 1100,
      margin: '0 auto',
      display: narrow ? 'block' : 'grid',
      gridTemplateColumns: narrow ? undefined : '1fr 1.4fr',
      gap: 'var(--space-7)',
      alignItems: 'start',
      textAlign: narrow ? 'center' : 'left'
    }
  }, /*#__PURE__*/React.createElement("div", null, eyebrow && /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mb-3"
  }, eyebrow), /*#__PURE__*/React.createElement("h2", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-2xl)',
      fontWeight: 400,
      letterSpacing: '-0.015em',
      lineHeight: 1.25,
      margin: 0,
      maxWidth: narrow ? '100%' : undefined
    }
  }, title)), /*#__PURE__*/React.createElement("div", {
    className: "zs-body"
  }, children)));
}

// HowItWorks — 3-column Watch · Notice · Adopt block
function HowItWorks() {
  const steps = [{
    kanji: "観",
    phase: "Watch",
    text: "Sensei sits beside your editor and AI tools, capturing the shape of each session — the prompts, the responses, the corrections.",
    sub: "Local only. Nothing leaves your machine."
  }, {
    kanji: "察",
    phase: "Notice",
    text: "After a few days, patterns begin to surface. Recurring frictions. Idioms forming. Things you taught the assistant once and may want to teach it again.",
    sub: "You decide what's signal and what isn't."
  }, {
    kanji: "覚",
    phase: "Adopt",
    text: "Worthy patterns become memories — small, named lessons sensei can apply to future sessions on your behalf, with your blessing.",
    sub: "Adopt, refine, or dismiss. Always your call."
  }];
  return /*#__PURE__*/React.createElement("section", {
    id: "how",
    style: {
      borderTop: 'var(--hairline)',
      padding: 'var(--space-8) var(--space-7)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      maxWidth: 1100,
      margin: '0 auto'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mb-3"
  }, "How it works"), /*#__PURE__*/React.createElement("h2", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-3xl)',
      fontWeight: 300,
      letterSpacing: '-0.02em',
      margin: 0,
      marginBottom: 'var(--space-7)'
    }
  }, "\u89B3 \xB7 \u5BDF \xB7 \u899A \u2014 watch, notice, adopt."), /*#__PURE__*/React.createElement("div", {
    className: "grid grid-cols-3 gap-7"
  }, steps.map((s, i) => /*#__PURE__*/React.createElement("div", {
    key: i
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-3 mb-4"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 36,
      color: 'var(--accent)',
      lineHeight: 1
    }
  }, s.kanji), /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow"
  }, s.phase)), /*#__PURE__*/React.createElement("div", {
    className: "text-sm text-ink",
    style: {
      lineHeight: 1.65,
      marginBottom: 'var(--space-3)'
    }
  }, s.text), /*#__PURE__*/React.createElement("div", {
    className: "text-xs text-ink-3",
    style: {
      fontStyle: 'italic'
    }
  }, s.sub))))));
}

// Philosophy — centered single-kanji statement
function Philosophy() {
  return /*#__PURE__*/React.createElement("section", {
    id: "philosophy",
    style: {
      borderTop: 'var(--hairline)',
      padding: '120px var(--space-7)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    style: {
      maxWidth: 760,
      margin: '0 auto',
      textAlign: 'center'
    }
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 80,
      color: 'var(--accent)',
      lineHeight: 1
    }
  }, "\u9759"), /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow mt-4 mb-5"
  }, "Sei \xB7 stillness"), /*#__PURE__*/React.createElement("h2", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-2xl)',
      fontWeight: 300,
      lineHeight: 1.3,
      letterSpacing: '-0.02em',
      margin: '0 0 var(--space-5)'
    }
  }, "The master observes for a long time before teaching."), /*#__PURE__*/React.createElement("p", {
    className: "zs-body",
    style: {
      maxWidth: 600,
      margin: '0 auto'
    }
  }, "AI tools are getting louder. More suggestions, more autocompletes, more interrupting. Sensei moves the other way. It speaks rarely, and only when it has something specific to say. Most days it is completely silent \u2014 and that is the feature.")));
}
window.Section = Section;
window.HowItWorks = HowItWorks;
window.Philosophy = Philosophy;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/site/Sections.jsx", error: String((e && e.message) || e) }); }

// ui_kits/site/SiteHero.jsx
try { (() => {
// SiteHero — kanji anchor, eyebrow, big display headline, lead, CTA + product mock.

function SiteHero() {
  const {
    DownloadCTA,
    MockToday
  } = window;
  return /*#__PURE__*/React.createElement("section", {
    style: {
      maxWidth: 1100,
      margin: '0 auto',
      padding: 'var(--space-6) var(--space-7) var(--space-8)'
    }
  }, /*#__PURE__*/React.createElement("div", {
    className: "flex items-baseline gap-3 mb-5"
  }, /*#__PURE__*/React.createElement("span", {
    className: "zs-kanji",
    style: {
      fontSize: 56,
      color: 'var(--accent)',
      lineHeight: 1
    }
  }, "\u89B3"), /*#__PURE__*/React.createElement("div", {
    className: "zs-eyebrow"
  }, "Kan \xB7 to observe")), /*#__PURE__*/React.createElement("h1", {
    style: {
      fontFamily: 'var(--font-display)',
      fontSize: 'var(--text-4xl)',
      fontWeight: 300,
      lineHeight: 1.1,
      letterSpacing: '-0.025em',
      maxWidth: 820,
      margin: 0
    }
  }, "A quiet companion for AI-assisted work."), /*#__PURE__*/React.createElement("p", {
    className: "zs-body",
    style: {
      marginTop: 'var(--space-5)',
      maxWidth: 560
    }
  }, "Sensei watches your sessions with AI assistants \u2014 then surfaces the patterns you're too close to see. Not a chatbot. Not a copilot. A patient observer."), /*#__PURE__*/React.createElement("div", {
    className: "flex items-center gap-4",
    style: {
      marginTop: 'var(--space-6)'
    }
  }, /*#__PURE__*/React.createElement(DownloadCTA, {
    size: "lg"
  }), /*#__PURE__*/React.createElement("a", {
    href: "#how",
    className: "text-ink-2",
    style: {
      fontSize: 'var(--text-sm)'
    }
  }, "See how it works \u2193")), /*#__PURE__*/React.createElement("div", {
    className: "text-xs text-ink-3 mt-3"
  }, "Free \xB7 Local-first \xB7 No account"), /*#__PURE__*/React.createElement("div", {
    style: {
      marginTop: 'var(--space-7)',
      display: 'flex',
      justifyContent: 'center'
    }
  }, /*#__PURE__*/React.createElement(MockToday, {
    width: 900,
    height: 520
  })));
}
window.SiteHero = SiteHero;
})(); } catch (e) { __ds_ns.__errors.push({ path: "ui_kits/site/SiteHero.jsx", error: String((e && e.message) || e) }); }

__ds_ns.WindowChrome = __ds_scope.WindowChrome;

__ds_ns.Badge = __ds_scope.Badge;

__ds_ns.Button = __ds_scope.Button;

__ds_ns.Eyebrow = __ds_scope.Eyebrow;

__ds_ns.IconButton = __ds_scope.IconButton;

__ds_ns.Kanji = __ds_scope.Kanji;

__ds_ns.StatusDot = __ds_scope.StatusDot;

__ds_ns.Insight = __ds_scope.Insight;

__ds_ns.Sparkline = __ds_scope.Sparkline;

__ds_ns.Input = __ds_scope.Input;

__ds_ns.Card = __ds_scope.Card;

})();
