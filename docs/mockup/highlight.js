/* ============================================================
   Tiny syntax highlighter.
   Returns token DATA only — { t: text, c: tokenClass } — so all
   color lives in the semantic system (tok-* utilities). No colors here.
   Supported langs: yaml, sh, text. Falls back to plain.
   ============================================================ */
(function () {
  function plainLine(line) {
    return [{ t: line, c: "punct" }];
  }

  // YAML: keys, comments, $VARS, [lists], strings
  function yamlLine(line) {
    const toks = [];
    const commentIdx = line.indexOf("#");
    let code = line, comment = "";
    if (commentIdx >= 0) {
      code = line.slice(0, commentIdx);
      comment = line.slice(commentIdx);
    }
    const re = /(\s+)|([A-Za-z0-9_-]+)(\s*:)|(\$[A-Za-z_][A-Za-z0-9_]*)|([\[\],])|(.)/g;
    let m;
    while ((m = re.exec(code)) !== null) {
      if (m[1]) toks.push({ t: m[1], c: "ws" });
      else if (m[2]) { toks.push({ t: m[2], c: "key" }); toks.push({ t: m[3], c: "punct" }); }
      else if (m[4]) toks.push({ t: m[4], c: "num" });
      else if (m[5]) toks.push({ t: m[5], c: "punct" });
      else if (m[6]) toks.push({ t: m[6], c: "str" });
    }
    if (comment) toks.push({ t: comment, c: "comment" });
    return toks.length ? toks : plainLine(line);
  }

  // SHELL: $ prompt, # comments, --flags, "strings"
  function shLine(line) {
    const toks = [];
    let rest = line;
    const promptM = rest.match(/^(\s*\$\s)/);
    if (promptM) { toks.push({ t: promptM[1], c: "arrow" }); rest = rest.slice(promptM[1].length); }
    if (/^\s*#/.test(rest)) { toks.push({ t: rest, c: "comment" }); return toks; }
    const re = /(\s+)|("[^"]*")|(--?[A-Za-z][\w-]*)|(\bdbd\b)|([^\s]+)/g;
    let m;
    while ((m = re.exec(rest)) !== null) {
      if (m[1]) toks.push({ t: m[1], c: "ws" });
      else if (m[2]) toks.push({ t: m[2], c: "str" });
      else if (m[3]) toks.push({ t: m[3], c: "num" });
      else if (m[4]) toks.push({ t: m[4], c: "key" });
      else if (m[5]) toks.push({ t: m[5], c: "punct" });
    }
    return toks.length ? toks : plainLine(line);
  }

  // TEXT mapping lines: "left  → right"  (arrow + comment rhs)
  function textLine(line) {
    const idx = line.indexOf("→");
    if (idx >= 0) {
      return [
        { t: line.slice(0, idx), c: "punct" },
        { t: "→", c: "arrow" },
        { t: line.slice(idx + 1), c: "comment" },
      ];
    }
    return plainLine(line);
  }

  function highlight(source, lang) {
    const lines = String(source).split("\n");
    const fn = lang === "yaml" ? yamlLine : lang === "sh" ? shLine : lang === "text" ? textLine : plainLine;
    return lines.map(fn);
  }

  window.highlightCode = highlight;
})();
