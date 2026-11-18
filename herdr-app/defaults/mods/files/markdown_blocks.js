// Lossless Markdown block parser, serializer, task toggling, and remote image rewriting for live Preview editing.
import { resolveRelativeAssetPath } from "./themes.js";

const ALERT_LABELS = {
  NOTE: "ⓘ Note",
  TIP: "💡 Tip",
  IMPORTANT: "❗ Important",
  WARNING: "⚠ Warning",
  CAUTION: "🛑 Caution",
};

const REMOTE_IMAGE_EXT_RE = /\.(png|jpe?g|gif|webp|svg|bmp|ico|tiff?)(?:[?#].*)?$/i;
const MULTI_HTML_TAGS = new Set(["details", "table", "figure", "picture", "svg", "pre", "div", "section", "article"]);

export function splitLinesWithEndings(raw) {
  const source = String(raw ?? "");
  const lines = [];
  let start = 0;
  for (let i = 0; i < source.length; i++) {
    if (source[i] === "\n") {
      const hasCr = i > start && source[i - 1] === "\r";
      lines.push({
        text: source.slice(start, hasCr ? i - 1 : i),
        eol: hasCr ? "\r\n" : "\n",
      });
      start = i + 1;
    } else if (source[i] === "\r" && (i + 1 >= source.length || source[i + 1] !== "\n")) {
      lines.push({ text: source.slice(start, i), eol: "\r" });
      start = i + 1;
    }
  }
  if (start < source.length) {
    lines.push({ text: source.slice(start), eol: "" });
  }
  return lines;
}

const isBlank = (line) => line.text.trim() === "";

function matchFenceStart(text) {
  const m = text.match(/^\s{0,3}(`{3,}|~{3,})(.*)$/);
  if (!m) return null;
  const marker = m[1];
  const info = m[2] ?? "";
  if (marker[0] === "`" && info.includes("`")) return null;
  return { char: marker[0], len: marker.length, lang: info.trim().split(/\s+/)[0] || "" };
}

function isFenceEnd(text, char, len) {
  const trimmed = text.trim();
  if (trimmed.length < len) return false;
  for (let i = 0; i < trimmed.length; i++) {
    if (trimmed[i] !== char) return false;
  }
  return true;
}

const isAtxHeading = (text) => /^\s{0,3}#{1,6}(?:\s+.*|$)/.test(text);
const isSetextUnderline = (text) => /^\s{0,3}(?:=+|-+)\s*$/.test(text);
const isHr = (text) => /^\s{0,3}(?:(?:\*\s*){3,}|(?:-\s*){3,}|(?:_\s*){3,})\s*$/.test(text);
const isTableSeparator = (text) =>
  /^\s*\|?\s*:?-{1,}:?\s*(?:\|\s*:?-{1,}:?\s*)+\|?\s*$/.test(text) ||
  /^\s*\|\s*:?-{1,}:?\s*\|\s*$/.test(text);
const isBlockquote = (text) => /^\s{0,3}>/.test(text);
const isListItem = (text) => /^\s*(?:[-*+]|\d{1,9}[.)])(?:\s+.*|$)/.test(text);
const isRefDef = (text) => /^\s{0,3}\[[^\]]+\]:\s*\S+/.test(text);
const isStandaloneImage = (text) =>
  /^\s*(?:!\[[^\]]*\]\([^)]*\)|\[!\[[^\]]*\]\([^)]*\)\]\([^)]*\)|!\[[^\]]*\]\[[^\]]*\]|<img\b[^>]*>)\s*$/i.test(text);

function parseTaskLine(text) {
  const m = text.match(/^(\s*(?:[-*+]|\d{1,9}[.)])\s+\[)([ xX])(\]\s*)(.*)$/);
  if (!m) return null;
  return { checked: m[2].toLowerCase() === "x", label: m[4].trim() };
}

function isBlockStarter(lines, i) {
  const text = lines[i].text;
  if (matchFenceStart(text) || isAtxHeading(text) || isHr(text) || isBlockquote(text) || isListItem(text) || isRefDef(text)) {
    return true;
  }
  if (text.includes("|") && i + 1 < lines.length && isTableSeparator(lines[i + 1].text)) {
    return true;
  }
  if (/^\s*<!--/.test(text)) return true;
  const html = text.match(/^\s*<([a-zA-Z][a-zA-Z0-9-]*)\b/);
  if (html && MULTI_HTML_TAGS.has(html[1].toLowerCase())) return true;
  return false;
}

function extractRawSlice(lines, start, end) {
  let out = "";
  for (let k = start; k < end - 1; k++) {
    out += lines[k].text + lines[k].eol;
  }
  out += lines[end - 1].text;
  return out;
}

export function parseMarkdownBlocks(raw) {
  const lines = splitLinesWithEndings(raw);
  let i = 0;
  let leading = "";
  while (i < lines.length && isBlank(lines[i])) {
    leading += lines[i].text + lines[i].eol;
    i++;
  }

  const blocks = [];
  const defLines = [];

  while (i < lines.length) {
    const start = i;
    let end = i + 1;
    let kind = "paragraph";
    let lang = "";

    const first = lines[i].text;
    const fence = matchFenceStart(first);

    if (i === 0 && first.trim() === "---" && lines.length > 2 && !isBlank(lines[1])) {
      let close = -1;
      for (let k = 1; k < lines.length; k++) {
        if (isBlank(lines[k])) break;
        if (/^\s*(?:---|\.\.\.)\s*$/.test(lines[k].text)) {
          close = k;
          break;
        }
      }
      if (close > 1) {
        kind = "frontmatter";
        end = close + 1;
      }
    }

    if (kind === "paragraph" && end === start + 1 && fence) {
      kind = "code";
      lang = fence.lang;
      end = start + 1;
      while (end < lines.length) {
        const lineText = lines[end].text;
        end++;
        if (isFenceEnd(lineText, fence.char, fence.len)) break;
      }
    } else if (kind === "paragraph" && /^\s*<!--/.test(first)) {
      kind = "html";
      if (!first.includes("-->")) {
        while (end < lines.length) {
          const hasEnd = lines[end].text.includes("-->");
          end++;
          if (hasEnd) break;
        }
      }
    } else if (kind === "paragraph") {
      const htmlMatch = first.match(/^\s*<([a-zA-Z][a-zA-Z0-9-]*)\b/);
      const tagName = htmlMatch ? htmlMatch[1].toLowerCase() : "";
      if (tagName && MULTI_HTML_TAGS.has(tagName)) {
        const openRe = new RegExp(`<${tagName}\\b[^>]*>`, "gi");
        const closeRe = new RegExp(`</${tagName}\\s*>`, "gi");
        let depth = (first.match(openRe) || []).length - (first.match(closeRe) || []).length;
        if (depth <= 0 || /\/>\s*$/.test(first)) {
          kind = "html";
        } else {
          let scan = end;
          while (scan < lines.length && depth > 0) {
            const t = lines[scan].text;
            depth += (t.match(openRe) || []).length - (t.match(closeRe) || []).length;
            scan++;
          }
          if (depth <= 0) {
            kind = "html";
            end = scan;
          }
        }
      } else if (isAtxHeading(first)) {
        kind = "heading";
      } else if (isHr(first)) {
        kind = "hr";
      } else if (first.includes("|") && start + 1 < lines.length && isTableSeparator(lines[start + 1].text)) {
        kind = "table";
        end = start + 2;
        while (end < lines.length && !isBlank(lines[end]) && lines[end].text.includes("|") && !matchFenceStart(lines[end].text) && !isAtxHeading(lines[end].text)) {
          end++;
        }
      } else if (isBlockquote(first)) {
        kind = "blockquote";
        while (end < lines.length && !isBlank(lines[end])) {
          const t = lines[end].text;
          if (!isBlockquote(t) && (matchFenceStart(t) || isAtxHeading(t) || isHr(t) || isListItem(t))) break;
          end++;
        }
      } else if (isListItem(first)) {
        kind = "list";
        while (end < lines.length) {
          if (!isBlank(lines[end])) {
            const t = lines[end].text;
            if (!isListItem(t) && !/^\s{2,}\S/.test(t) && (isAtxHeading(t) || matchFenceStart(t) || isHr(t) || isBlockquote(t) || isRefDef(t))) {
              break;
            }
            if (!isListItem(t) && !/^\s{2,}/.test(t) && end > start && isBlank(lines[end - 1])) {
              break;
            }
            end++;
          } else {
            let k = end + 1;
            while (k < lines.length && isBlank(lines[k])) k++;
            if (k >= lines.length) break;
            if (k - end > 1) break;
            if (!isListItem(lines[k].text) && !/^\s{2,}\S/.test(lines[k].text)) break;
            end = k + 1;
          }
        }
      } else if (isRefDef(first)) {
        kind = "definition";
        while (end < lines.length && !isBlank(lines[end]) && isRefDef(lines[end].text)) {
          end++;
        }
      } else if (isStandaloneImage(first) && (start + 1 >= lines.length || isBlank(lines[start + 1]) || isBlockStarter(lines, start + 1))) {
        kind = "image";
      } else {
        while (end < lines.length && !isBlank(lines[end])) {
          if (end === start + 1 && isSetextUnderline(lines[end].text)) {
            kind = "heading";
            end++;
            break;
          }
          if (isBlockStarter(lines, end)) break;
          end++;
        }
      }
    }

    const blockRaw = extractRawSlice(lines, start, end);
    let gapAfter = lines[end - 1].eol;
    let next = end;
    while (next < lines.length && isBlank(lines[next])) {
      gapAfter += lines[next].text + lines[next].eol;
      next++;
    }

    const tasks = [];
    if (kind === "list") {
      for (let k = start; k < end; k++) {
        const parsed = parseTaskLine(lines[k].text);
        if (parsed) tasks.push({ lineIndex: k - start, ...parsed });
      }
    }
    if (kind === "definition") {
      for (let k = start; k < end; k++) {
        defLines.push(lines[k].text);
      }
    }

    blocks.push({ raw: blockRaw, gapAfter, kind, lang, headingLevel: kind === "heading" ? detectHeadingLevel(blockRaw) : 0, tasks });
    i = next;
  }

  return { leading, blocks, defs: defLines.join("\n") };
}

export function serializeMarkdownBlocks(doc) {
  if (!doc) return "";
  let out = doc.leading ?? "";
  for (const block of doc.blocks ?? []) {
    out += (block.raw ?? "") + (block.gapAfter ?? "");
  }
  return out;
}

export function toggleMarkdownTask(blockRaw, lineIndex) {
  const lines = splitLinesWithEndings(blockRaw);
  const line = lines[lineIndex];
  if (!line) return blockRaw;
  const m = line.text.match(/^(\s*(?:[-*+]|\d{1,9}[.)])\s+\[)([ xX])(\]\s*.*)$/);
  if (!m) return blockRaw;
  const nextMark = m[2].toLowerCase() === "x" ? " " : "x";
  lines[lineIndex] = { ...line, text: `${m[1]}${nextMark}${m[3]}` };
  return lines.map((l) => l.text + l.eol).join("");
}

function splitMarkdownImageDestination(inner) {
  const trimmed = String(inner ?? "").trim();
  if (!trimmed) return { target: "", tail: "" };
  if (trimmed.startsWith("<")) {
    const close = trimmed.indexOf(">");
    if (close > 0) {
      return { target: trimmed.slice(1, close).trim(), tail: trimmed.slice(close + 1), angle: true };
    }
  }
  const ws = trimmed.search(/\s/);
  if (ws < 0) return { target: trimmed, tail: "", angle: false };
  return { target: trimmed.slice(0, ws), tail: trimmed.slice(ws), angle: false };
}

function splitOutsideInlineCode(line, transform) {
  let out = "";
  let i = 0;
  let plainStart = 0;
  while (i < line.length) {
    if (line[i] === "`") {
      let ticks = 0;
      while (i + ticks < line.length && line[i + ticks] === "`") ticks++;
      const marker = "`".repeat(ticks);
      const close = line.indexOf(marker, i + ticks);
      if (close >= 0) {
        out += transform(line.slice(plainStart, i));
        out += line.slice(i, close + ticks);
        i = close + ticks;
        plainStart = i;
        continue;
      }
      i += ticks;
      continue;
    }
    i++;
  }
  out += transform(line.slice(plainStart));
  return out;
}

function rewriteImageTargetsInSegment(segment, remoteImages, currentFile) {
  let out = segment.replace(/!\[([^\]]*)\]\(([^)]+)\)/g, (full, alt, inner) => {
    const { target, tail } = splitMarkdownImageDestination(inner);
    const rel = resolveRelativeAssetPath(currentFile, target);
    const dataUrl = rel ? remoteImages.get(rel) : null;
    return dataUrl ? `![${alt}](${dataUrl}${tail})` : full;
  });
  out = out.replace(/<img\b([^>]*?)\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)')([^>]*)>/gi, (full, pre, dq, sq, post) => {
    const target = dq ?? sq ?? "";
    const rel = resolveRelativeAssetPath(currentFile, target);
    const dataUrl = rel ? remoteImages.get(rel) : null;
    if (!dataUrl) return full;
    const quote = dq !== undefined ? '"' : "'";
    return `<img${pre}src=${quote}${dataUrl}${quote}${post}>`;
  });
  return out;
}

export function extractMarkdownImageUrls(raw) {
  const lines = String(raw ?? "").split(/\r?\n/);
  const urls = [];
  const seen = new Set();
  const add = (target) => {
    const t = String(target ?? "").trim();
    if (!t || seen.has(t)) return;
    seen.add(t);
    urls.push(t);
  };
  let fenceChar = "";
  let fenceLen = 0;
  for (const line of lines) {
    const fence = matchFenceStart(line);
    if (!fenceChar && fence) {
      fenceChar = fence.char;
      fenceLen = fence.len;
      continue;
    }
    if (fenceChar) {
      if (isFenceEnd(line, fenceChar, fenceLen)) {
        fenceChar = "";
        fenceLen = 0;
      }
      continue;
    }
    const def = line.match(/^\s{0,3}\[[^\]]+\]:\s*(\S+)/);
    if (def) {
      const target = def[1].replace(/^<|>$/g, "");
      if (REMOTE_IMAGE_EXT_RE.test(target)) add(target);
      continue;
    }
    splitOutsideInlineCode(line, (segment) => {
      for (const m of segment.matchAll(/!\[[^\]]*\]\(([^)]+)\)/g)) {
        const { target } = splitMarkdownImageDestination(m[1]);
        add(target);
      }
      for (const m of segment.matchAll(/<img\b[^>]*?\bsrc\s*=\s*(?:"([^"]*)"|'([^']*)')[^>]*>/gi)) {
        add(m[1] ?? m[2] ?? "");
      }
      return segment;
    });
  }
  return urls;
}

export function prepareBlockMarkdown(blockRaw, defs = "", remoteImages = null, currentFile = "") {
  const source = String(blockRaw ?? "");
  const hasAlerts = source.includes("[!");
  const hasRemote = Boolean(remoteImages && remoteImages.size > 0);
  let body = source;

  if (hasAlerts || hasRemote) {
    const lines = source.split("\n");
    let fenceChar = "";
    let fenceLen = 0;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      const fence = line.match(/^\s*(`{3,}|~{3,})/);
      if (fence) {
        const marker = fence[1];
        if (!fenceChar) {
          fenceChar = marker[0];
          fenceLen = marker.length;
        } else if (marker[0] === fenceChar && marker.length >= fenceLen) {
          fenceChar = "";
          fenceLen = 0;
        }
        continue;
      }
      if (fenceChar) continue;
      let nextLine = line;
      if (hasAlerts) {
        const alert = nextLine.match(/^(\s*>\s*)\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*(.*)$/i);
        if (alert) {
          const kind = alert[2].toUpperCase();
          const label = ALERT_LABELS[kind] ?? kind;
          const tail = alert[3] ? ` ${alert[3]}` : "";
          nextLine = `${alert[1]}**${label}**${tail}`;
        }
      }
      if (hasRemote) {
        const def = nextLine.match(/^(\s{0,3}\[[^\]]+\]:\s*)(\S+)(.*)$/);
        if (def) {
          const rel = resolveRelativeAssetPath(currentFile, def[2].replace(/^<|>$/g, ""));
          const dataUrl = rel ? remoteImages.get(rel) : null;
          if (dataUrl) nextLine = `${def[1]}${dataUrl}${def[3]}`;
        } else {
          nextLine = splitOutsideInlineCode(nextLine, (seg) =>
            rewriteImageTargetsInSegment(seg, remoteImages, currentFile)
          );
        }
      }
      lines[i] = nextLine;
    }
    body = lines.join("\n");
  }

  if (defs && body.includes("[") && body.trim() !== defs.trim()) {
    const rewrittenDefs = hasRemote
      ? defs
          .split("\n")
          .map((line) => {
            const def = line.match(/^(\s{0,3}\[[^\]]+\]:\s*)(\S+)(.*)$/);
            if (!def) return line;
            const rel = resolveRelativeAssetPath(currentFile, def[2].replace(/^<|>$/g, ""));
            const dataUrl = rel ? remoteImages.get(rel) : null;
            return dataUrl ? `${def[1]}${dataUrl}${def[3]}` : line;
          })
          .join("\n")
      : defs;
    return `${body}\n\n${rewrittenDefs}`;
  }
  return body;
}

export function estimateEditorRows(text, wrapCols = 56) {
  const lines = String(text ?? "").split("\n");
  let rows = 0;
  for (const line of lines) {
    rows += Math.max(1, Math.ceil(Math.max(1, line.length) / wrapCols));
  }
  return rows;
}

export function detectHeadingLevel(raw) {
  const lines = splitLinesWithEndings(raw);
  if (lines.length === 0) return 0;
  const atx = lines[0].text.match(/^\s{0,3}(#{1,6})(?:\s+.*|$)/);
  if (atx) return atx[1].length;
  if (lines.length === 2 && lines[0].text.trim() && isSetextUnderline(lines[1].text)) {
    return lines[1].text.trim()[0] === "=" ? 1 : 2;
  }
  return 0;
}

function splitTableCells(lineText) {
  let s = String(lineText ?? "").trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) {
    let bs = 0;
    for (let idx = s.length - 2; idx >= 0 && s[idx] === "\\"; idx--) bs++;
    if (bs % 2 === 0) s = s.slice(0, -1);
  }
  const cells = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && i + 1 < s.length && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (s[i] === "|") {
      cells.push(cur.trim());
      cur = "";
    } else {
      cur += s[i];
    }
  }
  cells.push(cur.trim());
  return cells;
}

function parseTableAlignment(cell) {
  const trimmed = String(cell ?? "").trim();
  const left = trimmed.startsWith(":");
  const right = trimmed.endsWith(":");
  if (left && right) return "center";
  if (right) return "right";
  if (left) return "left";
  return null;
}

export function parseMarkdownTable(raw) {
  const lines = splitLinesWithEndings(raw);
  if (lines.length < 2 || !isTableSeparator(lines[1].text)) return null;
  const headerCells = splitTableCells(lines[0].text);
  const sepCells = splitTableCells(lines[1].text);
  const rawRows = lines.slice(2).map((l) => splitTableCells(l.text));
  const colCount = Math.max(1, headerCells.length, sepCells.length, ...rawRows.map((r) => r.length));
  const padRow = (cells) => Array.from({ length: colCount }, (_, i) => cells[i] ?? "");
  const headers = padRow(headerCells);
  const alignments = Array.from({ length: colCount }, (_, i) =>
    i < sepCells.length ? parseTableAlignment(sepCells[i]) : null
  );
  const rows = rawRows.map(padRow);
  const eol = lines[0].eol || "\n";
  return { headers, alignments, rows, eol };
}

function escapeTableCell(value) {
  const s = String(value ?? "").replace(/\r?\n/g, " ").trim();
  let out = "";
  let backslashes = 0;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    if (ch === "\\") {
      backslashes++;
      out += ch;
    } else if (ch === "|") {
      if (backslashes % 2 === 0) out += "\\";
      out += "|";
      backslashes = 0;
    } else {
      backslashes = 0;
      out += ch;
    }
  }
  return out;
}

function formatSeparatorCell(align, width) {
  const w = Math.max(3, width);
  if (align === "center") return `:${"-".repeat(Math.max(1, w - 2))}:`;
  if (align === "right") return `${"-".repeat(Math.max(2, w - 1))}:`;
  if (align === "left") return `:${"-".repeat(Math.max(2, w - 1))}`;
  return "-".repeat(w);
}

export function serializeMarkdownTable(table) {
  if (!table) return "";
  const headers = (table.headers ?? []).map(escapeTableCell);
  const colCount = Math.max(1, headers.length);
  const rows = (table.rows ?? []).map((r) => Array.from({ length: colCount }, (_, c) => escapeTableCell(r?.[c] ?? "")));
  const widths = Array.from({ length: colCount }, (_, c) =>
    Math.max(3, (headers[c] ?? "").length, ...rows.map((r) => (r[c] ?? "").length))
  );
  const alignments = Array.from({ length: colCount }, (_, c) => table.alignments?.[c] ?? null);
  const padCell = (val, w, align) => {
    const len = val.length;
    if (len >= w) return val;
    const diff = w - len;
    if (align === "right") return " ".repeat(diff) + val;
    if (align === "center") {
      const left = Math.floor(diff / 2);
      return " ".repeat(left) + val + " ".repeat(diff - left);
    }
    return val + " ".repeat(diff);
  };
  const headerLine = `| ${Array.from({ length: colCount }, (_, c) => padCell(headers[c] ?? "", widths[c], alignments[c])).join(" | ")} |`;
  const sepLine = `| ${alignments.map((a, c) => formatSeparatorCell(a, widths[c])).join(" | ")} |`;
  const bodyLines = rows.map((r) => `| ${r.map((cell, c) => padCell(cell, widths[c], alignments[c])).join(" | ")} |`);
  const eol = table.eol || "\n";
  return [headerLine, sepLine, ...bodyLines].join(eol);
}

const FENCE_LANG_ALIASES = {
  rs: "rs",
  rust: "rs",
  js: "js",
  javascript: "js",
  mjs: "js",
  cjs: "js",
  jsx: "jsx",
  ts: "ts",
  typescript: "ts",
  tsx: "tsx",
  py: "py",
  python: "py",
  sh: "sh",
  bash: "sh",
  zsh: "sh",
  shell: "sh",
  json: "json",
  jsonc: "json",
  toml: "toml",
  yaml: "yaml",
  yml: "yaml",
  html: "html",
  htm: "html",
  xml: "html",
  svg: "html",
  css: "css",
  scss: "css",
  go: "go",
  golang: "go",
  c: "c",
  h: "c",
  cpp: "cpp",
  "c++": "cpp",
  cc: "cpp",
  cxx: "cpp",
  hpp: "cpp",
  java: "java",
  sql: "sql",
  md: "md",
  markdown: "md",
  dockerfile: "dockerfile",
  makefile: "makefile",
  ini: "ini",
  env: "ini",
};

export function resolveFenceLanguage(lang) {
  const key = String(lang ?? "").trim().toLowerCase();
  return FENCE_LANG_ALIASES[key] || "txt";
}

export function parseCodeFence(raw) {
  const lines = splitLinesWithEndings(raw);
  if (lines.length === 0) return null;
  const openMatch = lines[0].text.match(/^(\s{0,3})(`{3,}|~{3,})(.*)$/);
  if (!openMatch) return null;
  const indent = openMatch[1];
  const fenceChar = openMatch[2][0];
  const fenceLen = openMatch[2].length;
  const rawInfo = openMatch[3] ?? "";
  if (fenceChar === "`" && rawInfo.includes("`")) return null;
  const info = rawInfo.trim();
  const lang = info.split(/\s+/)[0] || "";
  const hasClosing = lines.length >= 2 && isFenceEnd(lines[lines.length - 1].text, fenceChar, fenceLen);
  const bodyEnd = hasClosing ? lines.length - 1 : lines.length;
  const body = bodyEnd > 1 ? extractRawSlice(lines, 1, bodyEnd) : "";
  const eol = lines[0].eol || "\n";
  return { indent, fenceChar, fenceLen, info, lang, body, hasClosing, eol };
}

function longestCharRun(text, ch) {
  let max = 0;
  let cur = 0;
  const src = String(text ?? "");
  for (let i = 0; i < src.length; i++) {
    if (src[i] === ch) {
      cur++;
      if (cur > max) max = cur;
    } else {
      cur = 0;
    }
  }
  return max;
}

export function serializeCodeFence(fence, nextBody, nextLang) {
  if (!fence) return String(nextBody ?? "");
  const fenceChar = fence.fenceChar || "`";
  const body = String(nextBody ?? "");
  const bodyMaxRun = longestCharRun(body, fenceChar);
  const len = Math.max(3, fence.fenceLen || 3, bodyMaxRun + 1);
  const marker = fenceChar.repeat(len);
  const indent = fence.indent ?? "";
  let info = fence.info ?? "";
  if (nextLang !== undefined) {
    const cleanLang = String(nextLang ?? "").trim().replace(/`/g, "");
    const tail = fence.lang && info.startsWith(fence.lang) ? info.slice(fence.lang.length) : "";
    info = cleanLang ? `${cleanLang}${tail}` : tail.trim();
  }
  const eol = fence.eol || "\n";
  const openLine = `${indent}${marker}${info}`;
  const closeLine = `${indent}${marker}`;
  if (body.length === 0) {
    return `${openLine}${eol}${closeLine}`;
  }
  return `${openLine}${eol}${body}${eol}${closeLine}`;
}
