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

/**
 * Strips herdr2's `<h2 ...>...</h2>` envelope around delegated user prompts and
 * returns the extracted sender metadata plus clean message text.
 */
export function parseUserEnvelope(rawText, entryFrom = null) {
  let text = String(rawText ?? "").replace(/\r\n/g, "\n");
  let from = entryFrom || null;
  let reach = null;

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

  return {
    from,
    reach,
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
  if (kind === "execute" || kind === "command" || /\b(bash|sh|exec|run)\b/.test(label)) {
    return "mods/workspace-chrome/terminal.svg";
  }
  if (kind === "read" || kind === "edit" || kind === "write" || /\b(read|write|edit|file)\b/.test(label)) {
    return "mods/workspace-chrome/file-text.svg";
  }
  if (kind === "search" || /\b(search|grep|rg|find|webreader|fetch)\b/.test(label)) {
    return "mods/workspace-chrome/search.svg";
  }
  if (kind === "think" || kind === "thought") {
    return "mods/workspace-chrome/clock.svg";
  }
  return "mods/workspace-chrome/layers.svg";
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
 * Cleans raw terminal screen snapshots by normalizing non-breaking spaces,
 * trimming trailing whitespace, removing full-width divider lines, and
 * collapsing large runs of empty lines left by fixed-row PTY screens.
 */
export function cleanTerminalSnapshot(rawText) {
  const src = String(rawText ?? "").replace(/\r\n/g, "\n").replace(/\u00a0/g, " ");
  if (!src.trim()) return "";
  const cleaned = [];
  let blankRun = 0;
  for (const rawLine of src.split("\n")) {
    const line = rawLine.replace(/[ \t]+$/g, "");
    const trimmed = line.trim();
    if (/^[─━═┄┈\-_]{24,}$/.test(trimmed)) {
      continue;
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
      items.push({
        kind: "user",
        id: `u-${idx}`,
        turn: entry.turn ?? 0,
        from: env.from,
        reach: env.reach,
        body: env.body || String(entry.text ?? "").trim(),
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
          ts_ms: entry.ts_ms ?? 0,
          defaultExpanded: false,
        };
      }

      if (kind === "thought") {
        const text = normalizeThoughtText(entry.text);
        if (text) {
          activeGroup.thoughtCount += 1;
          activeGroup.steps.push({ kind: "thought", id: `th-${idx}`, text, ts_ms: entry.ts_ms ?? 0 });
        }
      } else if (kind === "tool_call") {
        const status = entry.status || "completed";
        const title = entry.title || entry.id || "Tool call";
        const info = summarizeToolTitle(title);
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
          tool_kind: entry.tool_kind || "other",
          status,
          ts_ms: entry.ts_ms ?? 0,
        });
      } else if (kind === "plan") {
        const planItems = Array.isArray(entry.items) ? entry.items.filter(Boolean) : [];
        if (planItems.length > 0) {
          activeGroup.planCount += 1;
          activeGroup.steps.push({ kind: "plan", id: `pl-${idx}`, items: planItems, ts_ms: entry.ts_ms ?? 0 });
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
  const compact = items.filter((it) => it.kind !== "activity_group" || it.steps.length > 0);
  const lastIdx = compact.length - 1;
  const finalAssistantIdxByTurn = new Map();
  compact.forEach((it, idx) => {
    if (it.kind === "assistant") {
      finalAssistantIdxByTurn.set(it.turn ?? 0, idx);
    }
  });
  compact.forEach((it, idx) => {
    if (it.kind === "activity_group") {
      it.defaultExpanded = it.runningCount > 0 || (agentWorking && idx === lastIdx);
    } else if (it.kind === "assistant") {
      it.isFinalTurnReply = finalAssistantIdxByTurn.get(it.turn ?? 0) === idx;
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
  (o) => o.category === "thought_level" || o.id === "reasoning_effort" || o.id === "thinking",
  (o) => o.id === "fast-mode" || o.id === "fast" || (o.kind === "boolean" && o.category === "model_config"),
  isModeOption,
];

function shortLabelFor(opt, rankIdx) {
  if (rankIdx === 0) return "Model";
  if (rankIdx === 1) return "Reasoning";
  if (rankIdx === 2) return "Fast";
  if (rankIdx === 3) return "Mode";
  if (opt.id === "collaboration_mode") return "Collab";
  return opt.name || opt.id;
}

function iconKindFor(rankIdx) {
  if (rankIdx === 0) return "model";
  if (rankIdx === 1) return "thinking";
  if (rankIdx === 2) return "fast";
  if (rankIdx === 3) return "mode";
  return "extra";
}

function pillLabelFor(opt, shortLabel, iconKind, currentValue, currentLabel) {
  if (iconKind === "fast") return shortLabel;
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
        name: String(c?.name ?? c?.value ?? ""),
        description: c?.description ? String(c.description) : null,
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
      : (currentValue != null && String(currentValue) !== "" && String(currentValue).toLowerCase() !== "default");
    normalized.push({
      ...descriptor,
      shortLabel,
      iconKind,
      pillLabel: pillLabelFor(descriptor, shortLabel, iconKind, currentValue, currentLabel),
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
