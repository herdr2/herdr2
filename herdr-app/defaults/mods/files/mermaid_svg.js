// Pure-JS Mermaid diagram parser, layout engine, and usvg-compatible SVG renderer.
import { escapeXml, svgToDataUrl, measureTextWidth, splitBrLines, wrapLines, mixHex, fmtNum } from "./svg_utils.js";

const FONT_SANS = "sans-serif";
const FONT_MONO = "monospace";

const DIAGRAM_LABELS = {
  flowchart: "Flowchart",
  sequence: "Sequence",
  state: "State",
  class: "Class",
  er: "ER Diagram",
  pie: "Pie Chart",
  gantt: "Gantt",
  mindmap: "Mindmap",
  gitgraph: "Git Graph",
  timeline: "Timeline",
  xychart: "XY Chart",
};

function buildPalette(theme) {
  const dark = Boolean(theme?.dark ?? theme?.textView?.dark ?? false);
  const bg = theme?.background || (dark ? "#0d1117" : "#ffffff");
  const surface = theme?.surface || (dark ? "#161b22" : "#f6f8fa");
  const fg = theme?.foreground || theme?.textView?.foreground || (dark ? "#e6edf3" : "#1f2328");
  const muted = theme?.muted || theme?.textView?.muted || (dark ? "#8b949e" : "#656d76");
  const border = theme?.border || theme?.textView?.border || (dark ? "#30363d" : "#d0d7de");
  const accent = theme?.accent || theme?.textView?.link || (dark ? "#58a6ff" : "#0969da");

  const nodeFill = mixHex(surface, accent, dark ? 0.14 : 0.08);
  const nodeStroke = mixHex(border, accent, dark ? 0.55 : 0.45);
  const subFill = mixHex(bg, surface, dark ? 0.72 : 0.65);
  const subStroke = mixHex(border, accent, 0.3);
  const edgeStroke = mixHex(muted, fg, dark ? 0.25 : 0.2);
  const labelBg = mixHex(bg, surface, 0.6);
  const noteFill = dark ? "#2d2618" : "#fff8db";
  const noteStroke = dark ? "#9e7b2f" : "#d4a72c";
  const series = dark
    ? ["#58a6ff", "#3fb950", "#d2a8ff", "#ffa657", "#ff7b72", "#39c5cf", "#f2cc60", "#f778ba"]
    : ["#0969da", "#1a7f37", "#8250df", "#bc4c00", "#cf222e", "#0891b2", "#9a6700", "#bf3989"];

  return {
    dark, bg, surface, fg, muted, border, accent,
    nodeFill, nodeStroke, subFill, subStroke, edgeStroke, labelBg, noteFill, noteStroke, series,
  };
}

function stripDirectivesAndComments(raw) {
  const cleaned = String(raw ?? "").replace(/%%\{[\s\S]*?\}%%/g, "");
  const out = [];
  for (const line of cleaned.split(/\r?\n/)) {
    // Strip %% comments outside quotes
    let inQuote = false;
    let cut = line.length;
    for (let i = 0; i < line.length - 1; i++) {
      if (line[i] === '"') inQuote = !inQuote;
      if (!inQuote && line[i] === "%" && line[i + 1] === "%") {
        cut = i;
        break;
      }
    }
    const trimmed = line.slice(0, cut).trim();
    if (trimmed) out.push(trimmed);
  }
  return out;
}

function unquote(str) {
  const s = String(str ?? "").trim();
  if (s.length >= 2 && ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'")))) {
    return s.slice(1, -1).replace(/\\"/g, '"').replace(/\\'/g, "'");
  }
  if (s.length >= 4 && s.startsWith('"`') && s.endsWith('`"')) {
    return s.slice(2, -2);
  }
  return s;
}

function renderMultilineText(lines, cx, cy, fontSize, fill, opts = {}) {
  const lh = opts.lineHeight || fontSize * 1.32;
  const weight = opts.bold ? ' font-weight="600"' : "";
  const italic = opts.italic ? ' font-style="italic"' : "";
  const anchor = opts.anchor || "middle";
  const family = opts.mono ? FONT_MONO : FONT_SANS;
  const totalH = (lines.length - 1) * lh;
  const firstY = cy - totalH / 2 + fontSize * 0.35;
  return lines
    .map(
      (line, idx) =>
        `<text x="${fmtNum(cx)}" y="${fmtNum(firstY + idx * lh)}" text-anchor="${anchor}" fill="${escapeXml(fill)}" font-family="${family}" font-size="${fmtNum(fontSize)}"${weight}${italic}>${escapeXml(line)}</text>`
    )
    .join("");
}

function drawArrowhead(tipX, tipY, fromX, fromY, color, style = "arrow", size = 8) {
  const dx = tipX - fromX;
  const dy = tipY - fromY;
  const len = Math.hypot(dx, dy) || 1;
  const ux = dx / len;
  const uy = dy / len;
  const px = -uy;
  const py = ux;

  if (style === "circle") {
    const cx = tipX - ux * (size * 0.55);
    const cy = tipY - uy * (size * 0.55);
    return `<circle cx="${fmtNum(cx)}" cy="${fmtNum(cy)}" r="${fmtNum(size * 0.45)}" fill="${escapeXml(color)}"/>`;
  }
  if (style === "cross") {
    const cx = tipX - ux * (size * 0.55);
    const cy = tipY - uy * (size * 0.55);
    const r = size * 0.45;
    return `<line x1="${fmtNum(cx - r)}" y1="${fmtNum(cy - r)}" x2="${fmtNum(cx + r)}" y2="${fmtNum(cy + r)}" stroke="${escapeXml(color)}" stroke-width="1.7"/><line x1="${fmtNum(cx - r)}" y1="${fmtNum(cy + r)}" x2="${fmtNum(cx + r)}" y2="${fmtNum(cy - r)}" stroke="${escapeXml(color)}" stroke-width="1.7"/>`;
  }
  if (style === "open") {
    const bx = tipX - ux * size;
    const by = tipY - uy * size;
    const w = size * 0.52;
    return `<path d="M ${fmtNum(bx + px * w)} ${fmtNum(by + py * w)} L ${fmtNum(tipX)} ${fmtNum(tipY)} L ${fmtNum(bx - px * w)} ${fmtNum(by - py * w)}" fill="none" stroke="${escapeXml(color)}" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/>`;
  }
  if (style === "hollow_triangle") {
    const bx = tipX - ux * (size * 1.25);
    const by = tipY - uy * (size * 1.25);
    const w = size * 0.65;
    const pts = `${fmtNum(tipX)},${fmtNum(tipY)} ${fmtNum(bx + px * w)},${fmtNum(by + py * w)} ${fmtNum(bx - px * w)},${fmtNum(by - py * w)}`;
    return `<polygon points="${pts}" fill="${escapeXml(style === "hollow_triangle" ? "#ffffff" : color)}" stroke="${escapeXml(color)}" stroke-width="1.5"/>`;
  }
  if (style === "diamond" || style === "hollow_diamond") {
    const half = size * 0.72;
    const mx = tipX - ux * half;
    const my = tipY - uy * half;
    const tailX = tipX - ux * (half * 2);
    const tailY = tipY - uy * (half * 2);
    const w = size * 0.48;
    const pts = `${fmtNum(tipX)},${fmtNum(tipY)} ${fmtNum(mx + px * w)},${fmtNum(my + py * w)} ${fmtNum(tailX)},${fmtNum(tailY)} ${fmtNum(mx - px * w)},${fmtNum(my - py * w)}`;
    return `<polygon points="${pts}" fill="${escapeXml(style === "hollow_diamond" ? "#ffffff" : color)}" stroke="${escapeXml(color)}" stroke-width="1.4"/>`;
  }

  // Default filled triangle arrowhead
  const bx = tipX - ux * size;
  const by = tipY - uy * size;
  const w = size * 0.48;
  const pts = `${fmtNum(tipX)},${fmtNum(tipY)} ${fmtNum(bx + px * w)},${fmtNum(by + py * w)} ${fmtNum(bx - px * w)},${fmtNum(by - py * w)}`;
  return `<polygon points="${pts}" fill="${escapeXml(color)}"/>`;
}

// ============================================================================
// Shared Layered DAG Layout Engine (Flowchart, State, Class, ER)
// ============================================================================

function layoutLayeredGraph(nodesMap, edges, opts = {}) {
  const dir = opts.direction || "TD"; // TD, TB, BT, LR, RL
  const horizontal = dir === "LR" || dir === "RL";
  const reverse = dir === "BT" || dir === "RL";
  const rankGap = opts.rankGap ?? (horizontal ? 64 : 52);
  const nodeGap = opts.nodeGap ?? (horizontal ? 32 : 34);
  const pad = opts.pad ?? 28;

  const nodeIds = [...nodesMap.keys()];
  if (nodeIds.length === 0) return { width: 120, height: 60, positions: new Map(), backEdges: new Set() };

  // Build adjacency for DFS cycle breaking
  const adj = new Map(nodeIds.map((id) => [id, []]));
  edges.forEach((e, idx) => {
    if (nodesMap.has(e.from) && nodesMap.has(e.to)) {
      adj.get(e.from).push({ to: e.to, idx });
    }
  });

  const visited = new Set();
  const inStack = new Set();
  const backEdges = new Set();

  function dfs(u) {
    visited.add(u);
    inStack.add(u);
    for (const { to, idx } of adj.get(u) ?? []) {
      if (to === u || inStack.has(to)) {
        backEdges.add(idx);
      } else if (!visited.has(to)) {
        dfs(to);
      }
    }
    inStack.delete(u);
  }
  for (const id of nodeIds) {
    if (!visited.has(id)) dfs(id);
  }

  // DAG topological rank assignment (longest path from sources)
  const inDeg = new Map(nodeIds.map((id) => [id, 0]));
  const dagOut = new Map(nodeIds.map((id) => [id, []]));
  const dagIn = new Map(nodeIds.map((id) => [id, []]));
  edges.forEach((e, idx) => {
    if (backEdges.has(idx)) return;
    if (!nodesMap.has(e.from) || !nodesMap.has(e.to)) return;
    dagOut.get(e.from).push(e.to);
    dagIn.get(e.to).push(e.from);
    inDeg.set(e.to, (inDeg.get(e.to) ?? 0) + 1);
  });

  const queue = nodeIds.filter((id) => (inDeg.get(id) ?? 0) === 0);
  const rank = new Map(nodeIds.map((id) => [id, 0]));
  const topo = [];
  while (queue.length > 0) {
    const u = queue.shift();
    topo.push(u);
    const ru = rank.get(u) ?? 0;
    for (const v of dagOut.get(u) ?? []) {
      if ((rank.get(v) ?? 0) < ru + 1) rank.set(v, ru + 1);
      inDeg.set(v, (inDeg.get(v) ?? 1) - 1);
      if (inDeg.get(v) === 0) queue.push(v);
    }
  }
  for (const id of nodeIds) {
    if (!topo.includes(id)) topo.push(id);
  }

  // Group nodes by rank
  let maxRank = 0;
  for (const r of rank.values()) if (r > maxRank) maxRank = r;
  const layers = Array.from({ length: maxRank + 1 }, () => []);
  const orderIdx = new Map(nodeIds.map((id, i) => [id, i]));
  for (const id of nodeIds) {
    layers[rank.get(id) ?? 0].push(id);
  }
  for (const layer of layers) {
    layer.sort((a, b) => (orderIdx.get(a) ?? 0) - (orderIdx.get(b) ?? 0));
  }

  // Barycenter crossing minimization (3 sweeps)
  for (let sweep = 0; sweep < 3; sweep++) {
    for (let r = 1; r < layers.length; r++) {
      const prevPos = new Map(layers[r - 1].map((id, i) => [id, i]));
      layers[r].sort((a, b) => {
        const score = (id) => {
          const parents = (dagIn.get(id) ?? []).filter((p) => prevPos.has(p));
          if (!parents.length) return (orderIdx.get(id) ?? 0) * 0.001 + 1000;
          return parents.reduce((s, p) => s + prevPos.get(p), 0) / parents.length;
        };
        return score(a) - score(b);
      });
    }
    for (let r = layers.length - 2; r >= 0; r--) {
      const nextPos = new Map(layers[r + 1].map((id, i) => [id, i]));
      layers[r].sort((a, b) => {
        const score = (id) => {
          const kids = (dagOut.get(id) ?? []).filter((k) => nextPos.has(k));
          if (!kids.length) return (orderIdx.get(id) ?? 0) * 0.001 + 1000;
          return kids.reduce((s, k) => s + nextPos.get(k), 0) / kids.length;
        };
        return score(a) - score(b);
      });
    }
  }

  if (reverse) layers.reverse();

  // Compute layer extents & centered coordinates
  const layerPrimarySize = layers.map((layer) =>
    Math.max(...layer.map((id) => (horizontal ? nodesMap.get(id).w : nodesMap.get(id).h)), 36)
  );
  const layerCrossSize = layers.map((layer) => {
    const sum = layer.reduce((acc, id) => acc + (horizontal ? nodesMap.get(id).h : nodesMap.get(id).w), 0);
    return sum + Math.max(0, layer.length - 1) * nodeGap;
  });
  const maxCross = Math.max(...layerCrossSize, 100);

  const positions = new Map();
  let primaryCursor = pad;
  for (let r = 0; r < layers.length; r++) {
    const layer = layers[r];
    const pSize = layerPrimarySize[r];
    let crossCursor = pad + (maxCross - layerCrossSize[r]) / 2;
    for (const id of layer) {
      const n = nodesMap.get(id);
      if (horizontal) {
        const x = primaryCursor + (pSize - n.w) / 2;
        const y = crossCursor;
        positions.set(id, { x, y, w: n.w, h: n.h, cx: x + n.w / 2, cy: y + n.h / 2 });
        crossCursor += n.h + nodeGap;
      } else {
        const x = crossCursor;
        const y = primaryCursor + (pSize - n.h) / 2;
        positions.set(id, { x, y, w: n.w, h: n.h, cx: x + n.w / 2, cy: y + n.h / 2 });
        crossCursor += n.w + nodeGap;
      }
    }
    primaryCursor += pSize + rankGap;
  }

  const totalPrimary = primaryCursor - rankGap + pad;
  const totalCross = maxCross + pad * 2;
  return {
    width: Math.ceil(horizontal ? totalPrimary : totalCross),
    height: Math.ceil(horizontal ? totalCross : totalPrimary),
    positions,
    backEdges,
    horizontal,
    reverse,
  };
}

function clipPointToNode(pos, targetX, targetY) {
  const dx = targetX - pos.cx;
  const dy = targetY - pos.cy;
  if (Math.abs(dx) < 0.001 && Math.abs(dy) < 0.001) return { x: pos.cx, y: pos.cy };
  const hw = pos.w / 2;
  const hh = pos.h / 2;
  const sx = Math.abs(dx) > 0.001 ? hw / Math.abs(dx) : Infinity;
  const sy = Math.abs(dy) > 0.001 ? hh / Math.abs(dy) : Infinity;
  const t = Math.min(sx, sy);
  return { x: pos.cx + dx * t, y: pos.cy + dy * t };
}

function drawLayeredEdge(edge, fromPos, toPos, isBack, horizontal, pal, opts = {}) {
  const stroke = edge.color || pal.edgeStroke;
  const sw = edge.style === "thick" ? 2.6 : 1.5;
  const dash = edge.style === "dotted" ? ' stroke-dasharray="5,4"' : "";
  const strokeAttrs = `stroke="${escapeXml(stroke)}" stroke-width="${fmtNum(sw)}" fill="none" stroke-linecap="round"${dash}`;

  let pathD = "";
  let labelX = 0;
  let labelY = 0;
  let endFromX = fromPos.cx;
  let endFromY = fromPos.cy;
  let endTipX = toPos.cx;
  let endTipY = toPos.cy;
  let startFromX = toPos.cx;
  let startFromY = toPos.cy;
  let startTipX = fromPos.cx;
  let startTipY = fromPos.cy;

  if (edge.from === edge.to) {
    // Self loop
    const x1 = fromPos.x + fromPos.w;
    const y1 = fromPos.cy - Math.min(10, fromPos.h * 0.25);
    const y2 = fromPos.cy + Math.min(10, fromPos.h * 0.25);
    const loopX = x1 + 36;
    pathD = `M ${fmtNum(x1)} ${fmtNum(y1)} C ${fmtNum(loopX)} ${fmtNum(y1 - 16)}, ${fmtNum(loopX)} ${fmtNum(y2 + 16)}, ${fmtNum(x1)} ${fmtNum(y2)}`;
    labelX = loopX + 6;
    labelY = fromPos.cy;
    endFromX = loopX;
    endFromY = y2 + 4;
    endTipX = x1;
    endTipY = y2;
  } else if (isBack) {
    // Curved side arc for back-edges so cycles stay clear
    const offset = (edge.parallelIdx ?? 0) * 18;
    if (horizontal) {
      const start = { x: fromPos.cx, y: fromPos.y };
      const end = { x: toPos.cx, y: toPos.y };
      const arcY = Math.min(fromPos.y, toPos.y) - 34 - offset;
      pathD = `M ${fmtNum(start.x)} ${fmtNum(start.y)} C ${fmtNum(start.x)} ${fmtNum(arcY)}, ${fmtNum(end.x)} ${fmtNum(arcY)}, ${fmtNum(end.x)} ${fmtNum(end.y)}`;
      labelX = (start.x + end.x) / 2;
      labelY = arcY + 4;
      endFromX = end.x;
      endFromY = arcY;
      endTipX = end.x;
      endTipY = end.y;
      startFromX = start.x;
      startFromY = arcY;
      startTipX = start.x;
      startTipY = start.y;
    } else {
      const start = { x: fromPos.x + fromPos.w, y: fromPos.cy };
      const end = { x: toPos.x + toPos.w, y: toPos.cy };
      const arcX = Math.max(start.x, end.x) + 34 + offset;
      pathD = `M ${fmtNum(start.x)} ${fmtNum(start.y)} C ${fmtNum(arcX)} ${fmtNum(start.y)}, ${fmtNum(arcX)} ${fmtNum(end.y)}, ${fmtNum(end.x)} ${fmtNum(end.y)}`;
      labelX = arcX + 4;
      labelY = (start.y + end.y) / 2;
      endFromX = arcX;
      endFromY = end.y;
      endTipX = end.x;
      endTipY = end.y;
      startFromX = arcX;
      startFromY = start.y;
      startTipX = start.x;
      startTipY = start.y;
    }
  } else {
    const start = clipPointToNode(fromPos, toPos.cx, toPos.cy);
    const end = clipPointToNode(toPos, fromPos.cx, fromPos.cy);
    const bend = (edge.parallelOffset ?? 0) * 20;
    if (horizontal) {
      const mx = (start.x + end.x) / 2;
      const c1x = mx;
      const c1y = start.y + bend;
      const c2x = mx;
      const c2y = end.y + bend;
      pathD = `M ${fmtNum(start.x)} ${fmtNum(start.y)} C ${fmtNum(c1x)} ${fmtNum(c1y)}, ${fmtNum(c2x)} ${fmtNum(c2y)}, ${fmtNum(end.x)} ${fmtNum(end.y)}`;
      labelX = mx;
      labelY = (start.y + end.y) / 2 + bend * 0.75;
      endFromX = c2x;
      endFromY = c2y;
      startFromX = c1x;
      startFromY = c1y;
    } else {
      const my = (start.y + end.y) / 2;
      const c1x = start.x + bend;
      const c1y = my;
      const c2x = end.x + bend;
      const c2y = my;
      pathD = `M ${fmtNum(start.x)} ${fmtNum(start.y)} C ${fmtNum(c1x)} ${fmtNum(c1y)}, ${fmtNum(c2x)} ${fmtNum(c2y)}, ${fmtNum(end.x)} ${fmtNum(end.y)}`;
      labelX = (start.x + end.x) / 2 + bend * 0.75;
      labelY = my;
      endFromX = c2x;
      endFromY = c2y;
      startFromX = c1x;
      startFromY = c1y;
    }
    endTipX = end.x;
    endTipY = end.y;
    startTipX = start.x;
    startTipY = start.y;
  }

  let out = `<path d="${pathD}" ${strokeAttrs}/>`;
  const endMarker = edge.endMarker ?? (edge.hasArrow !== false ? "arrow" : "none");
  const startMarker = edge.startMarker ?? "none";
  if (endMarker !== "none") {
    out += drawArrowhead(endTipX, endTipY, endFromX, endFromY, stroke, endMarker, opts.arrowSize || 8);
  }
  if (startMarker !== "none") {
    out += drawArrowhead(startTipX, startTipY, startFromX, startFromY, stroke, startMarker, opts.arrowSize || 8);
  }

  if (edge.label) {
    const lines = splitBrLines(edge.label);
    const fs = 11;
    const lw = Math.max(...lines.map((l) => measureTextWidth(l, fs))) + 12;
    const lh = lines.length * 14 + 6;
    out += `<rect x="${fmtNum(labelX - lw / 2)}" y="${fmtNum(labelY - lh / 2)}" width="${fmtNum(lw)}" height="${fmtNum(lh)}" rx="4" fill="${escapeXml(pal.labelBg)}" stroke="${escapeXml(pal.border)}" stroke-width="0.7"/>`;
    out += renderMultilineText(lines, labelX, labelY, fs, pal.muted, { lineHeight: 14 });
  }
  return out;
}

// ============================================================================
// 1. Flowchart / Graph
// ============================================================================

const FLOW_SHAPES = [
  { open: "(((", close: ")))", shape: "double_circle" },
  { open: "([", close: "])", shape: "stadium" },
  { open: "[[", close: "]]", shape: "subroutine" },
  { open: "[(", close: ")]", shape: "cylinder" },
  { open: "((", close: "))", shape: "circle" },
  { open: "{{", close: "}}", shape: "hexagon" },
  { open: "[/", close: "/]", shape: "parallelogram" },
  { open: "[\\", close: "\\]", shape: "parallelogram_alt" },
  { open: "[/", close: "\\]", shape: "trapezoid" },
  { open: "[\\", close: "/]", shape: "trapezoid_alt" },
  { open: "[", close: "]", shape: "rect" },
  { open: "(", close: ")", shape: "round" },
  { open: "{", close: "}", shape: "diamond" },
  { open: ">", close: "]", shape: "asymmetric" },
];

function parseFlowNodeToken(rawToken) {
  let token = String(rawToken ?? "").trim();
  if (!token) return null;
  let className = null;
  const clsIdx = token.indexOf(":::");
  if (clsIdx > 0) {
    className = token.slice(clsIdx + 3).trim();
    token = token.slice(0, clsIdx).trim();
  }
  const idMatch = token.match(/^([A-Za-z0-9_\-\u4e00-\u9fff.]+)/);
  if (!idMatch) return null;
  const id = idMatch[1];
  const rest = token.slice(id.length).trim();
  if (!rest) return { id, label: null, shape: null, className };

  for (const spec of FLOW_SHAPES) {
    if (rest.startsWith(spec.open) && rest.endsWith(spec.close) && rest.length >= spec.open.length + spec.close.length) {
      const inner = rest.slice(spec.open.length, rest.length - spec.close.length).trim();
      return { id, label: unquote(inner), shape: spec.shape, className };
    }
  }
  return { id, label: unquote(rest), shape: "rect", className };
}

function splitFlowChain(line) {
  // Split a flowchart statement into alternating [nodeGroup, edgeSpec, nodeGroup, ...]
  // respecting quotes and bracket pairs.
  const parts = [];
  let i = 0;
  let buf = "";
  let depthParen = 0;
  let depthBracket = 0;
  let depthBrace = 0;
  let inQuote = false;

  const edgeRe = /^((?:<)?(?:--+|==+|-\.-+)(?:[>ox])?|(?:--|==|-\.)\s+[^-=.]+?\s+(?:--+>|==+>|-\.+>|---|===|-\.-))/;

  while (i < line.length) {
    const ch = line[i];
    if (ch === '"') {
      inQuote = !inQuote;
      buf += ch;
      i++;
      continue;
    }
    if (!inQuote) {
      if (ch === "(") depthParen++;
      else if (ch === ")" && depthParen > 0) depthParen--;
      else if (ch === "[") depthBracket++;
      else if (ch === "]" && depthBracket > 0) depthBracket--;
      else if (ch === "{") depthBrace++;
      else if (ch === "}" && depthBrace > 0) depthBrace--;

      if (depthParen === 0 && depthBracket === 0 && depthBrace === 0) {
        const slice = line.slice(i);
        // Check labeled inline arrow: -- text --> | -. text .-> | == text ==>
        const inlineLbl =
          slice.match(/^--\s+(.+?)\s+(--+>|---|--+[ox])/) ||
          slice.match(/^-\.\s+(.+?)\s+(\.-+>|-\.-)/) ||
          slice.match(/^==\s+(.+?)\s+(==+>|===)/);
        if (inlineLbl && buf.trim()) {
          parts.push({ kind: "nodes", text: buf.trim() });
          buf = "";
          const rawArrow = inlineLbl[2];
          parts.push({ kind: "edge", raw: rawArrow, label: unquote(inlineLbl[1]) });
          i += inlineLbl[0].length;
          continue;
        }
        // Check standard arrow possibly followed by |label|
        const arrowM = slice.match(/^([<ox]?)(-\.+->|-\.+-|==+>|===+|--+[>ox]|---+)(?:\|([^|]*)\|)?/);
        if (arrowM && buf.trim()) {
          parts.push({ kind: "nodes", text: buf.trim() });
          buf = "";
          parts.push({
            kind: "edge",
            raw: arrowM[1] + arrowM[2],
            label: arrowM[3] !== undefined ? unquote(arrowM[3]) : "",
          });
          i += arrowM[0].length;
          continue;
        }
      }
    }
    buf += ch;
    i++;
  }
  if (buf.trim()) parts.push({ kind: "nodes", text: buf.trim() });
  return parts;
}

function classifyFlowEdge(raw) {
  const s = String(raw ?? "");
  const style = s.includes("=") ? "thick" : s.includes(".") ? "dotted" : "solid";
  const startMarker = s.startsWith("<") ? "arrow" : s.startsWith("o") ? "circle" : s.startsWith("x") ? "cross" : "none";
  const endMarker = s.endsWith(">") ? "arrow" : s.endsWith("o") ? "circle" : s.endsWith("x") ? "cross" : "none";
  return { style, startMarker, endMarker };
}

function parseStyleProps(propStr) {
  const out = {};
  for (const part of String(propStr ?? "").split(",")) {
    const idx = part.indexOf(":");
    if (idx > 0) {
      out[part.slice(0, idx).trim().toLowerCase()] = part.slice(idx + 1).trim();
    }
  }
  return out;
}

function renderFlowNodeShape(node, pos, pal) {
  const fill = node.style?.fill || pal.nodeFill;
  const stroke = node.style?.stroke || pal.nodeStroke;
  const textColor = node.style?.color || pal.fg;
  const sw = node.style?.["stroke-width"] ? parseFloat(node.style["stroke-width"]) || 1.5 : 1.5;
  const dash = node.style?.["stroke-dasharray"] ? ` stroke-dasharray="${escapeXml(node.style["stroke-dasharray"])}"` : "";
  const { x, y, w, h, cx, cy } = pos;
  const attrs = `fill="${escapeXml(fill)}" stroke="${escapeXml(stroke)}" stroke-width="${fmtNum(sw)}"${dash}`;
  let shapeSvg = "";

  switch (node.shape) {
    case "round":
      shapeSvg = `<rect x="${fmtNum(x)}" y="${fmtNum(y)}" width="${fmtNum(w)}" height="${fmtNum(h)}" rx="10" ${attrs}/>`;
      break;
    case "stadium":
      shapeSvg = `<rect x="${fmtNum(x)}" y="${fmtNum(y)}" width="${fmtNum(w)}" height="${fmtNum(h)}" rx="${fmtNum(h / 2)}" ${attrs}/>`;
      break;
    case "subroutine":
      shapeSvg = `<rect x="${fmtNum(x)}" y="${fmtNum(y)}" width="${fmtNum(w)}" height="${fmtNum(h)}" rx="3" ${attrs}/><line x1="${fmtNum(x + 8)}" y1="${fmtNum(y)}" x2="${fmtNum(x + 8)}" y2="${fmtNum(y + h)}" stroke="${escapeXml(stroke)}" stroke-width="${fmtNum(sw)}"/><line x1="${fmtNum(x + w - 8)}" y1="${fmtNum(y)}" x2="${fmtNum(x + w - 8)}" y2="${fmtNum(y + h)}" stroke="${escapeXml(stroke)}" stroke-width="${fmtNum(sw)}"/>`;
      break;
    case "cylinder": {
      const ry = Math.min(8, h * 0.18);
      const topY = y + ry;
      const botY = y + h - ry;
      shapeSvg =
        `<path d="M ${fmtNum(x)} ${fmtNum(topY)} L ${fmtNum(x)} ${fmtNum(botY)} A ${fmtNum(w / 2)} ${fmtNum(ry)} 0 0 0 ${fmtNum(x + w)} ${fmtNum(botY)} L ${fmtNum(x + w)} ${fmtNum(topY)} Z" ${attrs}/>` +
        `<ellipse cx="${fmtNum(cx)}" cy="${fmtNum(topY)}" rx="${fmtNum(w / 2)}" ry="${fmtNum(ry)}" ${attrs}/>`;
      break;
    }
    case "circle": {
      const r = Math.max(w, h) / 2;
      shapeSvg = `<circle cx="${fmtNum(cx)}" cy="${fmtNum(cy)}" r="${fmtNum(r)}" ${attrs}/>`;
      break;
    }
    case "double_circle": {
      const r = Math.max(w, h) / 2;
      shapeSvg = `<circle cx="${fmtNum(cx)}" cy="${fmtNum(cy)}" r="${fmtNum(r)}" ${attrs}/><circle cx="${fmtNum(cx)}" cy="${fmtNum(cy)}" r="${fmtNum(Math.max(4, r - 4))}" fill="none" stroke="${escapeXml(stroke)}" stroke-width="${fmtNum(sw)}"/>`;
      break;
    }
    case "diamond": {
      const pts = `${fmtNum(cx)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(cy)} ${fmtNum(cx)},${fmtNum(y + h)} ${fmtNum(x)},${fmtNum(cy)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "hexagon": {
      const dx = Math.min(16, w * 0.18);
      const pts = `${fmtNum(x + dx)},${fmtNum(y)} ${fmtNum(x + w - dx)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(cy)} ${fmtNum(x + w - dx)},${fmtNum(y + h)} ${fmtNum(x + dx)},${fmtNum(y + h)} ${fmtNum(x)},${fmtNum(cy)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "parallelogram": {
      const dx = 12;
      const pts = `${fmtNum(x + dx)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y)} ${fmtNum(x + w - dx)},${fmtNum(y + h)} ${fmtNum(x)},${fmtNum(y + h)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "parallelogram_alt": {
      const dx = 12;
      const pts = `${fmtNum(x)},${fmtNum(y)} ${fmtNum(x + w - dx)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y + h)} ${fmtNum(x + dx)},${fmtNum(y + h)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "trapezoid": {
      const dx = 12;
      const pts = `${fmtNum(x + dx)},${fmtNum(y)} ${fmtNum(x + w - dx)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y + h)} ${fmtNum(x)},${fmtNum(y + h)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "trapezoid_alt": {
      const dx = 12;
      const pts = `${fmtNum(x)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y)} ${fmtNum(x + w - dx)},${fmtNum(y + h)} ${fmtNum(x + dx)},${fmtNum(y + h)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    case "asymmetric": {
      const notch = 10;
      const pts = `${fmtNum(x)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y)} ${fmtNum(x + w)},${fmtNum(y + h)} ${fmtNum(x)},${fmtNum(y + h)} ${fmtNum(x + notch)},${fmtNum(cy)}`;
      shapeSvg = `<polygon points="${pts}" ${attrs}/>`;
      break;
    }
    default:
      shapeSvg = `<rect x="${fmtNum(x)}" y="${fmtNum(y)}" width="${fmtNum(w)}" height="${fmtNum(h)}" rx="6" ${attrs}/>`;
      break;
  }

  return shapeSvg + renderMultilineText(node.lines, cx, cy, 12.5, textColor);
}

function renderFlowchart(lines, pal) {
  const header = lines[0];
  const dirMatch = header.match(/^(?:flowchart|graph)\s+(TD|TB|BT|LR|RL)/i);
  const direction = (dirMatch ? dirMatch[1].toUpperCase() : "TD");

  const nodes = new Map();
  const edges = [];
  const subgraphs = [];
  const subStack = [];
  const classDefs = new Map();
  const nodeClasses = new Map();
  const nodeStyles = new Map();

  const ensureNode = (spec) => {
    if (!spec?.id) return null;
    let existing = nodes.get(spec.id);
    if (!existing) {
      existing = { id: spec.id, label: spec.label ?? spec.id, shape: spec.shape || "rect" };
      nodes.set(spec.id, existing);
    } else {
      if (spec.label !== null && spec.label !== undefined) existing.label = spec.label;
      if (spec.shape) existing.shape = spec.shape;
    }
    if (spec.className) nodeClasses.set(spec.id, spec.className);
    if (subStack.length > 0) {
      for (const sg of subStack) sg.nodes.add(spec.id);
    }
    return existing;
  };

  // Expand semicolon-separated statements
  const stmts = [];
  for (let i = 1; i < lines.length; i++) {
    for (const part of lines[i].split(";")) {
      const t = part.trim();
      if (t) stmts.push(t);
    }
  }

  for (const stmt of stmts) {
    if (/^subgraph\b/i.test(stmt)) {
      const rest = stmt.slice(8).trim();
      const mBracket = rest.match(/^([A-Za-z0-9_\-\u4e00-\u9fff]+)\s*\[(.+)\]$/);
      const id = mBracket ? mBracket[1] : rest || `sg_${subgraphs.length}`;
      const title = unquote(mBracket ? mBracket[2] : rest || "");
      const sg = { id, title, nodes: new Set(), depth: subStack.length };
      subgraphs.push(sg);
      subStack.push(sg);
      continue;
    }
    if (/^end$/i.test(stmt)) {
      subStack.pop();
      continue;
    }
    if (/^direction\s+(TD|TB|BT|LR|RL)/i.test(stmt)) continue;
    if (/^classDef\s+/i.test(stmt)) {
      const m = stmt.match(/^classDef\s+(\S+)\s+(.+)$/i);
      if (m) classDefs.set(m[1], parseStyleProps(m[2]));
      continue;
    }
    if (/^class\s+/i.test(stmt)) {
      const m = stmt.match(/^class\s+(.+?)\s+(\S+)$/i);
      if (m) {
        for (const id of m[1].split(",").map((s) => s.trim()).filter(Boolean)) {
          nodeClasses.set(id, m[2]);
        }
      }
      continue;
    }
    if (/^style\s+/i.test(stmt)) {
      const m = stmt.match(/^style\s+(\S+)\s+(.+)$/i);
      if (m) nodeStyles.set(m[1], parseStyleProps(m[2]));
      continue;
    }
    if (/^(?:linkStyle|click)\s+/i.test(stmt)) continue;

    const chain = splitFlowChain(stmt);
    if (chain.length === 1 && chain[0].kind === "nodes") {
      for (const rawNode of chain[0].text.split("&")) {
        ensureNode(parseFlowNodeToken(rawNode));
      }
      continue;
    }
    for (let k = 0; k + 2 < chain.length; k += 2) {
      const leftGroup = chain[k];
      const edgePart = chain[k + 1];
      const rightGroup = chain[k + 2];
      if (leftGroup.kind !== "nodes" || edgePart.kind !== "edge" || rightGroup.kind !== "nodes") continue;
      const fromNodes = leftGroup.text.split("&").map((s) => ensureNode(parseFlowNodeToken(s))).filter(Boolean);
      const toNodes = rightGroup.text.split("&").map((s) => ensureNode(parseFlowNodeToken(s))).filter(Boolean);
      const edgeMeta = classifyFlowEdge(edgePart.raw);
      for (const f of fromNodes) {
        for (const t of toNodes) {
          edges.push({ from: f.id, to: t.id, label: edgePart.label, ...edgeMeta });
        }
      }
    }
  }

  if (nodes.size === 0) {
    throw new Error("No nodes found in flowchart");
  }

  // Measure every node
  for (const node of nodes.values()) {
    const cls = nodeClasses.get(node.id);
    node.style = { ...(cls ? classDefs.get(cls) : null), ...(nodeStyles.get(node.id) || null) };
    node.lines = wrapLines(node.label || node.id, 180, 12.5);
    const textW = Math.max(...node.lines.map((l) => measureTextWidth(l, 12.5)), 24);
    const textH = node.lines.length * 16.5;
    if (node.shape === "diamond") {
      node.w = Math.max(76, textW * 1.48 + 26);
      node.h = Math.max(54, textH * 1.65 + 22);
    } else if (node.shape === "circle" || node.shape === "double_circle") {
      const d = Math.max(46, Math.hypot(textW, textH) + 22);
      node.w = d;
      node.h = d;
    } else if (node.shape === "hexagon" || node.shape === "parallelogram" || node.shape === "parallelogram_alt" || node.shape === "trapezoid" || node.shape === "trapezoid_alt") {
      node.w = Math.max(76, textW + 40);
      node.h = Math.max(38, textH + 18);
    } else if (node.shape === "cylinder") {
      node.w = Math.max(72, textW + 28);
      node.h = Math.max(46, textH + 26);
    } else {
      node.w = Math.max(66, textW + 28);
      node.h = Math.max(36, textH + 16);
    }
  }

  const extraTop = subgraphs.length > 0 ? 34 : 24;
  const layout = layoutLayeredGraph(nodes, edges, { direction, pad: extraTop });

  // Detect parallel edges between same node pair
  const pairCount = new Map();
  for (const e of edges) {
    const key = e.from < e.to ? `${e.from}|${e.to}` : `${e.to}|${e.from}`;
    pairCount.set(key, (pairCount.get(key) ?? 0) + 1);
  }
  const pairSeen = new Map();
  for (const e of edges) {
    const key = e.from < e.to ? `${e.from}|${e.to}` : `${e.to}|${e.from}`;
    const total = pairCount.get(key) ?? 1;
    const idx = pairSeen.get(key) ?? 0;
    pairSeen.set(key, idx + 1);
    e.parallelOffset = total > 1 ? idx - (total - 1) / 2 : 0;
  }

  let minX = 0;
  let minY = 0;
  let maxX = layout.width;
  let maxY = layout.height;

  // Subgraph boxes
  let sgSvg = "";
  for (const sg of subgraphs) {
    const memberPos = [...sg.nodes].map((id) => layout.positions.get(id)).filter(Boolean);
    if (!memberPos.length) continue;
    const padX = 16 + sg.depth * 6;
    const padTop = sg.title ? 30 : 16;
    const padBot = 14 + sg.depth * 6;
    const x1 = Math.min(...memberPos.map((p) => p.x)) - padX;
    const y1 = Math.min(...memberPos.map((p) => p.y)) - padTop;
    const x2 = Math.max(...memberPos.map((p) => p.x + p.w)) + padX;
    const y2 = Math.max(...memberPos.map((p) => p.y + p.h)) + padBot;
    minX = Math.min(minX, x1 - 12);
    minY = Math.min(minY, y1 - 12);
    maxX = Math.max(maxX, x2 + 12);
    maxY = Math.max(maxY, y2 + 12);
    sgSvg += `<rect x="${fmtNum(x1)}" y="${fmtNum(y1)}" width="${fmtNum(x2 - x1)}" height="${fmtNum(y2 - y1)}" rx="8" fill="${escapeXml(pal.subFill)}" stroke="${escapeXml(pal.subStroke)}" stroke-width="1.2" stroke-dasharray="5,3"/>`;
    if (sg.title) {
      sgSvg += `<text x="${fmtNum(x1 + 12)}" y="${fmtNum(y1 + 19)}" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="11.5" font-weight="600">${escapeXml(sg.title)}</text>`;
    }
  }

  // Reserve room for back-edges & self-loops
  if (layout.backEdges.size > 0) {
    if (layout.horizontal) minY = Math.min(minY, -28);
    else maxX += 46;
  }

  let edgeSvg = "";
  let backIdx = 0;
  edges.forEach((e, idx) => {
    const fp = layout.positions.get(e.from);
    const tp = layout.positions.get(e.to);
    if (!fp || !tp) return;
    const isBack = layout.backEdges.has(idx);
    if (isBack) e.parallelIdx = backIdx++;
    edgeSvg += drawLayeredEdge(e, fp, tp, isBack, layout.horizontal, pal);
  });

  let nodeSvg = "";
  for (const [id, node] of nodes) {
    const pos = layout.positions.get(id);
    if (pos) nodeSvg += renderFlowNodeShape(node, pos, pal);
  }

  const W = Math.max(120, Math.ceil(maxX - minX));
  const H = Math.max(70, Math.ceil(maxY - minY));
  const tx = minX < 0 ? -minX : 0;
  const ty = minY < 0 ? -minY : 0;
  const body = tx || ty ? `<g transform="translate(${fmtNum(tx)},${fmtNum(ty)})">${sgSvg}${edgeSvg}${nodeSvg}</g>` : sgSvg + edgeSvg + nodeSvg;
  return { width: W, height: H, svg: wrapSvg(W, H, body) };
}

// ============================================================================
// 2. Sequence Diagram
// ============================================================================

function renderSequenceDiagram(lines, pal) {
  const participants = [];
  const pMap = new Map();
  let autonumber = false;

  const ensureParticipant = (id, label = null, kind = "participant") => {
    const cleanId = String(id ?? "").trim();
    if (!cleanId) return null;
    if (!pMap.has(cleanId)) {
      const p = { id: cleanId, label: label ? unquote(label) : cleanId, kind };
      pMap.set(cleanId, p);
      participants.push(p);
    } else if (label) {
      pMap.get(cleanId).label = unquote(label);
      if (kind === "actor") pMap.get(cleanId).kind = "actor";
    }
    return pMap.get(cleanId);
  };

  const events = [];
  const blockStack = [];
  let msgSeq = 0;

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (/^autonumber$/i.test(line)) {
      autonumber = true;
      continue;
    }
    const pMatch = line.match(/^(participant|actor)\s+(.+?)(?:\s+as\s+(.+))?$/i);
    if (pMatch) {
      ensureParticipant(pMatch[2].trim(), pMatch[3]?.trim() || null, pMatch[1].toLowerCase());
      continue;
    }
    const actMatch = line.match(/^(activate|deactivate)\s+(.+)$/i);
    if (actMatch) {
      const p = ensureParticipant(actMatch[2].trim());
      if (p) events.push({ type: actMatch[1].toLowerCase(), target: p.id });
      continue;
    }
    const noteMatch = line.match(/^Note\s+(left of|right of|over)\s+([^:]+):(.*)$/i);
    if (noteMatch) {
      const where = noteMatch[1].toLowerCase();
      const targets = noteMatch[2].split(",").map((s) => ensureParticipant(s.trim())?.id).filter(Boolean);
      if (targets.length) {
        events.push({ type: "note", where, targets, text: noteMatch[3].trim() });
      }
      continue;
    }
    const blkStart = line.match(/^(loop|alt|opt|par|critical|break|rect)\b(.*)$/i);
    if (blkStart) {
      const blk = {
        type: "block",
        kind: blkStart[1].toLowerCase(),
        label: blkStart[2].trim(),
        startIdx: events.length,
        dividers: [],
      };
      blockStack.push(blk);
      events.push({ type: "block_open", block: blk });
      continue;
    }
    const blkDiv = line.match(/^(else|and|option)\b(.*)$/i);
    if (blkDiv && blockStack.length > 0) {
      const top = blockStack[blockStack.length - 1];
      const divEvt = { type: "block_div", block: top, kind: blkDiv[1].toLowerCase(), label: blkDiv[2].trim() };
      top.dividers.push(divEvt);
      events.push(divEvt);
      continue;
    }
    if (/^end$/i.test(line) && blockStack.length > 0) {
      const closed = blockStack.pop();
      events.push({ type: "block_close", block: closed });
      continue;
    }

    // Message arrow: A->>B: text, A-->>+B: text, A->>-B: text, A->B: text, A-xB: text, A-)B: text
    const msgMatch = line.match(/^(.+?)(<<?-->>|<<?->>|-->>|->>|-->|->|--x|-x|--\)|-\))([+-]?)(.+?):(.*)$/);
    if (msgMatch) {
      const from = ensureParticipant(msgMatch[1].trim());
      const arrow = msgMatch[2];
      const actMod = msgMatch[3];
      const to = ensureParticipant(msgMatch[4].trim());
      const text = msgMatch[5].trim();
      if (from && to) {
        msgSeq++;
        events.push({
          type: "msg",
          from: from.id,
          to: to.id,
          arrow,
          text: autonumber ? `${msgSeq}. ${text}` : text,
          activate: actMod === "+" ? to.id : null,
          deactivate: actMod === "-" ? from.id : null,
        });
      }
    }
  }

  if (participants.length === 0) {
    throw new Error("No participants found in sequence diagram");
  }

  // Compute participant box widths & x centers
  const padX = 28;
  const topY = 20;
  const headerH = 38;
  const minGap = 130;

  const pWidths = participants.map((p) => {
    const ls = splitBrLines(p.label);
    return Math.max(82, Math.max(...ls.map((l) => measureTextWidth(l, 12.5, { bold: true }))) + 24);
  });

  const pX = [];
  let cursorX = padX + pWidths[0] / 2;
  for (let i = 0; i < participants.length; i++) {
    if (i > 0) {
      const need = (pWidths[i - 1] + pWidths[i]) / 2 + 44;
      cursorX += Math.max(minGap, need);
    }
    pX.push(cursorX);
  }
  const idxOf = new Map(participants.map((p, i) => [p.id, i]));

  // Walk events to assign y coordinates
  let y = topY + headerH + 26;
  const activeStacks = new Map(participants.map((p) => [p.id, []]));
  const activations = [];
  const blockRects = [];
  const openBlocks = [];
  const placedEvents = [];

  for (const ev of events) {
    if (ev.type === "activate") {
      activeStacks.get(ev.target)?.push(y - 8);
    } else if (ev.type === "deactivate") {
      const st = activeStacks.get(ev.target);
      if (st?.length) activations.push({ id: ev.target, y1: st.pop(), y2: y + 4 });
    } else if (ev.type === "block_open") {
      ev.block.y1 = y - 6;
      ev.block.depth = openBlocks.length;
      openBlocks.push(ev.block);
      y += 28;
    } else if (ev.type === "block_div") {
      ev.y = y;
      y += 26;
    } else if (ev.type === "block_close") {
      ev.block.y2 = y + 6;
      blockRects.push(ev.block);
      const i = openBlocks.indexOf(ev.block);
      if (i >= 0) openBlocks.splice(i, 1);
      y += 18;
    } else if (ev.type === "note") {
      const noteLines = wrapLines(ev.text, 165, 11.5);
      const nh = Math.max(28, noteLines.length * 15 + 12);
      const nw = Math.max(76, Math.max(...noteLines.map((l) => measureTextWidth(l, 11.5))) + 18);
      placedEvents.push({ ...ev, y, nw, nh, noteLines });
      y += nh + 18;
    } else if (ev.type === "msg") {
      const msgLines = splitBrLines(ev.text);
      const selfLoop = ev.from === ev.to;
      const stepH = selfLoop ? Math.max(46, msgLines.length * 15 + 32) : Math.max(36, msgLines.length * 15 + 22);
      const msgY = y + msgLines.length * 14 + 4;
      placedEvents.push({ ...ev, y: msgY, msgLines, selfLoop });
      if (ev.activate) activeStacks.get(ev.activate)?.push(msgY);
      if (ev.deactivate) {
        const st = activeStacks.get(ev.deactivate);
        if (st?.length) activations.push({ id: ev.deactivate, y1: st.pop(), y2: msgY });
      }
      y += stepH;
    }
  }
  for (const [id, st] of activeStacks) {
    while (st.length) activations.push({ id, y1: st.pop(), y2: y });
  }
  while (openBlocks.length) {
    const b = openBlocks.pop();
    b.y2 = y + 4;
    blockRects.push(b);
  }

  const bottomBoxY = y + 14;
  const totalH = bottomBoxY + headerH + 20;
  let rightBound = pX[pX.length - 1] + pWidths[pWidths.length - 1] / 2 + padX;
  let leftBound = 0;

  // Check if notes or self-loops extend past bounds
  for (const ev of placedEvents) {
    if (ev.type === "note") {
      const x0 = pX[idxOf.get(ev.targets[0])];
      if (ev.where === "left of") leftBound = Math.min(leftBound, x0 - ev.nw - 24);
      if (ev.where === "right of") rightBound = Math.max(rightBound, x0 + ev.nw + 24);
    } else if (ev.type === "msg" && ev.selfLoop) {
      const x0 = pX[idxOf.get(ev.from)];
      const tw = Math.max(...ev.msgLines.map((l) => measureTextWidth(l, 11.5)));
      rightBound = Math.max(rightBound, x0 + 52 + tw + 16);
    }
  }

  const shiftX = leftBound < 0 ? -leftBound + 12 : 0;
  const totalW = Math.ceil(rightBound + shiftX);

  const drawParticipantBox = (p, cx, boxY, w) => {
    const linesArr = splitBrLines(p.label);
    if (p.kind === "actor") {
      const headY = boxY + 9;
      return (
        `<circle cx="${fmtNum(cx)}" cy="${fmtNum(headY)}" r="6" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
        `<line x1="${fmtNum(cx)}" y1="${fmtNum(headY + 6)}" x2="${fmtNum(cx)}" y2="${fmtNum(headY + 18)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
        `<line x1="${fmtNum(cx - 9)}" y1="${fmtNum(headY + 11)}" x2="${fmtNum(cx + 9)}" y2="${fmtNum(headY + 11)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
        `<line x1="${fmtNum(cx)}" y1="${fmtNum(headY + 18)}" x2="${fmtNum(cx - 7)}" y2="${fmtNum(headY + 26)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
        `<line x1="${fmtNum(cx)}" y1="${fmtNum(headY + 18)}" x2="${fmtNum(cx + 7)}" y2="${fmtNum(headY + 26)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
        renderMultilineText(linesArr, cx, boxY + headerH + 6, 11.5, pal.fg, { bold: true })
      );
    }
    return (
      `<rect x="${fmtNum(cx - w / 2)}" y="${fmtNum(boxY)}" width="${fmtNum(w)}" height="${fmtNum(headerH)}" rx="6" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>` +
      renderMultilineText(linesArr, cx, boxY + headerH / 2, 12.5, pal.fg, { bold: true })
    );
  };

  let svgParts = "";

  // Combined fragment blocks (behind messages)
  for (const blk of blockRects) {
    const bx1 = 16 + blk.depth * 8;
    const bx2 = totalW - 16 - blk.depth * 8;
    const fill = blk.kind === "rect" ? mixHex(pal.bg, pal.accent, 0.1) : pal.subFill;
    svgParts += `<rect x="${fmtNum(bx1)}" y="${fmtNum(blk.y1)}" width="${fmtNum(bx2 - bx1)}" height="${fmtNum(blk.y2 - blk.y1)}" rx="5" fill="${escapeXml(fill)}" fill-opacity="0.55" stroke="${escapeXml(pal.subStroke)}" stroke-width="1.2"/>`;
    if (blk.kind !== "rect") {
      const tagW = Math.max(46, measureTextWidth(blk.kind, 10.5, { bold: true }) + 16);
      svgParts += `<path d="M ${fmtNum(bx1)} ${fmtNum(blk.y1)} L ${fmtNum(bx1 + tagW)} ${fmtNum(blk.y1)} L ${fmtNum(bx1 + tagW)} ${fmtNum(blk.y1 + 14)} L ${fmtNum(bx1 + tagW - 7)} ${fmtNum(blk.y1 + 20)} L ${fmtNum(bx1)} ${fmtNum(blk.y1 + 20)} Z" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.subStroke)}" stroke-width="1"/>`;
      svgParts += `<text x="${fmtNum(bx1 + 8)}" y="${fmtNum(blk.y1 + 14)}" fill="${escapeXml(pal.accent)}" font-family="${FONT_SANS}" font-size="10.5" font-weight="700">${escapeXml(blk.kind)}</text>`;
      if (blk.label) {
        svgParts += `<text x="${fmtNum(bx1 + tagW + 8)}" y="${fmtNum(blk.y1 + 14)}" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="11">[${escapeXml(blk.label)}]</text>`;
      }
    }
    for (const d of blk.dividers) {
      svgParts += `<line x1="${fmtNum(bx1)}" y1="${fmtNum(d.y)}" x2="${fmtNum(bx2)}" y2="${fmtNum(d.y)}" stroke="${escapeXml(pal.subStroke)}" stroke-width="1" stroke-dasharray="4,3"/>`;
      if (d.label || d.kind) {
        const lbl = d.label ? `${d.kind} [${d.label}]` : d.kind;
        svgParts += `<text x="${fmtNum(bx1 + 10)}" y="${fmtNum(d.y + 15)}" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10.5" font-weight="600">${escapeXml(lbl)}</text>`;
      }
    }
  }

  // Lifelines
  for (let i = 0; i < participants.length; i++) {
    const cx = pX[i] + shiftX;
    svgParts += `<line x1="${fmtNum(cx)}" y1="${fmtNum(topY + headerH)}" x2="${fmtNum(cx)}" y2="${fmtNum(bottomBoxY)}" stroke="${escapeXml(pal.border)}" stroke-width="1.3" stroke-dasharray="5,4"/>`;
  }

  // Activation bars
  for (const act of activations) {
    const cx = pX[idxOf.get(act.id)] + shiftX;
    const h = Math.max(14, act.y2 - act.y1);
    svgParts += `<rect x="${fmtNum(cx - 5)}" y="${fmtNum(act.y1)}" width="10" height="${fmtNum(h)}" rx="2" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.accent)}" stroke-width="1.2"/>`;
  }

  // Messages & notes
  for (const ev of placedEvents) {
    if (ev.type === "note") {
      const xs = ev.targets.map((id) => pX[idxOf.get(id)] + shiftX);
      let nx;
      let nw = ev.nw;
      if (ev.where === "left of") nx = xs[0] - nw - 14;
      else if (ev.where === "right of") nx = xs[0] + 14;
      else {
        const lo = Math.min(...xs);
        const hi = Math.max(...xs);
        nw = Math.max(nw, hi - lo + 40);
        nx = (lo + hi) / 2 - nw / 2;
      }
      svgParts += `<rect x="${fmtNum(nx)}" y="${fmtNum(ev.y)}" width="${fmtNum(nw)}" height="${fmtNum(ev.nh)}" rx="4" fill="${escapeXml(pal.noteFill)}" stroke="${escapeXml(pal.noteStroke)}" stroke-width="1.1"/>`;
      svgParts += renderMultilineText(ev.noteLines, nx + nw / 2, ev.y + ev.nh / 2, 11.5, pal.fg, { lineHeight: 15 });
    } else if (ev.type === "msg") {
      const x1 = pX[idxOf.get(ev.from)] + shiftX;
      const x2 = pX[idxOf.get(ev.to)] + shiftX;
      const dashed = ev.arrow.startsWith("--") || ev.arrow.includes("--");
      const dashAttr = dashed ? ' stroke-dasharray="5,4"' : "";
      const markerStyle = ev.arrow.endsWith("x") ? "cross" : ev.arrow.endsWith(")") ? "open" : ev.arrow.endsWith(">>") ? "arrow" : "open";
      const bidir = ev.arrow.startsWith("<<");
      if (ev.selfLoop) {
        const loopW = 36;
        const y1 = ev.y;
        const y2 = ev.y + 22;
        svgParts += `<path d="M ${fmtNum(x1)} ${fmtNum(y1)} L ${fmtNum(x1 + loopW)} ${fmtNum(y1)} L ${fmtNum(x1 + loopW)} ${fmtNum(y2)} L ${fmtNum(x1)} ${fmtNum(y2)}" fill="none" stroke="${escapeXml(pal.edgeStroke)}" stroke-width="1.5"${dashAttr}/>`;
        svgParts += drawArrowhead(x1, y2, x1 + loopW, y2, pal.edgeStroke, markerStyle, 7.5);
        svgParts += renderMultilineText(ev.msgLines, x1 + loopW + 8, (y1 + y2) / 2, 11.5, pal.fg, { anchor: "start", lineHeight: 14.5 });
      } else {
        svgParts += `<line x1="${fmtNum(x1)}" y1="${fmtNum(ev.y)}" x2="${fmtNum(x2)}" y2="${fmtNum(ev.y)}" stroke="${escapeXml(pal.edgeStroke)}" stroke-width="1.5"${dashAttr}/>`;
        svgParts += drawArrowhead(x2, ev.y, x1, ev.y, pal.edgeStroke, markerStyle, 7.5);
        if (bidir) svgParts += drawArrowhead(x1, ev.y, x2, ev.y, pal.edgeStroke, "arrow", 7.5);
        svgParts += renderMultilineText(ev.msgLines, (x1 + x2) / 2, ev.y - 10 - (ev.msgLines.length - 1) * 7, 11.5, pal.fg, { lineHeight: 14.5 });
      }
    }
  }

  // Top & bottom participant headers
  for (let i = 0; i < participants.length; i++) {
    const cx = pX[i] + shiftX;
    svgParts += drawParticipantBox(participants[i], cx, topY, pWidths[i]);
    svgParts += drawParticipantBox(participants[i], cx, bottomBoxY, pWidths[i]);
  }

  return { width: totalW, height: Math.ceil(totalH), svg: wrapSvg(totalW, Math.ceil(totalH), svgParts) };
}

// ============================================================================
// 3. State Diagram (stateDiagram / stateDiagram-v2)
// ============================================================================

function renderStateDiagram(lines, pal) {
  let direction = "TD";
  const nodes = new Map();
  const edges = [];
  const compositeStack = [];
  const composites = [];
  let startCount = 0;
  let endCount = 0;

  const ensureState = (rawId, label = null) => {
    const id = rawId.trim();
    if (!nodes.has(id)) {
      nodes.set(id, { id, label: label ?? id, desc: [], kind: "state" });
    } else if (label) {
      nodes.get(id).label = label;
    }
    for (const c of compositeStack) c.nodes.add(id);
    return id;
  };

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const dirM = line.match(/^direction\s+(TD|TB|BT|LR|RL)/i);
    if (dirM) { direction = dirM[1].toUpperCase(); continue; }
    if (/^note\b/i.test(line) || /^end note$/i.test(line)) continue;
    if (line === "}") { compositeStack.pop(); continue; }

    const compM = line.match(/^state\s+(?:"([^"]+)"\s+as\s+)?([A-Za-z0-9_\-\u4e00-\u9fff]+)\s*\{$/i);
    if (compM) {
      const c = { id: compM[2], title: compM[1] || compM[2], nodes: new Set() };
      composites.push(c);
      compositeStack.push(c);
      continue;
    }
    const aliasM = line.match(/^state\s+"([^"]+)"\s+as\s+([A-Za-z0-9_\-\u4e00-\u9fff]+)$/i);
    if (aliasM) {
      ensureState(aliasM[2], aliasM[1]);
      continue;
    }
    const trM = line.match(/^(\[\*\]|[A-Za-z0-9_\-\u4e00-\u9fff]+)\s*-->\s*(\[\*\]|[A-Za-z0-9_\-\u4e00-\u9fff]+)(?:\s*:\s*(.+))?$/);
    if (trM) {
      let fromId = trM[1];
      let toId = trM[2];
      if (fromId === "[*]") {
        fromId = `__start_${startCount++}`;
        nodes.set(fromId, { id: fromId, label: "", desc: [], kind: "start" });
        for (const c of compositeStack) c.nodes.add(fromId);
      } else {
        ensureState(fromId);
      }
      if (toId === "[*]") {
        toId = `__end_${endCount++}`;
        nodes.set(toId, { id: toId, label: "", desc: [], kind: "end" });
        for (const c of compositeStack) c.nodes.add(toId);
      } else {
        ensureState(toId);
      }
      edges.push({ from: fromId, to: toId, label: trM[3]?.trim() || "", style: "solid", endMarker: "arrow" });
      continue;
    }
    const descM = line.match(/^([A-Za-z0-9_\-\u4e00-\u9fff]+)\s*:\s*(.+)$/);
    if (descM) {
      const id = ensureState(descM[1]);
      nodes.get(id).desc.push(descM[2].trim());
    }
  }

  if (nodes.size === 0) throw new Error("No states found in state diagram");

  for (const n of nodes.values()) {
    if (n.kind === "start" || n.kind === "end") {
      n.w = 22;
      n.h = 22;
    } else {
      const titleW = measureTextWidth(n.label, 12.5, { bold: true });
      const descW = n.desc.length ? Math.max(...n.desc.map((d) => measureTextWidth(d, 11))) : 0;
      n.w = Math.max(84, Math.max(titleW, descW) + 28);
      n.h = n.desc.length ? 30 + n.desc.length * 16 + 10 : 38;
    }
  }

  const layout = layoutLayeredGraph(nodes, edges, { direction, pad: composites.length ? 36 : 26 });
  let minX = 0, minY = 0, maxX = layout.width, maxY = layout.height;
  let compSvg = "";
  for (const c of composites) {
    const ps = [...c.nodes].map((id) => layout.positions.get(id)).filter(Boolean);
    if (!ps.length) continue;
    const x1 = Math.min(...ps.map((p) => p.x)) - 18;
    const y1 = Math.min(...ps.map((p) => p.y)) - 30;
    const x2 = Math.max(...ps.map((p) => p.x + p.w)) + 18;
    const y2 = Math.max(...ps.map((p) => p.y + p.h)) + 16;
    minX = Math.min(minX, x1 - 10);
    minY = Math.min(minY, y1 - 10);
    maxX = Math.max(maxX, x2 + 10);
    maxY = Math.max(maxY, y2 + 10);
    compSvg += `<rect x="${fmtNum(x1)}" y="${fmtNum(y1)}" width="${fmtNum(x2 - x1)}" height="${fmtNum(y2 - y1)}" rx="10" fill="${escapeXml(pal.subFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.3"/>`;
    compSvg += `<line x1="${fmtNum(x1)}" y1="${fmtNum(y1 + 24)}" x2="${fmtNum(x2)}" y2="${fmtNum(y1 + 24)}" stroke="${escapeXml(pal.border)}" stroke-width="1"/>`;
    compSvg += `<text x="${fmtNum(x1 + 12)}" y="${fmtNum(y1 + 16)}" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="11.5" font-weight="600">${escapeXml(c.title)}</text>`;
  }

  let edgeSvg = "";
  edges.forEach((e, idx) => {
    const fp = layout.positions.get(e.from);
    const tp = layout.positions.get(e.to);
    if (fp && tp) edgeSvg += drawLayeredEdge(e, fp, tp, layout.backEdges.has(idx), layout.horizontal, pal);
  });

  let nodeSvg = "";
  for (const [id, n] of nodes) {
    const pos = layout.positions.get(id);
    if (!pos) continue;
    if (n.kind === "start") {
      nodeSvg += `<circle cx="${fmtNum(pos.cx)}" cy="${fmtNum(pos.cy)}" r="9" fill="${escapeXml(pal.accent)}"/>`;
    } else if (n.kind === "end") {
      nodeSvg += `<circle cx="${fmtNum(pos.cx)}" cy="${fmtNum(pos.cy)}" r="10" fill="none" stroke="${escapeXml(pal.accent)}" stroke-width="2"/><circle cx="${fmtNum(pos.cx)}" cy="${fmtNum(pos.cy)}" r="5.5" fill="${escapeXml(pal.accent)}"/>`;
    } else {
      nodeSvg += `<rect x="${fmtNum(pos.x)}" y="${fmtNum(pos.y)}" width="${fmtNum(pos.w)}" height="${fmtNum(pos.h)}" rx="10" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>`;
      if (n.desc.length === 0) {
        nodeSvg += renderMultilineText(splitBrLines(n.label), pos.cx, pos.cy, 12.5, pal.fg, { bold: true });
      } else {
        nodeSvg += `<text x="${fmtNum(pos.cx)}" y="${fmtNum(pos.y + 19)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12.5" font-weight="600">${escapeXml(n.label)}</text>`;
        nodeSvg += `<line x1="${fmtNum(pos.x)}" y1="${fmtNum(pos.y + 27)}" x2="${fmtNum(pos.x + pos.w)}" y2="${fmtNum(pos.y + 27)}" stroke="${escapeXml(pal.border)}" stroke-width="1"/>`;
        n.desc.forEach((d, idx) => {
          nodeSvg += `<text x="${fmtNum(pos.x + 12)}" y="${fmtNum(pos.y + 43 + idx * 16)}" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="11">${escapeXml(d)}</text>`;
        });
      }
    }
  }

  if (layout.backEdges.size > 0 && !layout.horizontal) maxX += 46;
  const W = Math.max(120, Math.ceil(maxX - minX));
  const H = Math.max(70, Math.ceil(maxY - minY));
  const tx = minX < 0 ? -minX : 0;
  const ty = minY < 0 ? -minY : 0;
  const body = tx || ty ? `<g transform="translate(${fmtNum(tx)},${fmtNum(ty)})">${compSvg}${edgeSvg}${nodeSvg}</g>` : compSvg + edgeSvg + nodeSvg;
  return { width: W, height: H, svg: wrapSvg(W, H, body) };
}

// ============================================================================
// 4. Class Diagram
// ============================================================================

function renderClassDiagram(lines, pal) {
  let direction = "TD";
  const classes = new Map();
  const edges = [];
  let activeClass = null;

  const ensureClass = (name) => {
    const clean = name.trim().replace(/~[^~]+~/g, "");
    if (!classes.has(clean)) {
      classes.set(clean, { id: clean, name: name.trim(), stereotype: "", fields: [], methods: [] });
    }
    return classes.get(clean);
  };

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const dirM = line.match(/^direction\s+(TD|TB|BT|LR|RL)/i);
    if (dirM) { direction = dirM[1].toUpperCase(); continue; }
    if (activeClass) {
      if (line === "}") { activeClass = null; continue; }
      if (/^<<.+>>$/.test(line)) { activeClass.stereotype = line; continue; }
      if (line.includes("(")) activeClass.methods.push(line);
      else activeClass.fields.push(line);
      continue;
    }
    const blkM = line.match(/^class\s+([A-Za-z0-9_~<>,\u4e00-\u9fff]+)\s*\{$/);
    if (blkM) {
      activeClass = ensureClass(blkM[1]);
      continue;
    }
    const clsOnly = line.match(/^class\s+([A-Za-z0-9_~<>,\u4e00-\u9fff]+)$/);
    if (clsOnly) { ensureClass(clsOnly[1]); continue; }

    const relM = line.match(/^([A-Za-z0-9_\u4e00-\u9fff]+)(?:\s+"([^"]+)")?\s*(<\|--|--\|>|<\|\.\.|\.\.\|>|\*--|--\*|o--|--o|<--|-->|<\.\.|\.\.>|<\.\.\.|\.\.\.>|---?|\.\.)\s*(?:"([^"]+)"\s+)?([A-Za-z0-9_\u4e00-\u9fff]+)(?:\s*:\s*(.+))?$/);
    if (relM) {
      const left = ensureClass(relM[1]);
      const right = ensureClass(relM[5]);
      const op = relM[3];
      const dashed = op.includes(".");
      const startMarker = op.startsWith("<|") ? "hollow_triangle" : op.startsWith("*") ? "diamond" : op.startsWith("o") ? "hollow_diamond" : op.startsWith("<") ? "arrow" : "none";
      const endMarker = op.endsWith("|>") ? "hollow_triangle" : op.endsWith("*") ? "diamond" : op.endsWith("o") ? "hollow_diamond" : op.endsWith(">") ? "arrow" : "none";
      const labelParts = [relM[2], relM[6]?.trim(), relM[4]].filter(Boolean);
      edges.push({ from: left.id, to: right.id, label: labelParts.join(" "), style: dashed ? "dotted" : "solid", startMarker, endMarker });
      continue;
    }
    const memberM = line.match(/^([A-Za-z0-9_\u4e00-\u9fff]+)\s*:\s*(.+)$/);
    if (memberM) {
      const c = ensureClass(memberM[1]);
      const m = memberM[2].trim();
      if (/^<<.+>>$/.test(m)) c.stereotype = m;
      else if (m.includes("(")) c.methods.push(m);
      else c.fields.push(m);
    }
  }

  if (classes.size === 0) throw new Error("No classes found in class diagram");

  for (const c of classes.values()) {
    const widths = [
      measureTextWidth(c.name, 13, { bold: true }),
      c.stereotype ? measureTextWidth(c.stereotype, 10.5) : 0,
      ...c.fields.map((f) => measureTextWidth(f, 11.5, { mono: true })),
      ...c.methods.map((m) => measureTextWidth(m, 11.5, { mono: true })),
    ];
    c.w = Math.max(120, Math.max(...widths) + 28);
    const headerH = c.stereotype ? 40 : 28;
    const fieldsH = c.fields.length ? c.fields.length * 17 + 10 : 0;
    const methodsH = c.methods.length ? c.methods.length * 17 + 10 : 0;
    c.headerH = headerH;
    c.fieldsH = fieldsH;
    c.h = headerH + fieldsH + methodsH + (fieldsH === 0 && methodsH === 0 ? 6 : 0);
  }

  const layout = layoutLayeredGraph(classes, edges, { direction, rankGap: 60, nodeGap: 36, pad: 26 });
  let edgeSvg = "";
  edges.forEach((e, idx) => {
    const fp = layout.positions.get(e.from);
    const tp = layout.positions.get(e.to);
    if (fp && tp) edgeSvg += drawLayeredEdge(e, fp, tp, layout.backEdges.has(idx), layout.horizontal, pal, { arrowSize: 9 });
  });

  let nodeSvg = "";
  for (const [id, c] of classes) {
    const pos = layout.positions.get(id);
    if (!pos) continue;
    nodeSvg += `<rect x="${fmtNum(pos.x)}" y="${fmtNum(pos.y)}" width="${fmtNum(pos.w)}" height="${fmtNum(pos.h)}" rx="6" fill="${escapeXml(pal.surface)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>`;
    nodeSvg += `<rect x="${fmtNum(pos.x)}" y="${fmtNum(pos.y)}" width="${fmtNum(pos.w)}" height="${fmtNum(c.headerH)}" rx="6" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>`;
    if (c.stereotype) {
      nodeSvg += `<text x="${fmtNum(pos.cx)}" y="${fmtNum(pos.y + 15)}" text-anchor="middle" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10" font-style="italic">${escapeXml(c.stereotype)}</text>`;
      nodeSvg += `<text x="${fmtNum(pos.cx)}" y="${fmtNum(pos.y + 31)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12.5" font-weight="700">${escapeXml(c.name)}</text>`;
    } else {
      nodeSvg += `<text x="${fmtNum(pos.cx)}" y="${fmtNum(pos.y + 19)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12.5" font-weight="700">${escapeXml(c.name)}</text>`;
    }
    let cy = pos.y + c.headerH;
    if (c.fields.length) {
      c.fields.forEach((f, idx) => {
        nodeSvg += `<text x="${fmtNum(pos.x + 10)}" y="${fmtNum(cy + 16 + idx * 17)}" fill="${escapeXml(pal.fg)}" font-family="${FONT_MONO}" font-size="11">${escapeXml(f)}</text>`;
      });
      cy += c.fieldsH;
    }
    if (c.methods.length) {
      if (c.fields.length) {
        nodeSvg += `<line x1="${fmtNum(pos.x)}" y1="${fmtNum(cy)}" x2="${fmtNum(pos.x + pos.w)}" y2="${fmtNum(cy)}" stroke="${escapeXml(pal.border)}" stroke-width="1"/>`;
      }
      c.methods.forEach((m, idx) => {
        nodeSvg += `<text x="${fmtNum(pos.x + 10)}" y="${fmtNum(cy + 16 + idx * 17)}" fill="${escapeXml(pal.fg)}" font-family="${FONT_MONO}" font-size="11">${escapeXml(m)}</text>`;
      });
    }
  }

  const W = layout.width + (layout.backEdges.size > 0 && !layout.horizontal ? 44 : 0);
  return { width: W, height: layout.height, svg: wrapSvg(W, layout.height, edgeSvg + nodeSvg) };
}

// ============================================================================
// 5. ER Diagram
// ============================================================================

function renderErDiagram(lines, pal) {
  const entities = new Map();
  const edges = [];
  let activeEntity = null;

  const ensureEntity = (name) => {
    const id = unquote(name).trim();
    if (!entities.has(id)) entities.set(id, { id, attrs: [] });
    return entities.get(id);
  };

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (activeEntity) {
      if (line === "}") { activeEntity = null; continue; }
      const m = line.match(/^(\S+)\s+(\S+)(?:\s+(PK|FK|UK(?:,\s*(?:PK|FK|UK))*))?(?:\s+"([^"]*)")?/i);
      if (m) {
        activeEntity.attrs.push({ type: m[1], name: m[2], key: m[3] || "", comment: m[4] || "" });
      }
      continue;
    }
    const blkM = line.match(/^("[^"]+"|[A-Za-z0-9_\-\u4e00-\u9fff]+)\s*\{$/);
    if (blkM) { activeEntity = ensureEntity(blkM[1]); continue; }

    const relM = line.match(/^("[^"]+"|[A-Za-z0-9_\-\u4e00-\u9fff]+)\s+([|o}][|o{])(--|\.\.)([|o{][|o}])\s+("[^"]+"|[A-Za-z0-9_\-\u4e00-\u9fff]+)\s*:\s*(.+)$/);
    if (relM) {
      const left = ensureEntity(relM[1]);
      const right = ensureEntity(relM[5]);
      const card = `${relM[2]}..${relM[4]}`;
      const lbl = unquote(relM[6]);
      edges.push({
        from: left.id,
        to: right.id,
        label: lbl ? `${lbl} (${card})` : card,
        style: relM[3] === ".." ? "dotted" : "solid",
        startMarker: "none",
        endMarker: "none",
      });
    }
  }

  if (entities.size === 0) throw new Error("No entities found in ER diagram");

  for (const e of entities.values()) {
    const widths = [
      measureTextWidth(e.id, 13, { bold: true }),
      ...e.attrs.map((a) => measureTextWidth(`${a.type}  ${a.name}  ${a.key}`, 11, { mono: true })),
    ];
    e.w = Math.max(130, Math.max(...widths) + 32);
    e.h = 30 + (e.attrs.length ? e.attrs.length * 19 + 8 : 6);
  }

  const layout = layoutLayeredGraph(entities, edges, { direction: "LR", rankGap: 88, nodeGap: 36, pad: 26 });
  let edgeSvg = "";
  edges.forEach((e, idx) => {
    const fp = layout.positions.get(e.from);
    const tp = layout.positions.get(e.to);
    if (fp && tp) edgeSvg += drawLayeredEdge(e, fp, tp, layout.backEdges.has(idx), layout.horizontal, pal);
  });

  let nodeSvg = "";
  for (const [id, e] of entities) {
    const pos = layout.positions.get(id);
    if (!pos) continue;
    nodeSvg += `<rect x="${fmtNum(pos.x)}" y="${fmtNum(pos.y)}" width="${fmtNum(pos.w)}" height="${fmtNum(pos.h)}" rx="6" fill="${escapeXml(pal.surface)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>`;
    nodeSvg += `<rect x="${fmtNum(pos.x)}" y="${fmtNum(pos.y)}" width="${fmtNum(pos.w)}" height="28" rx="6" fill="${escapeXml(pal.nodeFill)}" stroke="${escapeXml(pal.nodeStroke)}" stroke-width="1.5"/>`;
    nodeSvg += `<text x="${fmtNum(pos.cx)}" y="${fmtNum(pos.y + 19)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12.5" font-weight="700">${escapeXml(e.id)}</text>`;
    e.attrs.forEach((a, idx) => {
      const ry = pos.y + 28 + idx * 19;
      if (idx % 2 === 1) {
        nodeSvg += `<rect x="${fmtNum(pos.x + 1)}" y="${fmtNum(ry)}" width="${fmtNum(pos.w - 2)}" height="19" fill="${escapeXml(pal.subFill)}"/>`;
      }
      nodeSvg += `<text x="${fmtNum(pos.x + 10)}" y="${fmtNum(ry + 13.5)}" fill="${escapeXml(pal.muted)}" font-family="${FONT_MONO}" font-size="10.5">${escapeXml(a.type)}</text>`;
      const typeW = measureTextWidth(a.type, 10.5, { mono: true });
      nodeSvg += `<text x="${fmtNum(pos.x + 18 + typeW)}" y="${fmtNum(ry + 13.5)}" fill="${escapeXml(pal.fg)}" font-family="${FONT_MONO}" font-size="11">${escapeXml(a.name)}</text>`;
      if (a.key) {
        nodeSvg += `<text x="${fmtNum(pos.x + pos.w - 10)}" y="${fmtNum(ry + 13.5)}" text-anchor="end" fill="${escapeXml(pal.accent)}" font-family="${FONT_MONO}" font-size="10" font-weight="700">${escapeXml(a.key)}</text>`;
      }
    });
  }

  return { width: layout.width, height: layout.height, svg: wrapSvg(layout.width, layout.height, edgeSvg + nodeSvg) };
}

// ============================================================================
// 6. Pie Chart
// ============================================================================

function renderPieChart(lines, pal) {
  const showData = /showData/i.test(lines[0]);
  let title = "";
  const slices = [];

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const tM = line.match(/^title\s+(.+)$/i);
    if (tM) { title = tM[1].trim(); continue; }
    const sM = line.match(/^"([^"]+)"\s*:\s*([0-9]+(?:\.[0-9]+)?)$/);
    if (sM) {
      const val = parseFloat(sM[2]);
      if (Number.isFinite(val) && val > 0) slices.push({ label: sM[1], value: val });
    }
  }

  if (slices.length === 0) throw new Error("No data slices found in pie chart");
  const total = slices.reduce((s, item) => s + item.value, 0);

  const r = 92;
  const titleH = title ? 30 : 0;
  const cx = 120;
  const cy = titleH + 114;
  const legendX = cx + r + 34;
  const maxLegendW = Math.max(...slices.map((s) => measureTextWidth(showData ? `${s.label} (${s.value})` : s.label, 12)), 80);
  const W = Math.ceil(legendX + 20 + maxLegendW + 24);
  const H = Math.ceil(Math.max(cy + r + 24, titleH + 34 + slices.length * 24 + 20));

  let svg = "";
  if (title) {
    svg += `<text x="${fmtNum(W / 2)}" y="24" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="14" font-weight="700">${escapeXml(title)}</text>`;
  }

  let angle = -Math.PI / 2;
  slices.forEach((s, idx) => {
    const frac = s.value / total;
    const sweep = frac * Math.PI * 2;
    const color = pal.series[idx % pal.series.length];
    if (slices.length === 1) {
      svg += `<circle cx="${fmtNum(cx)}" cy="${fmtNum(cy)}" r="${fmtNum(r)}" fill="${escapeXml(color)}" stroke="${escapeXml(pal.bg)}" stroke-width="1.5"/>`;
    } else {
      const x1 = cx + r * Math.cos(angle);
      const y1 = cy + r * Math.sin(angle);
      const x2 = cx + r * Math.cos(angle + sweep);
      const y2 = cy + r * Math.sin(angle + sweep);
      const large = sweep > Math.PI ? 1 : 0;
      svg += `<path d="M ${fmtNum(cx)} ${fmtNum(cy)} L ${fmtNum(x1)} ${fmtNum(y1)} A ${fmtNum(r)} ${fmtNum(r)} 0 ${large} 1 ${fmtNum(x2)} ${fmtNum(y2)} Z" fill="${escapeXml(color)}" stroke="${escapeXml(pal.bg)}" stroke-width="1.5"/>`;
    }
    if (frac >= 0.05) {
      const mid = angle + sweep / 2;
      const lx = cx + r * 0.64 * Math.cos(mid);
      const ly = cy + r * 0.64 * Math.sin(mid) + 4;
      svg += `<text x="${fmtNum(lx)}" y="${fmtNum(ly)}" text-anchor="middle" fill="#ffffff" font-family="${FONT_SANS}" font-size="11" font-weight="700">${Math.round(frac * 100)}%</text>`;
    }
    angle += sweep;

    // Legend item
    const ly = titleH + 36 + idx * 24;
    svg += `<rect x="${fmtNum(legendX)}" y="${fmtNum(ly - 10)}" width="12" height="12" rx="3" fill="${escapeXml(color)}"/>`;
    const legendText = showData ? `${s.label} (${s.value})` : `${s.label} (${Math.round(frac * 100)}%)`;
    svg += `<text x="${fmtNum(legendX + 18)}" y="${fmtNum(ly)}" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12">${escapeXml(legendText)}</text>`;
  });

  return { width: W, height: H, svg: wrapSvg(W, H, svg) };
}

// ============================================================================
// 7. Gantt Chart
// ============================================================================

function parseGanttDate(token) {
  const m = String(token ?? "").trim().match(/^(\d{4})-(\d{1,2})-(\d{1,2})$/);
  if (!m) return null;
  return Date.UTC(Number(m[1]), Number(m[2]) - 1, Number(m[3])) / 86400000;
}

function parseGanttDuration(token) {
  const m = String(token ?? "").trim().match(/^(\d+(?:\.\d+)?)(d|w|h|m)$/i);
  if (!m) return null;
  const v = parseFloat(m[1]);
  const u = m[2].toLowerCase();
  if (u === "w") return v * 7;
  if (u === "h") return v / 24;
  if (u === "m") return v / 1440;
  return v;
}

function formatDayShort(dayNum) {
  const d = new Date(dayNum * 86400000);
  const mm = String(d.getUTCMonth() + 1).padStart(2, "0");
  const dd = String(d.getUTCDate()).padStart(2, "0");
  return `${mm}-${dd}`;
}

function renderGanttChart(lines, pal) {
  let title = "";
  let currentSection = "";
  const tasks = [];
  const taskById = new Map();
  let lastEnd = 0;

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (/^(?:dateFormat|axisFormat|excludes|includes|todayMarker|tickInterval)\b/i.test(line)) continue;
    const tM = line.match(/^title\s+(.+)$/i);
    if (tM) { title = tM[1].trim(); continue; }
    const sM = line.match(/^section\s+(.+)$/i);
    if (sM) { currentSection = sM[1].trim(); continue; }

    const idx = line.indexOf(":");
    if (idx <= 0) continue;
    const name = line.slice(0, idx).trim();
    const parts = line.slice(idx + 1).split(",").map((s) => s.trim()).filter(Boolean);
    const flags = new Set();
    let id = null;
    let start = null;
    let end = null;
    let dur = null;

    for (const p of parts) {
      const low = p.toLowerCase();
      if (low === "done" || low === "active" || low === "crit" || low === "milestone") {
        flags.add(low);
      } else if (/^after\s+/i.test(p)) {
        const deps = p.slice(6).trim().split(/\s+/);
        let maxEnd = lastEnd;
        for (const dep of deps) {
          const prev = taskById.get(dep);
          if (prev) maxEnd = Math.max(maxEnd, prev.end);
        }
        start = maxEnd;
      } else {
        const dt = parseGanttDate(p);
        if (dt !== null) {
          if (start === null) start = dt;
          else end = dt;
        } else {
          const dd = parseGanttDuration(p);
          if (dd !== null) dur = dd;
          else if (!id) id = p;
        }
      }
    }
    if (start === null) start = lastEnd;
    if (end === null) end = start + (dur ?? (flags.has("milestone") ? 0 : 3));
    if (end < start) end = start + 1;
    lastEnd = end;
    const task = { name, section: currentSection, id, start, end, flags };
    tasks.push(task);
    if (id) taskById.set(id, task);
  }

  if (tasks.length === 0) throw new Error("No tasks found in Gantt chart");

  const minDay = Math.min(...tasks.map((t) => t.start));
  const maxDay = Math.max(...tasks.map((t) => t.end), minDay + 1);
  const span = Math.max(1, maxDay - minDay);
  const useDates = minDay > 10000;

  const labelColW = Math.min(200, Math.max(110, Math.max(...tasks.map((t) => measureTextWidth(t.name, 11.5))) + 24));
  const chartW = 420;
  const W = Math.ceil(labelColW + chartW + 28);
  const titleH = title ? 32 : 8;
  const axisH = 26;
  const rowH = 28;
  const H = Math.ceil(titleH + axisH + tasks.length * rowH + 18);

  let svg = "";
  if (title) {
    svg += `<text x="${fmtNum(W / 2)}" y="24" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="14" font-weight="700">${escapeXml(title)}</text>`;
  }

  const chartX = labelColW + 10;
  const gridTop = titleH + axisH;
  const gridBot = gridTop + tasks.length * rowH;

  // Vertical grid ticks
  const ticks = 5;
  for (let k = 0; k <= ticks; k++) {
    const frac = k / ticks;
    const x = chartX + frac * chartW;
    const dayVal = minDay + frac * span;
    svg += `<line x1="${fmtNum(x)}" y1="${fmtNum(gridTop)}" x2="${fmtNum(x)}" y2="${fmtNum(gridBot)}" stroke="${escapeXml(pal.border)}" stroke-width="0.8" stroke-dasharray="3,3"/>`;
    const lbl = useDates ? formatDayShort(dayVal) : `D${Math.round(dayVal)}`;
    svg += `<text x="${fmtNum(x)}" y="${fmtNum(gridTop - 8)}" text-anchor="middle" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10">${escapeXml(lbl)}</text>`;
  }

  const sectionOrder = [...new Set(tasks.map((t) => t.section))];
  tasks.forEach((t, idx) => {
    const y = gridTop + idx * rowH;
    if (idx % 2 === 1) {
      svg += `<rect x="10" y="${fmtNum(y)}" width="${fmtNum(W - 20)}" height="${fmtNum(rowH)}" fill="${escapeXml(pal.subFill)}" fill-opacity="0.55"/>`;
    }
    svg += `<text x="${fmtNum(labelColW)}" y="${fmtNum(y + rowH / 2 + 4)}" text-anchor="end" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="11.5">${escapeXml(t.name)}</text>`;

    const x1 = chartX + ((t.start - minDay) / span) * chartW;
    const x2 = chartX + ((t.end - minDay) / span) * chartW;
    const secIdx = Math.max(0, sectionOrder.indexOf(t.section));
    let barColor = pal.series[secIdx % pal.series.length];
    if (t.flags.has("crit")) barColor = pal.dark ? "#ff7b72" : "#cf222e";
    else if (t.flags.has("done")) barColor = mixHex(pal.muted, pal.surface, 0.35);

    if (t.flags.has("milestone")) {
      const cy = y + rowH / 2;
      const r = 7;
      svg += `<polygon points="${fmtNum(x1)},${fmtNum(cy - r)} ${fmtNum(x1 + r)},${fmtNum(cy)} ${fmtNum(x1)},${fmtNum(cy + r)} ${fmtNum(x1 - r)},${fmtNum(cy)}" fill="${escapeXml(barColor)}"/>`;
    } else {
      const bw = Math.max(8, x2 - x1);
      svg += `<rect x="${fmtNum(x1)}" y="${fmtNum(y + 5)}" width="${fmtNum(bw)}" height="${fmtNum(rowH - 10)}" rx="4" fill="${escapeXml(barColor)}" fill-opacity="${t.flags.has("done") ? "0.65" : "0.9"}"/>`;
    }
  });

  return { width: W, height: H, svg: wrapSvg(W, H, svg) };
}

// ============================================================================
// 8. Mindmap
// ============================================================================

function parseMindmapNodeLabel(raw) {
  let s = raw.trim();
  let shape = "round";
  if (s.startsWith("((") && s.endsWith("))")) { s = s.slice(2, -2); shape = "circle"; }
  else if (s.startsWith("(") && s.endsWith(")")) { s = s.slice(1, -1); shape = "stadium"; }
  else if (s.startsWith("[") && s.endsWith("]")) { s = s.slice(1, -1); shape = "rect"; }
  else if (s.startsWith("))") && s.endsWith("((")) { s = s.slice(2, -2); shape = "round"; }
  else if (s.startsWith(")") && s.endsWith("(")) { s = s.slice(1, -1); shape = "round"; }
  else if (/^root\s*\(/i.test(s)) {
    const m = s.match(/^root\s*\(+(.+?)\)+$/i);
    if (m) { s = m[1]; shape = "circle"; }
  }
  return { label: unquote(s.trim()), shape };
}

function renderMindmap(rawLines, pal) {
  // Keep indentation from raw lines (skip comments and header)
  const items = [];
  for (let i = 1; i < rawLines.length; i++) {
    const raw = rawLines[i];
    const trimmed = raw.trim();
    if (!trimmed || trimmed.startsWith("%%") || trimmed.startsWith("::icon(")) continue;
    const indent = raw.search(/\S/);
    const { label, shape } = parseMindmapNodeLabel(trimmed);
    items.push({ indent, label, shape, children: [] });
  }
  if (items.length === 0) throw new Error("No nodes found in mindmap");

  const root = items[0];
  const stack = [root];
  for (let i = 1; i < items.length; i++) {
    const node = items[i];
    while (stack.length > 1 && stack[stack.length - 1].indent >= node.indent) stack.pop();
    stack[stack.length - 1].children.push(node);
    stack.push(node);
  }

  // Measure nodes & subtree heights
  const assignMetrics = (node, depth, branchIdx) => {
    node.depth = depth;
    node.branchIdx = branchIdx;
    const fs = depth === 0 ? 13.5 : depth === 1 ? 12.5 : 11.5;
    node.fs = fs;
    node.lines = wrapLines(node.label, 160, fs);
    node.w = Math.max(64, Math.max(...node.lines.map((l) => measureTextWidth(l, fs, { bold: depth <= 1 }))) + 24);
    node.h = Math.max(30, node.lines.length * (fs * 1.35) + 12);
    if (node.children.length === 0) {
      node.subH = node.h;
    } else {
      let sum = 0;
      node.children.forEach((ch, idx) => {
        assignMetrics(ch, depth + 1, depth === 0 ? idx : branchIdx);
        sum += ch.subH;
      });
      node.subH = Math.max(node.h, sum + (node.children.length - 1) * 14);
    }
  };
  assignMetrics(root, 0, 0);

  let maxX = 0;
  let maxY = 0;
  const place = (node, x, topY) => {
    node.x = x;
    node.y = topY + (node.subH - node.h) / 2;
    maxX = Math.max(maxX, node.x + node.w);
    maxY = Math.max(maxY, node.y + node.h);
    let cy = topY;
    for (const ch of node.children) {
      place(ch, x + node.w + 42, cy);
      cy += ch.subH + 14;
    }
  };
  place(root, 22, 22);

  let edgesSvg = "";
  let nodesSvg = "";
  const draw = (node) => {
    const accent = node.depth === 0 ? pal.accent : pal.series[node.branchIdx % pal.series.length];
    const fill = node.depth === 0 ? accent : mixHex(pal.surface, accent, pal.dark ? 0.22 : 0.14);
    const textCol = node.depth === 0 ? "#ffffff" : pal.fg;
    const rx = node.shape === "rect" ? 4 : node.shape === "stadium" || node.shape === "circle" ? node.h / 2 : 8;
    nodesSvg += `<rect x="${fmtNum(node.x)}" y="${fmtNum(node.y)}" width="${fmtNum(node.w)}" height="${fmtNum(node.h)}" rx="${fmtNum(rx)}" fill="${escapeXml(fill)}" stroke="${escapeXml(accent)}" stroke-width="1.5"/>`;
    nodesSvg += renderMultilineText(node.lines, node.x + node.w / 2, node.y + node.h / 2, node.fs, textCol, { bold: node.depth <= 1 });

    for (const ch of node.children) {
      const chAccent = pal.series[ch.branchIdx % pal.series.length];
      const x1 = node.x + node.w;
      const y1 = node.y + node.h / 2;
      const x2 = ch.x;
      const y2 = ch.y + ch.h / 2;
      const mx = (x1 + x2) / 2;
      edgesSvg += `<path d="M ${fmtNum(x1)} ${fmtNum(y1)} C ${fmtNum(mx)} ${fmtNum(y1)}, ${fmtNum(mx)} ${fmtNum(y2)}, ${fmtNum(x2)} ${fmtNum(y2)}" fill="none" stroke="${escapeXml(chAccent)}" stroke-width="1.7"/>`;
      draw(ch);
    }
  };
  draw(root);

  const W = Math.ceil(maxX + 24);
  const H = Math.ceil(maxY + 24);
  return { width: W, height: H, svg: wrapSvg(W, H, edgesSvg + nodesSvg) };
}

// ============================================================================
// 9. GitGraph
// ============================================================================

function renderGitGraph(lines, pal) {
  const branches = ["main"];
  const branchHead = new Map([["main", null]]);
  let curBranch = "main";
  const commits = [];

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const brM = line.match(/^branch\s+(\S+)/i);
    if (brM) {
      const name = unquote(brM[1]);
      if (!branches.includes(name)) {
        branches.push(name);
        branchHead.set(name, branchHead.get(curBranch));
      }
      curBranch = name;
      continue;
    }
    const coM = line.match(/^(?:checkout|switch)\s+(\S+)/i);
    if (coM) {
      const name = unquote(coM[1]);
      if (branches.includes(name)) curBranch = name;
      continue;
    }
    const mergeM = line.match(/^merge\s+(\S+)(.*)$/i);
    if (mergeM) {
      const fromBr = unquote(mergeM[1]);
      const tagM = mergeM[2].match(/tag:\s*"([^"]+)"/i);
      const idM = mergeM[2].match(/id:\s*"([^"]+)"/i);
      const parents = [branchHead.get(curBranch), branchHead.get(fromBr)].filter((p) => p !== null && p !== undefined);
      const c = { idx: commits.length, branch: curBranch, parents, id: idM ? idM[1] : `m${commits.length + 1}`, tag: tagM ? tagM[1] : "", isMerge: true };
      commits.push(c);
      branchHead.set(curBranch, c.idx);
      continue;
    }
    if (/^commit\b/i.test(line)) {
      const idM = line.match(/id:\s*"([^"]+)"/i);
      const tagM = line.match(/tag:\s*"([^"]+)"/i);
      const typeM = line.match(/type:\s*(NORMAL|REVERSE|HIGHLIGHT)/i);
      const parent = branchHead.get(curBranch);
      const c = {
        idx: commits.length,
        branch: curBranch,
        parents: parent !== null && parent !== undefined ? [parent] : [],
        id: idM ? idM[1] : `c${commits.length + 1}`,
        tag: tagM ? tagM[1] : "",
        kind: typeM ? typeM[1].toUpperCase() : "NORMAL",
      };
      commits.push(c);
      branchHead.set(curBranch, c.idx);
    }
  }

  if (commits.length === 0) throw new Error("No commits found in gitGraph");

  const labelW = Math.max(70, Math.max(...branches.map((b) => measureTextWidth(b, 11.5, { bold: true }))) + 24);
  const stepX = 64;
  const laneH = 44;
  const topPad = 30;
  const W = Math.ceil(labelW + commits.length * stepX + 36);
  const H = Math.ceil(topPad + branches.length * laneH + 28);

  const laneY = (br) => topPad + branches.indexOf(br) * laneH + laneH / 2;
  const commitX = (idx) => labelW + 26 + idx * stepX;

  let svg = "";
  // Branch rails & labels
  branches.forEach((br, idx) => {
    const y = laneY(br);
    const col = pal.series[idx % pal.series.length];
    svg += `<rect x="10" y="${fmtNum(y - 11)}" width="${fmtNum(labelW - 16)}" height="22" rx="11" fill="${escapeXml(mixHex(pal.surface, col, 0.2))}" stroke="${escapeXml(col)}" stroke-width="1.2"/>`;
    svg += `<text x="${fmtNum(10 + (labelW - 16) / 2)}" y="${fmtNum(y + 4)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="11" font-weight="600">${escapeXml(br)}</text>`;
    svg += `<line x1="${fmtNum(labelW)}" y1="${fmtNum(y)}" x2="${fmtNum(W - 16)}" y2="${fmtNum(y)}" stroke="${escapeXml(col)}" stroke-opacity="0.28" stroke-width="1.5" stroke-dasharray="4,4"/>`;
  });

  // Edges between commits
  for (const c of commits) {
    const x2 = commitX(c.idx);
    const y2 = laneY(c.branch);
    const col = pal.series[branches.indexOf(c.branch) % pal.series.length];
    for (const pIdx of c.parents) {
      const p = commits[pIdx];
      if (!p) continue;
      const x1 = commitX(p.idx);
      const y1 = laneY(p.branch);
      const edgeCol = p.branch === c.branch ? col : pal.series[branches.indexOf(p.branch) % pal.series.length];
      if (Math.abs(y1 - y2) < 1) {
        svg += `<line x1="${fmtNum(x1)}" y1="${fmtNum(y1)}" x2="${fmtNum(x2)}" y2="${fmtNum(y2)}" stroke="${escapeXml(edgeCol)}" stroke-width="2.2"/>`;
      } else {
        const mx = (x1 + x2) / 2;
        svg += `<path d="M ${fmtNum(x1)} ${fmtNum(y1)} C ${fmtNum(mx)} ${fmtNum(y1)}, ${fmtNum(mx)} ${fmtNum(y2)}, ${fmtNum(x2)} ${fmtNum(y2)}" fill="none" stroke="${escapeXml(edgeCol)}" stroke-width="2.2"/>`;
      }
    }
  }

  // Commit nodes
  for (const c of commits) {
    const x = commitX(c.idx);
    const y = laneY(c.branch);
    const col = pal.series[branches.indexOf(c.branch) % pal.series.length];
    if (c.kind === "HIGHLIGHT") {
      svg += `<rect x="${fmtNum(x - 7)}" y="${fmtNum(y - 7)}" width="14" height="14" rx="3" fill="${escapeXml(col)}" stroke="${escapeXml(pal.bg)}" stroke-width="2"/>`;
    } else if (c.isMerge) {
      svg += `<circle cx="${fmtNum(x)}" cy="${fmtNum(y)}" r="8" fill="${escapeXml(pal.bg)}" stroke="${escapeXml(col)}" stroke-width="2.5"/><circle cx="${fmtNum(x)}" cy="${fmtNum(y)}" r="3.5" fill="${escapeXml(col)}"/>`;
    } else {
      svg += `<circle cx="${fmtNum(x)}" cy="${fmtNum(y)}" r="7" fill="${escapeXml(col)}" stroke="${escapeXml(pal.bg)}" stroke-width="2"/>`;
    }
    svg += `<text x="${fmtNum(x)}" y="${fmtNum(y + 20)}" text-anchor="middle" fill="${escapeXml(pal.muted)}" font-family="${FONT_MONO}" font-size="9.5">${escapeXml(c.id)}</text>`;
    if (c.tag) {
      const tw = measureTextWidth(c.tag, 9.5, { bold: true }) + 10;
      svg += `<rect x="${fmtNum(x - tw / 2)}" y="${fmtNum(y - 25)}" width="${fmtNum(tw)}" height="15" rx="4" fill="${escapeXml(pal.noteFill)}" stroke="${escapeXml(pal.noteStroke)}" stroke-width="1"/>`;
      svg += `<text x="${fmtNum(x)}" y="${fmtNum(y - 14.5)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="9.5" font-weight="600">${escapeXml(c.tag)}</text>`;
    }
  }

  return { width: W, height: H, svg: wrapSvg(W, H, svg) };
}

// ============================================================================
// 10. Timeline
// ============================================================================

function renderTimeline(lines, pal) {
  let title = "";
  const periods = [];
  let cur = null;

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const tM = line.match(/^title\s+(.+)$/i);
    if (tM) { title = tM[1].trim(); continue; }
    if (/^section\s+/i.test(line)) continue;
    if (line.startsWith(":") && cur) {
      cur.events.push(line.slice(1).trim());
      continue;
    }
    const parts = line.split(":").map((s) => s.trim()).filter(Boolean);
    if (parts.length >= 1) {
      cur = { label: parts[0], events: parts.slice(1) };
      periods.push(cur);
    }
  }
  if (periods.length === 0) throw new Error("No entries found in timeline");

  const colW = 148;
  const gap = 16;
  const pad = 20;
  const titleH = title ? 34 : 10;
  const maxEvents = Math.max(1, ...periods.map((p) => p.events.length));
  const W = Math.ceil(pad * 2 + periods.length * colW + (periods.length - 1) * gap);
  const H = Math.ceil(titleH + 54 + maxEvents * 42 + 20);

  let svg = "";
  if (title) {
    svg += `<text x="${fmtNum(W / 2)}" y="24" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="14" font-weight="700">${escapeXml(title)}</text>`;
  }

  const axisY = titleH + 40;
  svg += `<line x1="${fmtNum(pad)}" y1="${fmtNum(axisY)}" x2="${fmtNum(W - pad)}" y2="${fmtNum(axisY)}" stroke="${escapeXml(pal.accent)}" stroke-width="2"/>`;

  periods.forEach((p, idx) => {
    const col = pal.series[idx % pal.series.length];
    const x = pad + idx * (colW + gap);
    const cx = x + colW / 2;
    svg += `<rect x="${fmtNum(x)}" y="${fmtNum(titleH)}" width="${fmtNum(colW)}" height="28" rx="6" fill="${escapeXml(mixHex(pal.surface, col, 0.22))}" stroke="${escapeXml(col)}" stroke-width="1.3"/>`;
    svg += `<text x="${fmtNum(cx)}" y="${fmtNum(titleH + 18)}" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="12" font-weight="700">${escapeXml(p.label)}</text>`;
    svg += `<circle cx="${fmtNum(cx)}" cy="${fmtNum(axisY)}" r="5" fill="${escapeXml(col)}"/>`;
    p.events.forEach((ev, eIdx) => {
      const ey = axisY + 14 + eIdx * 40;
      const evLines = wrapLines(ev, colW - 14, 11).slice(0, 2);
      svg += `<rect x="${fmtNum(x)}" y="${fmtNum(ey)}" width="${fmtNum(colW)}" height="34" rx="5" fill="${escapeXml(pal.surface)}" stroke="${escapeXml(pal.border)}" stroke-width="1"/>`;
      svg += renderMultilineText(evLines, cx, ey + 17, 11, pal.fg, { lineHeight: 13.5 });
    });
  });

  return { width: W, height: H, svg: wrapSvg(W, H, svg) };
}

// ============================================================================
// 11. XY Chart (xychart-beta)
// ============================================================================

function renderXyChart(lines, pal) {
  let title = "";
  let xLabels = [];
  let yTitle = "";
  let yMin = null;
  let yMax = null;
  const bars = [];
  const lineSeries = [];

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    const tM = line.match(/^title\s+(.+)$/i);
    if (tM) { title = unquote(tM[1]); continue; }
    const xM = line.match(/^x-axis\s+(?:(?:"[^"]+")?\s*)?\[([^\]]+)\]/i);
    if (xM) { xLabels = xM[1].split(",").map((s) => unquote(s.trim())); continue; }
    const yM = line.match(/^y-axis\s+(?:"([^"]+)"\s*)?(?:(-?\d+(?:\.\d+)?)\s*-->\s*(-?\d+(?:\.\d+)?))?/i);
    if (yM) {
      if (yM[1]) yTitle = yM[1];
      if (yM[2] !== undefined) yMin = parseFloat(yM[2]);
      if (yM[3] !== undefined) yMax = parseFloat(yM[3]);
      continue;
    }
    const barM = line.match(/^bar\s+\[([^\]]+)\]/i);
    if (barM) { bars.push(barM[1].split(",").map((v) => parseFloat(v.trim())).filter(Number.isFinite)); continue; }
    const lnM = line.match(/^line\s+\[([^\]]+)\]/i);
    if (lnM) { lineSeries.push(lnM[1].split(",").map((v) => parseFloat(v.trim())).filter(Number.isFinite)); continue; }
  }

  const allVals = [...bars.flat(), ...lineSeries.flat()];
  if (allVals.length === 0) throw new Error("No data series found in xychart");
  const count = Math.max(xLabels.length, ...bars.map((b) => b.length), ...lineSeries.map((l) => l.length));
  if (xLabels.length === 0) xLabels = Array.from({ length: count }, (_, i) => String(i + 1));

  const lo = yMin !== null ? yMin : Math.min(0, ...allVals);
  const hi = yMax !== null ? yMax : Math.max(...allVals, lo + 1);
  const span = Math.max(1e-6, hi - lo);

  const W = Math.max(380, Math.min(680, 96 + count * 52));
  const H = 260;
  const padL = 56, padR = 24, padT = title ? 40 : 22, padB = 40;
  const plotW = W - padL - padR;
  const plotH = H - padT - padB;

  const yOf = (v) => padT + plotH - ((v - lo) / span) * plotH;
  const xOf = (i) => padL + ((i + 0.5) / count) * plotW;

  let svg = "";
  if (title) {
    svg += `<text x="${fmtNum(W / 2)}" y="24" text-anchor="middle" fill="${escapeXml(pal.fg)}" font-family="${FONT_SANS}" font-size="13.5" font-weight="700">${escapeXml(title)}</text>`;
  }

  // Grid & Y labels
  for (let k = 0; k <= 4; k++) {
    const val = lo + (span * k) / 4;
    const y = yOf(val);
    svg += `<line x1="${fmtNum(padL)}" y1="${fmtNum(y)}" x2="${fmtNum(W - padR)}" y2="${fmtNum(y)}" stroke="${escapeXml(pal.border)}" stroke-width="0.8"/>`;
    svg += `<text x="${fmtNum(padL - 8)}" y="${fmtNum(y + 4)}" text-anchor="end" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10">${fmtNum(val)}</text>`;
  }
  if (yTitle) {
    svg += `<text x="14" y="${fmtNum(padT + plotH / 2)}" text-anchor="middle" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10.5" transform="rotate(-90 14 ${fmtNum(padT + plotH / 2)})">${escapeXml(yTitle)}</text>`;
  }

  // Bars
  const slotW = plotW / count;
  bars.forEach((series, sIdx) => {
    const col = pal.series[sIdx % pal.series.length];
    const bw = Math.max(8, (slotW * 0.62) / Math.max(1, bars.length));
    series.forEach((v, i) => {
      const bx = xOf(i) - (bw * bars.length) / 2 + sIdx * bw;
      const by = yOf(v);
      const baseY = yOf(Math.max(0, lo));
      const bh = Math.max(1, Math.abs(baseY - by));
      svg += `<rect x="${fmtNum(bx)}" y="${fmtNum(Math.min(by, baseY))}" width="${fmtNum(bw - 2)}" height="${fmtNum(bh)}" rx="3" fill="${escapeXml(col)}" fill-opacity="0.85"/>`;
    });
  });

  // Lines
  lineSeries.forEach((series, sIdx) => {
    const col = pal.series[(bars.length + sIdx) % pal.series.length];
    const pts = series.map((v, i) => `${fmtNum(xOf(i))},${fmtNum(yOf(v))}`).join(" ");
    svg += `<polyline points="${pts}" fill="none" stroke="${escapeXml(col)}" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>`;
    series.forEach((v, i) => {
      svg += `<circle cx="${fmtNum(xOf(i))}" cy="${fmtNum(yOf(v))}" r="3.5" fill="${escapeXml(col)}" stroke="${escapeXml(pal.bg)}" stroke-width="1.2"/>`;
    });
  });

  // Axes & X labels
  svg += `<line x1="${fmtNum(padL)}" y1="${fmtNum(padT + plotH)}" x2="${fmtNum(W - padR)}" y2="${fmtNum(padT + plotH)}" stroke="${escapeXml(pal.edgeStroke)}" stroke-width="1.2"/>`;
  svg += `<line x1="${fmtNum(padL)}" y1="${fmtNum(padT)}" x2="${fmtNum(padL)}" y2="${fmtNum(padT + plotH)}" stroke="${escapeXml(pal.edgeStroke)}" stroke-width="1.2"/>`;
  xLabels.forEach((lbl, i) => {
    svg += `<text x="${fmtNum(xOf(i))}" y="${fmtNum(padT + plotH + 18)}" text-anchor="middle" fill="${escapeXml(pal.muted)}" font-family="${FONT_SANS}" font-size="10.5">${escapeXml(lbl)}</text>`;
  });

  return { width: W, height: H, svg: wrapSvg(W, H, svg) };
}

// ============================================================================
// Main Dispatcher
// ============================================================================

function wrapSvg(width, height, inner, scale = 1) {
  const sw = Math.max(16, Math.round(width * scale));
  const sh = Math.max(16, Math.round(height * scale));
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${sw}" height="${sh}" viewBox="0 0 ${width} ${height}">${inner}</svg>`;
}

export function detectMermaidKind(source) {
  const lines = stripDirectivesAndComments(source);
  if (!lines.length) return null;
  const first = lines[0];
  if (/^(?:flowchart|graph)\b/i.test(first)) return "flowchart";
  if (/^sequenceDiagram\b/i.test(first)) return "sequence";
  if (/^stateDiagram(?:-v2)?\b/i.test(first)) return "state";
  if (/^classDiagram(?:-v2)?\b/i.test(first)) return "class";
  if (/^erDiagram\b/i.test(first)) return "er";
  if (/^pie\b/i.test(first)) return "pie";
  if (/^gantt\b/i.test(first)) return "gantt";
  if (/^mindmap\b/i.test(first)) return "mindmap";
  if (/^gitGraph\b/i.test(first)) return "gitgraph";
  if (/^timeline\b/i.test(first)) return "timeline";
  if (/^xychart-beta\b/i.test(first)) return "xychart";
  return null;
}

export function renderMermaidSvg(source, options = {}) {
  const raw = String(source ?? "");
  const lines = stripDirectivesAndComments(raw);
  if (lines.length === 0) {
    return { ok: false, kind: null, label: "Mermaid", error: "Empty Mermaid diagram", width: 0, height: 0, svg: "", dataUrl: "" };
  }

  const kind = detectMermaidKind(raw);
  if (!kind) {
    return {
      ok: false,
      kind: null,
      label: "Mermaid",
      error: `Unsupported Mermaid diagram header: ${lines[0].slice(0, 48)}`,
      width: 0,
      height: 0,
      svg: "",
      dataUrl: "",
    };
  }

  const pal = buildPalette(options.theme);
  const scale = Math.max(0.5, Math.min(2.5, Number(options.scale) || 1));

  try {
    let result;
    switch (kind) {
      case "flowchart": result = renderFlowchart(lines, pal); break;
      case "sequence": result = renderSequenceDiagram(lines, pal); break;
      case "state": result = renderStateDiagram(lines, pal); break;
      case "class": result = renderClassDiagram(lines, pal); break;
      case "er": result = renderErDiagram(lines, pal); break;
      case "pie": result = renderPieChart(lines, pal); break;
      case "gantt": result = renderGanttChart(lines, pal); break;
      case "mindmap": result = renderMindmap(raw.replace(/%%\{[\s\S]*?\}%%/g, "").split(/\r?\n/), pal); break;
      case "gitgraph": result = renderGitGraph(lines, pal); break;
      case "timeline": result = renderTimeline(lines, pal); break;
      case "xychart": result = renderXyChart(lines, pal); break;
      default:
        throw new Error(`Unsupported diagram type: ${kind}`);
    }

    const scaledW = Math.max(16, Math.round(result.width * scale));
    const scaledH = Math.max(16, Math.round(result.height * scale));
    const svg =
      scale === 1
        ? result.svg
        : result.svg.replace(
            /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg" width="\d+" height="\d+"/,
            `<svg xmlns="http://www.w3.org/2000/svg" width="${scaledW}" height="${scaledH}"`
          );
    return {
      ok: true,
      kind,
      label: DIAGRAM_LABELS[kind] || "Mermaid",
      width: scaledW,
      height: scaledH,
      baseWidth: result.width,
      baseHeight: result.height,
      svg,
      dataUrl: svgToDataUrl(svg),
    };
  } catch (e) {
    return {
      ok: false,
      kind,
      label: DIAGRAM_LABELS[kind] || "Mermaid",
      error: String(e?.message || e),
      width: 0,
      height: 0,
      svg: "",
      dataUrl: "",
    };
  }
}
