import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, v_resizable, resizable_panel, Popup, InputState, Input, Scrollbar } from "gpui-base";
import { spaces, panes, focused, terminals, knownAgents, shared, setShared, theme, settings, h2, checking, openOverlay, notify } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

const KNOWN_AGENTS_KEY = "new-menu.agents";
const INITIAL_VISIBLE_ITEMS = 60;
const VISIBLE_ITEMS_STEP = 60;
const MAX_ENTRY_CHARS = 12000;
const DEFAULT_SPLIT_TOP_HEIGHT = 220;
const MIN_SPLIT_TOP_HEIGHT = 96;
const MAX_SPLIT_TOP_HEIGHT = 680;

const SHELL_NAMES = new Set([
  "sh", "bash", "zsh", "fish", "nu", "nushell", "dash", "ksh", "csh", "tcsh",
  "pwsh", "powershell", "cmd", "elvish", "xonsh", "ion",
]);

// Canonical CLI agents that persist local session history, with fallback display metadata & icons.
const BUILTIN_CLI_AGENTS = [
  { key: "claude", name: "Claude Code", icon: "icons/claude.svg", tint: false },
  { key: "codex", name: "Codex", icon: "icons/codex.svg", tint: false },
  { key: "agy", name: "Antigravity", icon: "icons/antigravity.svg", tint: false },
  { key: "cursor", name: "Cursor", icon: "icons/cursor.svg", tint: true },
  { key: "zcode", name: "ZCode", icon: "icons/zcode.svg", tint: true },
  { key: "opencode", name: "OpenCode", icon: "icons/opencode.svg", tint: true },
  { key: "qwen", name: "Qwen Code", icon: "icons/agent.svg", tint: true },
  { key: "gemini", name: "Gemini CLI", icon: "icons/agent.svg", tint: true },
  { key: "hermes", name: "Hermes", icon: "icons/hermes.svg", tint: true },
  { key: "kimi", name: "Kimi", icon: "icons/kimi.svg", tint: false },
  { key: "pi", name: "Pi", icon: "icons/pi.svg", tint: true },
  { key: "omp", name: "oh-my-pi", icon: "icons/omp.svg", tint: true },
  { key: "copilot", name: "Copilot", icon: "icons/copilot.svg", tint: true },
  { key: "qoder", name: "Qoder", icon: "icons/qoder.svg", tint: true },
];

const SUPPORTED_AGENT_SET = new Set(BUILTIN_CLI_AGENTS.map((a) => a.key));

function normalizeAgentKey(raw) {
  const s = String(raw ?? "").trim().toLowerCase().replace(/\.exe$/i, "").replace(/\.real$/i, "");
  if (!s) return null;
  if (s === "claude" || s === "claude-code" || s === "claude code") return "claude";
  if (s === "codex" || s === "codex-cli") return "codex";
  if (s === "agy" || s === "antigravity" || s === "antigravity-cli") return "agy";
  if (s === "cursor" || s === "cursor-agent") return "cursor";
  if (s === "zcode" || s === "zcode-cli") return "zcode";
  if (s === "opencode") return "opencode";
  if (s === "qwen" || s === "qwen-code") return "qwen";
  if (s === "gemini" || s === "gemini-cli") return "gemini";
  if (s === "hermes" || s === "hermes-agent") return "hermes";
  if (s === "kimi" || s === "kimi-code" || s === "kimicodecli") return "kimi";
  if (s === "pi") return "pi";
  if (s === "omp" || s === "oh-my-pi") return "omp";
  if (s === "copilot" || s === "github-copilot") return "copilot";
  if (s === "qoder" || s === "qodercli") return "qoder";
  if (s === "amp") return "amp";
  if (s === "kiro" || s === "kiro-cli") return "kiro";
  if (s === "crush") return "crush";
  return null;
}

function getAgentVisual(agentKey, catalog) {
  const norm = normalizeAgentKey(agentKey) || String(agentKey ?? "").trim().toLowerCase();
  const fromCatalog = Array.isArray(catalog) ? catalog.find((c) => c.key === norm) : null;
  if (fromCatalog) return fromCatalog;
  const builtin = BUILTIN_CLI_AGENTS.find((c) => c.key === norm);
  if (builtin) return builtin;
  return { key: norm || "claude", name: norm || "CLI Agent", icon: "icons/agent.svg", tint: true };
}

function renderAgentIcon(info, size, t) {
  const iconPath = info?.icon || "icons/agent.svg";
  if (info?.tint) {
    return svg(iconPath).size(size).flex_none().text_color(t.foreground);
  }
  return image(iconPath).size(size).flex_none();
}

function readSavedTopHeight() {
  try {
    if (typeof localStorage !== "undefined" && localStorage?.getItem) {
      const raw = Number(localStorage.getItem("sessions.topHeight"));
      if (Number.isFinite(raw) && raw >= MIN_SPLIT_TOP_HEIGHT && raw <= MAX_SPLIT_TOP_HEIGHT) {
        return Math.round(raw);
      }
    }
  } catch {}
  return DEFAULT_SPLIT_TOP_HEIGHT;
}

function readPreferredAgent() {
  try {
    if (typeof localStorage !== "undefined" && localStorage?.getItem) {
      const raw = normalizeAgentKey(localStorage.getItem("sessions.preferredAgent"));
      if (raw) return raw;
    }
  } catch {}
  return null;
}

function writePreferredAgent(agentKey) {
  const norm = normalizeAgentKey(agentKey);
  if (!norm) return;
  try {
    if (typeof localStorage !== "undefined" && localStorage?.setItem) {
      localStorage.setItem("sessions.preferredAgent", norm);
    }
  } catch {}
}

function isGenericProgramKey(key) {
  const k = String(key ?? "").trim().toLowerCase();
  return !k || k === "agent" || k === "program" || k === "shell" || k === "terminal" || SHELL_NAMES.has(k);
}

function detectCliAgentOnPane(pane, term) {
  if (pane?.program?.agent && !pane?.program?.shell && !isGenericProgramKey(pane.program.key)) {
    const norm = normalizeAgentKey(pane.program.key);
    if (norm) return norm;
  }
  if (term?.saved_cli?.kind && !isGenericProgramKey(term.saved_cli.kind)) {
    const norm = normalizeAgentKey(term.saved_cli.kind);
    if (norm) return norm;
  }
  for (const raw of [pane?.app, term?.report?.app]) {
    if (raw && !isGenericProgramKey(raw)) {
      const norm = normalizeAgentKey(raw);
      if (norm) return norm;
    }
  }
  if (term?.program?.agent && !term?.program?.shell && !isGenericProgramKey(term.program.key)) {
    const norm = normalizeAgentKey(term.program.key);
    if (norm) return norm;
  }
  if (Array.isArray(term?.argv) && term.argv.length > 0) {
    const first = String(term.argv[0] ?? "").trim();
    const base = (first.split(/[\\/]/).pop() ?? first).replace(/\.exe$/i, "").toLowerCase();
    if (base && !SHELL_NAMES.has(base)) {
      const norm = normalizeAgentKey(base);
      if (norm) return norm;
    }
  }
  return null;
}

function resolveActiveTerminalContext() {
  const sList = typeof spaces === "function" ? spaces() : [];
  const pList = typeof panes === "function" ? panes() : [];
  const tList = typeof terminals === "function" ? terminals() : [];
  const foc = typeof focused === "function" ? focused() : null;

  const termByAddr = new Map();
  if (Array.isArray(tList)) {
    for (const rec of tList) {
      if (rec?.address) termByAddr.set(rec.address, rec);
    }
  }

  const activeSpace = Array.isArray(sList) ? sList.find((s) => s?.active) : null;
  const activeTab = activeSpace?.tabs?.find((t) => t?.active) ?? null;
  const tabPanes = activeTab && Array.isArray(pList)
    ? pList.filter((p) => p?.tab === activeTab.id && p?.terminal)
    : [];

  let chosenPane = null;
  const focusedPane = tabPanes.find((p) => p.focused) ?? null;
  if (focusedPane) {
    const fTerm = termByAddr.get(focusedPane.terminal) ?? null;
    if (focusedPane.acp || fTerm?.acp) {
      return {
        supported: false,
        target: null,
        detectedAgent: null,
        isPlainTerminal: false,
        cwd: activeSpace?.folder ?? "",
        machine: activeSpace?.machine ?? null,
        activeSessionId: null,
        turn: 0,
      };
    }
    chosenPane = focusedPane;
  } else {
    chosenPane = tabPanes.find((p) => {
      const tRec = termByAddr.get(p.terminal) ?? null;
      return !(p.acp || tRec?.acp);
    }) ?? null;
  }

  let target = chosenPane?.terminal ?? null;
  let term = target ? (termByAddr.get(target) ?? null) : null;

  if (!target) {
    const fallbackAddr = foc?.shown || foc?.terminal || null;
    if (fallbackAddr) {
      const fbTerm = termByAddr.get(fallbackAddr) ?? null;
      const fbPane = Array.isArray(pList) ? pList.find((p) => p?.terminal === fallbackAddr) ?? null : null;
      if (!(fbPane?.acp || fbTerm?.acp)) {
        target = fallbackAddr;
        term = fbTerm;
        chosenPane = fbPane;
      }
    }
  }

  const spaceFolder = String(activeSpace?.folder ?? "").trim();
  if (!target) {
    return {
      supported: Boolean(spaceFolder),
      target: null,
      detectedAgent: null,
      isPlainTerminal: true,
      cwd: spaceFolder,
      machine: activeSpace?.machine ?? null,
      activeSessionId: null,
      turn: 0,
    };
  }

  const detectedAgent = detectCliAgentOnPane(chosenPane, term);
  const cwd = String(
    chosenPane?.cwd
      || term?.foreground?.cwd
      || term?.cwd
      || foc?.cwd
      || spaceFolder
      || "",
  ).trim();
  const slash = target.indexOf("/");
  const machine = slash > 0 ? target.slice(0, slash) : (activeSpace?.machine ?? null);

  return {
    supported: true,
    target,
    detectedAgent,
    isPlainTerminal: !detectedAgent,
    cwd,
    machine,
    activeSessionId: term?.saved_cli?.session_id ?? null,
    turn: Number(term?.turn ?? 0) || 0,
  };
}

function folderBasename(rawPath) {
  const s = String(rawPath ?? "").trim().replace(/[\\/]+$/, "");
  if (!s) return "";
  const parts = s.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? s;
}

function formatFriendlyTime(tsMs) {
  const ts = Number(tsMs) || 0;
  if (ts <= 0) return "";
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 60) return tr("Just now", "刚刚");
  if (diffSec < 3600) {
    const m = Math.floor(diffSec / 60);
    return tr(`${m}m ago`, `${m} 分钟前`);
  }
  if (diffSec < 86400) {
    const h = Math.floor(diffSec / 3600);
    return tr(`${h}h ago`, `${h} 小时前`);
  }
  const days = Math.floor(diffSec / 86400);
  if (days === 1) return tr("Yesterday", "昨天");
  if (days < 7) return tr(`${days}d ago`, `${days} 天前`);
  const d = new Date(ts);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${mm}-${dd}`;
}

function formatEntryClock(tsMs) {
  const ts = Number(tsMs) || 0;
  if (ts <= 0) return "";
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, "0");
  const mi = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mi}`;
}

/**
 * Strips internal handoff/envelope wrappers from session titles so rows read cleanly.
 */
function formatCleanSessionTitle(raw, maxChars = 110) {
  let s = String(raw ?? "")
    .replace(/<\/?h2\b[^>]*>/gi, " ")
    .replace(/<herdr2-handoff\b[^>]*>[\s\S]*?<\/herdr2-handoff>/gi, " ")
    .replace(/<\/?herdr2-handoff\b[^>]*>/gi, " ")
    .replace(/<\/?pasted_content\b[^>]*>/gi, " ")
    .replace(/[\r\n\t]+/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();

  // Condense verbose handoff preamble if a clear user goal follows it.
  const goalMatch = s.match(/承接上一轮目标继续推进[：:]\s*(.+)$/);
  if (goalMatch && goalMatch[1]?.trim()) {
    s = `${tr("[Handoff] ", "[接力] ")}${goalMatch[1].trim()}`;
  } else if (/^\[来自\s+[^\]]+的旁路讨论\]/.test(s)) {
    const rest = s
      .replace(/^\[来自\s+[^\]]+的旁路讨论\]\s*/, "")
      .replace(/^请先阅读并消化上述携带的交接上下文[。.]?[^。.]*[。.]?\s*/, "")
      .replace(/（当前为只读旁路讨论[^）]*）\s*/g, "")
      .trim();
    s = rest ? `${tr("[Side chat] ", "[旁路] ")}${rest}` : tr("Side chat discussion", "旁路讨论会话");
  }

  return s.length > maxChars ? `${s.slice(0, maxChars)}…` : s;
}

function cleanUserPromptText(raw) {
  let s = String(raw ?? "").replace(/\r\n/g, "\n");
  s = s
    .replace(/^<h2\b[^>]*>\s*/i, "")
    .replace(/\s*<\/h2>$/i, "")
    .replace(/^<herdr2-handoff\b[^>]*>[\s\S]*?<\/herdr2-handoff>\s*/i, "")
    .replace(/<\/?pasted_content\b[^>]*>/gi, "")
    .trim();
  if (s.length > MAX_ENTRY_CHARS) return `${s.slice(0, MAX_ENTRY_CHARS)}\n…`;
  return s;
}

function cleanSingleLine(raw, maxChars = 140) {
  const s = String(raw ?? "")
    .replace(/<\/?pasted_content\b[^>]*>/gi, " ")
    .replace(/<\/?herdr2-handoff\b[^>]*>/gi, " ")
    .replace(/[\r\n\t]+/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();
  return s.length > maxChars ? `${s.slice(0, maxChars)}…` : s;
}

function clipBodyText(raw) {
  const s = String(raw ?? "").replace(/\r\n/g, "\n").trim();
  if (s.length <= MAX_ENTRY_CHARS) return s;
  return `${s.slice(0, MAX_ENTRY_CHARS)}\n…`;
}

/**
 * Splits markdown assistant text into prose paragraphs/headings/lists and fenced code cards
 * so responses look clean and readable without raw triple-backtick noise.
 */
function parseAssistantBlocks(raw) {
  const text = clipBodyText(raw);
  if (!text) return [];
  const lines = text.split("\n");
  const blocks = [];
  let inFence = false;
  let fenceLang = "";
  let buf = [];

  const flushProse = () => {
    if (!buf.length) return;
    const joined = buf.join("\n").trim();
    if (joined) {
      for (const para of joined.split(/\n{2,}/)) {
        const p = para.trim();
        if (p) blocks.push({ kind: "prose", text: p });
      }
    }
    buf = [];
  };

  for (const line of lines) {
    const trimmed = line.trimStart();
    const fenceMatch = trimmed.match(/^```([\w.+-]*)\s*$/);
    if (fenceMatch) {
      if (!inFence) {
        flushProse();
        inFence = true;
        fenceLang = fenceMatch[1] || "";
      } else {
        blocks.push({ kind: "code", lang: fenceLang, text: buf.join("\n") });
        buf = [];
        inFence = false;
        fenceLang = "";
      }
      continue;
    }
    buf.push(line);
  }
  if (inFence) {
    blocks.push({ kind: "code", lang: fenceLang, text: buf.join("\n") });
  } else {
    flushProse();
  }
  return blocks;
}

function cleanInlineMarkdown(line) {
  return String(line ?? "")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1");
}

/**
 * Groups consecutive intermediate steps (`thought`, `tool_call`, `plan`) into
 * collapsible `activity_group` items so the conversation detail view shows only
 * dialogue (`user` / `assistant`) by default while keeping steps accessible on demand.
 */
export function groupTranscriptItems(rawEntries) {
  const entries = Array.isArray(rawEntries) ? rawEntries : [];
  const items = [];
  let pendingGroup = null;

  const flushGroup = () => {
    if (pendingGroup && pendingGroup.steps.length > 0) {
      items.push(pendingGroup);
    }
    pendingGroup = null;
  };

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    if (!entry || typeof entry !== "object") continue;
    const kind = String(entry.kind || entry.type || "");

    if (kind === "thought") {
      const text = clipBodyText(entry.text);
      if (!text) continue;
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.thoughtCount += 1;
      pendingGroup.steps.push({ kind: "thought", idx: i, text, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "tool_call") {
      const title = cleanSingleLine(entry.title || entry.tool_kind || tr("Tool call", "工具调用"), 160);
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.toolCount += 1;
      pendingGroup.steps.push({ kind: "tool_call", idx: i, title, status: entry.status, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "plan" && Array.isArray(entry.items) && entry.items.length > 0) {
      const summary = cleanSingleLine(entry.items.join(" · "), 180);
      if (!summary) continue;
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.planCount += 1;
      pendingGroup.steps.push({ kind: "plan", idx: i, summary, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "user") {
      const body = cleanUserPromptText(entry.text);
      if (!body) continue;
      flushGroup();
      items.push({ kind: "user", idx: i, entry, body });
      continue;
    }

    if (kind === "assistant") {
      const body = clipBodyText(entry.text);
      if (!body) continue;
      flushGroup();
      items.push({ kind: "assistant", idx: i, entry, body });
      continue;
    }

    if (kind === "error") {
      const msg = cleanSingleLine(entry.message || entry.text, 240);
      if (!msg) continue;
      flushGroup();
      items.push({ kind: "error", idx: i, msg });
    }
  }

  flushGroup();
  return items;
}

function formatActivityGroupSummary(group) {
  const parts = [];
  if (group.thoughtCount > 0) {
    parts.push(tr("Thought", "思考过程"));
  }
  if (group.toolCount > 0) {
    parts.push(
      tr(
        `${group.toolCount} tool call${group.toolCount === 1 ? "" : "s"}`,
        `${group.toolCount} 次工具调用`,
      ),
    );
  }
  if (group.planCount > 0) {
    parts.push(tr("Plan", "执行计划"));
  }
  return parts.length > 0 ? parts.join(" · ") : tr("Steps", "中间步骤");
}

function buildAgentCatalog(rawKnown) {
  const seen = new Set();
  const installedList = [];
  const fallbackList = [];

  if (Array.isArray(rawKnown)) {
    for (const item of rawKnown) {
      const norm = normalizeAgentKey(item?.key);
      if (!norm || seen.has(norm) || !SUPPORTED_AGENT_SET.has(norm)) continue;
      seen.add(norm);
      const builtin = BUILTIN_CLI_AGENTS.find((b) => b.key === norm);
      const entry = {
        key: norm,
        name: item?.name || builtin?.name || norm,
        icon: item?.icon || builtin?.icon || "icons/agent.svg",
        tint: typeof item?.tint === "boolean" ? item.tint : Boolean(builtin?.tint),
        installed: Boolean(item?.installed),
      };
      if (entry.installed) installedList.push(entry);
      else fallbackList.push(entry);
    }
  }

  for (const b of BUILTIN_CLI_AGENTS) {
    if (seen.has(b.key)) continue;
    seen.add(b.key);
    fallbackList.push({ ...b, installed: false });
  }

  return installedList.length > 0
    ? [...installedList, ...fallbackList.filter((x) => x.key === "qwen" || x.key === "gemini")]
    : BUILTIN_CLI_AGENTS.slice(0, 8).map((b) => ({ ...b, installed: true }));
}

export default class Sessions extends View {
  init(_props, cx) {
    this.sessions = [];
    this.selectedId = null;
    this.selectedSession = null;
    this.loadedSessionKey = null;
    this.entries = [];
    this.visibleLimit = INITIAL_VISIBLE_ITEMS;
    this.expandedGroups = new Set();
    this.expandAllSteps = false;
    this.splitTopHeight = readSavedTopHeight();
    this.hoveredRowId = null;
    this.copiedSessionId = null;
    this.copiedUntil = 0;

    // Agent selection state:
    // - On a plain terminal, uses `preferredAgent` (persisted) or first installed CLI agent.
    // - On a CLI agent tab, follows `detectedAgent` unless user explicitly picked an override for that target.
    this.preferredAgent = readPreferredAgent();
    this.manualOverrideByTarget = new Map();
    this.lastDetectedByTarget = new Map();
    this.agentPopupOpen = false;
    this.knownByMachine = new Map();
    this.probingMachines = new Set();

    this.agent = null;
    this.agentName = null;
    this.cwd = null;
    this.loadingList = false;
    this.loadingTranscript = false;
    this.error = null;
    this.transcriptError = null;
    this.context = null;
    this.reqSeq = 0;
    this.readSeq = 0;
    this.handle = cx.focus_handle();

    this.searchInput = InputState.new({
      placeholder: tr("Search…", "搜索会话…"),
    });
    this.searchFocused = false;
    this.searchInput.on("focus", (_e, cx) => {
      this.searchFocused = true;
      cx.notify();
    });
    this.searchInput.on("blur", (_e, cx) => {
      this.searchFocused = false;
      cx.notify();
    });
    this.searchInput.on("change", (_e, cx) => {
      cx.notify();
    });

    if (typeof checking !== "function" || !checking()) {
      const initCtx = resolveActiveTerminalContext();
      this.ensureKnownAgents(initCtx.machine, cx);
      this.refresh(cx, false);
      cx.timer.every(15000, (cx) => this.refresh(cx, true));
    }
  }

  ensureKnownAgents(machine, cx) {
    if (typeof knownAgents !== "function") return;
    if (typeof checking === "function" && checking()) return;
    const mKey = machine ?? "";
    if (!this.knownByMachine.has(mKey)) {
      const cached = typeof shared === "function" ? shared(KNOWN_AGENTS_KEY)?.[mKey] : null;
      if (Array.isArray(cached) && cached.length > 0) {
        this.knownByMachine.set(mKey, cached);
      }
    }
    if (this.probingMachines.has(mKey)) return;
    this.probingMachines.add(mKey);
    cx?.spawn?.(async (acx) => {
      try {
        const list = await knownAgents({ on: machine ?? null });
        if (Array.isArray(list)) {
          this.knownByMachine.set(mKey, list);
          try {
            if (typeof setShared === "function" && typeof shared === "function") {
              const prev = shared(KNOWN_AGENTS_KEY);
              setShared(KNOWN_AGENTS_KEY, { ...(prev && typeof prev === "object" ? prev : {}), [mKey]: list });
            }
          } catch {}
          acx.notify();
        }
      } catch {} finally {
        this.probingMachines.delete(mKey);
      }
    });
  }

  agentCatalogFor(machine) {
    const mKey = machine ?? "";
    const raw = this.knownByMachine.get(mKey)
      ?? (typeof shared === "function" ? shared(KNOWN_AGENTS_KEY)?.[mKey] : null)
      ?? null;
    return buildAgentCatalog(raw);
  }

  resolveEffectiveAgent(ctx) {
    const catalog = this.agentCatalogFor(ctx.machine);
    const targetKey = ctx.target || `space:${ctx.cwd || ""}`;

    // If a terminal transitioned into a different live CLI agent, follow the new live agent automatically.
    const prevDet = this.lastDetectedByTarget.get(targetKey) ?? null;
    if (ctx.detectedAgent && ctx.detectedAgent !== prevDet) {
      this.lastDetectedByTarget.set(targetKey, ctx.detectedAgent);
      this.manualOverrideByTarget.delete(targetKey);
    } else if (!ctx.detectedAgent && prevDet) {
      this.lastDetectedByTarget.delete(targetKey);
    }

    const manual = this.manualOverrideByTarget.get(targetKey) ?? null;
    if (manual) return manual;

    if (ctx.detectedAgent) return ctx.detectedAgent;

    if (this.preferredAgent && catalog.some((a) => a.key === this.preferredAgent)) {
      return this.preferredAgent;
    }
    const firstInstalled = catalog.find((a) => a.installed)?.key ?? catalog[0]?.key ?? "claude";
    return firstInstalled;
  }

  chooseAgent(agentKey, ctx, cx) {
    const norm = normalizeAgentKey(agentKey);
    const targetKey = ctx.target || `space:${ctx.cwd || ""}`;
    this.agentPopupOpen = false;

    if (!norm) {
      this.manualOverrideByTarget.delete(targetKey);
    } else {
      if (ctx.detectedAgent) {
        if (norm === ctx.detectedAgent) {
          this.manualOverrideByTarget.delete(targetKey);
        } else {
          this.manualOverrideByTarget.set(targetKey, norm);
        }
      } else {
        this.manualOverrideByTarget.set(targetKey, norm);
      }
      this.preferredAgent = norm;
      writePreferredAgent(norm);
    }

    this.selectedId = null;
    this.selectedSession = null;
    this.loadedSessionKey = null;
    this.entries = [];
    this.expandedGroups = new Set();
    this.expandAllSteps = false;
    this.refresh(cx, false);
    cx?.notify?.();
  }

  computeContextKey(ctx, effectiveAgent) {
    return `sessions:${ctx.supported ? 1 : 0}:${ctx.target ?? ""}:${effectiveAgent ?? ""}:${ctx.cwd ?? ""}:${ctx.activeSessionId ?? ""}:${ctx.turn ?? 0}`;
  }

  copyResumeCommand(session, cx) {
    const cmd = String(session?.resume_command ?? "").trim();
    if (!cmd) return;
    try { cx?.write_to_clipboard?.(cmd); } catch {}
    this.copiedSessionId = session.session_id;
    this.copiedUntil = Date.now() + 1800;
    if (typeof notify === "function") {
      notify({
        title: tr("Copied resume command (full access)", "已复制恢复对话命令（Full Access）"),
        body: cmd,
        level: "info",
      });
    }
    cx?.notify?.();
  }

  openSessionMenu(session, cx) {
    if (!session) return;
    cx?.stop_propagation?.();
    this.hoveredRowId = null;
    this.agentPopupOpen = false;
    this.selectSession(session, cx);
    openOverlay({
      mod: "menu",
      kind: "popup",
      width: 300,
      props: {
        tab: "session",
        target: {
          sessionId: session.session_id,
          title: session.title,
          agent: session.agent || this.agent,
          agentName: this.agentName || session.agent || this.agent,
          resumeCommand: session.resume_command,
        },
      },
    });
  }

  selectSession(session, cx) {
    if (!session || !session.session_id) return;
    const changed = this.selectedId !== session.session_id;
    this.selectedId = session.session_id;
    this.selectedSession = session;
    if (changed) {
      this.visibleLimit = INITIAL_VISIBLE_ITEMS;
      this.expandedGroups = new Set();
      this.expandAllSteps = false;
      this.transcriptError = null;
    }
    const loadKey = `${session.agent || this.agent || ""}:${session.session_id}:${session.updated_ms || 0}`;
    if (changed || this.loadedSessionKey !== loadKey) {
      this.loadSessionTranscript(session, resolveActiveTerminalContext(), cx);
    }
    cx?.notify?.();
  }

  toggleActivityGroup(groupId, cx) {
    if (this.expandAllSteps) {
      const all = groupTranscriptItems(this.entries)
        .filter((it) => it.kind === "activity_group")
        .map((it) => it.id);
      this.expandAllSteps = false;
      this.expandedGroups = new Set(all.filter((id) => id !== groupId));
    } else if (this.expandedGroups.has(groupId)) {
      this.expandedGroups.delete(groupId);
    } else {
      this.expandedGroups.add(groupId);
    }
    cx?.notify?.();
  }

  toggleAllActivitySteps(cx) {
    if (this.expandAllSteps || this.expandedGroups.size > 0) {
      this.expandAllSteps = false;
      this.expandedGroups = new Set();
    } else {
      this.expandAllSteps = true;
    }
    cx?.notify?.();
  }

  buildListArgs(ctx, effectiveAgent) {
    const args = ["session", "ls", "--json"];
    if (ctx.target) args.push(ctx.target);
    if (ctx.cwd) args.push("--cwd", ctx.cwd);
    if (effectiveAgent) args.push("--agent", effectiveAgent);
    if (!ctx.target && ctx.machine) args.push("--on", ctx.machine);
    return args;
  }

  buildReadArgs(session, ctx) {
    const args = ["session", "read", "--json", String(session.session_id)];
    const ag = session.agent || this.agent || this.resolveEffectiveAgent(ctx);
    if (ag) args.push("--agent", String(ag));
    if (ctx.target) args.push("--target", ctx.target);
    const cwd = this.cwd || ctx.cwd;
    if (cwd) args.push("--cwd", cwd);
    if (!ctx.target && ctx.machine) args.push("--on", ctx.machine);
    return args;
  }

  refresh(cx, background = false) {
    const ctx = resolveActiveTerminalContext();
    this.ensureKnownAgents(ctx.machine, cx);
    const effectiveAgent = ctx.supported ? this.resolveEffectiveAgent(ctx) : null;
    const nextKey = this.computeContextKey(ctx, effectiveAgent);
    const keyChanged = nextKey !== this.context;
    if (background && !keyChanged && this.loadingList) return;
    this.context = nextKey;

    if (!ctx.supported || (!ctx.target && !ctx.cwd)) {
      this.reqSeq += 1;
      this.readSeq += 1;
      this.loadingList = false;
      this.loadingTranscript = false;
      this.sessions = [];
      this.selectedId = null;
      this.selectedSession = null;
      this.loadedSessionKey = null;
      this.entries = [];
      this.expandedGroups = new Set();
      this.expandAllSteps = false;
      this.agent = null;
      this.agentName = null;
      this.cwd = ctx.cwd || null;
      this.error = tr("Open a terminal or CLI agent tab to view workspace session history", "打开终端或 CLI Agent 标签页以查看当前工作区历史会话");
      cx?.notify?.();
      return;
    }

    if (keyChanged && !background) {
      this.error = null;
    }

    const seq = ++this.reqSeq;
    this.loadingList = true;
    const args = this.buildListArgs(ctx, effectiveAgent);
    cx.spawn(async (acx) => {
      try {
        const res = await h2(args);
        if (seq !== this.reqSeq) return;
        if (!res || res.code !== 0) {
          this.sessions = [];
          this.entries = [];
          this.selectedSession = null;
          this.loadedSessionKey = null;
          this.error = String(res?.stderr || res?.stdout || tr("Failed to load sessions", "加载历史会话失败")).trim();
          return;
        }
        const parsed = JSON.parse(res.stdout || "{}");
        const list = Array.isArray(parsed.sessions) ? parsed.sessions : [];
        const prevAgent = this.agent;
        this.agent = normalizeAgentKey(parsed.agent) || effectiveAgent || null;
        this.agentName = parsed.agent_name || getAgentVisual(this.agent, this.agentCatalogFor(ctx.machine)).name;
        this.cwd = parsed.cwd || ctx.cwd || null;
        this.sessions = list;
        this.error = null;

        if (prevAgent && this.agent && prevAgent !== this.agent) {
          this.selectedId = null;
          this.loadedSessionKey = null;
          this.expandedGroups = new Set();
          this.expandAllSteps = false;
        }

        let sel = this.selectedId ? list.find((s) => s?.session_id === this.selectedId) : null;
        if (!sel) {
          sel = list[0] ?? null;
          this.selectedId = sel?.session_id ?? null;
          this.visibleLimit = INITIAL_VISIBLE_ITEMS;
          this.expandedGroups = new Set();
          this.expandAllSteps = false;
        }
        this.selectedSession = sel;

        if (sel) {
          const loadKey = `${sel.agent || this.agent || ""}:${sel.session_id}:${sel.updated_ms || 0}`;
          if (this.loadedSessionKey !== loadKey) {
            await this.loadSessionTranscriptAsync(sel, ctx, acx);
          }
        } else {
          this.entries = [];
          this.loadedSessionKey = null;
          this.transcriptError = null;
        }
      } catch (e) {
        if (seq !== this.reqSeq) return;
        this.error = String(e?.message || e || tr("Failed to load sessions", "加载历史会话失败"));
      } finally {
        if (seq === this.reqSeq) {
          this.loadingList = false;
          acx.notify();
        }
      }
    });
  }

  loadSessionTranscript(session, ctx, cx) {
    if (!cx?.spawn) return;
    cx.spawn(async (acx) => {
      await this.loadSessionTranscriptAsync(session, ctx, acx);
    });
  }

  async loadSessionTranscriptAsync(session, ctx, cx) {
    const seq = ++this.readSeq;
    this.loadingTranscript = true;
    this.transcriptError = null;
    cx?.notify?.();
    const args = this.buildReadArgs(session, ctx);
    try {
      const res = await h2(args);
      if (seq !== this.readSeq) return;
      if (!res || res.code !== 0) {
        this.entries = [];
        this.transcriptError = String(res?.stderr || res?.stdout || tr("Failed to read session transcript", "读取会话内容失败")).trim();
        return;
      }
      const parsed = JSON.parse(res.stdout || "{}");
      this.entries = Array.isArray(parsed.entries) ? parsed.entries : [];
      if (parsed.session_id && this.selectedId === session.session_id) {
        this.selectedSession = {
          ...session,
          title: parsed.title || session.title,
          model: parsed.model || session.model,
          resume_command: parsed.resume_command || session.resume_command,
        };
      }
      this.loadedSessionKey = `${session.agent || this.agent || ""}:${session.session_id}:${session.updated_ms || 0}`;
      this.transcriptError = null;
    } catch (e) {
      if (seq !== this.readSeq) return;
      this.entries = [];
      this.transcriptError = String(e?.message || e || tr("Failed to read session transcript", "读取会话内容失败"));
    } finally {
      if (seq === this.readSeq) {
        this.loadingTranscript = false;
        cx?.notify?.();
      }
    }
  }

  filteredSessions() {
    const q = String(this.searchInput?.value?.() ?? "").trim().toLowerCase();
    if (!q) return this.sessions;
    return this.sessions.filter((s) => {
      const title = String(s?.title ?? "").toLowerCase();
      const preview = String(s?.preview ?? "").toLowerCase();
      const sid = String(s?.session_id ?? "").toLowerCase();
      const branch = String(s?.git_branch ?? "").toLowerCase();
      return title.includes(q) || preview.includes(q) || sid.includes(q) || branch.includes(q);
    });
  }

  renderAssistantFormatted(body, t) {
    const blocks = parseAssistantBlocks(body);
    const mono = settings()?.font_family ?? "JetBrains Mono";
    return v_flex()
      .w_full()
      .min_w(0)
      .gap(6)
      .children(
        blocks.map((b) => {
          if (b.kind === "code") {
            return v_flex()
              .w_full()
              .min_w(0)
              .rounded(7)
              .overflow_hidden()
              .bg(t.code_bg ?? t.surface ?? t.raised)
              .border_1()
              .border_color(t.border)
              .when(Boolean(b.lang), (el) =>
                el.child(
                  div()
                    .px(10)
                    .py(3)
                    .border_b(1)
                    .border_color(t.border)
                    .text_size(10)
                    .text_color(t.dim)
                    .child(b.lang),
                ),
              )
              .child(
                div()
                  .w_full()
                  .min_w(0)
                  .px(10)
                  .py(7)
                  .font_family(mono)
                  .text_size(11.5)
                  .line_height("17px")
                  .text_color(t.foreground)
                  .whitespace_normal()
                  .child(b.text || " "),
              );
          }

          const headingMatch = b.text.match(/^(#{1,4})\s+([\s\S]+)$/);
          if (headingMatch && !headingMatch[2].includes("\n")) {
            const lvl = headingMatch[1].length;
            return div()
              .w_full()
              .min_w(0)
              .pt(lvl <= 2 ? 3 : 1)
              .text_size(lvl === 1 ? 14.5 : lvl === 2 ? 13.5 : 13)
              .line_height("20px")
              .font_semibold()
              .text_color(t.foreground)
              .whitespace_normal()
              .child(cleanInlineMarkdown(headingMatch[2]));
          }

          return div()
            .w_full()
            .min_w(0)
            .text_size(13)
            .line_height("20px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(cleanInlineMarkdown(b.text));
        }),
      );
  }

  renderActivityStep(step, t) {
    if (step.kind === "thought") {
      return v_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .py(2)
        .gap(2)
        .child(
          div()
            .w_full()
            .min_w(0)
            .text_size(11.5)
            .line_height("17px")
            .text_color(t.muted)
            .whitespace_normal()
            .child(step.text),
        );
    }

    if (step.kind === "tool_call") {
      return h_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .py(1.5)
        .gap(6)
        .items_center()
        .child(div().flex_none().text_size(10).text_color(t.dim).child("·"))
        .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.muted).child(step.title));
    }

    if (step.kind === "plan") {
      return h_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .py(1.5)
        .gap(6)
        .items_center()
        .child(div().flex_none().text_size(10).text_color(t.dim).child("☰"))
        .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.muted).child(step.summary));
    }

    return null;
  }

  renderGroupedItem(item, t, activeVisual) {
    // Minimal, feather-light inline disclosure for thinking & tool calls (collapsed by default)
    if (item.kind === "activity_group") {
      const isOpen = this.expandAllSteps || this.expandedGroups.has(item.id);
      const summaryLabel = formatActivityGroupSummary(item);
      return v_flex()
        .id(`sess-act-${item.id}`)
        .w_full()
        .min_w(0)
        .gap(4)
        .child(
          h_flex()
            .id(`sess-act-toggle-${item.id}`)
            .min_w(0)
            .h(22)
            .px(4)
            .gap(5)
            .items_center()
            .rounded(5)
            .cursor_pointer()
            .text_color(t.dim)
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => this.toggleActivityGroup(item.id, cx))
            .child(div().flex_none().text_size(10).child(isOpen ? "▾" : "▸"))
            .child(div().min_w(0).truncate().text_size(11.5).child(summaryLabel)),
        )
        .when(isOpen, (el) =>
          el.child(
            v_flex()
              .id(`sess-act-body-${item.id}`)
              .w_full()
              .min_w(0)
              .ml(8)
              .pl(10)
              .py(2)
              .gap(3)
              .border_l(1)
              .border_color(t.border)
              .children(item.steps.map((st) => this.renderActivityStep(st, t)).filter(Boolean)),
          ),
        );
    }

    if (item.kind === "user") {
      const clock = formatEntryClock(item.entry?.ts_ms);
      return v_flex()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(12)
        .py(9)
        .gap(4)
        .rounded(10)
        .bg(t.message ?? t.raised)
        .child(
          h_flex()
            .w_full()
            .gap(6)
            .items_center()
            .child(
              div()
                .text_size(11)
                .font_medium()
                .text_color(t.muted)
                .child(tr("You", "提问")),
            )
            .child(div().flex_1())
            .when(Boolean(clock), (el) => el.child(div().text_size(10.5).text_color(t.dim).child(clock))),
        )
        .child(
          div()
            .w_full()
            .min_w(0)
            .text_size(13)
            .line_height("20px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(item.body),
        );
    }

    if (item.kind === "assistant") {
      const clock = formatEntryClock(item.entry?.ts_ms);
      return v_flex()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(4)
        .py(2)
        .gap(5)
        .child(
          h_flex()
            .w_full()
            .gap(6)
            .items_center()
            .child(renderAgentIcon(activeVisual, 13, t))
            .child(
              div()
                .text_size(11.5)
                .font_medium()
                .text_color(t.muted)
                .child(this.agentName || activeVisual.name),
            )
            .child(div().flex_1())
            .when(Boolean(clock), (el) => el.child(div().text_size(10.5).text_color(t.dim).child(clock))),
        )
        .child(this.renderAssistantFormatted(item.body, t));
    }

    if (item.kind === "error") {
      return div()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(10)
        .py(6)
        .rounded(7)
        .bg(t.raised)
        .text_size(12)
        .text_color(t.ansi1 ?? t.foreground)
        .whitespace_normal()
        .child(item.msg);
    }

    return null;
  }

  renderAgentPickerPopup(liveCtx, effectiveAgent, catalog, t) {
    const activeVisual = getAgentVisual(effectiveAgent, catalog);
    const targetKey = liveCtx.target || `space:${liveCtx.cwd || ""}`;
    const hasManualOverride = this.manualOverrideByTarget.has(targetKey);

    const trigger = h_flex()
      .id("sessions-agent-trigger")
      .h(26)
      .px(8)
      .gap(6)
      .flex_none()
      .items_center()
      .rounded(6)
      .bg(this.agentPopupOpen ? (t.selected ?? t.raised) : (t.surface ?? t.background))
      .border_1()
      .border_color(this.agentPopupOpen ? (t.accent ?? t.border) : t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        this.agentPopupOpen = !this.agentPopupOpen;
        cx.notify();
      })
      .child(renderAgentIcon(activeVisual, 14, t))
      .child(
        div()
          .text_size(12)
          .font_semibold()
          .text_color(t.foreground)
          .child(activeVisual.name),
      )
      .child(div().text_size(10).text_color(t.dim).child("▾"));

    return Popup.new("sessions-agent-popup", trigger)
      .anchor("bottom_left")
      .when(this.agentPopupOpen, (el) =>
        el.content(
          v_flex()
            .id("sessions-agent-menu")
            .w(216)
            .max_h(280)
            .p(4)
            .gap(2)
            .rounded(8)
            .bg(t.popover ?? t.raised)
            .border_1()
            .border_color(t.border)
            .overflow_y_scroll()
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .when(Boolean(liveCtx.detectedAgent && hasManualOverride), (menu) => {
              const detVis = getAgentVisual(liveCtx.detectedAgent, catalog);
              return menu
                .child(
                  h_flex()
                    .id("sessions-agent-opt-auto")
                    .h(29)
                    .px(8)
                    .gap(8)
                    .items_center()
                    .rounded(6)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover))
                    .on_click((_e, cx) => this.chooseAgent(null, liveCtx, cx))
                    .child(renderAgentIcon(detVis, 14, t))
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .truncate()
                        .text_size(12)
                        .text_color(t.foreground)
                        .child(tr(`Follow tab (${detVis.name})`, `跟随当前标签页 (${detVis.name})`)),
                    ),
                )
                .child(div().h(1).mx(4).my(2).bg(t.border));
            })
            .children(
              catalog.map((item) => {
                const chosen = item.key === effectiveAgent;
                return h_flex()
                  .id(`sessions-agent-opt-${item.key}`)
                  .h(29)
                  .px(8)
                  .gap(8)
                  .items_center()
                  .rounded(6)
                  .cursor_pointer()
                  .when(chosen, (r) => r.bg(t.selected))
                  .hover((s) => s.bg(chosen ? t.selected : t.hover))
                  .on_click((_e, cx) => this.chooseAgent(item.key, liveCtx, cx))
                  .child(renderAgentIcon(item, 14, t))
                  .child(
                    div()
                      .flex_1()
                      .min_w(0)
                      .truncate()
                      .text_size(12)
                      .text_color(t.foreground)
                      .child(item.name),
                  )
                  .when(chosen, (r) =>
                    r.child(div().text_size(11).text_color(t.accent ?? t.foreground).child("✓")),
                  );
              }),
            ),
        ),
      );
  }

  renderTerminalQuickAgentBar(liveCtx, effectiveAgent, catalog, t) {
    // Quick 1-click CLI agent switcher shown when the user is on a plain Terminal tab.
    const quickChoices = catalog.filter((a) => a.installed);
    const pills = (quickChoices.length > 0 ? quickChoices : catalog).slice(0, 6);
    if (pills.length === 0) return null;

    return h_flex()
      .id("sessions-terminal-agent-bar")
      .flex_none()
      .w_full()
      .min_w(0)
      .px(10)
      .py(6)
      .gap(5)
      .flex_wrap()
      .items_center()
      .bg(t.surface ?? t.background)
      .border_b(1)
      .border_color(t.border)
      .child(
        div()
          .flex_none()
          .text_size(11)
          .text_color(t.dim)
          .mr(2)
          .child(tr("Agent:", "选择 Agent：")),
      )
      .children(
        pills.map((item) => {
          const active = item.key === effectiveAgent;
          return h_flex()
            .id(`sessions-agent-pill-${item.key}`)
            .h(24)
            .px(8)
            .gap(5)
            .flex_none()
            .items_center()
            .rounded(6)
            .cursor_pointer()
            .bg(active ? (t.selected ?? t.raised) : t.background)
            .border_1()
            .border_color(active ? (t.accent ?? t.border) : t.border)
            .text_color(active ? t.foreground : t.muted)
            .hover((s) => s.bg(active ? (t.selected ?? t.raised) : t.hover).text_color(t.foreground))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => this.chooseAgent(item.key, liveCtx, cx))
            .child(renderAgentIcon(item, 12, t))
            .child(
              div()
                .text_size(11.5)
                .when(active, (e) => e.font_medium())
                .child(item.name),
            );
        }),
      );
  }

  render(cx) {
    const liveCtx = resolveActiveTerminalContext();
    const effectiveAgent = liveCtx.supported ? this.resolveEffectiveAgent(liveCtx) : null;
    if (this.computeContextKey(liveCtx, effectiveAgent) !== this.context && (typeof checking !== "function" || !checking())) {
      this.refresh(cx, false);
    }

    const t = theme();
    const accent = t.accent ?? "#3b82f6";
    const catalog = this.agentCatalogFor(liveCtx.machine);
    const activeVisual = getAgentVisual(this.agent || effectiveAgent, catalog);
    const filtered = this.filteredSessions();
    const query = String(this.searchInput?.value?.() ?? "");
    const wsFolder = this.cwd || liveCtx.cwd || "";
    const wsName = folderBasename(wsFolder) || tr("Workspace", "当前目录");

    const groupedItems = groupTranscriptItems(this.entries);
    const activityGroupCount = groupedItems.reduce((n, it) => n + (it.kind === "activity_group" ? 1 : 0), 0);
    const anyStepsOpen = this.expandAllSteps || this.expandedGroups.size > 0;

    const totalItems = groupedItems.length;
    const hiddenCount = Math.max(0, totalItems - this.visibleLimit);
    const visibleItems = totalItems > 0 ? groupedItems.slice(hiddenCount) : [];
    const renderedEntries = [];
    for (let i = 0; i < visibleItems.length; i++) {
      const node = this.renderGroupedItem(visibleItems[i], t, activeVisual);
      if (node) renderedEntries.push(node);
    }

    const selected = this.selectedSession
      || (this.selectedId ? this.sessions.find((s) => s?.session_id === this.selectedId) : null)
      || null;

    const justCopied = Boolean(selected && this.copiedSessionId === selected.session_id && Date.now() < this.copiedUntil);

    // Upper pane: clean, breathable session list (no floating tooltips -> zero overlap with right-click menu)
    const sessionListPane = div()
      .id("sessions-list-pane")
      .size_full()
      .min_h(0)
      .relative()
      .on_mouse_down("left", () => {
        if (this.agentPopupOpen) this.agentPopupOpen = false;
      })
      .child(
        v_flex()
          .id("sessions-list")
          .absolute()
          .inset_0()
          .overflow_y_scroll()
          .px(8)
          .py(6)
          .gap(3)
          .children(
            filtered.map((s, idx) => {
              const isSel = selected?.session_id === s.session_id;
              const isHovered = this.hoveredRowId === s.session_id;
              const relTime = formatFriendlyTime(s.updated_ms || s.created_ms);
              const turns = Number(s.turns) || 0;
              const title = formatCleanSessionTitle(s.title || s.session_id, 110);

              const subMeta = [];
              if (turns > 0) subMeta.push(tr(`${turns} turn${turns === 1 ? "" : "s"}`, `${turns} 轮对话`));
              if (s.git_branch) subMeta.push(s.git_branch);

              return v_flex()
                .id(`session-row-${idx}`)
                .flex_none()
                .w_full()
                .min_w(0)
                .px(10)
                .py(7)
                .gap(3)
                .rounded(8)
                .overflow_hidden()
                .cursor_pointer()
                .when(isSel, (e) => e.bg(t.selected))
                .hover((st) => st.bg(isSel ? t.selected : t.hover))
                .on_hover((hovered, cx) => {
                  const next = hovered ? s.session_id : (this.hoveredRowId === s.session_id ? null : this.hoveredRowId);
                  if (next !== this.hoveredRowId) {
                    this.hoveredRowId = next;
                    cx.notify();
                  }
                })
                .on_click((_e, cx) => {
                  this.agentPopupOpen = false;
                  this.selectSession(s, cx);
                })
                .on_mouse_down("right", (_e, cx) => this.openSessionMenu(s, cx))
                .child(
                  h_flex()
                    .w_full()
                    .min_w(0)
                    .gap(7)
                    .items_center()
                    .when(Boolean(s.current), (e) =>
                      e.child(
                        div()
                          .size(6)
                          .flex_none()
                          .rounded(999)
                          .bg(t.ansi2 ?? accent),
                      ),
                    )
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .truncate()
                        .text_size(12.5)
                        .line_height("18px")
                        .when(isSel, (e) => e.font_semibold())
                        .when(!isSel, (e) => e.font_medium())
                        .text_color(t.foreground)
                        .child(title),
                    )
                    .when(Boolean(s.current), (e) =>
                      e.child(
                        div()
                          .flex_none()
                          .whitespace_nowrap()
                          .text_size(10.5)
                          .text_color(t.ansi2 ?? accent)
                          .child(tr("Current", "当前")),
                      ),
                    )
                    .when(Boolean(relTime), (e) =>
                      e.child(
                        div()
                          .flex_none()
                          .whitespace_nowrap()
                          .text_size(11)
                          .text_color(t.dim)
                          .child(relTime),
                      ),
                    ),
                )
                .when(subMeta.length > 0 || isSel || isHovered, (el) =>
                  el.child(
                    h_flex()
                      .w_full()
                      .min_w(0)
                      .gap(6)
                      .items_center()
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .truncate()
                          .text_size(11)
                          .text_color(t.dim)
                          .child(subMeta.join(" · ") || shortNameFallback(s.session_id)),
                      )
                      .when(isSel || isHovered, (r) =>
                        r.child(
                          div()
                            .flex_none()
                            .whitespace_nowrap()
                            .text_size(10.5)
                            .text_color(t.muted)
                            .child(tr("Right-click to copy command", "右键复制恢复命令")),
                        ),
                      ),
                  ),
                );
            }),
          )
          .when(filtered.length === 0, (el) =>
            el.child(
              v_flex()
                .w_full()
                .px(12)
                .py(20)
                .gap(4)
                .items_center()
                .justify_center()
                .child(
                  div()
                    .text_size(12.5)
                    .text_color(t.muted)
                    .whitespace_normal()
                    .child(
                      this.error
                        || (this.loadingList
                          ? tr("Loading session history…", "正在读取历史会话…")
                          : query.length > 0
                            ? tr("No matching sessions", "没有匹配的历史会话")
                            : tr(
                                `No ${activeVisual.name} sessions in ${wsName}`,
                                `当前目录暂无 ${activeVisual.name} 的历史会话`,
                              )),
                    ),
                )
                .when(!this.loadingList && !this.error && query.length === 0 && liveCtx.isPlainTerminal, (sub) =>
                  sub.child(
                    div()
                      .text_size(11)
                      .text_color(t.dim)
                      .child(tr("Switch agent above to inspect another CLI agent", "可在上方切换其他 CLI Agent 查看历史会话")),
                  ),
                ),
            ),
          ),
      )
      .child(Scrollbar.vertical("sessions-list").absolute().inset_0());

    // Lower pane: clean, editorial conversation detail
    const transcriptDetailPane = v_flex()
      .id("sessions-detail-pane")
      .size_full()
      .min_w(0)
      .min_h(0)
      .on_mouse_down("left", () => {
        if (this.agentPopupOpen) this.agentPopupOpen = false;
      })
      .child(
        h_flex()
          .id("sessions-detail-toolbar")
          .h(36)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(12)
          .gap(8)
          .items_center()
          .bg(t.surface ?? t.raised)
          .border_b(1)
          .border_color(t.border)
          .when(Boolean(selected), (el) =>
            el.on_mouse_down("right", (_e, cx) => this.openSessionMenu(selected, cx)),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(12)
              .font_medium()
              .text_color(selected ? t.foreground : t.muted)
              .child(
                selected
                  ? formatCleanSessionTitle(selected.title || selected.session_id, 90)
                  : tr("Conversation", "对话内容"),
              ),
          )
          .when(activityGroupCount > 0, (el) =>
            el.child(
              div()
                .id("sessions-toggle-steps")
                .flex_none()
                .px(7)
                .py(2)
                .rounded(5)
                .text_size(11)
                .text_color(anyStepsOpen ? t.foreground : t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => this.toggleAllActivitySteps(cx))
                .child(
                  anyStepsOpen
                    ? tr("Hide steps", "折叠过程")
                    : tr(`Show steps (${activityGroupCount})`, `展开过程 (${activityGroupCount})`),
                ),
            ),
          )
          .when(Boolean(selected?.resume_command), (el) =>
            el.child(
              h_flex()
                .id("sessions-copy-cmd")
                .flex_none()
                .h(24)
                .px(8)
                .gap(4)
                .items_center()
                .rounded(6)
                .bg(justCopied ? (t.selected ?? t.raised) : t.background)
                .border_1()
                .border_color(t.border)
                .text_size(11)
                .text_color(justCopied ? (t.ansi2 ?? accent) : t.foreground)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => this.copyResumeCommand(selected, cx))
                .child(svg("mods/peek/copy.svg").size(11).text_color(justCopied ? (t.ansi2 ?? accent) : t.muted))
                .child(justCopied ? tr("Copied", "已复制") : tr("Copy resume cmd", "复制恢复命令")),
            ),
          ),
      )
      .child(
        div()
          .flex_1()
          .w_full()
          .min_w(0)
          .min_h(0)
          .relative()
          .overflow_hidden()
          .when(Boolean(selected), (el) =>
            el.on_mouse_down("right", (_e, cx) => this.openSessionMenu(selected, cx)),
          )
          .child(
            v_flex()
              .id("sessions-transcript")
              .absolute()
              .inset_0()
              .overflow_y_scroll()
              .px(14)
              .py(12)
              .gap(12)
              .when(hiddenCount > 0, (el) =>
                el.child(
                  div()
                    .id("sessions-load-earlier")
                    .w_full()
                    .py(5)
                    .px(10)
                    .rounded(6)
                    .bg(t.surface ?? t.raised)
                    .text_size(11.5)
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => {
                      this.visibleLimit += VISIBLE_ITEMS_STEP;
                      cx.notify();
                    })
                    .child(
                      tr(
                        `Show earlier messages (${hiddenCount} more)`,
                        `显示更早的对话（还有 ${hiddenCount} 项）`,
                      ),
                    ),
                ),
              )
              .children(renderedEntries)
              .when(renderedEntries.length === 0, (el) =>
                el.child(
                  div()
                    .py(16)
                    .text_size(12.5)
                    .text_color(t.dim)
                    .whitespace_normal()
                    .child(
                      this.transcriptError
                        || (this.loadingTranscript
                          ? tr("Loading conversation…", "正在加载对话内容…")
                          : selected
                            ? tr("No text messages recorded in this session", "该会话暂无可展示的对话消息")
                            : tr("Select a session above to read its conversation", "在上方选择一条历史会话查看对话记录")),
                    ),
                ),
              ),
          )
          .child(Scrollbar.vertical("sessions-transcript").absolute().inset_0()),
      );

    const terminalAgentBar = liveCtx.supported && liveCtx.isPlainTerminal
      ? this.renderTerminalQuickAgentBar(liveCtx, effectiveAgent, catalog, t)
      : null;

    return v_flex()
      .id("sessions")
      .size_full()
      .min_h(0)
      .rounded(10)
      .overflow_hidden()
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .text_color(t.foreground)
      .text_size(13)
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape" && this.agentPopupOpen) {
          this.agentPopupOpen = false;
          cx.stop_propagation();
          cx.notify();
        }
      })
      // Unified single top bar: Agent selector + workspace folder + compact search + refresh
      .child(
        h_flex()
          .id("sessions-header")
          .h(42)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(10)
          .gap(8)
          .items_center()
          .bg(t.raised)
          .border_b(1)
          .border_color(t.border)
          .when(liveCtx.supported, (el) =>
            el.child(this.renderAgentPickerPopup(liveCtx, effectiveAgent, catalog, t)),
          )
          .child(
            div()
              .flex_1()
              .min_w(40)
              .truncate()
              .text_size(12)
              .text_color(t.muted)
              .tooltip(wsFolder || wsName)
              .child(wsName),
          )
          .child(
            h_flex()
              .id("sessions-search-wrap")
              .w(156)
              .flex_none()
              .h(26)
              .px(8)
              .gap(4)
              .items_center()
              .rounded(6)
              .bg(t.background)
              .border_1()
              .border_color(this.searchFocused ? accent : t.border)
              .cursor_text()
              .on_mouse_down("left", (_e, cx) => {
                cx.stop_propagation();
                if (this.agentPopupOpen) this.agentPopupOpen = false;
              })
              .child(Input.new(this.searchInput).flex_1().min_w(0).h_full().text_size(11.5).text_color(t.foreground))
              .when(query.length > 0, (el) =>
                el.child(
                  div()
                    .id("sessions-search-clear")
                    .flex_none()
                    .px(3)
                    .rounded(4)
                    .text_size(11)
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => {
                      this.searchInput.set_value("");
                      cx.notify();
                    })
                    .child("×"),
                ),
              ),
          )
          .child(
            div()
              .id("sessions-refresh")
              .size(26)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .rounded(6)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(tr("Refresh", "刷新"))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click((_e, cx) => {
                this.agentPopupOpen = false;
                this.refresh(cx, false);
              })
              .child(
                svg(this.loadingList ? "mods/workspace-chrome/spinner.svg" : "frame/settings/refresh.svg")
                  .size(13)
                  .when(this.loadingList, (s) => s.spin(900)),
              ),
          ),
      )
      // When in a plain Terminal tab, show prominent 1-click CLI Agent pills
      .when(Boolean(terminalAgentBar), (el) => el.child(terminalAgentBar))
      // Vertically resizable split between session list (top) and conversation detail (bottom)
      .child(
        v_resizable("sessions-split")
          .flex_1()
          .min_h(0)
          .w_full()
          .on_resize((sizes) => {
            if (Array.isArray(sizes) && typeof sizes[0] === "number") {
              const clamped = Math.max(MIN_SPLIT_TOP_HEIGHT, Math.min(MAX_SPLIT_TOP_HEIGHT, Math.round(sizes[0])));
              this.splitTopHeight = clamped;
              try {
                if (typeof localStorage !== "undefined" && localStorage?.setItem) {
                  localStorage.setItem("sessions.topHeight", String(clamped));
                }
              } catch {}
            }
          })
          .child(
            resizable_panel()
              .size(this.splitTopHeight)
              .size_range(MIN_SPLIT_TOP_HEIGHT, MAX_SPLIT_TOP_HEIGHT)
              .child(sessionListPane),
          )
          .child(
            resizable_panel()
              .size_range(140)
              .child(transcriptDetailPane),
          ),
      );
  }
}

function shortNameFallback(sid) {
  const s = String(sid ?? "").trim();
  if (s.length <= 12) return s;
  return `${s.slice(0, 8)}…`;
}
