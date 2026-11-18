// Pure view-model helpers for rendering headless ACP transcripts in a T3 Code-style conversation UI.

function parseAttrs(raw) {
  const attrs = {};
  const re = /([a-zA-Z0-9_-]+)\s*=\s*"([^"]*)"/g;
  let m;
  while ((m = re.exec(raw)) !== null) {
    attrs[m[1]] = m[2];
  }
  return attrs;
}

export function stripHandoffEnvelope(rawText) {
  let text = String(rawText ?? "").replace(/\r\n/g, "\n");
  let handoffMeta = null;
  const openClosed = text.trimStart().match(/^<herdr2-handoff\b([^>]*)>([\s\S]*?)<\/herdr2-handoff>\s*([\s\S]*)$/i);
  if (openClosed) {
    const attrs = parseAttrs(openClosed[1] ?? "");
    handoffMeta = { kind: attrs.kind || "handoff", from: attrs.from || "" };
    text = openClosed[3] ?? "";
  }
  return { handoffMeta, text: text.trim() };
}

/**
 * Strips herdr2's `<h2 ...>...</h2>` envelope around delegated user prompts and
 * `<herdr2-handoff ...>...</herdr2-handoff>` context envelopes, returning the
 * extracted sender metadata plus clean message text.
 */
export function parseUserEnvelope(rawText, entryFrom = null) {
  let text = String(rawText ?? "").replace(/\r\n/g, "\n");
  let from = entryFrom || null;
  let reach = null;
  let handoff = null;

  for (let i = 0; i < 3; i++) {
    const trimmed = text.trim();
    const closed = trimmed.match(/^<h2\b([^>]*)>\s*([\s\S]*?)\s*<\/h2>$/i);
    if (closed) {
      const attrs = parseAttrs(closed[1] ?? "");
      if (!from && attrs.from) from = attrs.from;
      if (!reach && attrs.reach) reach = attrs.reach;
      text = closed[2] ?? "";
      continue;
    }
    const openOnly = trimmed.match(/^<h2\b([^>]*)>\s*([\s\S]*)$/i);
    if (openOnly) {
      const attrs = parseAttrs(openOnly[1] ?? "");
      if (!from && attrs.from) from = attrs.from;
      if (!reach && attrs.reach) reach = attrs.reach;
      text = openOnly[2] ?? "";
      continue;
    }
    break;
  }

  const ho = stripHandoffEnvelope(text);
  if (ho.handoffMeta) {
    handoff = ho.handoffMeta;
    text = ho.text;
  }

  return {
    from,
    reach,
    handoff,
    body: text.trim(),
  };
}

/**
 * Formats an epoch millisecond timestamp as local `HH:MM`.
 */
export function formatTime(tsMs) {
  if (!Number.isFinite(tsMs) || tsMs <= 0) return "";
  try {
    const d = new Date(tsMs);
    if (Number.isNaN(d.getTime())) return "";
    const hh = String(d.getHours()).padStart(2, "0");
    const mm = String(d.getMinutes()).padStart(2, "0");
    return `${hh}:${mm}`;
  } catch {
    return "";
  }
}

/**
 * Replaces a leading home directory path with `~` for header breadcrumbs.
 */
export function shortenPath(cwd, homeDir) {
  if (!cwd) return "";
  if (homeDir && cwd === homeDir) return "~";
  if (homeDir && (cwd.startsWith(`${homeDir}/`) || cwd.startsWith(`${homeDir}\\`))) {
    return `~${cwd.slice(homeDir.length)}`;
  }
  return cwd;
}

/**
 * Splits markdown into prose segments and fenced code blocks so code blocks can
 * render inside T3 Code-style headered code cards with one-click copy.
 */
export function splitMarkdownBlocks(markdown) {
  const src = String(markdown ?? "").replace(/\r\n/g, "\n");
  if (!src.trim()) return [];

  const lines = src.split("\n");
  const blocks = [];
  let proseLines = [];
  let inCode = false;
  let fenceChar = "`";
  let fenceLen = 3;
  let codeLang = "";
  let codeLines = [];

  const flushProse = () => {
    const joined = proseLines.join("\n").replace(/^\n+|\n+$/g, "");
    if (joined.trim().length > 0) {
      blocks.push({ type: "prose", text: joined });
    }
    proseLines = [];
  };

  for (const line of lines) {
    if (!inCode) {
      const open = line.match(/^(`{3,}|~{3,})(.*)$/);
      if (open) {
        const marker = open[1];
        const info = (open[2] ?? "").trim();
        // Backtick info strings cannot contain other backticks per CommonMark.
        if (marker[0] === "`" && info.includes("`")) {
          proseLines.push(line);
          continue;
        }
        flushProse();
        inCode = true;
        fenceChar = marker[0];
        fenceLen = marker.length;
        codeLang = info.split(/\s+/)[0] || "";
        codeLines = [];
        continue;
      }
      proseLines.push(line);
    } else {
      const closeRe = fenceChar === "`" ? /^(`{3,})\s*$/ : /^(~{3,})\s*$/;
      const close = line.match(closeRe);
      if (close && close[1].length >= fenceLen) {
        blocks.push({
          type: "code",
          lang: codeLang,
          code: codeLines.join("\n"),
          closed: true,
        });
        inCode = false;
        fenceChar = "`";
        fenceLen = 3;
        codeLang = "";
        codeLines = [];
      } else {
        codeLines.push(line);
      }
    }
  }

  if (inCode) {
    blocks.push({
      type: "code",
      lang: codeLang,
      code: codeLines.join("\n"),
      closed: false,
    });
  }
  flushProse();
  return blocks;
}

/**
 * Picks an existing shipped SVG icon matching a tool call kind or title.
 */
export function toolIconPath(toolKind, title = "") {
  const kind = String(toolKind ?? "").toLowerCase();
  const label = String(title ?? "").toLowerCase();
  if (kind === "execute" || kind === "command" || /\b(bash|sh|exec|run|shell|shell_command|exec_command)\b/.test(label)) {
    return "mods/workspace-chrome/terminal.svg";
  }
  if (kind === "read" || /\b(read|read_file|view_file)\b/.test(label)) {
    return "mods/workspace-chrome/file-text.svg";
  }
  if (kind === "edit" || kind === "write" || kind === "patch" || /\b(edit|write|patch|create|apply_patch|multiedit|notebookedit)\b/.test(label)) {
    return "mods/workspace-chrome/square-pen.svg";
  }
  if (kind === "search" || /\b(search|grep|rg|glob|find)\b/.test(label)) {
    return "mods/workspace-chrome/search.svg";
  }
  if (kind === "web" || kind === "fetch" || /\b(webfetch|webreader|fetch|curl|http|url)\b/.test(label)) {
    return "mods/peek/globe.svg";
  }
  if (kind === "think" || kind === "thought") {
    return "mods/peek/brain.svg";
  }
  if (kind === "plan" || kind === "todo") {
    return "mods/peek/list-todo.svg";
  }
  return "mods/peek/wrench.svg";
}

/**
 * Extracts a single-line inline preview from markdown reasoning text (matching T3 Code `liveThoughtLine`).
 */
export function thoughtPreview(rawText, maxChars = 110) {
  const src = String(rawText ?? "").replace(/\r\n/g, "\n").trim();
  if (!src) return "";
  const firstNonEmpty = src
    .split("\n")
    .map((l) => l.trim())
    .find(Boolean) ?? "";
  const clean = firstNonEmpty
    .replace(/^#{1,6}\s+/, "")
    .replace(/^[-*+]\s+/, "")
    .replace(/^\d+\.\s+/, "")
    .replace(/^>\s+/, "")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/[ \t]+/g, " ")
    .trim();
  if (clean.length <= maxChars) return clean;
  return `${clean.slice(0, Math.max(1, maxChars - 1))}…`;
}

/**
 * Parses `[x]` / `[~]` / `[ ]` plan entries into structured checklist items.
 */
export function parsePlanItems(items) {
  const list = Array.isArray(items) ? items : [];
  return list
    .map((raw) => {
      const src = String(raw ?? "").trim();
      if (!src) return null;
      if (/^\[x\]/i.test(src)) {
        const text = src.replace(/^\[x\]\s*/i, "").trim() || src;
        return { status: "completed", text };
      }
      if (/^\[[~*]\]/.test(src)) {
        const text = src.replace(/^\[[~*]\]\s*/, "").trim() || src;
        return { status: "in_progress", text };
      }
      if (/^\[\s*\]/.test(src)) {
        const text = src.replace(/^\[\s*\]\s*/, "").trim() || src;
        return { status: "pending", text };
      }
      return { status: "pending", text: src };
    })
    .filter(Boolean);
}

/**
 * Detects interactive shell prompt+command blocks (`$ cmd`, `user@host:~/dir$ cmd`, `~/dir % cmd`, `PS ...> cmd`)
 * inside cleaned terminal snapshots so shell output renders in T3 Code `CommandBlock` style.
 */
export function parseTerminalShellBlocks(cleanedText) {
  const src = String(cleanedText ?? "").replace(/\r\n/g, "\n");
  if (!src.trim()) return [];
  const promptMatchers = [
    /^([a-zA-Z0-9._-]+@[a-zA-Z0-9._-]+[^\n]{0,60}[$#%]\s+)(.+)$/,
    /^((?:\([^)]+\)\s+)?(?:~|\/|\.\.?)(?:\/[^\s$#%]*)*\s+[$%❯›]\s+)(.+)$/,
    /^(\$\s+)(\S[^\n]*)$/,
    /^(PS\s+[^\n>]*>\s+)(.+)$/,
  ];
  const matchPrompt = (line) => {
    for (const re of promptMatchers) {
      const m = line.match(re);
      if (m && m[2].trim()) {
        return { prompt: m[1].trim(), command: m[2].trim() };
      }
    }
    return null;
  };

  const blocks = [];
  let leadingLines = [];
  let current = null;

  const flushCurrent = () => {
    if (!current) return;
    const output = current.outputLines.join("\n").replace(/^\n+|\n+$/g, "");
    blocks.push({
      type: "command",
      prompt: current.prompt,
      command: current.command,
      output,
    });
    current = null;
  };

  for (const line of src.split("\n")) {
    const hit = matchPrompt(line);
    if (hit) {
      if (!current && leadingLines.length > 0) {
        const lead = leadingLines.join("\n").replace(/^\n+|\n+$/g, "");
        if (lead.trim()) blocks.push({ type: "output", text: lead });
        leadingLines = [];
      }
      flushCurrent();
      current = { prompt: hit.prompt, command: hit.command, outputLines: [] };
    } else if (current) {
      current.outputLines.push(line);
    } else {
      leadingLines.push(line);
    }
  }
  flushCurrent();

  if (!blocks.some((b) => b.type === "command")) return [];
  return blocks;
}

/**
 * Builds read-only Composer footer badges for structured PTY terminals from
 * `ReadResult.session_meta` (or record fallback).
 */
export function buildPtySessionBadges(sessionMeta, record) {
  const badges = [];
  const meta = sessionMeta ?? {};
  const model = meta.model || record?.model || null;
  if (model) {
    badges.push({
      id: "model",
      iconKind: "model",
      label: String(model),
      tooltip: `Model: ${model}`,
    });
  }
  if (meta.reasoning_effort) {
    const effort = String(meta.reasoning_effort);
    const nice = effort.charAt(0).toUpperCase() + effort.slice(1);
    badges.push({
      id: "reasoning",
      iconKind: "thinking",
      label: nice,
      effortVisual: resolveEffortVisual(effort),
      tooltip: `Reasoning: ${nice}`,
    });
  }
  if (meta.mode) {
    const mode = String(meta.mode);
    badges.push({
      id: "mode",
      iconKind: "mode",
      label: mode,
      tooltip: `Mode: ${mode}`,
    });
  }
  return badges;
}

/**
 * Extracts a clean one-line summary and extra-line count from potentially
 * multi-line ACP tool call titles (e.g. Codex heredoc shell scripts).
 */
export function summarizeToolTitle(rawTitle, maxChars = 140) {
  const full = String(rawTitle ?? "").replace(/\r\n/g, "\n").trim();
  const rawLines = full ? full.split("\n") : [];
  const nonEmpty = rawLines.map((l) => l.trim()).filter(Boolean);
  let summary = (nonEmpty[0] ?? "").replace(/[ \t]+/g, " ");
  if (summary.length > maxChars) {
    summary = `${summary.slice(0, Math.max(1, maxChars - 1))}…`;
  }
  const extraLines = Math.max(0, nonEmpty.length - 1);
  return {
    summary: summary || "Tool call",
    extraLines,
    full,
    multiline: extraLines > 0 || rawLines.length > 1,
  };
}

/**
 * Separates glued adjacent bold headings emitted when ACP thought chunks lack trailing newlines.
 */
export function normalizeThoughtText(rawText) {
  let out = String(rawText ?? "").replace(/\r\n/g, "\n");
  for (let i = 0; i < 8; i++) {
    const next = out.replace(/(\*\*[^*\n]+?\*\*)[ \t]*(\*\*[^*\n]+?\*\*)/g, "$1\n\n$2");
    if (next === out) break;
    out = next;
  }
  return out.trim();
}

/**
 * Builds a concise one-line T3 Code summary for a collapsed/expanded activity group.
 */
export function summarizeActivityGroup(group) {
  if (!group) return "Activity";
  if (group.runningCount > 0 && group.activeTitle) {
    const firstLine = summarizeToolTitle(group.activeTitle).summary;
    return `Running: ${firstLine}`;
  }
  const steps = group.steps ?? [];
  if (steps.length === 1) {
    const only = steps[0];
    if (only.kind === "thought") return "Reasoning";
    if (only.kind === "tool_call") {
      const info = only.summaryTitle
        ? { summary: only.summaryTitle, extraLines: only.extraLines ?? 0 }
        : summarizeToolTitle(only.title);
      return info.extraLines > 0 ? `${info.summary} (+${info.extraLines} lines)` : (info.summary || "Tool call");
    }
    if (only.kind === "plan") return `Plan (${only.items?.length ?? 0} steps)`;
    if (only.kind === "permission") return only.title || "Approval";
  }
  const parts = [];
  if (group.toolCount > 0) parts.push(`${group.toolCount} tool call${group.toolCount === 1 ? "" : "s"}`);
  if (group.thoughtCount > 0) parts.push(`${group.thoughtCount} thought${group.thoughtCount === 1 ? "" : "s"}`);
  if (group.planCount > 0) parts.push(`${group.planCount} plan${group.planCount === 1 ? "" : "s"}`);
  if (group.permissionCount > 0) parts.push(`${group.permissionCount} approval${group.permissionCount === 1 ? "" : "s"}`);
  if (group.failedCount > 0) parts.push(`${group.failedCount} failed`);
  return parts.length > 0 ? parts.join(" · ") : "Activity";
}

function isRunningStatus(status) {
  return status === "pending" || status === "in_progress";
}

/**
 * Enhances terminal snapshots by normalizing non-breaking spaces, trimming trailing
 * whitespace, removing TUI frame divider/border lines, unwrapping Unicode box rows,
 * and collapsing large runs of empty lines left by fixed-row PTY screens.
 */
export function cleanTerminalSnapshot(rawText) {
  const src = String(rawText ?? "").replace(/\r\n/g, "\n").replace(/\u00a0/g, " ");
  if (!src.trim()) return "";
  const rawLines = src.split("\n");
  const isBoxRule = (t) =>
    /^[╭╮╰╯┌┐└┘├┤┬┴┼─━═┄┈\-_\s]+$/.test(t) &&
    (/[╭╮╰╯┌┐└┘├┤┬┴┼─━═┄┈]{8,}/.test(t) || /^[-_]{24,}$/.test(t));
  const isNeighborBox = (idx) => {
    const prev = rawLines[idx - 1]?.trim() ?? "";
    const next = rawLines[idx + 1]?.trim() ?? "";
    const prevBoxed = Boolean(prev) && !/^[╰└]/.test(prev) && (isBoxRule(prev) || /^[│┃║].*[│┃║]$/.test(prev));
    const nextBoxed = Boolean(next) && !/^[╭┌]/.test(next) && (isBoxRule(next) || /^[│┃║].*[│┃║]$/.test(next));
    return prevBoxed || nextBoxed;
  };
  const cleaned = [];
  let blankRun = 0;
  for (let i = 0; i < rawLines.length; i++) {
    let line = rawLines[i].replace(/[ \t]+$/g, "");
    let trimmed = line.trim();
    if (isBoxRule(trimmed) || /^[│┃║]\s*[│┃║]$/.test(trimmed)) {
      continue;
    }
    const boxed = trimmed.match(/^[│┃]\s*(.+?)\s*[│┃]$/);
    if (boxed && isNeighborBox(i)) {
      line = boxed[1];
      trimmed = line.trim();
      if (!trimmed) continue;
    }
    if (!trimmed) {
      blankRun += 1;
      if (blankRun <= 1 && cleaned.length > 0) {
        cleaned.push("");
      }
      continue;
    }
    blankRun = 0;
    cleaned.push(line);
  }
  while (cleaned.length > 0 && cleaned[cleaned.length - 1] === "") {
    cleaned.pop();
  }
  return cleaned.join("\n");
}

/**
 * Extracts a single-line fold summary from a user prompt (used for delegated subagent prompts).
 */
export function summarizeUserPrompt(rawText, maxChars = 96) {
  const src = String(rawText ?? "").replace(/\r\n/g, "\n").trim();
  if (!src) return "";
  const firstNonEmpty = src
    .split("\n")
    .map((l) => l.trim())
    .find(Boolean) ?? "";
  const clean = firstNonEmpty
    .replace(/^#{1,6}\s+/, "")
    .replace(/^[-*+]\s+/, "")
    .replace(/^\d+\.\s+/, "")
    .replace(/^>\s+/, "")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/[ \t]+/g, " ")
    .trim();
  if (clean.length <= maxChars) return clean;
  return `${clean.slice(0, Math.max(1, maxChars - 1))}…`;
}

/**
 * Parses tool call detail into T3 Code display modes: unified/patch `diff`,
 * shell `command` (+ output), or `plain` monospace output.
 */
export function parseToolDetail(step) {
  const raw = String(step?.detail ?? "").replace(/\r\n/g, "\n");
  const title = String(step?.title ?? "").trim();
  const kind = String(step?.tool_kind ?? "").toLowerCase();
  const isCommand =
    kind === "execute" ||
    kind === "command" ||
    /\b(bash|sh|exec|run|shell|shell_command|exec_command)\b/i.test(title);

  const lines = raw ? raw.split("\n") : [];
  const hasDiffHeaders =
    raw.startsWith("--- ") ||
    raw.startsWith("diff --git") ||
    raw.startsWith("*** Begin Patch") ||
    raw.startsWith("*** Update File:") ||
    raw.startsWith("*** Add File:");
  const hasHunkOrSigns =
    /\n@@[^\n]*@@/.test(raw) ||
    ((/^(?:---|\+\+\+|\*\*\*)/m.test(raw)) && (/^[+-]/m.test(raw)));
  const isDiff = Boolean(raw) && !isCommand && (hasDiffHeaders || hasHunkOrSigns);

  if (isDiff) {
    let additions = 0;
    let deletions = 0;
    const diffLines = [];
    const limit = 120;
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      let lineKind = "ctx";
      if (
        line.startsWith("diff --git") ||
        line.startsWith("index ") ||
        line.startsWith("--- ") ||
        line.startsWith("+++ ") ||
        line.startsWith("*** ")
      ) {
        lineKind = "meta";
      } else if (line.startsWith("@@")) {
        lineKind = "hunk";
      } else if (line.startsWith("+")) {
        lineKind = "add";
        additions += 1;
      } else if (line.startsWith("-")) {
        lineKind = "del";
        deletions += 1;
      }
      if (i < limit) {
        diffLines.push({ kind: lineKind, text: line });
      }
    }
    return {
      mode: "diff",
      diffLines,
      additions,
      deletions,
      truncatedLines: Math.max(0, lines.length - limit),
      command: "",
      output: "",
      raw,
    };
  }

  if (isCommand) {
    let command = title;
    let output = raw.trim();
    if (title && raw) {
      if (raw === title) {
        output = "";
      } else if (raw.startsWith(`${title}\n\n`)) {
        output = raw.slice(title.length + 2).trim();
      } else if (raw.startsWith(`${title}\n`)) {
        output = raw.slice(title.length + 1).trim();
      }
    }
    return {
      mode: "command",
      diffLines: [],
      additions: 0,
      deletions: 0,
      truncatedLines: 0,
      command,
      output,
      raw,
    };
  }

  return {
    mode: "plain",
    diffLines: [],
    additions: 0,
    deletions: 0,
    truncatedLines: 0,
    command: "",
    output: raw.trim(),
    raw,
  };
}

function normalizeRelativeFilePath(rawPath, cwd = "") {
  let p = String(rawPath ?? "").trim().replace(/^['"]|['"]$/g, "");
  if (!p || p === "/dev/null") return "";
  if (p.startsWith("a/") || p.startsWith("b/")) p = p.slice(2);
  if (cwd) {
    const cleanCwd = String(cwd).replace(/[\\/]+$/, "");
    if (cleanCwd && (p.startsWith(`${cleanCwd}/`) || p.startsWith(`${cleanCwd}\\`))) {
      p = p.slice(cleanCwd.length + 1);
    }
  }
  if (p.startsWith("./")) p = p.slice(2);
  return p;
}

/**
 * Aggregates edited/written file paths and +/- line stats across a turn's tool steps
 * for T3 Code's `ChangedFilesTree` summary card.
 */
export function extractTurnChangedFiles(steps, cwd = "") {
  const list = Array.isArray(steps) ? steps : [];
  const byPath = new Map();

  const recordFile = (rawPath, additions, deletions) => {
    const clean = normalizeRelativeFilePath(rawPath, cwd);
    if (!clean) return;
    const prev = byPath.get(clean);
    if (prev) {
      prev.additions += additions;
      prev.deletions += deletions;
    } else {
      byPath.set(clean, { path: clean, additions, deletions });
    }
  };

  for (const step of list) {
    if (!step || step.kind !== "tool_call" || step.status === "failed") continue;
    const toolKind = String(step.tool_kind ?? "").toLowerCase();
    const title = String(step.title ?? "").trim();
    const raw = String(step.detail ?? "");
    const titleEdit = title.match(/^(?:Edit|MultiEdit|Write|Create|Patch|edit_file|write_file|replace_file_content|apply_patch)\s+(.+)$/i);
    const hasDiffDetail =
      raw.startsWith("--- ") ||
      raw.startsWith("diff --git") ||
      /\*\*\*\s+(?:Update|Add|Delete)\s+File:/.test(raw);
    const isEditTool = toolKind === "edit" || toolKind === "write" || toolKind === "patch" || Boolean(titleEdit) || hasDiffDetail;
    if (!isEditTool) continue;

    const parsed = parseToolDetail(step);
    const additions = parsed.mode === "diff" ? parsed.additions : 0;
    const deletions = parsed.mode === "diff" ? parsed.deletions : 0;

    const patchFiles = [...raw.matchAll(/^\*\*\*\s+(?:Update|Add|Delete)\s+File:\s*(.+)$/gm)].map((m) => m[1]);
    if (patchFiles.length > 0) {
      if (patchFiles.length === 1) {
        recordFile(patchFiles[0], additions, deletions);
      } else {
        patchFiles.forEach((pf) => recordFile(pf, 0, 0));
      }
      continue;
    }

    const plusMatch = raw.match(/^\+\+\+\s+([^\n]+)/m);
    const minusMatch = raw.match(/^---\s+([^\n]+)/m);
    const diffPath = (plusMatch && plusMatch[1].trim() !== "/dev/null" ? plusMatch[1] : minusMatch?.[1]) ?? null;
    if (diffPath) {
      recordFile(diffPath, additions, deletions);
      continue;
    }

    if (titleEdit?.[1]) {
      recordFile(titleEdit[1], additions, deletions);
    }
  }

  const files = [...byPath.values()];
  const totalAdditions = files.reduce((acc, f) => acc + f.additions, 0);
  const totalDeletions = files.reduce((acc, f) => acc + f.deletions, 0);
  return { files, totalAdditions, totalDeletions };
}

/**
 * Resolves T3 Code's 4-bar rising EffortGlyph levels from reasoning effort settings.
 */
export function resolveEffortVisual(rawValue) {
  const v = String(rawValue ?? "").trim().toLowerCase();
  if (!v) return null;
  if (v === "off" || v === "none") return { bars: 4, active: 0 };
  if (v === "minimal" || v === "low" || v === "min") return { bars: 4, active: 1 };
  if (v === "medium" || v === "med" || v === "normal" || v === "balanced" || v === "default") {
    return { bars: 4, active: 2 };
  }
  if (v === "high") return { bars: 4, active: 3 };
  if (v === "xhigh" || v === "extra_high" || v === "extra-high" || v === "max" || v === "ultrathink") {
    return { bars: 4, active: 4 };
  }
  return null;
}

/**
 * Groups structured `ReadResult.entries` into T3 Code conversation items:
 * user cards, collapsible reasoning/tool work logs, markdown assistant turns,
 * terminal snapshots, errors, and collapsible stderr drawers.
 */
export function buildConversationItems(entries, fallbackText = "", agentWorking = false, options = {}) {
  const list = Array.isArray(entries) ? entries : [];
  if (list.length === 0) {
    const { isPty = false, how = null, turn = 0, isAgent = false } = options ?? {};
    if (isPty || how === "screen") {
      const cleaned = cleanTerminalSnapshot(fallbackText);
      if (!cleaned || (isAgent && (turn ?? 0) <= 0 && !agentWorking)) {
        return [];
      }
      return [
        {
          kind: "terminal_snapshot",
          id: "term-snap-0",
          turn: turn ?? 0,
          text: cleaned,
          shellBlocks: parseTerminalShellBlocks(cleaned),
          lineCount: cleaned.split("\n").length,
          ts_ms: 0,
        },
      ];
    }
    const clean = String(fallbackText ?? "").trim();
    if (!clean) return [];
    return [
      {
        kind: "assistant",
        id: "fallback-0",
        turn: 0,
        text: clean,
        blocks: splitMarkdownBlocks(clean),
        showSpeakerHeader: true,
        isFinalTurnReply: true,
        ts_ms: 0,
      },
    ];
  }

  // Filter historical session/load replay artifacts recorded before the real
  // user prompt (`from != null`) on resumed turns (`turn > 1`).
  const firstAuthoritativeByTurn = new Map();
  list.forEach((entry, idx) => {
    const turn = entry?.turn ?? 0;
    if (
      turn > 1 &&
      entry?.kind === "user" &&
      entry?.from != null &&
      !firstAuthoritativeByTurn.has(turn)
    ) {
      firstAuthoritativeByTurn.set(turn, idx);
    }
  });

  const items = [];
  let activeGroup = null;
  let activeStderr = null;
  let currentSpeakerKey = null;
  const checkpointStatsByTurn = new Map();

  const normalizeCheckpointFiles = (rawFiles) => {
    const files = (Array.isArray(rawFiles) ? rawFiles : []).map((f) => ({
      path: String(f?.path ?? ""),
      additions: Number(f?.added ?? f?.additions ?? 0) || 0,
      deletions: Number(f?.deleted ?? f?.deletions ?? 0) || 0,
      ...(f?.binary ? { binary: true } : {}),
    })).filter((f) => f.path);
    const totalAdditions = files.reduce((s, f) => s + f.additions, 0);
    const totalDeletions = files.reduce((s, f) => s + f.deletions, 0);
    return { files, totalAdditions, totalDeletions };
  };

  if (options?.checkpointsByTurn) {
    const src = options.checkpointsByTurn instanceof Map
      ? Array.from(options.checkpointsByTurn.entries())
      : Array.isArray(options.checkpointsByTurn)
        ? options.checkpointsByTurn.map((c) => [c?.turn, c])
        : Object.entries(options.checkpointsByTurn);
    for (const [k, meta] of src) {
      const turn = Number(meta?.turn ?? k);
      if (!Number.isFinite(turn) || turn <= 0) continue;
      const norm = normalizeCheckpointFiles(meta?.files);
      if (norm.files.length > 0) {
        checkpointStatsByTurn.set(turn, {
          ...norm,
          checkpointTurn: turn,
          fromCheckpoint: true,
        });
      }
    }
  }

  const flushGroup = () => {
    if (!activeGroup) return;
    items.push(activeGroup);
    activeGroup = null;
  };

  const flushStderr = () => {
    if (!activeStderr) return;
    activeStderr.text = activeStderr.lines.join("\n");
    activeStderr.lineCount = activeStderr.text ? activeStderr.text.split("\n").length : 0;
    items.push(activeStderr);
    activeStderr = null;
  };

  const speakerNeeded = (turn) => {
    const key = `turn-${turn ?? 0}`;
    if (currentSpeakerKey !== key) {
      currentSpeakerKey = key;
      return true;
    }
    return false;
  };

  list.forEach((entry, idx) => {
    if (!entry || typeof entry !== "object") return;
    const cutoff = firstAuthoritativeByTurn.get(entry.turn ?? 0);
    if (cutoff !== undefined && idx < cutoff) return;
    const kind = entry.kind;

    if (kind === "user") {
      flushGroup();
      flushStderr();
      currentSpeakerKey = null;
      const env = parseUserEnvelope(entry.text, entry.from ?? null);
      const fallbackBody = env.handoff
        ? `[${env.handoff.kind || "handoff"}${env.handoff.from ? ` from ${env.handoff.from}` : ""}]`
        : "";
      const body = env.body || fallbackBody || String(entry.text ?? "").trim();
      items.push({
        kind: "user",
        id: `u-${idx}`,
        turn: entry.turn ?? 0,
        from: env.from,
        reach: env.reach,
        handoff: env.handoff,
        body,
        isDelegated: Boolean(env.from),
        foldSummary: summarizeUserPrompt(body),
        ts_ms: entry.ts_ms ?? 0,
      });
      return;
    }

    if (kind === "handoff") {
      flushGroup();
      flushStderr();
      currentSpeakerKey = null;
      items.push({
        kind: "handoff",
        id: `ho-${idx}`,
        turn: entry.turn ?? 0,
        handoff_kind: entry.handoff_kind || "handoff",
        from_address: entry.from_address || "",
        from_provider: entry.from_provider || null,
        from_session_id: entry.from_session_id || null,
        from_turns: entry.from_turns || null,
        artifact_path: entry.artifact_path || null,
        ts_ms: entry.ts_ms ?? 0,
      });
      return;
    }

    if (kind === "thought" || kind === "tool_call" || kind === "plan" || kind === "permission") {
      flushStderr();
      if (!activeGroup || activeGroup.turn !== (entry.turn ?? 0)) {
        flushGroup();
        activeGroup = {
          kind: "activity_group",
          id: `ag-${idx}`,
          turn: entry.turn ?? 0,
          showSpeakerHeader: speakerNeeded(entry.turn),
          steps: [],
          toolCount: 0,
          thoughtCount: 0,
          planCount: 0,
          permissionCount: 0,
          runningCount: 0,
          failedCount: 0,
          activeTitle: null,
          latestPlan: null,
          ts_ms: entry.ts_ms ?? 0,
          defaultExpanded: false,
        };
      }

      if (kind === "thought") {
        const text = normalizeThoughtText(entry.text);
        if (text) {
          activeGroup.thoughtCount += 1;
          activeGroup.steps.push({
            kind: "thought",
            id: `th-${idx}`,
            text,
            preview: thoughtPreview(text),
            ts_ms: entry.ts_ms ?? 0,
          });
        }
      } else if (kind === "tool_call") {
        const status = entry.status || "completed";
        const title = entry.title || entry.id || "Tool call";
        const info = summarizeToolTitle(title);
        const detail = entry.detail ? String(entry.detail) : null;
        const parsedDetail = detail ? parseToolDetail({ tool_kind: entry.tool_kind, title, detail }) : null;
        activeGroup.toolCount += 1;
        if (isRunningStatus(status)) {
          activeGroup.runningCount += 1;
          activeGroup.activeTitle = info.summary;
        } else if (status === "failed") {
          activeGroup.failedCount += 1;
        }
        activeGroup.steps.push({
          kind: "tool_call",
          id: `tc-${idx}`,
          call_id: entry.id || `tc-${idx}`,
          title,
          summaryTitle: info.summary,
          extraLines: info.extraLines,
          multiline: info.multiline,
          detail,
          diffStats: parsedDetail?.mode === "diff"
            ? { additions: parsedDetail.additions, deletions: parsedDetail.deletions }
            : null,
          expandable: Boolean(info.multiline || (title && title.length > 120) || detail),
          tool_kind: entry.tool_kind || "other",
          status,
          ts_ms: entry.ts_ms ?? 0,
        });
      } else if (kind === "plan") {
        const planItems = Array.isArray(entry.items) ? entry.items.filter(Boolean) : [];
        if (planItems.length > 0) {
          const parsedItems = parsePlanItems(planItems);
          const planStep = { kind: "plan", id: `pl-${idx}`, items: planItems, parsedItems, ts_ms: entry.ts_ms ?? 0 };
          activeGroup.planCount += 1;
          activeGroup.steps.push(planStep);
          activeGroup.latestPlan = planStep;
        }
      } else if (kind === "permission") {
        activeGroup.permissionCount += 1;
        activeGroup.steps.push({
          kind: "permission",
          id: `pm-${idx}`,
          title: entry.title || "Permission",
          outcome: entry.outcome || "",
          ts_ms: entry.ts_ms ?? 0,
        });
      }
      if (!activeGroup.ts_ms && entry.ts_ms) activeGroup.ts_ms = entry.ts_ms;
      return;
    }

    if (kind === "assistant") {
      flushGroup();
      flushStderr();
      const text = String(entry.text ?? "").trim();
      if (!text) return;
      const turnNum = entry.turn ?? 0;
      const prev = items[items.length - 1];
      if (prev && prev.kind === "assistant" && prev.turn === turnNum) {
        prev.text = `${prev.text}\n\n${text}`;
        prev.blocks = splitMarkdownBlocks(prev.text);
        if (entry.ts_ms) prev.ts_ms = entry.ts_ms;
        return;
      }
      items.push({
        kind: "assistant",
        id: `as-${idx}`,
        turn: turnNum,
        showSpeakerHeader: speakerNeeded(entry.turn),
        text,
        blocks: splitMarkdownBlocks(text),
        ts_ms: entry.ts_ms ?? 0,
      });
      return;
    }

    if (kind === "checkpoint") {
      const turn = Number(entry.turn ?? 0);
      if (turn > 0) {
        const norm = normalizeCheckpointFiles(entry.files);
        if (norm.files.length > 0) {
          checkpointStatsByTurn.set(turn, {
            ...norm,
            checkpointTurn: turn,
            fromCheckpoint: true,
          });
        }
      }
      return;
    }

    if (kind === "rewind") {
      flushGroup();
      flushStderr();
      currentSpeakerKey = null;
      items.push({
        kind: "rewind",
        id: `rw-${idx}`,
        to_turn: entry.to_turn ?? 0,
        restore_files: Boolean(entry.restore_files),
        ts_ms: entry.ts_ms ?? 0,
      });
      return;
    }

    if (kind === "error") {
      flushGroup();
      flushStderr();
      items.push({
        kind: "error",
        id: `er-${idx}`,
        turn: entry.turn ?? 0,
        showSpeakerHeader: speakerNeeded(entry.turn),
        message: String(entry.message ?? "Unknown error"),
        ts_ms: entry.ts_ms ?? 0,
      });
      return;
    }

    if (kind === "stderr") {
      flushGroup();
      const text = String(entry.text ?? "").replace(/\r\n/g, "\n").trim();
      if (!text) return;
      if (!activeStderr) {
        activeStderr = {
          kind: "stderr_group",
          id: `se-${idx}`,
          lines: [],
          text: "",
          lineCount: 0,
          ts_ms: entry.ts_ms ?? 0,
        };
      }
      activeStderr.lines.push(text);
    }
  });

  flushGroup();
  flushStderr();

  // Prune empty activity groups (for instance empty whitespace thoughts).
  const cwd = options?.cwd || "";
  const compact = items.filter((it) => it.kind !== "activity_group" || it.steps.length > 0);
  const lastIdx = compact.length - 1;
  const finalAssistantIdxByTurn = new Map();
  const lastGroupIdxByTurn = new Map();
  const toolStepsByTurn = new Map();
  compact.forEach((it, idx) => {
    const turn = it.turn ?? 0;
    if (it.kind === "assistant") {
      finalAssistantIdxByTurn.set(turn, idx);
    } else if (it.kind === "activity_group") {
      lastGroupIdxByTurn.set(turn, idx);
      const prev = toolStepsByTurn.get(turn) ?? [];
      for (const st of it.steps) {
        if (st.kind === "tool_call") prev.push(st);
      }
      toolStepsByTurn.set(turn, prev);
    }
  });
  const changedByTurn = new Map();
  for (const [turn, steps] of toolStepsByTurn.entries()) {
    const summary = extractTurnChangedFiles(steps, cwd);
    if (summary.files.length > 0) changedByTurn.set(turn, summary);
  }
  for (const [turn, ckptSummary] of checkpointStatsByTurn.entries()) {
    changedByTurn.set(turn, ckptSummary);
  }
  compact.forEach((it, idx) => {
    const turn = it.turn ?? 0;
    const changed = changedByTurn.get(turn) ?? null;
    if (it.kind === "activity_group") {
      it.defaultExpanded = it.runningCount > 0 || (agentWorking && idx === lastIdx);
      if (
        changed &&
        !finalAssistantIdxByTurn.has(turn) &&
        !agentWorking &&
        lastGroupIdxByTurn.get(turn) === idx
      ) {
        it.changedFiles = changed;
      }
    } else if (it.kind === "assistant") {
      const isFinal = finalAssistantIdxByTurn.get(turn) === idx;
      it.isFinalTurnReply = isFinal;
      if (isFinal && changed) {
        it.changedFiles = changed;
      }
    }
  });
  return compact;
}

const isModeOption = (o) =>
  o?.category === "mode" ||
  o?.id === "mode" ||
  o?.id === "permissions" ||
  o?.id === "permission_mode" ||
  o?.id === "approval_policy";

const CAPABILITY_ORDER = [
  (o) => o.category === "model" || o.id === "model",
  (o) => o.id === "context" || o.id === "context_window",
  (o) =>
    o.category === "thought_level" ||
    o.id === "reasoning_effort" ||
    o.id === "reasoning" ||
    o.id === "effort" ||
    o.id === "thinking",
  (o) => o.id === "fast-mode" || o.id === "fast" || (o.kind === "boolean" && o.category === "model_config"),
  isModeOption,
];

function shortLabelFor(opt, rankIdx) {
  if (rankIdx === 0) return "Model";
  if (rankIdx === 1) return "Ctx";
  if (rankIdx === 2) return "Reasoning";
  if (rankIdx === 3) return "Fast";
  if (rankIdx === 4) return "Mode";
  if (opt.id === "collaboration_mode") return "Collab";
  return opt.name || opt.id;
}

function iconKindFor(rankIdx) {
  if (rankIdx === 0) return "model";
  if (rankIdx === 1) return "extra";
  if (rankIdx === 2) return "thinking";
  if (rankIdx === 3) return "fast";
  if (rankIdx === 4) return "mode";
  return "extra";
}

function pillLabelFor(opt, shortLabel, iconKind, currentValue, currentLabel) {
  if (iconKind === "fast") {
    if (opt.kind !== "boolean") {
      const lower = String(currentValue ?? "").trim().toLowerCase();
      return lower === "false" || lower === "off" || lower === "" ? "Fast · Off" : shortLabel;
    }
    return shortLabel;
  }
  if (opt.id === "context" || opt.id === "context_window") {
    const cleanVal = String(currentLabel ?? currentValue ?? "").trim();
    return cleanVal ? `Ctx ${cleanVal}` : shortLabel;
  }
  if (iconKind === "extra") {
    return currentLabel.toLowerCase() === "default" ? shortLabel : currentLabel;
  }
  return currentLabel;
}

const CLAUDE_BYPASS_LABEL = "dangerously-skip-permissions";
const CLAUDE_BYPASS_DESC = "Skip all permission checks (bypassPermissions)";
const CLAUDE_MODE_DEFAULTS = [
  { value: "default", name: "Manual", description: "Always ask before making changes", group: null },
  { value: "acceptEdits", name: "Accept edits", description: "Automatically accept all file edits", group: null },
  { value: "plan", name: "Plan", description: "Create a plan before making changes", group: null },
  { value: "auto", name: "Auto", description: "Claude handles permission decisions", group: null },
  { value: "bypassPermissions", name: CLAUDE_BYPASS_LABEL, description: CLAUDE_BYPASS_DESC, group: null },
];
const HERDR_PERMISSION_DEFAULTS = [
  { value: "ask", name: "Ask every time", description: "Prompt before any permission request", group: null },
  { value: "approve-reads", name: "Approve reads", description: "Auto-approve reads/searches, ask on writes/exec", group: null },
  { value: "approve-all", name: "Approve all", description: "Auto-approve every ACP permission prompt", group: null },
  { value: "dangerously-skip-permissions", name: CLAUDE_BYPASS_LABEL, description: "Skip all permission checks (approve-all policy)", group: null },
  { value: "deny-all", name: "Deny all", description: "Automatically deny permission requests", group: null },
];

/**
 * Returns true when an ACP record or its mode options belong to Claude Code ACP.
 */
export function isClaudeAgentRecord(record, choices = []) {
  if (!record && (!Array.isArray(choices) || !choices.length)) return false;
  if (
    record?.agent === "claude" ||
    record?.program?.key === "claude" ||
    record?.report?.app === "claude-code" ||
    String(record?.program?.name ?? "").toLowerCase().includes("claude") ||
    String(record?.program?.icon ?? "").toLowerCase().includes("claude") ||
    String(record?.template ?? "").toLowerCase().includes("claude") ||
    String(record?.agent_info ?? "").toLowerCase().includes("claude") ||
    (Array.isArray(record?.argv) && String(record.argv[0] ?? "").toLowerCase().includes("claude"))
  ) {
    return true;
  }
  return Array.isArray(choices) && choices.some((c) => c?.value === "bypassPermissions");
}

function normalizeModeChoices(choices, isClaude) {
  if (!isClaude) return choices;
  const out = choices.map((c) =>
    c.value === "bypassPermissions"
      ? { ...c, name: CLAUDE_BYPASS_LABEL, description: c.description || CLAUDE_BYPASS_DESC }
      : c,
  );
  if (!out.some((c) => c.value === "bypassPermissions")) {
    out.push({ value: "bypassPermissions", name: CLAUDE_BYPASS_LABEL, description: CLAUDE_BYPASS_DESC, group: null });
  }
  return out;
}

/**
 * Normalizes ACP session `config_options` (plus `model`/`mode` record fallback)
 * into capability pill descriptors ordered by primary user controls.
 */
export function normalizeConfigOptions(rawOptions, record) {
  const list = Array.isArray(rawOptions) ? rawOptions : [];
  const normalized = [];

  list.forEach((raw, idx) => {
    if (!raw || !raw.id) return;
    const id = String(raw.id);
    const kind = String(raw.kind ?? raw.type ?? "select");
    const category = raw.category ? String(raw.category) : null;
    const currentValue = raw.current_value !== undefined ? raw.current_value : (raw.currentValue ?? null);
    const rawChoices = (Array.isArray(raw.options) ? raw.options : [])
      .map((c) => ({
        value: String(c?.value ?? ""),
        name: String(c?.name ?? c?.value ?? "").replace(/\u200b/g, "").trim(),
        description: c?.description ? String(c.description).replace(/\u200b/g, "").trim() : null,
        group: c?.group ? String(c.group) : null,
      }))
      .filter((c) => c.value.length > 0);
    const isMode = isModeOption({ category, id });
    const isClaudeMode = isMode && isClaudeAgentRecord(record, rawChoices);
    const choices = isMode ? normalizeModeChoices(rawChoices, isClaudeMode) : rawChoices;

    let currentLabel;
    if (kind === "boolean") {
      currentLabel = currentValue === true ? "On" : "Off";
    } else {
      const rawNeedle = currentValue == null ? "" : String(currentValue);
      const needle = isClaudeMode && rawNeedle === CLAUDE_BYPASS_LABEL ? "bypassPermissions" : rawNeedle;
      const hit = choices.find((c) => c.value === needle);
      currentLabel = hit?.name || (rawNeedle || "Default");
    }

    const descriptor = {
      id,
      name: String(raw.name || id),
      description: raw.description ? String(raw.description) : null,
      category,
      kind,
      currentValue,
      currentLabel,
      choices,
    };
    const rankIdx = CAPABILITY_ORDER.findIndex((pred) => pred(descriptor));
    const shortLabel = shortLabelFor(descriptor, rankIdx);
    const iconKind = iconKindFor(rankIdx);
    const isNonDefault = kind === "boolean"
      ? currentValue === true
      : iconKind === "fast"
        ? currentValue != null && !["", "false", "off", "default"].includes(String(currentValue).trim().toLowerCase())
        : (currentValue != null && String(currentValue) !== "" && String(currentValue).toLowerCase() !== "default");
    normalized.push({
      ...descriptor,
      shortLabel,
      iconKind,
      pillLabel: pillLabelFor(descriptor, shortLabel, iconKind, currentValue, currentLabel),
      effortVisual: iconKind === "thinking" ? resolveEffortVisual(currentValue ?? currentLabel) : null,
      isNonDefault,
      _rank: rankIdx >= 0 ? rankIdx : 10 + idx,
    });
  });

  if (!normalized.some((o) => o.category === "model" || o.id === "model")) {
    const fallbackModel = record?.model ? String(record.model) : "";
    const currentLabel = fallbackModel || "Default model";
    normalized.push({
      id: "model",
      name: "Model",
      shortLabel: "Model",
      iconKind: "model",
      pillLabel: currentLabel,
      isNonDefault: Boolean(fallbackModel),
      description: "Custom model identifier sent to the ACP session",
      category: "model",
      kind: "custom_text",
      currentValue: fallbackModel,
      currentLabel,
      choices: [],
      _rank: 0,
    });
  }

  if (!normalized.some(isModeOption)) {
    if (isClaudeAgentRecord(record)) {
      const fallbackMode = record?.mode
        ? String(record.mode)
        : (!record?.permission_mode || record?.permission_mode === "approve-all" ? "bypassPermissions" : "default");
      const needle = fallbackMode === CLAUDE_BYPASS_LABEL ? "bypassPermissions" : fallbackMode;
      const hit = CLAUDE_MODE_DEFAULTS.find((c) => c.value === needle);
      const currentLabel = hit?.name || fallbackMode || "Manual";
      normalized.push({
        id: "mode",
        name: "Mode",
        shortLabel: "Mode",
        iconKind: "mode",
        pillLabel: currentLabel,
        isNonDefault: needle !== "default",
        description: "Session permission mode",
        category: "mode",
        kind: "select",
        currentValue: needle,
        currentLabel,
        choices: CLAUDE_MODE_DEFAULTS.map((c) => ({ ...c })),
        _rank: 3,
      });
    } else if (record?.permission_mode) {
      const pm = String(record.permission_mode);
      const hit = HERDR_PERMISSION_DEFAULTS.find((c) => c.value === pm);
      const currentLabel = hit?.name || pm;
      normalized.push({
        id: "permissions",
        name: "Permissions",
        shortLabel: "Permissions",
        iconKind: "mode",
        pillLabel: currentLabel,
        isNonDefault: pm === "approve-all" || pm === "dangerously-skip-permissions",
        description: "Herdr2 ACP permission policy",
        category: "mode",
        kind: "select",
        currentValue: pm,
        currentLabel,
        choices: HERDR_PERMISSION_DEFAULTS.map((c) => ({ ...c })),
        _rank: 3,
      });
    }
  }

  normalized.sort((a, b) => a._rank - b._rank);
  return normalized.map(({ _rank, ...rest }) => rest);
}

/**
 * Case-insensitive search filter across config choice labels, IDs, groups, and hints.
 */
export function filterConfigChoices(choices, query) {
  const list = Array.isArray(choices) ? choices : [];
  const q = String(query ?? "").trim().toLowerCase();
  if (!q) return list;
  return list.filter((c) =>
    [c.name, c.value, c.description, c.group].some((f) => String(f ?? "").toLowerCase().includes(q)),
  );
}

/**
 * Normalizes queued ACP inbox items for display and inline edit/removal,
 * stripping `<h2 ...>...</h2>` envelopes and resolving friendly sender labels.
 */
export function normalizeInboxItems(rawInbox, terminalsList = [], currentTerminal = "") {
  if (!Array.isArray(rawInbox) || rawInbox.length === 0) return [];
  const slash = typeof currentTerminal === "string" ? currentTerminal.lastIndexOf("/") : -1;
  const prefix = slash >= 0 ? currentTerminal.slice(0, slash + 1) : "";
  const list = Array.isArray(terminalsList) ? terminalsList : [];
  return rawInbox
    .filter((raw) => raw && raw.id != null)
    .map((raw, idx) => {
      const parsed = parseUserEnvelope(raw.text ?? "", raw.from ?? null);
      const body = parsed.body || String(raw.text ?? "").trim();
      const from = parsed.from || null;
      let fromLabel = null;
      if (from) {
        const sender = list.find((x) => x?.address === from || (prefix && x?.address === `${prefix}${from}`));
        fromLabel = sender?.name && sender.name !== from ? `${sender.name} (${from})` : from;
      }
      return {
        id: Number(raw.id),
        index: idx + 1,
        rawText: String(raw.text ?? ""),
        body,
        preview: body.replace(/\s*\n\s*/g, " ↵ "),
        from,
        fromLabel,
        time: formatTime(raw.queued_ms),
        queued_ms: Number(raw.queued_ms ?? 0),
      };
    });
}
