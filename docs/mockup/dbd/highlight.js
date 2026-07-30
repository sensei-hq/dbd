/**
 * dbd — tiny syntax highlighter for the code frames.
 * Pure function: (source, lang) -> rows of tokens, each carrying the utility
 * class that colours it. Colours themselves live in uno.config.js (tok-*).
 *
 * rows: Array<{ tokens: Array<{ t: string, cls: string }> }>
 */
(function () {
  var CLS = {
    punct: 'text-tok-punct', key: 'text-tok-key', str: 'text-tok-str',
    num: 'text-tok-num', comment: 'text-tok-comment', arrow: 'text-tok-arrow', ws: '',
  };

  function plain(line) { return [{ t: line, c: 'punct' }]; }

  function yamlLine(line) {
    var toks = [], ci = line.indexOf('#'), code = line, comment = '';
    if (ci >= 0) { code = line.slice(0, ci); comment = line.slice(ci); }
    var re = /(\s+)|([A-Za-z0-9_-]+)(\s*:)|(\$[A-Za-z_][A-Za-z0-9_]*)|([\[\],])|(.)/g, m;
    while ((m = re.exec(code)) !== null) {
      if (m[1]) toks.push({ t: m[1], c: 'ws' });
      else if (m[2]) { toks.push({ t: m[2], c: 'key' }); toks.push({ t: m[3], c: 'punct' }); }
      else if (m[4]) toks.push({ t: m[4], c: 'num' });
      else if (m[5]) toks.push({ t: m[5], c: 'punct' });
      else if (m[6]) toks.push({ t: m[6], c: 'str' });
    }
    if (comment) toks.push({ t: comment, c: 'comment' });
    return toks.length ? toks : plain(line);
  }

  function shLine(line) {
    var toks = [], rest = line, pm = rest.match(/^(\s*\$\s)/);
    if (pm) { toks.push({ t: pm[1], c: 'arrow' }); rest = rest.slice(pm[1].length); }
    if (/^\s*#/.test(rest)) return [{ t: rest, c: 'comment' }];
    var re = /(\s+)|("[^"]*")|(--?[A-Za-z][\w-]*)|(\bdbd\b)|([^\s]+)/g, m;
    while ((m = re.exec(rest)) !== null) {
      if (m[1]) toks.push({ t: m[1], c: 'ws' });
      else if (m[2]) toks.push({ t: m[2], c: 'str' });
      else if (m[3]) toks.push({ t: m[3], c: 'num' });
      else if (m[4]) toks.push({ t: m[4], c: 'key' });
      else if (m[5]) toks.push({ t: m[5], c: 'punct' });
    }
    return toks.length ? toks : plain(line);
  }

  function textLine(line) {
    var i = line.indexOf('\u2192');
    if (i < 0) return plain(line);
    return [
      { t: line.slice(0, i), c: 'punct' },
      { t: '\u2192', c: 'arrow' },
      { t: line.slice(i + 1), c: 'comment' },
    ];
  }

  function tokenize(source, lang) {
    var fn = lang === 'yaml' ? yamlLine : lang === 'sh' ? shLine : lang === 'text' ? textLine : plain;
    return String(source == null ? '' : source).split('\n').map(function (line) {
      var toks = fn(line);
      return { tokens: toks.map(function (tk) { return { t: tk.t, cls: CLS[tk.c] || '' }; }) };
    });
  }

  // terminal transcripts: one row per line, gutter $ only on commands
  var LINE_CLS = { cmd: 'text-fg', out: 'text-muted', ok: 'text-tok-str' };
  function transcript(lines) {
    return (lines || []).map(function (ln) {
      return {
        gutterCls: ln.type === 'cmd' ? 'text-accent' : 'text-transparent',
        tokens: [{ t: ln.text, cls: LINE_CLS[ln.type] || 'text-muted' }],
      };
    });
  }

  var api = { tokenize: tokenize, transcript: transcript };
  if (typeof window !== 'undefined') window.DBD_HL = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})();
