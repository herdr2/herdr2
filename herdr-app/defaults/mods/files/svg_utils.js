// Shared SVG escaping, text metrics, color blending, and data-URL helpers for Mermaid & Math rendering.

export function escapeXml(str) {
  return String(str ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

export function svgToDataUrl(svgString) {
  const encoded = encodeURIComponent(String(svgString ?? ""))
    .replace(/\(/g, "%28")
    .replace(/\)/g, "%29")
    .replace(/'/g, "%27");
  return `data:image/svg+xml,${encoded}`;
}

export function isWideChar(code) {
  return (
    (code >= 0x1100 && code <= 0x115f) ||
    (code >= 0x2e80 && code <= 0x303e) ||
    (code >= 0x3040 && code <= 0xa4cf) ||
    (code >= 0xac00 && code <= 0xd7a3) ||
    (code >= 0xf900 && code <= 0xfaff) ||
    (code >= 0xfe10 && code <= 0xfe6f) ||
    (code >= 0xff01 && code <= 0xff60) ||
    (code >= 0xffe0 && code <= 0xffe6) ||
    (code >= 0x1f300 && code <= 0x1faff) ||
    (code >= 0x20000 && code <= 0x2fffd)
  );
}

const NARROW_CHARS = new Set("ilI1.,:;!'|[]()fltjr` ".split(""));
const WIDE_ASCII = new Set("mwMW@#%&QOGD".split(""));

export function measureTextWidth(text, fontSize = 13, opts = {}) {
  const str = String(text ?? "");
  if (!str) return 0;
  const mono = Boolean(opts.mono);
  const bold = Boolean(opts.bold);
  const italic = Boolean(opts.italic);
  const scale = (bold ? 1.05 : 1.0) * (italic ? 1.02 : 1.0);
  let width = 0;
  for (const ch of str) {
    const code = ch.codePointAt(0) ?? 0;
    if (isWideChar(code)) {
      width += fontSize * 1.02;
    } else if (mono) {
      width += fontSize * 0.62;
    } else if (ch === " ") {
      width += fontSize * 0.3;
    } else if (NARROW_CHARS.has(ch)) {
      width += fontSize * 0.34;
    } else if (WIDE_ASCII.has(ch)) {
      width += fontSize * 0.84;
    } else if (code >= 0x41 && code <= 0x5a) {
      width += fontSize * 0.68;
    } else if (code >= 0x30 && code <= 0x39) {
      width += fontSize * 0.58;
    } else if (code >= 0x0370 && code <= 0x03ff) {
      // Greek
      width += fontSize * 0.62;
    } else if (code >= 0x2190 && code <= 0x22ff) {
      // Arrows & math operators
      width += fontSize * 0.78;
    } else {
      width += fontSize * 0.56;
    }
  }
  return width * scale;
}

export function splitBrLines(raw) {
  const normalized = String(raw ?? "")
    .replace(/<br\s*\/?>/gi, "\n")
    .replace(/\\n/g, "\n");
  return normalized.split("\n").map((line) => line.trim());
}

export function wrapLines(raw, maxWidth, fontSize = 13, opts = {}) {
  const baseLines = splitBrLines(raw);
  if (!maxWidth || maxWidth <= 0) return baseLines.length ? baseLines : [""];
  const out = [];
  for (const line of baseLines) {
    if (!line || measureTextWidth(line, fontSize, opts) <= maxWidth) {
      out.push(line);
      continue;
    }
    let cur = "";
    let curW = 0;
    const tokens = line.split(/(\s+)/);
    for (const tok of tokens) {
      if (!tok) continue;
      const tokW = measureTextWidth(tok, fontSize, opts);
      if (cur && curW + tokW > maxWidth) {
        out.push(cur.trimEnd());
        cur = tok.trimStart();
        curW = measureTextWidth(cur, fontSize, opts);
        if (curW > maxWidth) {
          // Break long CJK or unspaced token by characters
          let seg = "";
          let segW = 0;
          for (const ch of cur) {
            const cw = measureTextWidth(ch, fontSize, opts);
            if (seg && segW + cw > maxWidth) {
              out.push(seg);
              seg = ch;
              segW = cw;
            } else {
              seg += ch;
              segW += cw;
            }
          }
          cur = seg;
          curW = segW;
        }
      } else {
        cur += tok;
        curW += tokW;
      }
    }
    if (cur) out.push(cur.trimEnd());
  }
  return out.length ? out : [""];
}

function parseHexRgb(hex) {
  const s = String(hex ?? "").trim();
  if (/^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(s)) {
    return [
      parseInt(s.slice(1, 3), 16),
      parseInt(s.slice(3, 5), 16),
      parseInt(s.slice(5, 7), 16),
    ];
  }
  if (/^#[0-9a-fA-F]{3}$/.test(s)) {
    return [
      parseInt(s[1] + s[1], 16),
      parseInt(s[2] + s[2], 16),
      parseInt(s[3] + s[3], 16),
    ];
  }
  return null;
}

export function mixHex(hexA, hexB, t = 0.5) {
  const a = parseHexRgb(hexA);
  const b = parseHexRgb(hexB);
  if (!a && !b) return "#808080";
  if (!a) return hexB;
  if (!b) return hexA;
  const k = Math.max(0, Math.min(1, t));
  const r = Math.round(a[0] + (b[0] - a[0]) * k);
  const g = Math.round(a[1] + (b[1] - a[1]) * k);
  const bl = Math.round(a[2] + (b[2] - a[2]) * k);
  const toHex = (n) => n.toString(16).padStart(2, "0");
  return `#${toHex(r)}${toHex(g)}${toHex(bl)}`;
}

export function fmtNum(n) {
  const v = Number(n);
  if (!Number.isFinite(v)) return "0";
  return Math.abs(v) < 0.005 ? "0" : String(Math.round(v * 100) / 100);
}
