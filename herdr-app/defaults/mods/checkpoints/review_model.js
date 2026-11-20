// Pure diff-review model shared inside the Checkpoints mod (self-contained for mod.clone).

export const ROW_HEIGHT = 20;
export const FILE_HEADER_HEIGHT = 24;
export const COMPOSER_HEIGHT = 124;
export const MAX_SNIPPET_CHARS = 1800;

export function withAlpha(hex, alphaHex) {
  if (typeof hex !== "string") return hex;
  if (/^#[0-9a-fA-F]{6}$/.test(hex)) return `${hex}${alphaHex}`;
  if (/^#[0-9a-fA-F]{8}$/.test(hex)) return `${hex.slice(0, 7)}${alphaHex}`;
  return hex;
}

export function normalizePath(p) {
  return typeof p === "string" ? p.replace(/\\/g, "/").replace(/\/+$/, "") : "";
}

export function formatLineTag(comment) {
  const start = Number(comment?.start_line) || 1;
  const end = Math.max(start, Number(comment?.end_line) || start);
  const range = end > start ? `L${start}–L${end}` : `L${start}`;
  if (comment?.side === "old") return `${range} (old)`;
  return range;
}

/**
 * Enriches existing `diffRows(lines)` output without altering `diffRows` itself.
 * Tracks file path (for multi-file diffs), enclosing hunk header, and old/new line numbers.
 */
export function annotateDiffRows(rows, defaultFile = null) {
  let file = defaultFile || null;
  let hunkHeader = "";
  let oldLine = 0;
  let newLine = 0;

  return rows.map((row, rowIndex) => {
    const kind = row?.kind ?? "";
    const text = row?.text ?? "";

    if (kind === "" && text.startsWith("File: ")) {
      file = text.slice(6).trim() || file;
      hunkHeader = "";
      return {
        ...row,
        rowIndex,
        file,
        hunkHeader,
        oldLine: null,
        newLine: null,
        side: null,
        lineNumber: null,
        isFileHeader: true,
        commentable: false,
      };
    }

    if (kind === "@") {
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(text);
      if (m) {
        oldLine = Number(m[1]) || 0;
        newLine = Number(m[2]) || 0;
      }
      hunkHeader = text;
      return {
        ...row,
        rowIndex,
        file,
        hunkHeader,
        oldLine: null,
        newLine: null,
        side: null,
        lineNumber: null,
        isFileHeader: false,
        commentable: false,
      };
    }

    if (kind === "-") {
      const n = oldLine++;
      return {
        ...row,
        rowIndex,
        file,
        hunkHeader,
        oldLine: n,
        newLine: null,
        side: "old",
        lineNumber: n,
        isFileHeader: false,
        commentable: Boolean(file && n > 0),
      };
    }

    if (kind === "+") {
      const n = newLine++;
      return {
        ...row,
        rowIndex,
        file,
        hunkHeader,
        oldLine: null,
        newLine: n,
        side: "new",
        lineNumber: n,
        isFileHeader: false,
        commentable: Boolean(file && n > 0),
      };
    }

    if (kind === " ") {
      const o = oldLine++;
      const n = newLine++;
      return {
        ...row,
        rowIndex,
        file,
        hunkHeader,
        oldLine: o,
        newLine: n,
        side: "context",
        lineNumber: n,
        isFileHeader: false,
        commentable: Boolean(file && n > 0),
      };
    }

    return {
      ...row,
      rowIndex,
      file,
      hunkHeader,
      oldLine: null,
      newLine: null,
      side: null,
      lineNumber: null,
      isFileHeader: false,
      commentable: false,
    };
  });
}

/**
 * Computes a normalized selection range within one file between two row indices.
 */
export function buildSelectionRange(annotatedRows, anchorIdx, focusIdx) {
  const focus = annotatedRows[focusIdx];
  if (!focus?.commentable) return null;
  const anchor = annotatedRows[anchorIdx];
  const sameFile = anchor?.commentable && anchor.file === focus.file;
  const lo = sameFile ? Math.min(anchorIdx, focusIdx) : focusIdx;
  const hi = sameFile ? Math.max(anchorIdx, focusIdx) : focusIdx;

  const picked = [];
  for (let i = lo; i <= hi; i += 1) {
    const r = annotatedRows[i];
    if (r?.commentable && r.file === focus.file) picked.push(r);
  }
  if (!picked.length) return null;

  const allOld = picked.every((r) => r.side === "old");
  const allContext = picked.every((r) => r.side === "context");
  const side = allOld ? "old" : allContext ? "context" : "new";

  const nums = picked
    .map((r) => (side === "old" ? (r.oldLine ?? r.lineNumber) : (r.newLine ?? r.oldLine ?? r.lineNumber)))
    .filter((n) => typeof n === "number" && n > 0);
  const startLine = nums.length ? Math.min(...nums) : 1;
  const endLine = nums.length ? Math.max(...nums) : startLine;

  return {
    file: focus.file,
    startRowIdx: picked[0].rowIndex,
    endRowIdx: picked[picked.length - 1].rowIndex,
    side,
    startLine,
    endLine,
    hunkHeader: picked[0].hunkHeader || focus.hunkHeader || "",
  };
}

/**
 * Extracts surrounding unified-diff lines within the same file and hunk for grounding.
 */
export function extractSnippet(annotatedRows, startRowIdx, endRowIdx, contextLines = 2, maxChars = MAX_SNIPPET_CHARS) {
  const target = annotatedRows[startRowIdx];
  if (!target?.file) return "";
  const file = target.file;

  let lo = Math.max(0, startRowIdx);
  let steps = 0;
  while (lo > 0 && steps < contextLines) {
    const prev = annotatedRows[lo - 1];
    if (!prev || prev.file !== file || prev.kind === "@" || prev.isFileHeader) break;
    lo -= 1;
    steps += 1;
  }

  let hi = Math.min(annotatedRows.length - 1, Math.max(startRowIdx, endRowIdx));
  steps = 0;
  while (hi + 1 < annotatedRows.length && steps < contextLines) {
    const next = annotatedRows[hi + 1];
    if (!next || next.file !== file || next.kind === "@" || next.isFileHeader) break;
    hi += 1;
    steps += 1;
  }

  const out = [];
  let chars = 0;
  for (let i = lo; i <= hi; i += 1) {
    const r = annotatedRows[i];
    if (!r || r.file !== file || r.isFileHeader) continue;
    const prefix = r.kind === "+" || r.kind === "-" || r.kind === " " ? r.kind : "";
    const line = `${prefix}${r.text}`;
    if (chars + line.length + 1 > maxChars) {
      out.push("… (truncated)");
      break;
    }
    out.push(line);
    chars += line.length + 1;
  }
  return out.join("\n");
}

export function estimateCommentHeight(body) {
  const paragraphs = String(body ?? "").split("\n");
  let lines = 0;
  for (const p of paragraphs) {
    lines += Math.max(1, Math.ceil(Math.max(1, p.length) / 42));
  }
  const clamped = Math.max(1, Math.min(8, lines));
  return 54 + clamped * 16;
}

export function matchCommentRowIndex(annotatedRows, comment) {
  const file = comment?.path;
  if (!file) return -1;
  const targetLine = Math.max(Number(comment?.start_line) || 1, Number(comment?.end_line) || 1);
  const side = comment?.side || "new";

  let exactFallback = -1;
  let nearestIdx = -1;
  let nearestDist = Infinity;

  for (let i = 0; i < annotatedRows.length; i += 1) {
    const r = annotatedRows[i];
    if (!r?.commentable || r.file !== file) continue;
    const lineForSide = side === "old" ? r.oldLine : (r.newLine ?? r.oldLine);
    if (lineForSide === targetLine && r.side === side) return i;
    if (lineForSide === targetLine && exactFallback < 0) exactFallback = i;
    if (typeof lineForSide === "number") {
      const dist = Math.abs(lineForSide - targetLine);
      if (dist < nearestDist) {
        nearestDist = dist;
        nearestIdx = i;
      }
    }
  }
  return exactFallback >= 0 ? exactFallback : nearestIdx;
}

/**
 * Flattens code rows and saved inline comment previews into pure display-only virtual-list items.
 * (Interactive composer stays docked outside `v_virtual_list` because gpui-kit forbids handlers inside virtual list renderers.)
 * Returns scalar `ROW_HEIGHT` when no comments or file headers exist for fast rendering.
 */
export function buildDisplayItems(annotatedRows, visibleComments, activeComposer) {
  const hasFileHeader = annotatedRows.some((r) => r.isFileHeader);
  if ((!visibleComments || visibleComments.length === 0) && !hasFileHeader) {
    return {
      items: annotatedRows.map((row) => ({ type: "code", key: `r-${row.rowIndex}`, row })),
      sizes: ROW_HEIGHT,
    };
  }

  const byRow = new Map();
  const orphaned = [];
  for (const c of visibleComments ?? []) {
    const anchor = matchCommentRowIndex(annotatedRows, c);
    if (anchor >= 0) {
      if (!byRow.has(anchor)) byRow.set(anchor, []);
      byRow.get(anchor).push(c);
    } else {
      orphaned.push(c);
    }
  }

  const items = [];
  const sizes = [];

  for (const row of annotatedRows) {
    items.push({ type: "code", key: `r-${row.rowIndex}`, row });
    sizes.push(row.isFileHeader ? FILE_HEADER_HEIGHT : ROW_HEIGHT);

    const attached = byRow.get(row.rowIndex);
    if (attached) {
      for (const comment of attached) {
        const editing = Boolean(activeComposer?.editingId && activeComposer.editingId === comment.id);
        items.push({ type: "comment", key: `c-${comment.id}`, comment, outdated: false, editing });
        sizes.push(estimateCommentHeight(comment.body));
      }
    }
  }

  for (const comment of orphaned) {
    const editing = Boolean(activeComposer?.editingId && activeComposer.editingId === comment.id);
    items.push({ type: "comment", key: `c-orphan-${comment.id}`, comment, outdated: true, editing });
    sizes.push(estimateCommentHeight(comment.body));
  }

  return { items, sizes };
}

function machineOfAddress(addr) {
  if (typeof addr !== "string") return null;
  const slash = addr.indexOf("/");
  return slash > 0 ? addr.slice(0, slash) : null;
}

function isAcpAddress(addr) {
  if (typeof addr !== "string") return false;
  const bare = addr.includes("/") ? addr.slice(addr.lastIndexOf("/") + 1) : addr;
  return bare.startsWith("a");
}

function sessionTitle(termRec, agentRec, paneRec) {
  const custom = termRec?.labels?.["herdr.name"];
  const prog = termRec?.program?.name || agentRec?.program?.name || paneRec?.program?.name;
  const name = custom || termRec?.name || prog || termRec?.template || termRec?.title || termRec?.address || "";
  const addr = termRec?.address || agentRec?.address || paneRec?.terminal || "";
  return name && name !== addr ? `${name} (${addr})` : addr;
}

/**
 * Resolves live ACP/PTY target candidates on the same machine as the diff,
 * ranking preferred target, focused session, same-repo agents, and fallback sessions.
 */
export function resolveTargetCandidates({
  panesList = [],
  agentsList = [],
  terminalsList = [],
  machine = null,
  focusedInfo = null,
  repoRoot = null,
  preferredAddr = null,
}) {
  const normMachine = machine || null;
  const normRoot = normalizePath(repoRoot);
  const paneByAddr = new Map();
  for (const p of panesList ?? []) {
    if (p?.terminal) paneByAddr.set(p.terminal, p);
  }
  const agentByAddr = new Map();
  for (const a of agentsList ?? []) {
    if (a?.address) agentByAddr.set(a.address, a);
  }

  const focusedAddr =
    panesList.find((p) => p?.focused && p?.terminal)?.terminal
    || agentsList.find((a) => a?.focused && a?.address)?.address
    || focusedInfo?.shown
    || focusedInfo?.terminal
    || focusedInfo?.agent
    || null;

  const seen = new Set();
  const candidates = [];

  for (const rec of terminalsList ?? []) {
    const addr = rec?.address;
    if (!addr || seen.has(addr)) continue;
    if (rec.state && rec.state !== "live") continue;
    if (rec.labels?.["herdr.trashed"]) continue;
    if (machineOfAddress(addr) !== normMachine) continue;
    seen.add(addr);

    const paneRec = paneByAddr.get(addr) ?? null;
    const agentRec = agentByAddr.get(addr) ?? null;
    const acp = Boolean(agentRec?.acp || paneRec?.acp || isAcpAddress(addr));
    const isAgent = acp || Boolean(rec?.program?.agent || agentRec?.program?.agent || paneRec?.program?.agent);
    const cwd = normalizePath(paneRec?.cwd || rec?.foreground?.cwd || rec?.last_cwd || rec?.cwd || "");
    const sameRepo = Boolean(normRoot && cwd && (cwd === normRoot || cwd.startsWith(`${normRoot}/`)));
    const status = agentRec?.status || paneRec?.status || rec?.status || "idle";

    let score = 0;
    if (preferredAddr && addr === preferredAddr) score += 1000;
    if (focusedAddr && addr === focusedAddr) score += 300;
    if (sameRepo) score += 200;
    if (isAgent) score += 100;
    if (acp) score += 20;

    candidates.push({
      address: addr,
      label: sessionTitle(rec, agentRec, paneRec),
      kind: acp ? "acp" : "pty",
      isAgent,
      status,
      sameRepo,
      score,
    });
  }

  candidates.sort((a, b) => b.score - a.score || a.address.localeCompare(b.address));
  return {
    candidates,
    defaultTarget: candidates[0]?.address ?? null,
  };
}

export function makeDraftScopeKey(machine, scope) {
  return `${machine || "local"}::${normalizePath(scope || "")}`;
}

export function readScopeDrafts(sharedStore, scopeKey) {
  if (!sharedStore || typeof sharedStore !== "object" || !scopeKey) return [];
  const list = sharedStore[scopeKey];
  return Array.isArray(list) ? list : [];
}

export function writeScopeDrafts(sharedStore, scopeKey, list, maxScopes = 20) {
  const next = sharedStore && typeof sharedStore === "object" && !Array.isArray(sharedStore) ? { ...sharedStore } : {};
  if (!list || list.length === 0) {
    delete next[scopeKey];
  } else {
    next[scopeKey] = list.slice(0, 100);
  }
  const keys = Object.keys(next);
  if (keys.length > maxScopes) {
    for (const k of keys.slice(0, keys.length - maxScopes)) {
      if (k !== scopeKey) delete next[k];
    }
  }
  return next;
}
