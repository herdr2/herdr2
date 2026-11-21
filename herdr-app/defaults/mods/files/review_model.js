// Pure text-file review model for the Files mod (self-contained for mod.clone).
// Shares `shared("diff_review.drafts")` scope format with Changes so file and diff comments
// in the same repository can be batched and sent to an Agent together.

export const ROW_HEIGHT = 20;
export const MAX_SNIPPET_CHARS = 1800;

// Text draws a tab as nothing: pad each to the next 4-column stop.
export const untab = (text) =>
  String(text ?? "").replace(/[^\t]*\t/g, (s) => s.slice(0, -1).padEnd((s.length - 1) - (s.length - 1) % 4 + 4));

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
 * Resolves a file's path relative to `repoRoot` when inside a Git repository,
 * matching the paths produced by the Changes mod.
 */
export function resolveRepoRelativePath(root, repoRoot, fileRelPath) {
  const rel = String(fileRelPath ?? "").replace(/\\/g, "/").replace(/^\/+/, "");
  const normRoot = normalizePath(root);
  const normRepo = normalizePath(repoRoot);
  if (!normRepo || !normRoot || normRoot === normRepo) return rel;
  if (normRoot.startsWith(`${normRepo}/`)) {
    const prefix = normRoot.slice(normRepo.length + 1);
    return prefix ? `${prefix}/${rel}` : rel;
  }
  return rel;
}

/**
 * Maps a UTF-16 offset range (`EditorState.selected_range()`) to inclusive 1-based line numbers.
 */
export function selectionToLines(text, range) {
  const src = String(text ?? "");
  const len = src.length;
  const rawStart = Math.max(0, Math.min(len, Number(range?.start) || 0));
  const rawEnd = Math.max(0, Math.min(len, Number(range?.end) || rawStart));
  const lo = Math.min(rawStart, rawEnd);
  let hi = Math.max(rawStart, rawEnd);

  // If the selection ends immediately after a newline (e.g. full-line selection), don't include the next line.
  if (hi > lo && src[hi - 1] === "\n") {
    hi -= 1;
  }

  let startLine = 1;
  for (let i = 0; i < lo; i += 1) {
    if (src[i] === "\n") startLine += 1;
  }
  let endLine = startLine;
  for (let i = lo; i < hi; i += 1) {
    if (src[i] === "\n") endLine += 1;
  }
  return { startLine, endLine: Math.max(startLine, endLine) };
}

/**
 * Maps inclusive 1-based line numbers back to UTF-16 `{ start, end }` offsets for `EditorState.set_selected_range()`.
 */
export function linesToSelectionRange(text, startLine, endLine) {
  const src = String(text ?? "");
  const targetStart = Math.max(1, Number(startLine) || 1);
  const targetEnd = Math.max(targetStart, Number(endLine) || targetStart);

  let line = 1;
  let start = 0;
  let foundStart = targetStart === 1;
  let end = src.length;

  for (let i = 0; i < src.length; i += 1) {
    if (src[i] === "\n") {
      if (line === targetEnd) {
        end = i;
        break;
      }
      line += 1;
      if (!foundStart && line === targetStart) {
        start = i + 1;
        foundStart = true;
      }
    }
  }
  if (!foundStart) start = src.length;
  return { start, end: Math.max(start, end) };
}

/**
 * Extracts lines around `[startLine, endLine]` with 1-based line numbers for Agent grounding.
 */
export function extractFileSnippet(text, startLine, endLine, contextLines = 2, maxChars = MAX_SNIPPET_CHARS) {
  const lines = String(text ?? "").replace(/\r\n/g, "\n").split("\n");
  if (!lines.length) return "";
  const s = Math.max(1, Math.min(lines.length, Number(startLine) || 1));
  const e = Math.max(s, Math.min(lines.length, Number(endLine) || s));
  const lo = Math.max(1, s - contextLines);
  const hi = Math.min(lines.length, e + contextLines);

  const out = [];
  let chars = 0;
  for (let ln = lo; ln <= hi; ln += 1) {
    const marker = ln >= s && ln <= e ? ">" : " ";
    const row = `${marker} ${ln} | ${lines[ln - 1] ?? ""}`;
    if (chars + row.length + 1 > maxChars) {
      out.push("… (truncated)");
      break;
    }
    out.push(row);
    chars += row.length + 1;
  }
  return out.join("\n");
}

function countNewlines(str) {
  let n = 0;
  const s = String(str ?? "");
  for (let i = 0; i < s.length; i += 1) {
    if (s[i] === "\n") n += 1;
  }
  return n;
}

/**
 * Computes exact 1-based `{ startLine, endLine }` for each block in `parseMarkdownBlocks()`.
 */
export function computeMarkdownBlockRanges(doc) {
  const blocks = doc?.blocks ?? [];
  let line = 1 + countNewlines(doc?.leading ?? "");
  const ranges = [];
  for (let i = 0; i < blocks.length; i += 1) {
    const b = blocks[i];
    const startLine = line;
    const rawNewlines = countNewlines(b?.raw ?? "");
    const endLine = startLine + rawNewlines;
    ranges.push({ index: i, startLine, endLine });
    line = endLine + countNewlines(b?.gapAfter ?? "");
  }
  return ranges;
}

/**
 * Maps each comment on a Markdown file to the index of the Markdown block containing its `end_line`.
 */
export function mapCommentsToMarkdownBlocks(blockRanges, fileComments) {
  const byBlock = new Map();
  const unmatched = [];
  for (const c of fileComments ?? []) {
    const anchorLine = Math.max(Number(c?.start_line) || 1, Number(c?.end_line) || 1);
    let idx = -1;
    for (const r of blockRanges) {
      if (anchorLine >= r.startLine && anchorLine <= r.endLine) {
        idx = r.index;
        break;
      }
    }
    if (idx < 0 && blockRanges.length > 0) {
      // Attach to nearest preceding block if anchor falls on a blank gap line.
      for (let i = blockRanges.length - 1; i >= 0; i -= 1) {
        if (blockRanges[i].startLine <= anchorLine) {
          idx = blockRanges[i].index;
          break;
        }
      }
      if (idx < 0) idx = 0;
    }
    if (idx >= 0) {
      if (!byBlock.has(idx)) byBlock.set(idx, []);
      byBlock.get(idx).push(c);
    } else {
      unmatched.push(c);
    }
  }
  return { byBlock, unmatched };
}

/**
 * Builds 1-based line rows for a source/text file so the Files mod can render the exact same
 * click-to-comment / Shift+Click range / inline-comment experience as Checkpoints and Changes.
 */
export function buildFileRows(text, filePath) {
  const normalized = String(text ?? "").replace(/\r\n/g, "\n");
  if (!normalized && !filePath) return [];
  const rawLines = normalized === "" ? [""] : normalized.split("\n");
  return rawLines.map((raw, rowIndex) => {
    const clipped = raw.length > 4000 ? `${raw.slice(0, 4000)}… (line truncated)` : raw;
    const lineNumber = rowIndex + 1;
    return {
      rowIndex,
      file: filePath || "",
      lineNumber,
      text: untab(clipped) || " ",
      commentable: Boolean(filePath),
    };
  });
}

/**
 * Computes a normalized selection range between two 0-based file row indices.
 */
export function buildFileSelectionRange(fileRows, anchorIdx, focusIdx) {
  const focus = fileRows[focusIdx];
  if (!focus?.commentable) return null;
  const anchor = fileRows[anchorIdx];
  const validAnchor = Boolean(anchor?.commentable && anchor.file === focus.file);
  const lo = validAnchor ? Math.min(anchorIdx, focusIdx) : focusIdx;
  const hi = validAnchor ? Math.max(anchorIdx, focusIdx) : focusIdx;
  const startRow = fileRows[lo];
  const endRow = fileRows[hi];
  if (!startRow || !endRow) return null;
  return {
    path: focus.file,
    startRowIdx: lo,
    endRowIdx: hi,
    startLine: startRow.lineNumber,
    endLine: endRow.lineNumber,
  };
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

export function matchFileCommentRowIndex(fileRows, comment) {
  if (!Array.isArray(fileRows) || fileRows.length === 0) return -1;
  const targetLine = Math.max(Number(comment?.start_line) || 1, Number(comment?.end_line) || 1);
  const idx = targetLine - 1;
  if (idx >= 0 && idx < fileRows.length) return idx;
  return -1;
}

/**
 * Flattens file lines and saved inline comment cards into display-only virtual-list items
 * matching the Checkpoints & Changes review layout.
 */
export function buildFileDisplayItems(fileRows, visibleComments, activeComposer) {
  if (!visibleComments || visibleComments.length === 0) {
    return {
      items: fileRows.map((row) => ({ type: "code", key: `r-${row.rowIndex}`, row })),
      sizes: ROW_HEIGHT,
    };
  }

  const byRow = new Map();
  const orphaned = [];
  for (const c of visibleComments) {
    const anchor = matchFileCommentRowIndex(fileRows, c);
    if (anchor >= 0) {
      if (!byRow.has(anchor)) byRow.set(anchor, []);
      byRow.get(anchor).push(c);
    } else {
      orphaned.push(c);
    }
  }

  const items = [];
  const sizes = [];

  for (const row of fileRows) {
    items.push({ type: "code", key: `r-${row.rowIndex}`, row });
    sizes.push(ROW_HEIGHT);

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
