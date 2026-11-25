// The bar on top of a peeked agent (or, for headless ACP agents, a T3 Code-style
// structured conversation, work-log, approval, and composer dialog).
// Props: { terminal, overlay, acp }.

import { View, div, image, svg } from "gpui-kit";
import { h_flex, v_flex, Popup, TextareaState, Textarea, InputState, Input, TextView, ScrollHandle } from "gpui-base";
import { agents, terminals, panes, spaces, focused, run, theme, settings, home, props, closeOverlay, openOverlay, checking, h2, shared, setShared, nextCommand, keys, window as appWindow, readImageDataUrl, pasteClipboardAttachments, prepareAttachments, pickAttachments } from "herdr";
import { buildConversationItems, buildPtySessionBadges, summarizeActivityGroup, toolIconPath, formatTime, shortenPath, normalizeConfigOptions, filterConfigChoices, normalizeInboxItems, parseToolDetail, resolveEffortVisual, resolvePeerAddress, isImageAttachment, formatAttachmentBytes, attachmentDisplayName, extractTurnFilePatch, resolveChangedFilePath, isAbsoluteFilePath } from "./acp_model.js";

function shortcutFor(action) {
  if (typeof keys !== "function" || !action) return "";
  const list = keys();
  if (!Array.isArray(list)) return "";
  const k = list.find((b) => b.section === "global" && b.action === action);
  if (!k?.keys?.trim()) return "";
  const mac = typeof appWindow === "function" && appWindow()?.platform === "macos";
  const names = mac ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" } : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: mac ? "cmd" : "ctrl", super: "cmd", win: "cmd" };
  return k.keys.trim().split(/\s+/).map((stroke) => {
    const parts = stroke.split("-");
    let key = parts.pop();
    if (key === "") { key = "-"; parts.pop(); }
    const held = new Set(parts.map((m) => alias[m.toLowerCase()] ?? m.toLowerCase()));
    if (/^[A-Z]$/.test(key)) held.add("shift");
    const label = key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + key.slice(1).toLowerCase();
    return [...Object.keys(names).filter((m) => held.has(m)).map((m) => names[m]), label].join(mac ? "" : "+");
  }).join(" ");
}

function withShortcut(text, action) {
  const sc = shortcutFor(action);
  return sc ? `${text} (${sc})` : text;
}

const LANE_MAX_W = 736; // 46rem editorial content column (--chat-content-max-width)
const SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const DEFAULT_VISIBLE_ITEMS = 30;
const VISIBLE_ITEMS_STEP = 30;
const DEFAULT_VISIBLE_STEPS = 24;
const LINKIFY_CACHE_MAX = 256;
const LINKIFY_CACHE = new Map();
let _frameSettings = null;
const readSettings = () => _frameSettings ?? (typeof settings === "function" ? settings() : null);
const isZh = () => String(readSettings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const KNOWN_ROOT_FILE_RE = /^(?:Makefile|Dockerfile|Cargo\.(?:toml|lock)|package(?:-lock)?\.json|tsconfig\.json|README\.md|CLAUDE\.md|AGENTS\.md|SKILL\.md|justfile|\.gitignore|\.env(?:\.\w+)?)$/i;

function splitFileLineSuffix(raw) {
  let s = String(raw ?? "").trim();
  if (!s) return null;
  let line = null;
  let endLine = null;
  const hashMatch = s.match(/#L(\d+)(?:-L?(\d+))?$/i);
  if (hashMatch) {
    s = s.slice(0, hashMatch.index);
    line = Number(hashMatch[1]) || null;
    endLine = hashMatch[2] ? Math.max(line ?? 1, Number(hashMatch[2]) || 0) : null;
  } else {
    const rangeMatch = s.match(/:(\d+)-(\d+)$/);
    if (rangeMatch) {
      s = s.slice(0, rangeMatch.index);
      line = Number(rangeMatch[1]) || null;
      endLine = Math.max(line ?? 1, Number(rangeMatch[2]) || 0);
    } else {
      const colMatch = s.match(/:(\d+):(\d+)$/);
      if (colMatch) {
        s = s.slice(0, colMatch.index);
        line = Number(colMatch[1]) || null;
      } else {
        const lineMatch = s.match(/:(\d+)$/);
        if (lineMatch) {
          s = s.slice(0, lineMatch.index);
          line = Number(lineMatch[1]) || null;
        }
      }
    }
  }
  return { path: s, line: line && line > 0 ? line : null, endLine: endLine && endLine > 0 ? endLine : null };
}

function looksLikeFilePath(raw) {
  const parsed = splitFileLineSuffix(raw);
  if (!parsed || !parsed.path) return false;
  const p = parsed.path;
  if (p.length > 260 || /[\s<>"'`|;&=(){}[\]*?]/.test(p)) return false;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(p)) return false;
  if (p.startsWith("//") || p === "/" || p === "./" || p === "../" || p === "~/") return false;
  if (/^[A-Za-z]:[\\/]/.test(p)) {
    return p.length > 3;
  }
  if (/^(?:\/|~\/|\.\/|\.\.\/)/.test(p)) {
    if (p.startsWith("/")) {
      const segs = p.split("/").filter(Boolean);
      const last = segs[segs.length - 1] ?? "";
      return segs.length >= 2 && (last.includes(".") || segs.length >= 3);
    }
    return p.length > 2;
  }
  if (p.includes("/") || p.includes("\\")) {
    const segs = p.split(/[\\/]/).filter(Boolean);
    if (segs.length < 2) return false;
    const last = segs[segs.length - 1];
    return /\.[a-zA-Z0-9]{1,10}$/.test(last) || KNOWN_ROOT_FILE_RE.test(last);
  }
  return KNOWN_ROOT_FILE_RE.test(p) || (Boolean(parsed.line) && /^[A-Za-z0-9_.-]+\.[a-zA-Z0-9]{1,8}$/.test(p));
}

function linkifyFilePaths(text) {
  if (!text || typeof text !== "string") return "";
  if (text.length > 65536) return text;
  const cached = LINKIFY_CACHE.get(text);
  if (cached !== undefined) return cached;
  const lines = text.split("\n");
  let inFence = false;
  let fenceChar = "";
  const tokenRe = /(!?\[[^\]]*\]\([^)]+\)|`[^`\n]+`|https?:\/\/[^\s)<>]+|(?:~\/|\.\.?\/|\/(?:[A-Za-z0-9._-]+\/)+|[A-Za-z0-9._-]+\/(?:[A-Za-z0-9._-]+\/)*)[A-Za-z0-9._-]+(?::\d+(?:-\d+|:\d+)?|#L\d+(?:-L?\d+)?)?)/g;
  const out = lines
    .map((line) => {
      const trimmed = line.trimStart();
      if (/^(`{3,}|~{3,})/.test(trimmed)) {
        const ch = trimmed[0];
        if (!inFence) {
          inFence = true;
          fenceChar = ch;
        } else if (ch === fenceChar) {
          inFence = false;
          fenceChar = "";
        }
        return line;
      }
      if (inFence) return line;
      return line.replace(tokenRe, (match, _g, offset, full) => {
        if (match.startsWith("[") || match.startsWith("![") || /^https?:\/\//i.test(match)) {
          return match;
        }
        if (match.startsWith("`") && match.endsWith("`")) {
          const inner = match.slice(1, -1).trim();
          if (!looksLikeFilePath(inner)) return match;
          return `[\`${inner}\`](herdr-file://${encodeURI(inner)})`;
        }
        // Avoid matching mid-identifier or after URL schemes like `foo://bar/baz.rs`.
        const prev = offset > 0 ? full[offset - 1] : "";
        if (prev && /[\w:@]/.test(prev)) return match;
        const clean = match.replace(/[.,;:]+$/, "");
        const trailing = match.slice(clean.length);
        if (!looksLikeFilePath(clean)) return match;
        return `[${clean}](herdr-file://${encodeURI(clean)})${trailing}`;
      });
    })
    .join("\n");
  if (LINKIFY_CACHE.size >= LINKIFY_CACHE_MAX) {
    LINKIFY_CACHE.clear();
  }
  LINKIFY_CACHE.set(text, out);
  return out;
}

function parseClickableFileLink(rawUrl) {
  const url = String(rawUrl ?? "").trim();
  if (!url || /^https?:\/\//i.test(url) || /^mailto:/i.test(url)) return null;
  let decoded = url;
  if (url.startsWith("herdr-file://")) {
    const rest = url.slice("herdr-file://".length);
    try { decoded = decodeURI(rest); } catch { decoded = rest; }
  } else if (url.startsWith("file://")) {
    let rest = url.slice("file://".length);
    if (rest.startsWith("localhost/")) rest = rest.slice("localhost".length);
    try { decoded = decodeURI(rest); } catch { decoded = rest; }
    if (/^\/[A-Za-z]:\//.test(decoded)) decoded = decoded.slice(1);
  } else if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(url)) {
    return null;
  } else {
    try { decoded = decodeURI(url); } catch { decoded = url; }
  }
  const parsed = splitFileLineSuffix(decoded);
  if (!parsed || !parsed.path) return null;
  return parsed;
}
const statusWords = () => ({
  starting: tr("Starting…", "启动中…"),
  working: tr("Working", "运行中"),
  blocked: tr("Needs approval", "待审批"),
  done: tr("Finished", "已完成"),
  ended: tr("Ended", "已结束"),
  idle: tr("Idle", "空闲"),
});

const ICONS = {
  bot: "frame/welcome/bot.svg",
  arrowRight: "frame/welcome/arrow-right.svg",
  arrowUp: "mods/peek/arrow-up.svg",
  brain: "mods/peek/brain.svg",
  circleAlert: "mods/peek/circle-alert.svg",
  copy: "mods/peek/copy.svg",
  cornerUpRight: "mods/peek/corner-up-right.svg",
  fileDiff: "mods/peek/file-diff.svg",
  globe: "mods/peek/globe.svg",
  lightbulb: "mods/peek/lightbulb.svg",
  listPlus: "mods/peek/list-plus.svg",
  listTodo: "mods/peek/list-todo.svg",
  messagesSquare: "mods/peek/messages-square.svg",
  paperclip: "mods/peek/paperclip.svg",
  sparkles: "mods/peek/sparkles.svg",
  wrapText: "mods/peek/wrap-text.svg",
  wrench: "mods/peek/wrench.svg",
  zap: "mods/peek/zap.svg",
  refresh: "frame/settings/refresh.svg",
  shield: "frame/settings/shield.svg",
  fileText: "mods/workspace-chrome/file-text.svg",
  folder: "mods/workspace-chrome/folder.svg",
  gitBranch: "mods/workspace-chrome/git-branch.svg",
  search: "mods/workspace-chrome/search.svg",
  server: "mods/workspace-chrome/server.svg",
  check: "mods/workspace-chrome/check.svg",
  circleCheck: "mods/workspace-chrome/circle-check.svg",
  chevronDown: "mods/workspace-chrome/chevron-down.svg",
  chevronUp: "mods/peek/chevron-up.svg",
  chevronRight: "mods/workspace-chrome/chevron-right.svg",
  layers: "mods/workspace-chrome/layers.svg",
  spinner: "mods/workspace-chrome/spinner.svg",
  square: "mods/workspace-chrome/square.svg",
  squarePen: "mods/workspace-chrome/square-pen.svg",
  terminal: "mods/workspace-chrome/terminal.svg",
  x: "mods/workspace-chrome/x.svg",
};

// The icon from the app's program table: a one-color glyph or a picture.
function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

function withAlpha(color, alphaHex) {
  if (typeof color !== "string") return color;
  if (/^#[0-9a-fA-F]{6}$/.test(color)) return `${color}${alphaHex}`;
  if (/^#[0-9a-fA-F]{8}$/.test(color)) return `${color.slice(0, 7)}${alphaHex}`;
  return color;
}

function stopMouse(_e, cx) {
  cx.stop_propagation();
}

function stopScroll(_e, cx) {
  cx.stop_propagation();
}

function preserveIndentForWrap(line) {
  if (!line) return " ";
  return String(line).replace(/\t/g, "  ").replace(/^( +)/, (m) => "\u00a0".repeat(m.length));
}

function fenceFor(code) {
  let max = 2;
  const re = /`+/g;
  const src = String(code ?? "");
  let m;
  while ((m = re.exec(src)) !== null) {
    if (m[0].length > max) max = m[0].length;
  }
  return "`".repeat(max + 1);
}

export default class Peek extends View {
  init(_props, cx) {
    this.cx = cx;
    this.boundProps = props() ?? _props ?? {};
    if (!this.boundProps?.acp && !this.boundProps?.sidePanel) return;
    this.loadedTerminal = this.activeTerminal();
    this.entries = [];
    this.fallbackText = "";
    this.readHow = null;
    this.sessionMeta = null;
    this.loadedOnce = false;
    this.error = null;
    this.busyAction = false;
    this.interrupting = false;
    this.loading = false;
    this.reloadQueued = false;
    this.reloadForceDetail = false;
    this.reloadEpoch = 0;
    this.lastCkptEpoch = "";
    this.lastReadStdout = "";
    this._itemsCache = null;
    this.visibleItemLimit = DEFAULT_VISIBLE_ITEMS;
    this.expandedStepLimits = {};
    this.ptyPollTick = 0;
    this._frame = null;
    this.expandedGroups = {};
    this.expandedSteps = {};
    this.wrappedCodes = {};
    this.hoveredRows = {};
    this.workingStartedAt = null;
    this.copiedId = null;
    this.autoFollow = true;
    this.revealedSig = "";
    this.lastKey = "";
    this.inputFocused = false;
    this.agentDetail = null;
    this.lastDetailTerminal = "";
    this.lastDetailInboxLen = 0;
    this.optimisticInbox = null;
    this.editingInboxId = null;
    this.inboxBusy = false;
    this.queueCollapsed = false;
    this.inboxEditFocused = false;
    this.inboxEditInput = TextareaState.new({
      value: "",
      placeholder: tr("Edit queued message…", "修改队列中的消息…"),
    });
    this.inboxEditInput.set_auto_grow(2, 6);
    this.inboxEditInput.on("focus", (_e, cx) => {
      this.inboxEditFocused = true;
      if (this.openConfigId) this.closeConfigPopover(cx);
      cx.notify();
    });
    this.inboxEditInput.on("blur", (_e, cx) => {
      this.inboxEditFocused = false;
      cx.notify();
    });
    this.inboxEditInput.on("change", (_e, cx) => {
      cx.notify();
    });
    this.inboxEditInput.on("submit", (e, cx) => {
      if (e?.shift) return;
      this.saveEditInboxItem(cx);
    });
    this.checkpoints = [];
    this.isolation = null;
    this.checkpointDiffs = {};
    this.checkpointDiffOpen = {};
    this.checkpointDiffLoading = {};
    this.rollbackPromptItem = null;
    this.rollbackBusy = false;
    this.rollbackError = null;
    this.maxConversationTurn = 0;
    this.openConfigId = null;
    this.enteringConfigId = null;
    this.closingConfigs = {};
    this.enterTimer = null;
    this.closeTimers = {};
    this.optimisticConfig = {};
    this.highlightedChoiceIdx = 0;
    this.configFilterFocused = false;
    this.scroll = ScrollHandle.new();
    this.menuFocus = cx.focus_handle();
    this.configFilter = InputState.new({
      value: "",
      placeholder: tr("Filter choices or enter custom value…", "过滤选项或输入自定义值…"),
    });
    this.configFilter.on("change", (_e, cx) => {
      this.highlightedChoiceIdx = 0;
      cx.notify();
    });
    this.configFilter.on("focus", (_e, cx) => {
      this.configFilterFocused = true;
      cx.notify();
    });
    this.configFilter.on("blur", (_e, cx) => {
      this.configFilterFocused = false;
      cx.notify();
    });
    this.configFilter.on("submit", (_e, cx) => {
      this.submitConfigFilter(cx);
    });
    this.input = TextareaState.new({
      value: "",
      placeholder: tr("Ask anything…", "输入任何问题…"),
    });
    this.polishingPrompt = false;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.polishApplied = false;
    this.draftAttachments = [];
    this.attachmentImages = new Map();
    this.expandedMedia = {};
    this.expandedResourceText = {};
    this.input.set_auto_grow(2, 8);
    this.input.on("focus", (_e, cx) => {
      this.inputFocused = true;
      if (this.openConfigId) this.closeConfigPopover(cx);
      cx.notify();
    });
    this.input.on("blur", (_e, cx) => {
      this.inputFocused = false;
      cx.notify();
    });
    this.input.on("change", (_e, cx) => {
      if (this.polishApplied) this.polishApplied = false;
      this.writeSavedDraft(this.loadedTerminal || this.activeTerminal(), this.input?.value?.() ?? "");
      cx.notify();
    });
    this.input.on("paste_other", (_e, cx) => {
      this.handlePasteAttachments(cx);
    });
    this.input.on("submit", (e, cx) => {
      if (e?.shift) return;
      const steer = Boolean(e?.secondary) && this.status() === "working";
      const trimmed = (this.input.value() ?? "").trim();
      const hasAttachments = Array.isArray(this.draftAttachments) && this.draftAttachments.length > 0;
      if (!trimmed && !hasAttachments) {
        this.input.set_value("");
        cx.notify();
        return;
      }
      this.sendPrompt(steer);
    });
    this.handle = cx.focus_handle();
    if (!checking()) this.handle.focus();
    if (!checking()) {
      this.loadedTerminal = this.activeTerminal();
      this.restoreDraftFor(this.loadedTerminal);
      this.scheduleReload(cx);
      if (typeof nextCommand === "function") {
        cx.spawn(async (cmdCx) => {
          for (;;) {
            const cmd = await nextCommand();
            if (cmd === "polish") await this.polishPrompt(cmdCx);
            else if (cmd === "polish_undo") this.revertPolishedPrompt(cmdCx);
          }
        });
      }
      cx.timer.every(1000, (tcx) => {
        if (this.status() === "working") tcx?.notify?.();
      });
      if (this.isPtyMode() || this.ownProps()?.sidePanel) {
        cx.timer.every(1200, (tcx) => {
          if (!this.isPtyMode() || this.term()?.state !== "live") return;
          const curSt = this.status();
          const busy = curSt === "working" || curSt === "starting" || !this.loadedOnce;
          this.ptyPollTick = (this.ptyPollTick || 0) + 1;
          if (busy || this.ptyPollTick % 3 === 0) {
            this.scheduleReload(tcx);
          }
        });
      }
    }
  }

  frameTerminals() {
    return this._frame?.terminals ?? terminals();
  }

  frameAgents() {
    return this._frame?.agents ?? agents();
  }

  framePanes() {
    return this._frame?.panes ?? panes();
  }

  frameSpaces() {
    return this._frame?.spaces ?? (typeof spaces === "function" ? spaces() : []);
  }

  ownProps() {
    if (this._frame?.props) return this._frame.props;
    const p = props();
    if (p && typeof p === "object" && Object.keys(p).length > 0) {
      return p;
    }
    return this.boundProps ?? p ?? {};
  }

  focusedMainAddress() {
    const pList = this.framePanes();
    const fp = pList.find((p) => p?.focused && p?.terminal);
    if (fp?.terminal) return fp.terminal;
    const aList = this.frameAgents();
    const fa = aList.find((a) => a?.focused && a?.address);
    if (fa?.address) return fa.address;
    const foc = typeof focused === "function" ? focused() : null;
    return foc?.shown || foc?.terminal || foc?.agent || null;
  }

  activeSideChatCandidates() {
    if (this._frame?.sideCandidates) return this._frame.sideCandidates;
    const mainAddr = this.focusedMainAddress();
    return this.frameTerminals().filter((x) => {
      if (!x?.address || x.state !== "live" || x.labels?.["herdr.trashed"]) return false;
      if (String(x.lineage?.kind || "").toLowerCase().replace("_", "-") !== "side-chat") return false;
      if (!mainAddr) return false;
      const parent = resolvePeerAddress(x.lineage?.parent || x.parent || "", x.address);
      return parent === mainAddr;
    });
  }

  activeTerminal() {
    if (this._frame && Object.prototype.hasOwnProperty.call(this._frame, "terminal")) {
      return this._frame.terminal;
    }
    const p = this.ownProps();
    if (!p.sidePanel) return p.terminal || null;
    const candidates = this.activeSideChatCandidates();
    if (candidates.length === 0) return null;
    const saved = typeof shared === "function" ? shared("side-chat.active") : null;
    if (saved && candidates.some((x) => x.address === saved)) {
      return saved;
    }
    return candidates[candidates.length - 1].address;
  }

  isPtyMode() {
    if (this._frame && typeof this._frame.isPty === "boolean") {
      return this._frame.isPty;
    }
    const p = this.ownProps();
    if (p.pty) return true;
    if (!p.sidePanel) return false;
    const addr = this.activeTerminal();
    if (!addr) return false;
    const rec = this.frameTerminals().find((t) => t.address === addr);
    return Boolean(rec && !rec.acp);
  }

  isSideChatSession(k = this.term()) {
    if (this._frame && typeof this._frame.isSideChat === "boolean" && k === this._frame.term) {
      return this._frame.isSideChat;
    }
    const p = this.ownProps();
    if (p.sidePanel || p.floating) return true;
    const lk = String(k?.lineage?.kind || this.agentDetail?.lineage?.kind || "").toLowerCase().replace("_", "-");
    return lk === "side-chat";
  }

  term() {
    if (this._frame && Object.prototype.hasOwnProperty.call(this._frame, "term")) {
      return this._frame.term;
    }
    const addr = this.activeTerminal();
    if (!addr) return null;
    return this.frameTerminals().find((t) => t.address === addr) ?? (this.lastDetailTerminal === addr ? this.agentDetail : null);
  }

  status() {
    if (this._frame && Object.prototype.hasOwnProperty.call(this._frame, "status")) {
      return this._frame.status;
    }
    const addr = this.activeTerminal();
    if (!addr) return null;
    const fromAgent = this.frameAgents().find((a) => a.address === addr)?.status;
    if (fromAgent) return fromAgent;
    const fromTerm = this.frameTerminals().find((t) => t.address === addr)?.status;
    if (typeof fromTerm === "string" && fromTerm) return fromTerm;
    if (fromTerm?.state) return fromTerm.state;
    const fromPane = this.framePanes().find((p) => p.terminal === addr)?.status;
    if (fromPane) return fromPane;
    if (this.lastDetailTerminal === addr && this.agentDetail?.status?.state === "working") return "working";
    return this.lastDetailTerminal === addr && this.agentDetail?.state === "finished" ? "ended" : null;
  }

  getConversationItems(k, st, isPty) {
    const working = st === "working";
    const how = this.readHow ?? null;
    const turn = k?.turn ?? 0;
    const isAgent = Boolean(k?.program?.agent || k?.status || st);
    const cwd = k?.cwd || "";
    const c = this._itemsCache;
    if (
      c
      && c.entries === this.entries
      && c.fallbackText === this.fallbackText
      && c.working === working
      && c.isPty === isPty
      && c.how === how
      && c.turn === turn
      && c.isAgent === isAgent
      && c.cwd === cwd
      && c.checkpoints === this.checkpoints
    ) {
      return c;
    }
    const items = buildConversationItems(this.entries, this.fallbackText, working, {
      isPty,
      how,
      turn,
      isAgent,
      cwd,
      checkpointsByTurn: this.checkpoints,
    });
    const visibleItems = items.filter((item) => item.kind !== "stderr_group");
    const maxTurn = items.reduce((m, it) => Math.max(m, Number(it?.turn) || 0), 0);
    const built = {
      entries: this.entries,
      fallbackText: this.fallbackText,
      working,
      isPty,
      how,
      turn,
      isAgent,
      cwd,
      checkpoints: this.checkpoints,
      items,
      visibleItems,
      maxTurn,
    };
    this._itemsCache = built;
    return built;
  }

  warmTailLinkify(visibleItems) {
    if (!Array.isArray(visibleItems) || visibleItems.length === 0) return;
    const limit = Math.max(DEFAULT_VISIBLE_ITEMS, this.visibleItemLimit || DEFAULT_VISIBLE_ITEMS);
    const start = Math.max(0, visibleItems.length - limit);
    for (let i = start; i < visibleItems.length; i++) {
      const it = visibleItems[i];
      if (it?.kind === "assistant" && Array.isArray(it.blocks)) {
        for (const b of it.blocks) {
          if (b && b.type !== "code" && b.text) linkifyFilePaths(b.text);
        }
      } else if (it?.kind === "user" && it.body) {
        const body = it.body.length > 900 ? `${it.body.slice(0, 720).trimEnd()}…` : it.body;
        linkifyFilePaths(body);
      }
    }
  }

  scheduleReload(cx = null, opts = {}) {
    const terminal = this.activeTerminal();
    if (!terminal || checking()) return;
    if (opts?.forceDetail) this.reloadForceDetail = true;
    if (this.loading) {
      this.reloadQueued = true;
      return;
    }
    const trySpawn = (target) => {
      if (!target?.spawn) return false;
      try {
        target.spawn(async (taskCx) => {
          await this.reloadTranscript(taskCx, opts);
        });
        return true;
      } catch {
        return false;
      }
    };
    if (!trySpawn(cx) && !trySpawn(this.cx)) {
      Promise.resolve().then(() => this.reloadTranscript(this.cx, opts));
    }
  }

  async reloadTranscript(notifyCx = null, opts = {}) {
    const terminal = this.activeTerminal();
    if (!terminal || checking()) return;
    if (opts?.forceDetail) this.reloadForceDetail = true;
    if (this.loading) {
      this.reloadQueued = true;
      return;
    }
    this.loading = true;
    const myEpoch = ++this.reloadEpoch;
    const forceDetail = Boolean(this.reloadForceDetail);
    this.reloadForceDetail = false;
    let yielded = false;
    let changed = true;
    try {
      const liveTerm = this.term();
      const curTurn = liveTerm?.turn ?? -1;
      const curInboxLen = Number(liveTerm?.inbox_len ?? 0);
      const needDetail = !this.isPtyMode() && (
        forceDetail
        || !this.agentDetail
        || this.lastDetailTerminal !== terminal
        || !this.agentDetail.config_options?.length
        || this.lastDetailTurn !== curTurn
        || (!Array.isArray(liveTerm?.inbox) && curInboxLen !== this.lastDetailInboxLen)
      );
      const req = Promise.all([
        h2(["read", "--json", terminal]),
        needDetail ? h2(["get", "--json", terminal]) : Promise.resolve(null),
      ]);
      yielded = true;
      const [res, detailRes] = await req;
      if (this.activeTerminal() !== terminal || myEpoch !== this.reloadEpoch) {
        this.reloadQueued = true;
        if (forceDetail) this.reloadForceDetail = true;
        return;
      }
      let detailUpdated = false;
      if (detailRes && detailRes.code === 0) {
        try {
          const parsedDetail = JSON.parse(detailRes.stdout || "{}");
          if (parsedDetail && typeof parsedDetail === "object") {
            this.agentDetail = parsedDetail;
            this.lastDetailTerminal = terminal;
            this.lastDetailTurn = Number(parsedDetail.turn ?? curTurn);
            this.lastDetailInboxLen = Number(parsedDetail.inbox_len ?? curInboxLen);
            detailUpdated = true;
          }
        } catch {}
      }
      if (res.code !== 0) {
        this.lastReadStdout = "";
        this.error = (res.stderr || res.stdout || "Failed to read transcript").trim();
      } else {
        const rawStdout = res.stdout || "{}";
        if (rawStdout === this.lastReadStdout && this.loadedOnce && !this.error && !detailUpdated) {
          changed = false;
        } else {
          this.lastReadStdout = rawStdout;
          const parsed = JSON.parse(rawStdout);
          this.entries = Array.isArray(parsed.entries) ? parsed.entries : [];
          this.fallbackText = parsed.text || "";
          this.readHow = parsed.how || null;
          this.sessionMeta = parsed.session_meta ?? null;
          this.loadedOnce = true;
          this.error = null;
          // Precompute conversation items & linkify inside the async task (500ms budget)
          // so the next GPUI render() (50ms budget) hits warm caches.
          const latestTerm = this.term();
          const built = this.getConversationItems(latestTerm, this.status(), this.isPtyMode());
          this.warmTailLinkify(built.visibleItems);
        }
      }
    } catch (e) {
      if (myEpoch === this.reloadEpoch) {
        this.error = e?.message ?? String(e);
      }
    } finally {
      this.loading = false;
      if (yielded && changed) {
        try {
          (notifyCx ?? this.cx)?.notify?.();
        } catch {}
      }
      if (this.reloadQueued) {
        const nextForceDetail = this.reloadForceDetail;
        this.reloadQueued = false;
        this.reloadForceDetail = false;
        this.scheduleReload(notifyCx ?? this.cx, { forceDetail: nextForceDetail });
      }
    }
  }

  hasCheckpointTurn(turn) {
    if (Array.isArray(this.checkpoints) && this.checkpoints.some((c) => Number(c?.turn) === Number(turn))) {
      return true;
    }
    if (Number(turn) === 0 && Array.isArray(this.entries) && this.entries.some((e) => e?.kind === "user" && Number(e?.turn) >= 1)) {
      return Boolean(this.term()?.cwd);
    }
    return Array.isArray(this.entries) && this.entries.some((e) => e?.kind === "checkpoint" && Number(e?.turn) === Number(turn));
  }

  async openRollbackConfirm(item, toTurn, prompt, cx) {
    const closing = this.rollbackPromptItem?.id === item.id;
    this.rollbackError = null;
    if (closing) {
      this.rollbackPromptItem = null;
      (cx ?? this.cx)?.notify?.();
      return;
    }
    this.rollbackPromptItem = { id: item.id, toTurn, prompt, checkingIsolation: true };
    (cx ?? this.cx)?.notify?.();
    const terminal = this.activeTerminal();
    if (!terminal || checking()) return;
    try {
      const ckptRes = await h2(["checkpoint", "ls", "--json", terminal]);
      if (ckptRes.code === 0) {
        const parsed = JSON.parse(ckptRes.stdout || "{}");
        this.checkpoints = Array.isArray(parsed.checkpoints) ? parsed.checkpoints : [];
        this.isolation = parsed.isolation ?? null;
      }
    } catch {}
    if (this.rollbackPromptItem?.id === item.id) {
      this.rollbackPromptItem.checkingIsolation = false;
    }
    (cx ?? this.cx)?.notify?.();
  }

  async toggleCheckpointDiff(turn, cx) {
    const terminal = this.activeTerminal();
    if (!terminal || checking() || !turn) return;
    const isOpen = Boolean(this.checkpointDiffOpen?.[turn]);
    this.checkpointDiffOpen[turn] = !isOpen;
    if (!isOpen && !this.checkpointDiffs[turn] && !this.checkpointDiffLoading[turn]) {
      this.checkpointDiffLoading[turn] = true;
      (cx ?? this.cx)?.notify?.();
      try {
        const res = await h2(["checkpoint", "diff", "--json", terminal, "--to", String(turn)]);
        if (res.code === 0) {
          this.checkpointDiffs[turn] = JSON.parse(res.stdout || "{}");
        } else {
          this.checkpointDiffs[turn] = { patch: "", error: (res.stderr || res.stdout || "Diff unavailable").trim() };
        }
      } catch (e) {
        this.checkpointDiffs[turn] = { patch: "", error: e?.message ?? String(e) };
      } finally {
        this.checkpointDiffLoading[turn] = false;
      }
    }
    (cx ?? this.cx)?.notify?.();
  }

  executeCheckpointRollback(toTurn, restoreFiles, fallbackPrompt, cx) {
    const terminal = this.activeTerminal();
    if (!terminal || checking() || this.rollbackBusy) return;
    this.rollbackBusy = true;
    this.rollbackError = null;
    (cx ?? this.cx)?.notify?.();
    const runRollback = async (taskCx) => {
      try {
        const args = [
          "checkpoint",
          "rollback",
          "--json",
          terminal,
          "--to",
          String(toTurn),
          ...(restoreFiles ? ["--confirm"] : ["--no-files"]),
        ];
        const res = await h2(args, { timeout: 90000 });
        if (res.code !== 0) {
          let msg = (res.stderr || res.stdout || "Rollback failed").trim();
          try {
            const parsedErr = JSON.parse(res.stderr || "{}");
            msg = parsedErr?.error?.message || msg;
          } catch {}
          this.rollbackError = msg;
          return;
        }
        JSON.parse(res.stdout || "{}");
        this.rollbackPromptItem = null;
        this.checkpointDiffs = {};
        this.checkpointDiffOpen = {};
        this.expandedGroups = {};
        this.expandedStepLimits = {};
        this.autoFollow = true;
        this.revealedSig = "";
        this.optimisticInbox = null;
        this.lastDetailTerminal = "";
        this.lastReadStdout = "";
        this._itemsCache = null;
        this.reloadEpoch = (this.reloadEpoch || 0) + 1;
        setShared("checkpoint.sync", `${terminal}:${toTurn}:${Date.now()}`);
        await this.reloadTranscript(taskCx ?? this.cx, { forceDetail: true });
      } catch (e) {
        this.rollbackError = e?.message ?? String(e);
      } finally {
        this.rollbackBusy = false;
        try {
          (taskCx ?? this.cx)?.notify?.();
        } catch {}
      }
    };
    const spawner = cx?.spawn ? cx : this.cx;
    if (spawner?.spawn) {
      spawner.spawn(runRollback);
    } else {
      runRollback(this.cx);
    }
  }

  async runH2(args) {
    if (checking() || this.busyAction) return;
    this.busyAction = true;
    this.error = null;
    this.cx?.notify();
    try {
      const res = await h2(args);
      if (res.code !== 0) {
        let msg = (res.stderr || res.stdout || "Command failed").trim();
        try {
          const parsed = JSON.parse(res.stdout || res.stderr || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        this.error = msg;
      }
    } catch (e) {
      this.error = e?.message ?? String(e);
    } finally {
      this.busyAction = false;
      this.cx?.notify();
      this.scheduleReload(this.cx);
    }
  }

  async interruptAgent(terminal = this.activeTerminal(), cx = this.cx) {
    if (!terminal || checking() || this.interrupting) return;
    this.interrupting = true;
    this.error = null;
    (cx ?? this.cx)?.notify?.();
    try {
      const res = await h2(["interrupt", "--json", terminal]);
      if (res.code !== 0) {
        let msg = (res.stderr || res.stdout || "Interrupt failed").trim();
        try {
          const parsed = JSON.parse(res.stdout || res.stderr || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        this.error = msg;
      }
    } catch (e) {
      this.error = e?.message ?? String(e);
    } finally {
      this.interrupting = false;
      this.cx?.notify();
      this.scheduleReload(this.cx);
    }
  }

  activeMachine() {
    const addr = this.activeTerminal();
    return typeof addr === "string" && addr.includes("/") ? addr.split("/")[0] : null;
  }

  addDraftAttachments(results, cx) {
    const MAX_ATTACH = 8;
    const list = Array.isArray(results) ? results : [];
    if (list.length === 0) return false;
    let added = 0;
    for (const item of list) {
      if (!item || typeof item !== "object") continue;
      if (item.error) {
        this.error = String(item.error);
        continue;
      }
      if (!item.path) continue;
      if (this.draftAttachments.length >= MAX_ATTACH) {
        this.error = tr(`Up to ${MAX_ATTACH} attachments per message.`, `单条消息最多附加 ${MAX_ATTACH} 个文件。`);
        break;
      }
      if (this.draftAttachments.some((d) => d.path === item.path)) continue;
      this.draftAttachments.push({
        kind: item.kind === "image" ? "image" : "resource",
        path: String(item.path),
        preview: item.preview ? String(item.preview) : null,
        name: String(item.name || attachmentDisplayName(item)),
        mimeType: String(item.mimeType || item.mime_type || ""),
        bytes: Number(item.bytes) || 0,
      });
      added += 1;
    }
    if (added > 0) {
      this.error = null;
      this.input?.focus?.();
    }
    (cx ?? this.cx)?.notify?.();
    return added > 0;
  }

  handlePasteAttachments(cx) {
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished" || checking()) return false;
    if (typeof pasteClipboardAttachments !== "function") return false;
    try {
      const res = pasteClipboardAttachments();
      return this.addDraftAttachments(res, cx);
    } catch (e) {
      this.error = String(e?.message ?? e);
      (cx ?? this.cx)?.notify?.();
      return false;
    }
  }

  handleDropFiles(paths, cx) {
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished" || checking()) return false;
    if (!Array.isArray(paths) || paths.length === 0 || typeof prepareAttachments !== "function") return false;
    try {
      const remaining = Math.max(0, 8 - (this.draftAttachments?.length ?? 0));
      if (remaining === 0) {
        this.error = tr("Up to 8 attachments per message.", "单条消息最多附加 8 个文件。");
        (cx ?? this.cx)?.notify?.();
        return false;
      }
      const res = prepareAttachments(paths.slice(0, remaining));
      return this.addDraftAttachments(res, cx);
    } catch (e) {
      this.error = String(e?.message ?? e);
      (cx ?? this.cx)?.notify?.();
      return false;
    }
  }

  pickDraftAttachments(cx) {
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished" || checking()) return;
    if (typeof pickAttachments !== "function") return;
    const targetCx = cx ?? this.cx;
    if (!targetCx?.spawn) return;
    targetCx.spawn(async (taskCx) => {
      try {
        const res = await pickAttachments({ prompt: tr("Attach", "附加"), multiple: true });
        this.addDraftAttachments(res, taskCx);
      } catch (e) {
        this.error = String(e?.message ?? e);
        taskCx?.notify?.();
      }
    });
  }

  removeDraftAttachment(idx, cx) {
    if (!Array.isArray(this.draftAttachments) || idx < 0 || idx >= this.draftAttachments.length) return;
    this.draftAttachments.splice(idx, 1);
    (cx ?? this.cx)?.notify?.();
  }

  attachmentCacheKey(att) {
    return `${this.activeMachine() ?? "local"}:${att?.path ?? ""}`;
  }

  ensureAttachmentImage(att, cx) {
    if (!att?.path || !isImageAttachment(att) || checking()) return null;
    if (typeof readImageDataUrl !== "function") return null;
    const key = this.attachmentCacheKey(att);
    const existing = this.attachmentImages.get(key);
    if (existing) return existing;
    const entry = { status: "loading", dataUrl: null };
    this.attachmentImages.set(key, entry);
    const on = this.activeMachine();
    const path = att.path;
    const spawnOn = cx ?? this.cx;
    const load = async (notifyCx) => {
      try {
        const dataUrl = await readImageDataUrl(path, on ? { on } : undefined);
        this.attachmentImages.set(key, dataUrl ? { status: "ready", dataUrl } : { status: "error", dataUrl: null });
      } catch {
        this.attachmentImages.set(key, { status: "error", dataUrl: null });
      }
      try {
        (notifyCx ?? this.cx)?.notify?.();
      } catch {}
    };
    if (spawnOn?.spawn) {
      spawnOn.spawn(load);
    } else {
      Promise.resolve().then(() => load(this.cx));
    }
    return entry;
  }

  openAttachment(att, cx) {
    if (!att) return;
    if (att.path) {
      const on = this.activeMachine();
      const args = { path: att.path };
      if (on) args.on = on;
      run("files.open", args);
      return;
    }
    if (att.uri && /^https?:\/\//i.test(String(att.uri))) {
      try { cx?.open_url?.(att.uri); } catch {}
    }
  }

  async sendPrompt(steer) {
    const terminal = this.activeTerminal();
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished") return;
    const text = (this.input?.value?.() ?? "").trim();
    const attachments = Array.isArray(this.draftAttachments) ? [...this.draftAttachments] : [];
    if (!terminal || (!text && attachments.length === 0) || checking() || this.busyAction) return;
    this.input.set_value("");
    this.writeSavedDraft(terminal, "");
    this.draftAttachments = [];
    this.polishUndoText = null;
    this.polishSummary = null;
    this.polishApplied = false;
    this.autoFollow = true;
    this.revealedSig = "";
    const args = ["send", "--json"];
    if (steer) args.push("--steer");
    for (const att of attachments) {
      if (att.kind === "image") args.push("--image", att.path);
      else args.push("--resource", att.path);
    }
    args.push("--", terminal);
    if (text) args.push(text);
    await this.runH2(args);
  }

  revertPolishedPrompt(cx) {
    if (this.polishUndoText == null) return;
    const orig = this.polishUndoText;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.polishApplied = false;
    this.input?.set_value?.(orig);
    const terminal = this.activeTerminal();
    if (terminal) this.writeSavedDraft(terminal, orig);
    this.input?.focus?.();
    (cx ?? this.cx)?.notify?.();
  }

  async polishPrompt(cx) {
    const terminal = this.activeTerminal();
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished") return;
    const raw = (this.input?.value?.() ?? "").trim();
    if (!raw || checking() || this.polishingPrompt || this.busyAction) return;
    const kind = this.isPtyMode() ? "cli" : "acp";
    const machine = terminal && terminal.includes("/") ? terminal.split("/")[0] : null;
    this.polishingPrompt = true;
    this.error = null;
    (cx ?? this.cx)?.notify?.();
    try {
      const args = [
        "ai",
        "polish-prompt",
        "--json",
        "--kind",
        kind,
        "--lang",
        isZh() ? "zh-CN" : "en",
      ];
      if (terminal) args.push("--target", terminal);
      if (machine) args.push("--on", machine);
      args.push("--", raw);
      const res = await h2(args, { timeout: 22000 });
      if (this.activeTerminal() !== terminal) return;
      if (res?.code === 0 && res.stdout) {
        const parsed = JSON.parse(res.stdout);
        const polished = String(parsed?.polished ?? "").trim();
        if (parsed?.usedAi && polished) {
          this.polishUndoText = raw;
          this.polishSummary = parsed?.summary ? String(parsed.summary).trim() : null;
          this.polishApplied = true;
          this.input?.set_value?.(polished);
          this.input?.focus?.();
        } else {
          const reason = String(parsed?.fallbackReason ?? "");
          this.error = reason === "ai_not_configured"
            ? tr("Core AI is not configured. Enable it in Settings → AI Assistant to polish prompts.", "Core AI 尚未配置，请先在「设置 → AI 助手」中启用后再使用提示词润色。")
            : reason === "ai_circuit_open"
              ? tr("Core AI is temporarily cooling down after recent failures. Please retry shortly.", "Core AI 因连续失败暂时熔断冷却中，请稍后重试。")
              : tr("AI prompt polish did not return an improved version. Please retry.", "AI 润色暂未返回有效结果，请稍后重试。");
        }
      } else {
        let msg = String(res?.stderr || res?.stdout || "").trim();
        try {
          const parsed = JSON.parse(res?.stderr || res?.stdout || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        this.error = msg || tr("AI prompt polish failed", "AI 提示词润色失败");
      }
    } catch (e) {
      this.error = tr(`AI prompt polish failed: ${e?.message ?? e}`, `AI 提示词润色失败：${e?.message ?? e}`);
    } finally {
      this.polishingPrompt = false;
      (cx ?? this.cx)?.notify?.();
    }
  }

  rawInboxFor(k = this.term()) {
    if (this.isPtyMode()) return [];
    if (Array.isArray(this.optimisticInbox)) return this.optimisticInbox;
    if (Array.isArray(k?.inbox)) return k.inbox;
    if (k && Number(k.inbox_len ?? 0) === 0) return [];
    if (Array.isArray(this.agentDetail?.inbox)) return this.agentDetail.inbox;
    return [];
  }

  inboxItemsFor(k = this.term()) {
    return normalizeInboxItems(this.rawInboxFor(k), this.frameTerminals(), this.activeTerminal() ?? "");
  }

  startEditInboxItem(item, cx) {
    if (!item || this.inboxBusy) return;
    this.editingInboxId = item.id;
    this.queueCollapsed = false;
    this.inboxEditInput?.set_value?.(item.body ?? "");
    this.inboxEditInput?.focus?.();
    (cx ?? this.cx)?.notify?.();
  }

  cancelEditInboxItem(cx) {
    this.editingInboxId = null;
    this.inboxEditFocused = false;
    this.inboxEditInput?.set_value?.("");
    (cx ?? this.cx)?.notify?.();
  }

  async applyInboxCommand(args, cx) {
    const terminal = this.activeTerminal();
    this.inboxBusy = true;
    this.error = null;
    (cx ?? this.cx)?.notify?.();
    try {
      const res = await h2(args);
      if (res.code !== 0) {
        let msg = (res.stderr || res.stdout || "Queue update failed").trim();
        try {
          const parsed = JSON.parse(res.stderr || res.stdout || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        this.optimisticInbox = null;
        this.error = msg;
        this.lastDetailTerminal = "";
        this.scheduleReload(this.cx, { forceDetail: true });
      } else {
        try {
          const parsed = JSON.parse(res.stdout || "{}");
          if (parsed && typeof parsed === "object") {
            this.agentDetail = parsed;
            this.lastDetailTerminal = terminal;
            this.lastDetailInboxLen = Number(parsed.inbox_len ?? (Array.isArray(parsed.inbox) ? parsed.inbox.length : 0));
          }
        } catch {}
        this.optimisticInbox = null;
      }
    } catch (e) {
      this.optimisticInbox = null;
      this.error = e?.message ?? String(e);
    } finally {
      this.inboxBusy = false;
      (cx ?? this.cx)?.notify?.();
    }
  }

  async saveEditInboxItem(cx) {
    const terminal = this.activeTerminal();
    const itemId = this.editingInboxId;
    const trimmed = (this.inboxEditInput?.value?.() ?? "").trim();
    if (!terminal || itemId == null || !trimmed || checking() || this.inboxBusy) return;
    const current = this.rawInboxFor();
    this.optimisticInbox = current.map((raw) => (Number(raw?.id) === Number(itemId) ? { ...raw, text: trimmed } : raw));
    this.editingInboxId = null;
    this.inboxEditFocused = false;
    this.inboxEditInput?.set_value?.("");
    await this.applyInboxCommand(["inbox", "edit", "--json", terminal, String(itemId), trimmed], cx);
  }

  async removeInboxItem(itemId, cx) {
    const terminal = this.activeTerminal();
    if (!terminal || itemId == null || checking() || this.inboxBusy) return;
    if (Number(this.editingInboxId) === Number(itemId)) {
      this.editingInboxId = null;
      this.inboxEditFocused = false;
      this.inboxEditInput?.set_value?.("");
    }
    const current = this.rawInboxFor();
    this.optimisticInbox = current.filter((raw) => Number(raw?.id) !== Number(itemId));
    await this.applyInboxCommand(["inbox", "rm", "--json", terminal, String(itemId)], cx);
  }

  async sendInboxItemNow(itemId, cx) {
    const terminal = this.activeTerminal();
    if (!terminal || itemId == null || checking() || this.inboxBusy) return;
    if (Number(this.editingInboxId) === Number(itemId)) {
      this.editingInboxId = null;
      this.inboxEditFocused = false;
      this.inboxEditInput?.set_value?.("");
    }
    const current = this.rawInboxFor();
    this.optimisticInbox = current.filter((raw) => Number(raw?.id) !== Number(itemId));
    await this.applyInboxCommand(["inbox", "send", "--json", terminal, String(itemId)], cx);
    this.scheduleReload(cx);
  }

  draftStorageKey(addr) {
    return addr ? `herdr2.peek.draft.v1:${addr}` : null;
  }

  readSavedDraft(addr) {
    if (checking() || typeof localStorage === "undefined") return "";
    const key = this.draftStorageKey(addr);
    if (!key) return "";
    try {
      const raw = localStorage.getItem(key);
      return typeof raw === "string" ? raw : "";
    } catch {
      return "";
    }
  }

  writeSavedDraft(addr, text) {
    if (checking() || typeof localStorage === "undefined") return;
    const key = this.draftStorageKey(addr);
    if (!key) return;
    try {
      const value = typeof text === "string" ? text : "";
      if (!value.trim()) {
        localStorage.removeItem(key);
      } else {
        localStorage.setItem(key, value.slice(0, 32 * 1024));
      }
    } catch {}
  }

  restoreDraftFor(addr) {
    if (!this.input?.set_value) return;
    const saved = this.readSavedDraft(addr);
    if ((this.input.value?.() ?? "") !== saved) {
      this.input.set_value(saved);
    }
  }

  pausedInboxItemsFor(k = this.term()) {
    const raw = Array.isArray(k?.paused_inbox)
      ? k.paused_inbox
      : Array.isArray(this.agentDetail?.paused_inbox)
        ? this.agentDetail.paused_inbox
        : [];
    return normalizeInboxItems(raw);
  }

  async resumePausedInbox(cx) {
    const terminal = this.activeTerminal();
    if (!terminal || checking() || this.inboxBusy) return;
    await this.applyInboxCommand(["inbox", "resume", "--json", terminal], cx);
    this.scheduleReload(cx, { forceDetail: true });
  }

  async clearPausedInbox(cx) {
    const terminal = this.activeTerminal();
    if (!terminal || checking() || this.inboxBusy) return;
    await this.applyInboxCommand(["inbox", "clear-paused", "--json", terminal], cx);
    this.scheduleReload(cx, { forceDetail: true });
  }

  async movePausedInboxToInput(pausedItems, cx) {
    const terminal = this.activeTerminal();
    if (!terminal || !Array.isArray(pausedItems) || pausedItems.length === 0 || checking() || this.inboxBusy) return;
    const joined = pausedItems
      .map((it) => String(it?.body ?? "").trim())
      .filter(Boolean)
      .join("\n\n");
    const existing = (this.input?.value?.() ?? "").trim();
    const next = existing && joined ? `${existing}\n\n${joined}` : (joined || existing);
    this.input?.set_value?.(next);
    this.writeSavedDraft(terminal, next);
    this.input?.focus?.();
    await this.clearPausedInbox(cx);
  }

  configOptionsFor(k) {
    if (this.isPtyMode()) return [];
    const rawOpts = k?.config_options?.length ? k.config_options : (this.agentDetail?.config_options ?? []);
    const patchedOpts = rawOpts.map((raw) =>
      raw && Object.prototype.hasOwnProperty.call(this.optimisticConfig, raw.id)
        ? { ...raw, current_value: this.optimisticConfig[raw.id] }
        : raw,
    );
    const mergedRecord = {
      ...this.agentDetail,
      ...k,
      ...(Object.prototype.hasOwnProperty.call(this.optimisticConfig, "model")
        ? { model: this.optimisticConfig.model }
        : {}),
    };
    return normalizeConfigOptions(patchedOpts, mergedRecord);
  }

  shouldShowConfigSearch(opt) {
    return Boolean(opt && (opt.choices.length > 6 || opt.kind === "custom_text" || opt.iconKind === "model"));
  }

  startClosingId(id, cx) {
    if (!id) return;
    if (this.enteringConfigId === id) {
      this.enteringConfigId = null;
    }
    if (checking()) {
      delete this.closingConfigs[id];
      return;
    }
    this.closingConfigs[id] = true;
    this.closeTimers[id]?.cancel?.();
    const timerCx = cx ?? this.cx;
    this.closeTimers[id] = timerCx?.timer?.after?.(155, (c) => {
      delete this.closingConfigs[id];
      delete this.closeTimers[id];
      c.notify();
    }) ?? null;
  }

  openConfigPopover(opt, cx) {
    if (!opt || opt.kind === "boolean") return;
    const prev = this.openConfigId;
    if (prev && prev !== opt.id) {
      this.startClosingId(prev, cx);
    }
    this.closeTimers[opt.id]?.cancel?.();
    delete this.closeTimers[opt.id];
    delete this.closingConfigs[opt.id];
    this.enterTimer?.cancel?.();
    this.enterTimer = null;

    this.openConfigId = opt.id;
    this.configFilter?.set_value?.("");
    const current = String(opt.currentValue ?? "");
    const activeIdx = opt.choices.findIndex((c) => c.value === current);
    this.highlightedChoiceIdx = activeIdx >= 0 ? activeIdx : 0;

    if (checking()) {
      this.enteringConfigId = null;
    } else {
      this.enteringConfigId = opt.id;
      const timerCx = cx ?? this.cx;
      this.enterTimer = timerCx?.timer?.after?.(16, (c) => {
        if (this.enteringConfigId === opt.id) {
          this.enteringConfigId = null;
          c.notify();
        }
      }) ?? null;
    }

    if (this.shouldShowConfigSearch(opt)) {
      this.configFilter?.focus?.();
    } else {
      this.menuFocus?.focus?.();
    }
    (cx ?? this.cx)?.notify?.();
  }

  closeConfigPopover(cx) {
    const prev = this.openConfigId;
    if (!prev) return;
    this.openConfigId = null;
    this.configFilter?.set_value?.("");
    this.startClosingId(prev, cx);
    (cx ?? this.cx)?.notify?.();
  }

  submitConfigFilter(cx) {
    if (!this.openConfigId) return;
    const opts = this.configOptionsFor(this.term());
    const opt = opts.find((o) => o.id === this.openConfigId);
    if (!opt || opt.kind === "boolean") return;
    const raw = (this.configFilter?.value?.() ?? "").trim();
    const filtered = filterConfigChoices(opt.choices, raw);
    const highlighted = filtered[this.highlightedChoiceIdx] ?? filtered[0];
    const exactMatch = filtered.find((c) => c.value === raw || c.name.toLowerCase() === raw.toLowerCase())?.value;
    const chosen = exactMatch ?? (raw && !filtered.length ? raw : (highlighted?.value ?? raw));
    if (chosen) {
      this.setConfigOption(opt, chosen, cx);
    }
  }

  async setConfigOption(opt, val, cx) {
    if (this.openConfigId) {
      this.closeConfigPopover(cx ?? this.cx);
    }
    const terminal = this.activeTerminal();
    if (!terminal || !opt || checking()) return;
    this.error = null;
    this.optimisticConfig[opt.id] = val;
    (cx ?? this.cx)?.notify?.();
    try {
      const args = opt.kind === "boolean"
        ? ["config", "--json", terminal, "--id", opt.id, "--bool", val ? "true" : "false"]
        : ["config", "--json", terminal, "--id", opt.id, "--value", String(val)];
      const res = await h2(args);
      if (res.code !== 0) {
        let msg = (res.stderr || res.stdout || "Failed to update capability").trim();
        try {
          const parsed = JSON.parse(res.stdout || res.stderr || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        delete this.optimisticConfig[opt.id];
        this.error = msg;
      } else {
        try {
          const parsed = JSON.parse(res.stdout || "{}");
          if (parsed && typeof parsed === "object") {
            this.agentDetail = parsed;
            this.lastDetailTerminal = terminal;
          }
        } catch {}
        delete this.optimisticConfig[opt.id];
      }
    } catch (e) {
      delete this.optimisticConfig[opt.id];
      this.error = e?.message ?? String(e);
    } finally {
      this.cx?.notify();
    }
  }

  copyText(key, text, cx) {
    if (!text) return;
    cx.write_to_clipboard(text);
    this.copiedId = key;
    cx.notify();
    cx.timer?.after?.(1500, () => {
      if (this.copiedId === key) {
        this.copiedId = null;
        this.cx?.notify();
      }
    });
  }

  button(t, id, label, onClick, primary = false) {
    return div()
      .id(id)
      .px(10)
      .py(4)
      .rounded(7)
      .flex_none()
      .text_size(12)
      .font_medium()
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).hover((s) => s.opacity(0.88)))
      .when(!primary, (el) => el.text_color(t.muted).hover((s) => s.bg(t.hover).text_color(t.foreground)))
      .cursor_pointer()
      .on_mouse_down("left", stopMouse)
      .on_click(onClick)
      .child(label);
  }

  pillButton(t, id, content, onClick, opts = {}) {
    const { primary = false, danger = false, outlined = false, h = 28, px = 10, disabled = false } = opts;
    return h_flex()
      .id(id)
      .h(h)
      .px(px)
      .gap(6)
      .items_center()
      .justify_center()
      .rounded(8)
      .flex_none()
      .text_size(12)
      .font_medium()
      .when(outlined && !primary, (el) => el.border_1().border_color(t.border).bg(t.raised))
      .when(primary && !disabled, (el) => el.bg(t.solid).text_color(t.solid_foreground).hover((s) => s.opacity(0.9)))
      .when(primary && disabled, (el) => el.bg(t.hover).text_color(t.dim))
      .when(!primary && danger, (el) => el.text_color(t.muted).hover((s) => s.bg(t.hover).text_color(t.blocked ?? "#ef4444")))
      .when(!primary && !danger && !disabled, (el) => el.text_color(t.muted).hover((s) => s.bg(t.hover).text_color(t.foreground)))
      .when(!primary && disabled, (el) => el.text_color(t.dim))
      .when(!disabled, (el) => el.cursor_pointer().on_mouse_down("left", stopMouse).on_click(onClick))
      .child(content);
  }

  copyPill(t, key, text, label = tr("Copy", "复制"), iconOnly = false) {
    const copied = this.copiedId === key;
    return h_flex()
      .id(`copy-${key}`)
      .h(22)
      .px(iconOnly ? 5 : 7)
      .gap(4)
      .items_center()
      .rounded(6)
      .cursor_pointer()
      .text_size(11)
      .text_color(copied ? (t.done ?? t.foreground) : t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .when(iconOnly && Boolean(label), (el) => el.tooltip(copied ? tr("Copied", "已复制") : label))
      .on_mouse_down("left", stopMouse)
      .on_click((_e, cx) => this.copyText(key, text, cx))
      .child(svg(copied ? ICONS.check : ICONS.copy).size(11).text_color(copied ? (t.done ?? t.foreground) : t.dim))
      .when(!iconOnly, (el) => el.child(copied ? tr("Copied", "已复制") : label));
  }

  avatarTile(t, program, pic, size = 30, glyph = 16) {
    const tile = program?.tile;
    const fg = tile ? (program?.color || "#ffffff") : t.foreground;
    const inner = icon(program, glyph, fg) ?? svg(ICONS.bot).size(glyph).flex_none().text_color(fg);
    return div()
      .size(size)
      .rounded(size >= 28 ? 9 : 6)
      .flex_none()
      .flex()
      .items_center()
      .justify_center()
      .bg(tile || t.raised)
      .when(!tile, (el) => el.border_1().border_color(t.border))
      .child(inner);
  }

  statusPill(t, st) {
    const spinning = st === "working" || st === "starting";
    const color = t[st] ?? (st === "starting" ? (t.working ?? t.accent) : t.dim);
    const label = statusWords()[st] ?? st ?? tr("Offline", "离线");
    return h_flex()
      .h(24)
      .px(9)
      .gap(6)
      .items_center()
      .rounded(999)
      .flex_none()
      .bg(t.raised)
      .border_1()
      .border_color(t.border)
      .text_size(11.5)
      .font_medium()
      .text_color(color)
      .when(spinning, (el) => el.child(svg(ICONS.spinner).size(11).text_color(color).spin(900)))
      .when(!spinning, (el) => el.child(div().size(6).rounded(999).bg(color)))
      .child(label);
  }

  mdOptions(t, cwd) {
    const isDark = readSettings()?.dark ?? true;
    const opts = {
      dark: Boolean(isDark),
      foreground: t.foreground,
      muted: t.muted,
      link: t.accent ?? t.foreground,
      selection: t.selection,
      code_background: t.code_bg ?? t.popover ?? t.surface,
      inline_code_foreground: t.foreground,
      inline_code_background: t.raised,
      border: t.border,
      heading: t.foreground,
      allow_data_images: true,
    };
    if (cwd) opts.root_dir = cwd;
    return opts;
  }

  renderAttachments(t, attachments, idPrefix, mono = "JetBrains Mono", opts = {}) {
    const list = Array.isArray(attachments) ? attachments : [];
    if (list.length === 0) return null;
    const isUser = Boolean(opts.isUser);
    const mdOpts = { ...this.mdOptions(t, this.term()?.cwd), allow_data_images: true };
    const cards = list.map((att, idx) => {
      const key = `${idPrefix}-att-${idx}`;
      const name = attachmentDisplayName(att);
      const sizeStr = formatAttachmentBytes(att.bytes);
      const mime = String(att.mime_type ?? att.mimeType ?? "");
      const clickable = Boolean(att.path || (att.uri && /^https?:\/\//i.test(att.uri)));

      if (isImageAttachment(att) && att.path) {
        const cached = this.ensureAttachmentImage(att, this.cx);
        const expanded = Boolean(this.expandedMedia?.[key]);
        const maxW = expanded ? LANE_MAX_W - 24 : (isUser ? 320 : 420);
        return v_flex()
          .id(`acp-media-${key}`)
          .w_full()
          .max_w(maxW)
          .min_w(0)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.code_bg ?? t.popover ?? t.surface)
          .overflow_hidden()
          .child(
            h_flex()
              .w_full()
              .h(26)
              .px(9)
              .gap(6)
              .items_center()
              .justify_between()
              .border_b(1)
              .border_color(t.border)
              .child(
                h_flex()
                  .flex_1()
                  .min_w(0)
                  .gap(5)
                  .items_center()
                  .child(svg(ICONS.paperclip).size(11).flex_none().text_color(t.dim))
                  .child(div().flex_1().min_w(0).truncate().text_size(11).font_medium().text_color(t.foreground).child(name))
                  .when(Boolean(sizeStr), (el) => el.child(div().flex_none().font_family(mono).text_size(10).text_color(t.dim).child(sizeStr))),
              )
              .child(
                h_flex()
                  .gap(4)
                  .flex_none()
                  .items_center()
                  .child(
                    div()
                      .id(`acp-media-zoom-${key}`)
                      .px(5)
                      .py(1)
                      .rounded(4)
                      .text_size(10.5)
                      .text_color(t.muted)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.hover).text_color(t.foreground))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => {
                        this.expandedMedia[key] = !expanded;
                        cx.notify();
                      })
                      .child(expanded ? tr("Compact", "缩小") : tr("Expand", "放大")),
                  )
                  .when(clickable, (el) =>
                    el.child(
                      div()
                        .id(`acp-media-open-${key}`)
                        .px(5)
                        .py(1)
                        .rounded(4)
                        .text_size(10.5)
                        .text_color(t.accent ?? t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => this.openAttachment(att, cx))
                        .child(tr("Open", "打开")),
                    ),
                  ),
              ),
          )
          .child(
            div()
              .w_full()
              .min_w(0)
              .p(6)
              .child(
                cached?.status === "ready" && cached.dataUrl
                  ? TextView.markdown(`acp-img-${key}`, `![${name.replace(/[\[\]]/g, "")}](${cached.dataUrl})`, mdOpts)
                      .w_full()
                      .min_w(0)
                      .scrollable(false)
                  : div()
                      .px(6)
                      .py(8)
                      .text_size(11.5)
                      .text_color(t.dim)
                      .child(
                        cached?.status === "error"
                          ? tr(`Image preview unavailable (${name})`, `无法预览图片（${name}）`)
                          : tr("Loading image…", "正在加载图片…"),
                      ),
              ),
          );
      }

      const hasTextSnippet = Boolean(att.text && String(att.text).trim().length > 0);
      const snippetOpen = Boolean(this.expandedResourceText?.[key]);
      const isLink = att.kind === "resource_link";
      const badgeText = [mime || att.kind, sizeStr].filter(Boolean).join(" · ");
      return v_flex()
        .id(`acp-res-${key}`)
        .w_full()
        .max_w(isUser ? 520 : LANE_MAX_W - 24)
        .min_w(0)
        .rounded(9)
        .border_1()
        .border_color(t.border)
        .bg(t.raised ?? t.surface)
        .overflow_hidden()
        .child(
          h_flex()
            .w_full()
            .min_h(30)
            .px(10)
            .py(5)
            .gap(7)
            .items_center()
            .when(clickable || hasTextSnippet, (el) =>
              el
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  if (hasTextSnippet && !clickable) {
                    this.expandedResourceText[key] = !snippetOpen;
                    cx.notify();
                  } else if (clickable) {
                    this.openAttachment(att, cx);
                  }
                }),
            )
            .child(svg(isLink ? ICONS.globe : ICONS.fileText).size(13).flex_none().text_color(t.accent ?? t.muted))
            .child(div().flex_1().min_w(0).truncate().text_size(12).font_medium().text_color(t.foreground).child(name))
            .when(Boolean(badgeText), (el) =>
              el.child(div().flex_none().font_family(mono).text_size(10.5).text_color(t.dim).child(badgeText)),
            )
            .when(hasTextSnippet, (el) =>
              el.child(
                div()
                  .id(`acp-res-toggle-${key}`)
                  .px(5)
                  .py(1)
                  .rounded(4)
                  .text_size(10.5)
                  .text_color(t.muted)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .on_mouse_down("left", stopMouse)
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.expandedResourceText[key] = !snippetOpen;
                    cx.notify();
                  })
                  .child(snippetOpen ? tr("Hide", "收起") : tr("Preview", "预览")),
              ),
            ),
        )
        .when(hasTextSnippet && snippetOpen, (el) =>
          el.child(
            div()
              .w_full()
              .min_w(0)
              .max_h(200)
              .overflow_y_scrollbar()
              .px(10)
              .py(7)
              .border_t(1)
              .border_color(t.border)
              .bg(t.code_bg ?? t.background)
              .font_family(mono)
              .text_size(11.5)
              .line_height("17px")
              .text_color(t.muted)
              .whitespace_normal()
              .child(String(att.text)),
          ),
        );
    });

    return v_flex()
      .w_full()
      .min_w(0)
      .gap(6)
      .children(cards);
  }

  handleMarkdownLinkClick(url, ev, cx) {
    const fileTarget = parseClickableFileLink(url);
    if (fileTarget) {
      const isMac = typeof appWindow === "function" && appWindow()?.platform === "macos";
      const ctrlClick = isMac
        ? (Boolean(ev?.meta) && !ev?.ctrl)
        : (Boolean(ev?.ctrl) && !ev?.meta);
      const isLeft = !ev?.button || ev.button === "left";
      if (!ctrlClick || !isLeft || ev?.alt || ev?.shift) return;
      const addr = this.activeTerminal();
      const on = typeof addr === "string" && addr.includes("/") ? addr.split("/")[0] : null;
      const cwd = this.term()?.cwd || spaces().find((s) => s.active)?.folder || null;
      const args = { path: fileTarget.path };
      if (fileTarget.line) args.line = fileTarget.line;
      if (fileTarget.endLine) args.end_line = fileTarget.endLine;
      if (on) args.on = on;
      if (cwd) args.cwd = cwd;
      run("files.open", args);
      return;
    }
    if (/^https?:\/\//i.test(String(url ?? ""))) {
      try { cx.open_url(url); } catch {}
    }
  }

  renderEffortGlyph(t, visual, activeColor) {
    if (!visual) return null;
    const heights = [4, 6.5, 9.5, 12.5];
    const bars = visual.bars ?? 4;
    const active = visual.active ?? 0;
    const offColor = withAlpha(t.dim ?? t.muted, "55");
    return h_flex()
      .h(13)
      .gap(1.5)
      .items_end()
      .flex_none()
      .children(
        heights.slice(0, bars).map((bh, i) =>
          div()
            .w(2)
            .h(bh)
            .rounded(1)
            .bg(i < active ? activeColor : offColor),
        ),
      );
  }

  renderSubagentDivider(t, parent) {
    const label = tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`);
    return h_flex()
      .id("acp-subagent-divider")
      .w_full()
      .max_w(LANE_MAX_W)
      .gap(10)
      .items_center()
      .py(2)
      .child(div().flex_1().h(1).bg(t.border))
      .child(
        h_flex()
          .gap(5)
          .px(8)
          .py(2)
          .rounded(999)
          .items_center()
          .text_size(11)
          .text_color(t.dim)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .on_mouse_down("left", stopMouse)
          .on_click(() => run("agent.focus", { terminal: parent.address }))
          .child(svg(ICONS.bot).size(12).flex_none().text_color(t.dim))
          .child(label),
      )
      .child(div().flex_1().h(1).bg(t.border));
  }

  renderUserItem(t, item, currentTerminal) {
    const isDark = Boolean(readSettings()?.dark ?? true);
    const time = formatTime(item.ts_ms);
    let fromLabel = item.from ? `from ${item.from}` : null;
    if (item.from) {
      const slash = typeof currentTerminal === "string" ? currentTerminal.lastIndexOf("/") : -1;
      const prefix = slash >= 0 ? currentTerminal.slice(0, slash + 1) : "";
      const sender = this.frameTerminals().find((x) => x.address === item.from || (prefix && x.address === `${prefix}${item.from}`));
      if (sender?.name && sender.name !== item.from) {
        fromLabel = `from ${sender.name} (${item.from})`;
      }
    }
    const fullBody = item.body || "";
    const attachments = Array.isArray(item.attachments) ? item.attachments : [];
    const mono = readSettings()?.font_family ?? "JetBrains Mono";
    const isDelegated = Boolean(item.isDelegated ?? item.from);
    const foldKey = `u-fold-${item.id}`;
    const foldOpen = !isDelegated || Boolean(this.expandedSteps?.[foldKey]);
    const lines = fullBody.split("\n");
    const isLong = lines.length > 14 || fullBody.length > 900;
    const moreKey = `u-more-${item.id}`;
    const showAll = !isLong || Boolean(this.expandedSteps?.[moreKey]);
    const displayBody = showAll
      ? fullBody
      : `${(lines.length > 14 ? lines.slice(0, 12).join("\n") : fullBody.slice(0, 720)).trimEnd()}…`;
    const isRich = displayBody.includes("\n") || displayBody.length > 72 || /[`*_#[\]|>]/.test(displayBody) || attachments.length > 0;
    const attachmentsEl = attachments.length > 0 ? this.renderAttachments(t, attachments, item.id, mono, { isUser: true }) : null;
    const rowHoverKey = `row-u-${item.id}`;
    const hovered = Boolean(this.hoveredRows?.[rowHoverKey]);

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .items_end()
      .gap(4)
      .on_hover((on, cx) => {
        if (Boolean(this.hoveredRows[rowHoverKey]) !== on) {
          this.hoveredRows[rowHoverKey] = on;
          cx.notify();
        }
      })
      .when(isDelegated, (el) =>
        el.child(
          h_flex()
            .id(`acp-u-fold-${item.id}`)
            .max_w(620)
            .min_h(32)
            .px(12)
            .py(5)
            .gap(7)
            .items_center()
            .rounded(12)
            .bg(t.message ?? t.raised)
            .border_1()
            .border_color(foldOpen ? withAlpha(t.accent ?? t.border, "66") : t.border)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .on_mouse_down("left", stopMouse)
            .on_click((_e, cx) => {
              this.expandedSteps[foldKey] = !foldOpen;
              cx.notify();
            })
            .child(svg(ICONS.messagesSquare).size(13).flex_none().text_color(t.dim))
            .when(Boolean(fromLabel), (b) => b.child(div().flex_none().font_medium().text_size(11.5).text_color(t.muted).child(fromLabel)))
            .when(Boolean(fromLabel) && !foldOpen, (b) => b.child(div().flex_none().text_size(11.5).text_color(t.dim).child("·")))
            .when(!foldOpen, (b) => b.child(div().max_w(440).truncate().text_size(12.5).text_color(t.foreground).child(item.foldSummary || fullBody)))
            .child(svg(foldOpen ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
        ),
      )
      .when(foldOpen, (el) =>
        el
          .child(
            v_flex()
              .when(isRich, (b) => b.w_full().max_w(588))
              .when(!isRich, (b) => b.max_w(588))
              .min_w(0)
              .px(15)
              .py(10)
              .rounded(18)
              .rounded_br(6)
              .bg(t.message ?? t.raised)
              .when(!isDark, (b) => b.border_1().border_color(t.border))
              .text_size(14)
              .line_height("22px")
              .text_color(t.foreground)
              .gap(8)
              .when(Boolean(attachmentsEl), (b) => b.child(attachmentsEl))
              .when(Boolean(displayBody), (b) =>
                b.child(
                  (() => {
                    const linkedUser = linkifyFilePaths(displayBody);
                    if (isRich || linkedUser !== displayBody) {
                      return TextView.markdown(`acp-u-${item.id}`, linkedUser, this.mdOptions(t, this.term()?.cwd))
                        .w_full()
                        .min_w(0)
                        .selectable(true)
                        .scrollable(false)
                        .on_link_click((url, cx, ev) => this.handleMarkdownLinkClick(url, ev, cx));
                    }
                    return div().whitespace_normal().child(displayBody);
                  })(),
                ),
              )
              .when(isLong, (b) =>
                b.child(
                  h_flex()
                    .justify_end()
                    .child(
                      div()
                        .id(`acp-u-toggle-${item.id}`)
                        .px(6)
                        .py(2)
                        .rounded(6)
                        .text_size(11)
                        .font_medium()
                        .text_color(t.accent ?? t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          this.expandedSteps[moreKey] = !showAll;
                          cx.notify();
                        })
                        .child(showAll ? tr("Show less", "收起") : tr("Show more", "展开全部")),
                    ),
                ),
              ),
          )
          .when(Boolean(fullBody) || Boolean(time), (b) => {
            const canRewind = item.turn >= 1 && item.turn < (this.maxConversationTurn || 0) && this.status() !== "working" && this.term()?.state !== "finished";
            const toTurn = Number(item.turn ?? 0);
            const confirmThis = this.rollbackPromptItem?.id === item.id;
            return b.child(
              v_flex()
                .items_end()
                .gap(6)
                .child(
                  h_flex()
                    .px(4)
                    .gap(6)
                    .items_center()
                    .opacity(hovered || confirmThis || this.copiedId === `u-${item.id}` ? 1 : 0.45)
                    .when(Boolean(time), (meta) => meta.child(div().text_size(11).text_color(t.dim).child(time)))
                    .when(Boolean(fullBody), (meta) => meta.child(this.copyPill(t, `u-${item.id}`, fullBody, tr("Copy message", "复制消息"), true)))
                    .when(canRewind, (meta) =>
                      meta.child(
                        div()
                          .id(`acp-edit-from-${item.id}`)
                          .px(7)
                          .py(2)
                          .rounded(6)
                          .border_1()
                          .border_color(confirmThis ? (t.accent ?? t.border) : t.border)
                          .text_size(11)
                          .text_color(confirmThis ? (t.accent ?? t.foreground) : t.muted)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover))
                          .on_mouse_down("left", stopMouse)
                          .on_click((_e, cx) => this.openRollbackConfirm(item, toTurn, fullBody, cx))
                          .child(tr("↺ Restore here", "↺ 恢复到此轮")),
                      ),
                    )
                    .when(Boolean(currentTerminal) && Number(item.turn) >= 1 && !this.isSideChatSession(), (meta) =>
                      meta.child(
                        div()
                          .id(`acp-fork-from-${item.id}`)
                          .px(7)
                          .py(2)
                          .rounded(6)
                          .border_1()
                          .border_color(t.border)
                          .text_size(11)
                          .text_color(t.muted)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.foreground))
                          .tooltip(tr(`Fork a new session up to turn ${item.turn}`, `从第 ${item.turn} 轮创建会话分叉`))
                          .on_mouse_down("left", stopMouse)
                          .on_click((_e, cx) => {
                            cx.stop_propagation();
                            run("agent.handoff", { source: currentTerminal, kind: "fork", to_turn: Number(item.turn) });
                          })
                          .child(tr("⑂ Fork here", "⑂ 从此轮分叉")),
                      ),
                    ),
                )
                .when(confirmThis, (panel) => panel.child(this.renderRollbackConfirmPanel(t, item, toTurn, fullBody))),
            );
          }),
      );
  }

  renderRollbackConfirmPanel(t, item, toTurn, promptText) {
    const iso = this.isolation;
    const isoLevel = iso?.level || (this.rollbackPromptItem?.checkingIsolation ? "checking" : "unknown");
    const filesBlocked = isoLevel === "shared_blocked" || (!this.rollbackPromptItem?.checkingIsolation && !this.hasCheckpointTurn(toTurn));
    const isDark = Boolean(readSettings()?.dark ?? true);
    const amber = isDark ? "#fbbf24" : "#b45309";
    const danger = isDark ? "#f87171" : "#dc2626";
    const blockersText = Array.isArray(iso?.blockers) ? iso.blockers.map((x) => x.detail || x.addr).join(", ") : "";
    const isoNote = isoLevel === "checking"
      ? tr("Checking workspace isolation safety…", "正在检查工作区并发与安全隔离状态…")
      : filesBlocked
      ? (isoLevel === "shared_blocked"
        ? tr(`Blocked by active peer (${blockersText}) — session-only restore to turn ${toTurn}`, `同工作区存在活跃窗格（${blockersText}），仅允许将对话恢复至第 ${toTurn} 轮`)
        : tr(`No file checkpoint recorded for turn ${toTurn}`, `第 ${toTurn} 轮无文件快照，仅支持恢复对话记忆`))
      : isoLevel === "exclusive_confirm"
        ? tr(`Main checkout — keep turn ${toTurn} & earlier, revert edits & turns after turn ${toTurn}`, `主仓目录：保留第 ${toTurn} 轮及之前会话，撤回后续会话与文件修改`)
        : tr(`Isolated worktree — keep turn ${toTurn} & earlier, remove turns & edits after turn ${toTurn}`, `独立工作树：保留第 ${toTurn} 轮及之前会话，撤回后续会话与文件修改`);
    return v_flex()
      .w_full()
      .max_w(540)
      .p(10)
      .gap(8)
      .rounded(10)
      .border_1()
      .border_color(t.border)
      .bg(t.popover ?? t.surface ?? t.raised)
      .child(
        div()
          .text_size(11.5)
          .text_color(filesBlocked ? danger : isoLevel === "exclusive_confirm" ? amber : t.muted)
          .child(isoNote),
      )
      .when(Boolean(this.rollbackError), (p) =>
        p.child(div().text_size(11.5).text_color(danger).child(this.rollbackError)),
      )
      .child(
        h_flex()
          .gap(8)
          .flex_wrap()
          .items_center()
          .child(
            div()
              .id(`acp-rb-both-${item.id}`)
              .px(10)
              .py(5)
              .rounded(7)
              .bg(filesBlocked || this.rollbackBusy ? t.raised : (t.solid ?? t.accent))
              .text_size(11.5)
              .font_medium()
              .text_color(filesBlocked || this.rollbackBusy ? t.dim : (t.solid_foreground ?? "#ffffff"))
              .when(!filesBlocked && !this.rollbackBusy, (el) =>
                el.cursor_pointer().on_mouse_down("left", stopMouse).on_click((_e, cx) =>
                  this.executeCheckpointRollback(toTurn, true, promptText, cx),
                ),
              )
              .child(this.rollbackBusy ? tr("Restoring…", "恢复中…") : tr("Restore files & session", "恢复文件与会话")),
          )
          .child(
            div()
              .id(`acp-rb-mem-${item.id}`)
              .px(10)
              .py(5)
              .rounded(7)
              .border_1()
              .border_color(t.border)
              .text_size(11.5)
              .text_color(this.rollbackBusy ? t.dim : t.foreground)
              .when(!this.rollbackBusy, (el) =>
                el.cursor_pointer().hover((s) => s.bg(t.hover)).on_mouse_down("left", stopMouse).on_click((_e, cx) =>
                  this.executeCheckpointRollback(toTurn, false, promptText, cx),
                ),
              )
              .child(tr("Session only (keep files)", "仅恢复会话（保留文件）")),
          )
          .child(
            div()
              .id(`acp-rb-cancel-${item.id}`)
              .px(8)
              .py(5)
              .rounded(7)
              .text_size(11.5)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                this.rollbackPromptItem = null;
                this.rollbackError = null;
                cx.notify();
              })
              .child(tr("Cancel", "取消")),
          ),
      );
  }

  renderToolStep(t, step, mono) {
    const running = step.status === "pending" || step.status === "in_progress";
    const failed = step.status === "failed";
    const iconPath = running ? ICONS.spinner : failed ? ICONS.circleAlert : toolIconPath(step.tool_kind, step.title);
    const summary = step.summaryTitle || step.title || "Tool call";
    const hasLongTitle = Boolean(step.multiline || (step.title && step.title.length > 120));
    const detailBody = step.detail
      ? (hasLongTitle && !step.detail.includes(step.title) ? `${step.title}\n\n${step.detail}` : step.detail)
      : (hasLongTitle ? step.title : "");
    const expandable = Boolean(step.expandable || detailBody);
    const open = Boolean(this.expandedSteps?.[step.id]);
    const iconColor = failed ? (t.blocked ?? "#ef4444") : running ? t.foreground : t.dim;
    const textColor = failed ? (t.blocked ?? "#ef4444") : running ? t.foreground : t.muted;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const addColor = isDark ? "#4ade80" : "#16a34a";
    const delColor = isDark ? "#f87171" : "#dc2626";
    const addBg = isDark ? "#22c55e1a" : "#16a34a14";
    const delBg = isDark ? "#ef44441a" : "#dc262614";
    const parsedDetail = expandable && open && Boolean(detailBody)
      ? parseToolDetail({ tool_kind: step.tool_kind, title: step.title, detail: detailBody })
      : null;

    return v_flex()
      .id(`acp-step-${step.id}`)
      .w_full()
      .min_w(0)
      .gap(4)
      .child(
        h_flex()
          .w_full()
          .min_h(24)
          .px(6)
          .py(2)
          .gap(7)
          .items_center()
          .rounded(6)
          .when(expandable, (el) =>
            el
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                this.expandedSteps[step.id] = !open;
                cx.notify();
              }),
          )
          .when(!expandable, (el) => el.hover((s) => s.bg(withAlpha(t.foreground, "06"))))
          .child(
            div()
              .size(16)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .child(
                svg(iconPath)
                  .size(14)
                  .text_color(iconColor)
                  .when(running, (s) => s.spin(900)),
              ),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(13)
              .when(running, (el) => el.font_medium())
              .text_color(textColor)
              .child(summary),
          )
          .when(Boolean(step.diffStats) && (step.diffStats.additions > 0 || step.diffStats.deletions > 0), (el) =>
            el.child(
              h_flex()
                .gap(4)
                .flex_none()
                .font_family(mono)
                .text_size(11)
                .when(step.diffStats.additions > 0, (s) => s.child(div().text_color(addColor).child(`+${step.diffStats.additions}`)))
                .when(step.diffStats.deletions > 0, (s) => s.child(div().text_color(delColor).child(`-${step.diffStats.deletions}`))),
            ),
          )
          .when(step.extraLines > 0, (el) =>
            el.child(div().text_size(11).text_color(t.dim).flex_none().child(tr(`+${step.extraLines}L`, `+${step.extraLines}行`))),
          )
          .when(Array.isArray(step.attachments) && step.attachments.length > 0, (el) =>
            el.child(
              h_flex()
                .flex_none()
                .px(5)
                .py(1)
                .gap(3)
                .items_center()
                .rounded(5)
                .bg(t.raised)
                .text_size(10.5)
                .text_color(t.muted)
                .child(svg(ICONS.paperclip).size(10).text_color(t.dim))
                .child(String(step.attachments.length)),
            ),
          )
          .when(failed, (el) =>
            el.child(div().text_size(11).font_medium().text_color(t.blocked ?? "#ef4444").flex_none().child(tr("failed", "失败"))),
          )
          .when(expandable, (el) =>
            el.child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
          ),
      )
      .when(Boolean(parsedDetail), (el) => {
        if (parsedDetail.mode === "diff") {
          return el.child(
            v_flex()
              .ml(24)
              .min_w(0)
              .rounded(8)
              .bg(t.code_bg ?? t.popover ?? t.surface)
              .border_1()
              .border_color(t.border)
              .overflow_hidden()
              .child(
                h_flex()
                  .h(26)
                  .pl(10)
                  .pr(6)
                  .items_center()
                  .justify_between()
                  .border_b(1)
                  .border_color(t.border)
                  .child(
                    h_flex()
                      .gap(6)
                      .items_center()
                      .child(svg(ICONS.fileDiff).size(12).text_color(t.dim))
                      .child(div().font_family(mono).text_size(11).text_color(t.muted).child(tr("diff", "差异")))
                      .when(parsedDetail.additions > 0, (s) => s.child(div().font_family(mono).text_size(11).text_color(addColor).child(`+${parsedDetail.additions}`)))
                      .when(parsedDetail.deletions > 0, (s) => s.child(div().font_family(mono).text_size(11).text_color(delColor).child(`-${parsedDetail.deletions}`))),
                  )
                  .child(this.copyPill(t, `tc-${step.id}`, detailBody, tr("Copy", "复制"), true)),
              )
              .child(
                v_flex()
                  .w_full()
                  .min_w(0)
                  .max_h(260)
                  .overflow_y_scrollbar()
                  .py(4)
                  .children(
                    parsedDetail.diffLines.map((dl) => {
                      const bg = dl.kind === "add" ? addBg : dl.kind === "del" ? delBg : null;
                      const fg = dl.kind === "add"
                        ? addColor
                        : dl.kind === "del"
                          ? delColor
                          : dl.kind === "hunk"
                            ? (t.accent ?? t.muted)
                            : dl.kind === "meta"
                              ? t.dim
                              : t.muted;
                      return div()
                        .w_full()
                        .min_w(0)
                        .px(10)
                        .py(1)
                        .when(Boolean(bg), (row) => row.bg(bg))
                        .font_family(mono)
                        .text_size(11.5)
                        .line_height("17px")
                        .text_color(fg)
                        .whitespace_normal()
                        .child(preserveIndentForWrap(dl.text));
                    }),
                  )
                  .when(parsedDetail.truncatedLines > 0, (d) =>
                    d.child(
                      div()
                        .px(10)
                        .py(4)
                        .text_size(11)
                        .text_color(t.dim)
                        .child(tr(`… ${parsedDetail.truncatedLines} more lines`, `… 还有 ${parsedDetail.truncatedLines} 行`)),
                    ),
                  ),
              ),
          );
        }
        if (parsedDetail.mode === "command") {
          return el.child(
            v_flex()
              .ml(24)
              .min_w(0)
              .rounded(8)
              .bg(t.code_bg ?? t.popover ?? t.surface)
              .border_1()
              .border_color(t.border)
              .overflow_hidden()
              .child(
                h_flex()
                  .min_h(28)
                  .pl(10)
                  .pr(6)
                  .py(4)
                  .gap(6)
                  .items_center()
                  .justify_between()
                  .when(Boolean(parsedDetail.output), (b) => b.border_b(1).border_color(t.border))
                  .child(
                    h_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(6)
                      .items_center()
                      .child(div().flex_none().font_family(mono).text_size(11.5).text_color(t.dim).child("$"))
                      .child(div().flex_1().min_w(0).font_family(mono).text_size(11.5).text_color(t.foreground).whitespace_normal().child(parsedDetail.command || step.title)),
                  )
                  .child(this.copyPill(t, `tc-${step.id}`, detailBody, tr("Copy", "复制"), true)),
              )
              .when(Boolean(parsedDetail.output), (b) =>
                b.child(
                  div()
                    .w_full()
                    .min_w(0)
                    .max_h(240)
                    .overflow_y_scrollbar()
                    .px(10)
                    .py(6)
                    .font_family(mono)
                    .text_size(11.5)
                    .line_height("17px")
                    .text_color(t.muted)
                    .whitespace_normal()
                    .child(parsedDetail.output),
                ),
              ),
          );
        }
        return el.child(
          v_flex()
            .ml(24)
            .min_w(0)
            .rounded(8)
            .bg(t.code_bg ?? t.popover ?? t.surface)
            .border_1()
            .border_color(t.border)
            .overflow_hidden()
            .child(
              h_flex()
                .h(26)
                .pl(10)
                .pr(6)
                .items_center()
                .justify_between()
                .child(div().font_family(mono).text_size(11).text_color(t.dim).child(step.tool_kind || tr("detail", "详情")))
                .child(this.copyPill(t, `tc-${step.id}`, detailBody, tr("Copy", "复制"), true)),
            )
            .child(
              div()
                .min_w(0)
                .max_h(240)
                .overflow_y_scrollbar()
                .px(2)
                .pb(2)
                .font_family(mono)
                .text_size(11.5)
                .line_height("18px")
                .text_color(t.muted)
                .child(
                  TextView.markdown(`acp-tc-${step.id}`, `${fenceFor(detailBody)}\n${detailBody}\n${fenceFor(detailBody)}`)
                    .w_full()
                    .min_w(0)
                    .selectable(true)
                    .scrollable(false),
                ),
            ),
        );
      })
      .when(open && Array.isArray(step.attachments) && step.attachments.length > 0, (el) =>
        el.child(
          div()
            .ml(24)
            .min_w(0)
            .child(this.renderAttachments(t, step.attachments, `tc-${step.id}`, mono)),
        ),
      );
  }

  renderThoughtStep(t, step, defaultOpen = false) {
    const open = this.expandedSteps?.[step.id] ?? defaultOpen;
    const preview = step.preview || tr("Reasoning", "推理过程");
    return v_flex()
      .id(`acp-step-${step.id}`)
      .w_full()
      .min_w(0)
      .gap(4)
      .child(
        h_flex()
          .w_full()
          .min_h(24)
          .px(6)
          .py(2)
          .gap(7)
          .items_center()
          .rounded(6)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            this.expandedSteps[step.id] = !open;
            cx.notify();
          })
          .child(
            div()
              .size(16)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .child(svg(ICONS.brain).size(14).text_color(t.dim)),
          )
          .child(div().flex_1().min_w(0).truncate().text_size(13).text_color(t.muted).child(open ? tr("Thought", "思考过程") : preview))
          .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
      )
      .when(open, (el) =>
        el.child(
          div()
            .ml(24)
            .pl(10)
            .py(2)
            .min_w(0)
            .border_l(1)
            .border_color(t.border)
            .text_size(12.5)
            .line_height("19px")
            .text_color(t.muted)
            .child(
              TextView.markdown(`acp-th-${step.id}`, linkifyFilePaths(step.text), this.mdOptions(t, this.term()?.cwd))
                .w_full()
                .min_w(0)
                .selectable(true)
                .scrollable(false)
                .on_link_click((url, cx, ev) => this.handleMarkdownLinkClick(url, ev, cx)),
            ),
        ),
      );
  }

  renderProposedPlanCard(t, plan) {
    if (!plan || !plan.items?.length) return null;
    const parsed = plan.parsedItems ?? plan.items.map((text) => ({ status: "pending", text }));
    const rawText = plan.items.join("\n");
    const canCollapse = parsed.length > 8;
    const expanded = !canCollapse || Boolean(this.expandedSteps?.[`plan-exp-${plan.id}`]);
    const visibleItems = expanded ? parsed : parsed.slice(0, 6);
    return v_flex()
      .id(`acp-plan-${plan.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .rounded(16)
      .border_1()
      .border_color(t.border)
      .bg(withAlpha(t.popover ?? t.surface, "d8"))
      .p(14)
      .gap(10)
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap(8)
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(
                div()
                  .px(7)
                  .py(2)
                  .rounded(6)
                  .bg(t.raised)
                  .text_size(11)
                  .font_medium()
                  .text_color(t.muted)
                  .child(tr("Plan", "计划")),
              )
              .child(div().text_size(13.5).font_semibold().text_color(t.foreground).child(tr("Proposed plan", "执行计划"))),
          )
          .child(this.copyPill(t, `plan-${plan.id}`, rawText, tr("Copy plan", "复制计划"), true)),
      )
      .child(
        v_flex()
          .w_full()
          .gap(6)
          .children(
            visibleItems.map((entry) => {
              const done = entry.status === "completed";
              const active = entry.status === "in_progress";
              return h_flex()
                .w_full()
                .gap(8)
                .items_center()
                .when(done, (el) => el.child(svg(ICONS.circleCheck).size(13).flex_none().text_color(t.done ?? "#10b981")))
                .when(active, (el) => el.child(svg(ICONS.spinner).size(13).flex_none().text_color(t.working ?? t.accent).spin(900)))
                .when(!done && !active, (el) =>
                  el.child(div().size(11).ml(1).mr(1).rounded(999).flex_none().border_1().border_color(t.dim)),
                )
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .text_size(12.5)
                    .line_height("18px")
                    .text_color(done ? t.muted : t.foreground)
                    .child(entry.text),
                );
            }),
          ),
      )
      .when(canCollapse, (el) =>
        el.child(
          h_flex()
            .pt(2)
            .child(
              div()
                .id(`acp-plan-toggle-${plan.id}`)
                .px(8)
                .py(3)
                .rounded(6)
                .text_size(11.5)
                .font_medium()
                .text_color(t.accent ?? t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  this.expandedSteps[`plan-exp-${plan.id}`] = !expanded;
                  cx.notify();
                })
                .child(
                  expanded
                    ? tr("Collapse plan", "收起计划")
                    : tr(`Expand plan (${parsed.length})`, `展开计划 (${parsed.length})`),
                ),
            ),
        ),
      );
  }

  openChangedFileInChanges(item, fileEntry) {
    const rawPath = String(fileEntry?.path ?? "").trim();
    if (!rawPath) return;
    const on = this.activeMachine() ?? null;
    const cwd = this.term()?.cwd || spaces().find((s) => s.active)?.folder || "";
    const homeDir = !on && typeof home === "function" ? String(home() ?? "") : "";
    const absPath = resolveChangedFilePath(rawPath, cwd, homeDir, on) || rawPath;
    const turn = item?.turn ?? null;
    const terminal = this.activeTerminal() ?? null;
    const primaryPatch = extractTurnFilePatch(this.entries, turn, rawPath, cwd, 48000);

    const fromCheckpoint = Boolean(item?.changedFiles?.fromCheckpoint);
    const cardFiles = Array.isArray(item?.changedFiles?.files) ? item.changedFiles.files : [];
    let siblingBudget = 64000;
    const siblingFiles = cardFiles.slice(0, 40).map((sf) => {
      const sfRaw = String(sf?.path ?? "").trim();
      const sfAbs = resolveChangedFilePath(sfRaw, cwd, homeDir, on) || sfRaw;
      const isClicked = sfAbs === absPath || sfRaw === rawPath;
      let patch = "";
      let isCreateOnly = false;
      if (isClicked) {
        patch = primaryPatch.patch;
        isCreateOnly = primaryPatch.isCreateOnly;
      } else if (isAbsoluteFilePath(sfRaw) && siblingBudget > 0) {
        const info = extractTurnFilePatch(this.entries, turn, sfRaw, cwd, Math.min(8000, siblingBudget));
        patch = info.patch;
        isCreateOnly = info.isCreateOnly;
        siblingBudget = Math.max(0, siblingBudget - patch.length);
      }
      return {
        path: sfAbs,
        displayPath: sfRaw,
        ...(fromCheckpoint && !isAbsoluteFilePath(sfRaw) ? { fromCheckpoint: true } : {}),
        additions: Number(sf?.additions ?? 0) || 0,
        deletions: Number(sf?.deletions ?? 0) || 0,
        ...(patch ? { patch } : {}),
        ...(isCreateOnly ? { isCreateOnly: true } : {}),
      };
    });

    let prevSeq = 0;
    try { prevSeq = Number(shared("changes.open_request")?.seq ?? 0) || 0; } catch {}
    const seq = Math.max(Date.now(), prevSeq + 1);
    if (typeof setShared === "function") {
      setShared("changes.open_request", {
        seq,
        ts: Date.now(),
        path: absPath,
        displayPath: rawPath,
        fromCheckpoint: fromCheckpoint && !isAbsoluteFilePath(rawPath),
        cwd,
        on,
        terminal,
        turn,
        additions: Number(fileEntry?.additions ?? 0) || 0,
        deletions: Number(fileEntry?.deletions ?? 0) || 0,
        patch: primaryPatch.patch,
        isCreateOnly: primaryPatch.isCreateOnly,
        files: siblingFiles,
      });
    }
    run("layout.select", { stack: "panel-body", child: "changes" });
    run("layout.show", { id: "panel" });
  }

  renderChangedFilesCard(t, item, mono) {
    const cf = item?.changedFiles;
    if (!cf || !Array.isArray(cf.files) || cf.files.length === 0) return null;
    const key = `cf-${item.id}`;
    const open = Boolean(this.expandedGroups?.[key]);
    const isDark = Boolean(readSettings()?.dark ?? true);
    const addColor = isDark ? "#4ade80" : "#16a34a";
    const delColor = isDark ? "#f87171" : "#dc2626";
    const count = cf.files.length;
    const label = tr(`${count} changed file${count === 1 ? "" : "s"}`, `${count} 个变更文件`);
    const ckptTurn = Number(cf.checkpointTurn ?? 0);
    const hasCkpt = Boolean(cf.fromCheckpoint && ckptTurn > 0);
    const diffOpen = Boolean(hasCkpt && this.checkpointDiffOpen?.[ckptTurn]);
    const diffLoading = Boolean(hasCkpt && this.checkpointDiffLoading?.[ckptTurn]);
    const diffData = hasCkpt ? this.checkpointDiffs?.[ckptTurn] : null;
    return v_flex()
      .id(`acp-changed-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .rounded(12)
      .border_1()
      .border_color(t.border)
      .bg(withAlpha(t.popover ?? t.surface, "b8"))
      .overflow_hidden()
      .child(
        h_flex()
          .h(32)
          .px(12)
          .gap(8)
          .items_center()
          .justify_between()
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            this.expandedGroups[key] = !open;
            cx.notify();
          })
          .child(
            h_flex()
              .flex_1()
              .min_w(0)
              .gap(7)
              .items_center()
              .child(svg(ICONS.fileDiff).size(13).flex_none().text_color(t.dim))
              .child(div().text_size(12).font_medium().text_color(t.foreground).child(label))
              .when(cf.totalAdditions > 0, (s) => s.child(div().font_family(mono).text_size(11).text_color(addColor).child(`+${cf.totalAdditions}`)))
              .when(cf.totalDeletions > 0, (s) => s.child(div().font_family(mono).text_size(11).text_color(delColor).child(`-${cf.totalDeletions}`))),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .when(hasCkpt, (el) =>
                el.child(
                  div()
                    .id(`acp-cf-diff-${item.id}`)
                    .px(7)
                    .py(2)
                    .rounded(5)
                    .border_1()
                    .border_color(t.border)
                    .text_size(10.5)
                    .text_color(diffOpen ? (t.accent ?? t.foreground) : t.muted)
                    .hover((s) => s.bg(t.hover))
                    .on_mouse_down("left", stopMouse)
                    .on_click((_e, cx) => {
                      if (!open) this.expandedGroups[key] = true;
                      this.toggleCheckpointDiff(ckptTurn, cx);
                    })
                    .child(diffLoading ? tr("Loading…", "加载中…") : diffOpen ? tr("Hide diff", "收起 Diff") : tr("Turn diff", "本轮 Diff")),
                ),
              )
              .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
          ),
      )
      .when(open, (el) =>
        el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .border_t(1)
            .border_color(t.border)
            .p(6)
            .gap(2)
            .children(
              cf.files.map((f, fi) => {
                const idx = f.path.lastIndexOf("/");
                const dir = idx >= 0 ? f.path.slice(0, idx + 1) : "";
                const name = idx >= 0 ? f.path.slice(idx + 1) : f.path;
                return h_flex()
                  .id(`acp-cf-file-${item.id}-${fi}`)
                  .w_full()
                  .h(26)
                  .px(8)
                  .gap(8)
                  .items_center()
                  .justify_between()
                  .rounded(6)
                  .cursor_pointer()
                  .tooltip(f.path)
                  .hover((s) => s.bg(t.hover))
                  .on_mouse_down("left", stopMouse)
                  .on_click(() => this.openChangedFileInChanges(item, f))
                  .child(
                    h_flex()
                      .flex_1()
                      .min_w(0)
                      .items_center()
                      .when(Boolean(dir), (s) => s.child(div().truncate().font_family(mono).text_size(11.5).text_color(t.dim).child(dir)))
                      .child(div().truncate().font_family(mono).text_size(11.5).font_medium().text_color(t.foreground).child(name)),
                  )
                  .child(
                    h_flex()
                      .gap(5)
                      .flex_none()
                      .font_family(mono)
                      .text_size(11)
                      .when(f.additions > 0, (s) => s.child(div().text_color(addColor).child(`+${f.additions}`)))
                      .when(f.deletions > 0, (s) => s.child(div().text_color(delColor).child(`-${f.deletions}`))),
                  );
              }),
            )
            .when(diffOpen, (diffBox) => {
              const rawPatch = String(diffData?.patch ?? "");
              const previewLines = rawPatch ? rawPatch.split("\n").slice(0, 180) : [];
              return diffBox.child(
                v_flex()
                  .w_full()
                  .mt(6)
                  .pt(6)
                  .border_t(1)
                  .border_color(t.border)
                  .font_family(mono)
                  .text_size(11)
                  .gap(1)
                  .when(Boolean(diffData?.error), (el) => el.child(div().px(6).text_color(delColor).child(diffData.error)))
                  .when(!diffLoading && !diffData?.error && previewLines.length === 0, (el) =>
                    el.child(div().px(6).text_color(t.dim).child(tr("No text changes in this turn.", "本轮无文本差异"))),
                  )
                  .children(
                    previewLines.map((line) => {
                      const isAdd = line.startsWith("+") && !line.startsWith("+++");
                      const isDel = line.startsWith("-") && !line.startsWith("---");
                      const isHunk = line.startsWith("@@") || line.startsWith("diff --git");
                      return div()
                        .w_full()
                        .px(6)
                        .truncate()
                        .when(isAdd, (e) => e.bg(withAlpha(addColor, "1a")).text_color(addColor))
                        .when(isDel, (e) => e.bg(withAlpha(delColor, "1a")).text_color(delColor))
                        .when(isHunk, (e) => e.bg(t.raised).text_color(t.dim))
                        .when(!isAdd && !isDel && !isHunk, (e) => e.text_color(t.muted))
                        .child(preserveIndentForWrap(line));
                    }),
                  ),
              );
            }),
        ),
      );
  }

  renderHandoffBanner(t, item) {
    const kindMeta = {
      handoff: { label: tr("Handoff", "上下文接力"), color: "#3b82f6" },
      fork: { label: tr("Fork Session", "会话分叉"), color: "#a855f7" },
      "side-chat": { label: tr("Side Chat", "旁路讨论"), color: "#14b8a6" },
      "merge-back": { label: tr("Merge Back", "合回上下文"), color: "#3b82f6" },
      "send-context": { label: tr("Shared Context", "发送上下文"), color: "#10b981" },
    };
    const meta = kindMeta[item.handoff_kind] || { label: item.handoff_kind || tr("Handoff", "上下文接力"), color: t.accent ?? "#3b82f6" };
    const accent = meta.color;
    const srcAddr = resolvePeerAddress(item.from_address, this.activeTerminal() || "");
    const src = srcAddr || "?";
    const prov = item.from_provider ? ` (${item.from_provider})` : "";
    const turns = item.from_turns ? tr(` · turns ${item.from_turns}`, ` · 已携带第 ${item.from_turns} 轮`) : "";
    const hasCtx = Boolean(item.context_markdown && String(item.context_markdown).trim());
    const kb = hasCtx ? `${(String(item.context_markdown).length / 1024).toFixed(1)} KB` : null;
    const injected = (this.maxConversationTurn || 0) >= 1;
    const expandKey = `ho-exp-${item.id}`;
    const expanded = Boolean(this.expandedGroups?.[expandKey]);
    const mdOpts = { ...this.mdOptions(t, this.term()?.cwd), link: accent };

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .my(4)
      .rounded(10)
      .bg(t.raised)
      .border_1()
      .border_color(withAlpha(accent, "55") ?? t.border)
      .overflow_hidden()
      .child(
        h_flex()
          .w_full()
          .min_w(0)
          .px(12)
          .py(8)
          .gap(8)
          .items_center()
          .justify_between()
          .bg(withAlpha(accent, "12") ?? t.raised)
          .child(
            h_flex()
              .flex_1()
              .min_w(0)
              .gap(6)
              .flex_wrap()
              .items_center()
              .child(
                div()
                  .flex_none()
                  .px(6)
                  .py(1)
                  .rounded(4)
                  .bg(withAlpha(accent, "24") ?? t.raised)
                  .border_1()
                  .border_color(withAlpha(accent, "66") ?? t.border)
                  .text_size(10.5)
                  .font_semibold()
                  .text_color(accent)
                  .child(meta.label),
              )
              .child(
                div()
                  .min_w(0)
                  .text_size(11.5)
                  .font_medium()
                  .text_color(t.foreground)
                  .when(Boolean(srcAddr), (el) =>
                    el
                      .cursor_pointer()
                      .hover((s) => s.text_color(accent))
                      .on_mouse_down("left", stopMouse)
                      .on_click(() => run("agent.focus", { terminal: srcAddr })),
                  )
                  .child(`${tr(`From ${src}${prov}`, `来自 ${src}${prov}`)}${turns}`),
              )
              .when(Boolean(kb), (el) =>
                el.child(div().flex_none().text_size(10.5).text_color(t.dim).child(`· ${kb}`)),
              )
              .child(
                div()
                  .flex_none()
                  .px(5)
                  .py(1)
                  .rounded(4)
                  .bg(injected ? "#22c55e1c" : "#f59e0b1c")
                  .border_1()
                  .border_color(injected ? "#22c55e55" : "#f59e0b55")
                  .text_size(10)
                  .font_medium()
                  .text_color(injected ? "#22c55e" : "#f59e0b")
                  .child(
                    injected
                      ? tr("Injected into model", "已注入模型上下文")
                      : tr("Auto-injected on 1st message", "发送首条消息时自动注入"),
                  ),
              ),
          )
          .when(hasCtx, (el) =>
            el.child(
              div()
                .id(`acp-ho-toggle-${item.id}`)
                .flex_none()
                .px(7)
                .py(2)
                .rounded(5)
                .bg(t.background)
                .border_1()
                .border_color(t.border)
                .text_size(10.5)
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  this.expandedGroups[expandKey] = !expanded;
                  cx.notify();
                })
                .child(expanded ? tr("Hide context", "收起上下文") : tr("Inspect context", "查看携带的上下文")),
            ),
          ),
      )
      .when(hasCtx && expanded, (el) =>
        el.child(
          div()
            .w_full()
            .min_w(0)
            .px(12)
            .py(10)
            .border_t(1)
            .border_color(t.border)
            .text_size(12)
            .line_height("19px")
            .text_color(t.muted)
            .child(
              TextView.markdown(`acp-ho-md-${item.id}`, linkifyFilePaths(String(item.context_markdown)), mdOpts)
                .w_full()
                .min_w(0)
                .selectable(true)
                .scrollable(false)
                .on_link_click((url, cx, ev) => this.handleMarkdownLinkClick(url, ev, cx)),
            ),
        ),
      );
  }

  renderRewindDivider(t, item) {
    const toTurn = Number(item.to_turn ?? 0);
    const label = toTurn === 0
      ? (item.restore_files
        ? tr("⏪ Reset to initial state (files & conversation restored)", "⏪ 已撤回至对话起点（文件与上下文已恢复）")
        : tr("⏪ Reset to initial state (conversation cleared)", "⏪ 已撤回至对话起点（仅对话上下文已清空）"))
      : (item.restore_files
        ? tr(`⏪ Restored to turn ${toTurn} (later turns & edits reverted)`, `⏪ 已恢复至第 ${toTurn} 轮（后续会话与文件修改已撤回）`)
        : tr(`⏪ Restored to turn ${toTurn} (later turns removed, files kept)`, `⏪ 已恢复至第 ${toTurn} 轮（后续会话已移除，文件保留）`));
    return h_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .py(6)
      .gap(10)
      .items_center()
      .child(div().flex_1().h(1).bg(t.border))
      .child(div().text_size(11).text_color(t.muted).child(label))
      .child(div().flex_1().h(1).bg(t.border));
  }

  renderActivityGroup(t, item, mono) {
    const open = this.expandedGroups[item.id] ?? item.defaultExpanded;
    const summary = summarizeActivityGroup(item);
    const running = item.runningCount > 0;
    const failed = item.failedCount > 0;
    const time = formatTime(item.ts_ms);
    const nonPlanSteps = item.steps.filter((s) => s.kind !== "plan");
    const onlyPlan = item.steps.length > 0 && nonPlanSteps.length === 0;
    const groupHoverKey = `row-ag-${item.id}`;
    const hovered = Boolean(this.hoveredRows?.[groupHoverKey]);

    if (onlyPlan && item.latestPlan) {
      return v_flex()
        .id(`acp-row-${item.id}`)
        .w_full()
        .max_w(LANE_MAX_W)
        .min_w(0)
        .flex_none()
        .child(this.renderProposedPlanCard(t, item.latestPlan));
    }

    const renderStep = (step) => {
      if (step.kind === "tool_call") return this.renderToolStep(t, step, mono);
      if (step.kind === "thought") return this.renderThoughtStep(t, step, running && item.steps.length === 1);
      if (step.kind === "permission") {
        return h_flex()
          .id(`acp-step-${step.id}`)
          .w_full()
          .min_h(24)
          .px(6)
          .py(2)
          .gap(7)
          .items_center()
          .rounded(6)
          .child(div().size(16).flex_none().flex().items_center().justify_center().child(svg(ICONS.shield).size(13).text_color(t.dim)))
          .child(div().flex_1().min_w(0).truncate().text_size(13).text_color(t.muted).child(step.title))
          .when(Boolean(step.outcome), (el) => el.child(div().text_size(11).text_color(t.dim).child(step.outcome)));
      }
      return null;
    };

    const singleNonPlan = item.steps.length === 1 && nonPlanSteps.length === 1;
    const rawSteps = item.latestPlan ? nonPlanSteps : item.steps;
    const stepLimit = this.expandedStepLimits?.[item.id] ?? DEFAULT_VISIBLE_STEPS;
    const hiddenStepCount = !singleNonPlan && open && rawSteps.length > stepLimit ? rawSteps.length - stepLimit : 0;
    const renderedSteps = (singleNonPlan || open)
      ? (hiddenStepCount > 0 ? rawSteps.slice(hiddenStepCount) : rawSteps)
      : [];
    const stepNodes = renderedSteps.map(renderStep).filter(Boolean);
    if (hiddenStepCount > 0) {
      stepNodes.unshift(
        h_flex()
          .id(`acp-grp-more-${item.id}`)
          .w_full()
          .min_h(24)
          .px(6)
          .py(2)
          .gap(6)
          .items_center()
          .rounded(6)
          .text_size(11.5)
          .text_color(t.accent ?? t.muted)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            this.expandedStepLimits[item.id] = rawSteps.length;
            cx.notify();
          })
          .child(tr(`Show ${hiddenStepCount} earlier steps`, `展开更早的 ${hiddenStepCount} 个步骤`)),
      );
    }
    const primaryStep = nonPlanSteps[0];
    const groupIconPath = running
      ? ICONS.spinner
      : failed
        ? ICONS.circleAlert
        : primaryStep?.kind === "thought"
          ? ICONS.brain
          : primaryStep?.kind === "tool_call"
            ? toolIconPath(primaryStep.tool_kind, primaryStep.title)
            : ICONS.wrench;

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .gap(6)
      .when(singleNonPlan, (el) => el.children(stepNodes))
      .when(!singleNonPlan, (el) =>
        el
          .child(
            h_flex()
              .id(`acp-grp-${item.id}`)
              .w_full()
              .min_h(24)
              .px(6)
              .py(2)
              .gap(7)
              .items_center()
              .rounded(6)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_hover((on, cx) => {
                if (Boolean(this.hoveredRows[groupHoverKey]) !== on) {
                  this.hoveredRows[groupHoverKey] = on;
                  cx.notify();
                }
              })
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                this.expandedGroups[item.id] = !open;
                cx.notify();
              })
              .child(
                div()
                  .size(16)
                  .flex_none()
                  .flex()
                  .items_center()
                  .justify_center()
                  .child(
                    svg(groupIconPath)
                      .size(14)
                      .text_color(failed ? (t.blocked ?? "#ef4444") : running ? t.foreground : t.dim)
                      .when(running, (s) => s.spin(900)),
                  ),
              )
              .child(
                div()
                  .flex_1()
                  .min_w(0)
                  .truncate()
                  .text_size(13)
                  .when(running, (tEl) => tEl.font_medium())
                  .text_color(running ? t.foreground : t.muted)
                  .child(summary),
              )
              .when(Boolean(time), (tEl) => tEl.child(div().text_size(11).text_color(t.dim).opacity(hovered ? 1 : 0.45).flex_none().child(time)))
              .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
          )
          .when(open && stepNodes.length > 0, (wrap) =>
            wrap.child(
              v_flex()
                .w_full()
                .min_w(0)
                .ml(13)
                .pl(11)
                .py(2)
                .gap(2)
                .border_l(1)
                .border_color(t.border)
                .children(stepNodes),
            ),
          ),
      )
      .when(Boolean(item.latestPlan), (el) => el.child(this.renderProposedPlanCard(t, item.latestPlan)))
      .when(Boolean(item.changedFiles), (el) => el.child(this.renderChangedFilesCard(t, item, mono)));
  }

  renderAssistantItem(t, item, mono) {
    const copyTurnKey = `turn-${item.id}`;
    const time = formatTime(item.ts_ms);
    const mdOpts = this.mdOptions(t, this.term()?.cwd);
    const isDark = Boolean(readSettings()?.dark ?? true);
    const rowHoverKey = `row-as-${item.id}`;
    const hovered = Boolean(this.hoveredRows?.[rowHoverKey]);
    const blocks = (item.blocks ?? []).map((block, bi) => {
      if (block.type === "code") {
        const key = `${item.id}-code-${bi}`;
        const fence = fenceFor(block.code);
        const fenced = `${fence}\n${block.code}\n${fence}`;
        const wrapped = Boolean(this.wrappedCodes?.[key]);
        const codeLines = String(block.code ?? "").replace(/\r\n/g, "\n").split("\n");
        return v_flex()
          .id(`acp-codecard-${key}`)
          .w_full()
          .min_w(0)
          .rounded(9)
          .border_1()
          .border_color(t.border)
          .bg(t.code_bg ?? t.popover ?? t.surface)
          .overflow_hidden()
          .child(
            h_flex()
              .h(28)
              .pl(12)
              .pr(6)
              .pt(2)
              .items_center()
              .justify_between()
              .child(div().font_family(mono).text_size(11).text_color(t.dim).child(block.lang || "text"))
              .child(
                h_flex()
                  .gap(4)
                  .items_center()
                  .child(
                    h_flex()
                      .id(`wrap-${key}`)
                      .h(22)
                      .px(5)
                      .items_center()
                      .justify_center()
                      .rounded(6)
                      .cursor_pointer()
                      .when(wrapped, (el) => el.bg(t.selected ?? t.hover))
                      .hover((s) => s.bg(t.hover))
                      .tooltip(wrapped ? tr("Disable line wrap", "关闭自动换行") : tr("Wrap long lines", "自动换行"))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => {
                        this.wrappedCodes[key] = !wrapped;
                        cx.notify();
                      })
                      .child(svg(ICONS.wrapText).size(11).text_color(wrapped ? t.foreground : t.dim)),
                  )
                  .child(this.copyPill(t, key, block.code, tr("Copy", "复制"))),
              ),
          )
          .when(!wrapped, (el) =>
            el.child(
              v_flex()
                .w_full()
                .min_w(0)
                .px(2)
                .pb(2)
                .overflow_hidden()
                .child(
                  div()
                    .min_w(0)
                    .m(-1)
                    .font_family(mono)
                    .text_size(12.5)
                    .child(TextView.markdown(`acp-cd-${key}`, fenced, mdOpts).w_full().min_w(0).selectable(true).scrollable(false)),
                ),
            ),
          )
          .when(wrapped, (el) =>
            el.child(
              v_flex()
                .w_full()
                .min_w(0)
                .px(12)
                .py(8)
                .gap(1)
                .children(
                  codeLines.map((line) =>
                    div()
                      .w_full()
                      .min_w(0)
                      .whitespace_normal()
                      .font_family(mono)
                      .text_size(12)
                      .line_height("19px")
                      .text_color(isDark ? "#e4e4e7" : t.foreground)
                      .child(preserveIndentForWrap(line)),
                  ),
                ),
            ),
          );
      }
      return div()
        .w_full()
        .min_w(0)
        .px(4)
        .text_size(14)
        .line_height("22px")
        .text_color(t.foreground)
        .child(
          TextView.markdown(`acp-md-${item.id}-${bi}`, linkifyFilePaths(block.text), mdOpts)
            .w_full()
            .min_w(0)
            .selectable(true)
            .scrollable(false)
            .on_link_click((url, cx, ev) => this.handleMarkdownLinkClick(url, ev, cx)),
        );
    });

    const attachmentsEl = Array.isArray(item.attachments) && item.attachments.length > 0
      ? this.renderAttachments(t, item.attachments, item.id, mono)
      : null;

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .gap(10)
      .on_hover((on, cx) => {
        if (Boolean(this.hoveredRows[rowHoverKey]) !== on) {
          this.hoveredRows[rowHoverKey] = on;
          cx.notify();
        }
      })
      .children(blocks)
      .when(Boolean(attachmentsEl), (el) => el.child(attachmentsEl))
      .when(Boolean(item.changedFiles), (el) => el.child(this.renderChangedFilesCard(t, item, mono)))
      .when(Boolean(item.isFinalTurnReply) && Boolean(item.text), (el) => {
        const curTerminal = this.activeTerminal();
        const canRewind = item.turn >= 1 && item.turn < (this.maxConversationTurn || 0) && this.status() !== "working" && this.term()?.state !== "finished";
        const toTurn = Number(item.turn ?? 0);
        const confirmThis = this.rollbackPromptItem?.id === item.id;
        return el.child(
          v_flex()
            .w_full()
            .gap(6)
            .child(
              h_flex()
                .w_full()
                .px(4)
                .pt(1)
                .gap(8)
                .items_center()
                .opacity(hovered || confirmThis || this.copiedId === copyTurnKey ? 1 : 0.45)
                .child(this.copyPill(t, copyTurnKey, item.text, tr("Copy", "复制"), true))
                .when(Boolean(time), (meta) => meta.child(div().text_size(11).text_color(t.dim).child(time)))
                .when(canRewind, (meta) =>
                  meta.child(
                    div()
                      .id(`acp-restore-turn-${item.id}`)
                      .px(7)
                      .py(2)
                      .rounded(6)
                      .border_1()
                      .border_color(confirmThis ? (t.accent ?? t.border) : t.border)
                      .text_size(11)
                      .text_color(confirmThis ? (t.accent ?? t.foreground) : t.muted)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.hover))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => this.openRollbackConfirm(item, toTurn, "", cx))
                      .child(tr("↺ Restore here", "↺ 恢复到此轮")),
                  ),
                )
                .when(Boolean(curTerminal) && Number(item.turn) >= 1 && !this.isSideChatSession(), (meta) =>
                  meta.child(
                    div()
                      .id(`acp-fork-turn-${item.id}`)
                      .px(7)
                      .py(2)
                      .rounded(6)
                      .border_1()
                      .border_color(t.border)
                      .text_size(11)
                      .text_color(t.muted)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.hover).text_color(t.foreground))
                      .tooltip(tr(`Fork a new session up to turn ${item.turn}`, `从第 ${item.turn} 轮创建会话分叉`))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => {
                        cx.stop_propagation();
                        run("agent.handoff", { source: curTerminal, kind: "fork", to_turn: Number(item.turn) });
                      })
                      .child(tr("⑂ Fork here", "⑂ 从此轮分叉")),
                  ),
                ),
            )
            .when(confirmThis, (panel) => panel.child(this.renderRollbackConfirmPanel(t, item, toTurn, ""))),
        );
      });
  }

  renderTerminalSnapshot(t, item, terminal, mono) {
    const src = item.text || "";
    const fence = fenceFor(src);
    const fenced = `${fence}\n${src}\n${fence}`;
    const lineLabel = tr(`${item.lineCount ?? 0} lines`, `${item.lineCount ?? 0} 行`);
    const shellBlocks = Array.isArray(item.shellBlocks) ? item.shellBlocks : [];

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .rounded(12)
      .border_1()
      .border_color(t.border)
      .bg(t.popover ?? t.surface)
      .overflow_hidden()
      .child(
        h_flex()
          .h(32)
          .px(12)
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(7)
              .items_center()
              .child(svg(ICONS.terminal).size(12).text_color(t.dim))
              .child(div().text_size(12).font_medium().text_color(t.muted).child(tr("Terminal snapshot", "终端画面快照")))
              .child(div().text_size(11).text_color(t.dim).child(lineLabel)),
          )
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(this.copyPill(t, `ts-${item.id}`, src, tr("Copy", "复制")))
              .when(Boolean(terminal), (el) =>
                el.child(
                  this.pillButton(
                    t,
                    "pty-snap-open-terminal",
                    tr("Open terminal", "打开终端"),
                    () => run("terminal.view", { terminal, view: "terminal" }),
                    { outlined: true, h: 22, px: 8 },
                  ),
                ),
              ),
          ),
      )
      .when(shellBlocks.length > 0, (el) =>
        el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .p(10)
            .gap(8)
            .children(
              shellBlocks.map((sb, idx) => {
                if (sb.type === "output") {
                  const outFence = fenceFor(sb.text);
                  return div()
                    .min_w(0)
                    .font_family(mono)
                    .text_size(12)
                    .child(TextView.markdown(`acp-tso-${item.id}-${idx}`, `${outFence}\n${sb.text}\n${outFence}`).w_full().min_w(0).selectable(true).scrollable(false));
                }
                return v_flex()
                  .w_full()
                  .min_w(0)
                  .gap(6)
                  .child(
                    h_flex()
                      .w_full()
                      .min_h(34)
                      .px(10)
                      .py(6)
                      .gap(8)
                      .items_center()
                      .justify_between()
                      .rounded(8)
                      .border_1()
                      .border_color(t.border)
                      .bg(t.raised)
                      .child(
                        h_flex()
                          .flex_1()
                          .min_w(0)
                          .gap(6)
                          .items_center()
                          .child(div().flex_none().font_family(mono).text_size(12).text_color(t.dim).child("$"))
                          .child(div().flex_1().min_w(0).truncate().font_family(mono).text_size(12).text_color(t.foreground).child(sb.command)),
                      )
                      .child(this.copyPill(t, `cmd-${item.id}-${idx}`, sb.command, tr("Copy command", "复制命令"), true)),
                  )
                  .when(Boolean(sb.output), (outEl) => {
                    const outFence = fenceFor(sb.output);
                    return outEl.child(
                      div()
                        .min_w(0)
                        .font_family(mono)
                        .text_size(11.5)
                        .child(TextView.markdown(`acp-tsc-${item.id}-${idx}`, `${outFence}\n${sb.output}\n${outFence}`).w_full().min_w(0).selectable(true).scrollable(false)),
                    );
                  });
              }),
            ),
        ),
      )
      .when(shellBlocks.length === 0, (el) =>
        el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .px(2)
            .pb(2)
            .overflow_hidden()
            .child(
              div()
                .min_w(0)
                .m(-1)
                .font_family(mono)
                .text_size(12)
                .child(TextView.markdown(`acp-ts-${item.id}`, fenced).w_full().min_w(0).selectable(true).scrollable(false)),
            ),
        ),
      );
  }

  renderErrorItem(t, item) {
    return h_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .px(14)
      .py(10)
      .gap(10)
      .items_start()
      .rounded(12)
      .bg(withAlpha(t.blocked ?? "#ef4444", "12"))
      .border_1()
      .border_color(withAlpha(t.blocked ?? "#ef4444", "44"))
      .child(svg(ICONS.circleAlert).size(15).mt(2).flex_none().text_color(t.blocked ?? "#ef4444"))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(div().text_size(12).font_semibold().text_color(t.blocked ?? "#ef4444").child(tr("Agent error", "智能体错误")))
          .child(div().text_size(13).line_height("19px").text_color(t.foreground).whitespace_normal().child(item.message)),
      );
  }

  renderStderrItem(t, item, mono) {
    const open = this.expandedGroups[item.id] ?? false;
    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_none()
      .gap(4)
      .child(
        h_flex()
          .id(`acp-se-${item.id}`)
          .w_full()
          .min_h(26)
          .px(6)
          .py(2)
          .gap(8)
          .items_center()
          .rounded(6)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            this.expandedGroups[item.id] = !open;
            cx.notify();
          })
          .child(div().size(16).flex_none().flex().items_center().justify_center().child(svg(ICONS.terminal).size(13).text_color(t.dim)))
          .child(div().flex_1().text_size(12).text_color(t.dim).child(tr(`Diagnostic stderr (${item.lineCount} line${item.lineCount === 1 ? "" : "s"})`, `诊断日志 stderr（${item.lineCount} 行）`)))
          .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
      )
      .when(open, (el) =>
        el.child(
          v_flex()
            .ml(24)
            .min_w(0)
            .rounded(8)
            .bg(t.popover ?? t.surface)
            .border_1()
            .border_color(t.border)
            .overflow_hidden()
            .child(
              div()
                .min_w(0)
                .m(-1)
                .font_family(mono)
                .text_size(11.5)
                .line_height("18px")
                .text_color(t.muted)
                .child(TextView.markdown(`acp-st-${item.id}`, `${fenceFor(item.text)}\n${item.text}\n${fenceFor(item.text)}`).w_full().min_w(0).selectable(true).scrollable(false)),
            ),
        ),
      );
  }

  renderEmptyState(t, k, program, agentName, shortCwd, st) {
    const folderName = (shortCwd || "").split(/[\\/]/).filter(Boolean).pop() || "";
    const starting = st === "starting" || (!this.isPtyMode() && k?.state === "live" && !k?.acp_session_id && !this.agentDetail?.acp_session_id && st !== "idle" && st !== "done");
    return v_flex()
      .w_full()
      .max_w(LANE_MAX_W)
      .min_w(0)
      .flex_1()
      .min_h(220)
      .py(36)
      .px(16)
      .gap(12)
      .items_center()
      .justify_center()
      .child(this.avatarTile(t, program, null, 38, 20))
      .child(
        h_flex()
          .gap(6)
          .flex_wrap()
          .justify_center()
          .items_baseline()
          .child(div().text_size(22).text_color(t.muted).child(tr("What should we build", "准备构建什么")))
          .when(Boolean(folderName), (el) =>
            el.child(div().text_size(22).font_semibold().text_color(t.foreground).child(tr(`in ${folderName}?`, `（${folderName}）`))),
          ),
      )
      .when(starting, (el) =>
        el.child(
          h_flex()
            .gap(8)
            .items_center()
            .child(svg(ICONS.spinner).size(13).flex_none().text_color(t.working ?? t.accent ?? t.dim).spin(900))
            .child(
              div()
                .text_size(12.5)
                .line_height("19px")
                .text_color(t.muted)
                .child(tr(`Starting ${agentName} ACP session… Prompts sent now will be queued.`, `正在启动 ${agentName} ACP 会话… 现在发送的消息将自动排队。`)),
            ),
        ),
      )
      .when(!starting, (el) =>
        el.child(
          div()
            .text_size(12.5)
            .line_height("19px")
            .text_color(t.dim)
            .child(this.loadedOnce ? tr("No messages yet. Send a prompt below to begin.", "暂无消息，请在下方输入提示词开始对话。") : tr("Loading conversation…", "正在加载对话…")),
        ),
      );
  }

  renderApprovalBanner(t, perm, terminal, k, isTab = false) {
    const modeOpt = this.configOptionsFor(k).find((o) => o.iconKind === "mode" || o.id === "mode");
    const canSkipAll = Boolean(
      modeOpt?.choices?.some((c) => c.value === "bypassPermissions") &&
      modeOpt.currentValue !== "bypassPermissions" &&
      modeOpt.currentValue !== "dangerously-skip-permissions",
    );
    const padX = isTab ? 20 : 28;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const warnColor = t.blocked ?? "#f59e0b";
    return v_flex()
      .w_full()
      .px(padX)
      .mb(6)
      .flex_none()
      .items_center()
      .child(
        v_flex()
          .w_full()
          .max_w(LANE_MAX_W)
          .px(14)
          .py(11)
          .gap(9)
          .rounded(14)
          .bg(t.glass ?? withAlpha(t.popover ?? t.surface, isDark ? "e4" : "f2"))
          .border_1()
          .border_color(withAlpha(warnColor, "77"))
          .shadow_lg()
          .on_mouse_down("left", stopMouse)
          .on_scroll_wheel(stopScroll)
          .child(
            h_flex()
              .w_full()
              .gap(8)
              .items_center()
              .child(svg(ICONS.shield).size(15).flex_none().text_color(warnColor))
              .child(div().text_size(12.5).font_semibold().text_color(warnColor).child(tr("Approval requested", "等待审批")))
              .child(div().flex_1().min_w(0).truncate().text_size(12.5).font_medium().text_color(t.foreground).child(perm.title || tr("Permission approval required", "需要权限确认"))),
          )
          .child(
            h_flex()
              .gap(6)
              .flex_wrap()
              .children(
                (perm.options ?? []).map((opt, idx) =>
                  this.pillButton(
                    t,
                    `perm-opt-${idx}`,
                    opt.name || opt.option_id,
                    () => this.runH2(["approve", "--json", terminal, "--option", opt.option_id]),
                    { primary: idx === 0, outlined: idx !== 0, h: 28, px: 11 },
                  ),
                ),
              )
              .when(!(perm.options?.length), (b) =>
                b.child(this.pillButton(t, "perm-allow", tr("Approve", "允许"), () => this.runH2(["approve", "--json", terminal]), { primary: true, h: 28, px: 11 })),
              )
              .when(canSkipAll, (b) =>
                b.child(
                  this.pillButton(
                    t,
                    "perm-skip-all",
                    "dangerously-skip-permissions",
                    (_e, cx) => this.setConfigOption(modeOpt, "bypassPermissions", cx),
                    { outlined: true, h: 28, px: 11 },
                  ),
                ),
              )
              .child(this.pillButton(t, "perm-deny", tr("Deny", "拒绝"), () => this.runH2(["deny", "--json", terminal]), { danger: true, outlined: true, h: 28, px: 11 })),
          ),
      );
  }

  capabilityIcon(opt) {
    if (opt.iconKind === "model") return ICONS.sparkles;
    if (opt.iconKind === "thinking") return ICONS.brain;
    if (opt.iconKind === "fast") return ICONS.zap;
    if (opt.iconKind === "mode") return ICONS.shield;
    return ICONS.layers;
  }

  localizeThinkingLabel(value, fallbackName) {
    const key = String(value ?? "").trim().toLowerCase();
    const map = {
      off: tr("Off", "关闭"),
      none: tr("None", "关闭"),
      disabled: tr("Disabled", "关闭"),
      on: tr("On", "开启"),
      enabled: tr("Enabled", "开启"),
      minimal: tr("Minimal", "极低"),
      low: tr("Low", "低"),
      medium: tr("Medium", "中"),
      med: tr("Medium", "中"),
      high: tr("High", "高"),
      xhigh: tr("Extra High", "极高"),
      "extra-high": tr("Extra High", "极高"),
      extra_high: tr("Extra High", "极高"),
      max: tr("Max", "最高"),
      ultra: tr("Ultra", "极致"),
      ultrathink: tr("Ultrathink", "极致"),
    };
    return map[key] ?? (fallbackName || String(value ?? ""));
  }

  capabilityTitle(opt) {
    if (opt.iconKind === "mode") return tr(opt.name, "权限模式");
    if (opt.iconKind === "thinking") return tr(opt.name || "Reasoning Effort", "思考强度");
    if (opt.iconKind === "model") return tr(opt.name || "Model", "模型");
    return opt.name;
  }

  renderConfigPopupContent(t, opt, phase) {
    const showSearch = this.shouldShowConfigSearch(opt);
    const query = showSearch ? (this.configFilter?.value?.() ?? "") : "";
    const trimmed = query.trim();
    const filtered = filterConfigChoices(opt.choices, query);
    const hasExact = opt.choices.some((c) => c.value === trimmed);
    const current = String(opt.currentValue ?? "");
    const activeIdx = Math.min(Math.max(this.highlightedChoiceIdx, 0), Math.max(filtered.length - 1, 0));
    const rows = [];
    let lastGroup = null;
    let estListH = 8;

    filtered.forEach((choice, idx) => {
      if (choice.group && choice.group !== lastGroup) {
        lastGroup = choice.group;
        estListH += 26;
        rows.push(
          div()
            .id(`acp-cfg-grp-${opt.id}-${idx}`)
            .flex_none()
            .px(8)
            .pt(6)
            .pb(2)
            .text_size(10.5)
            .font_semibold()
            .text_color(t.dim)
            .child(choice.group),
        );
      }
      const selected = current === choice.value;
      const highlighted = idx === activeIdx;
      const isBypassChoice = choice.name === "dangerously-skip-permissions"
        || choice.value === "bypassPermissions"
        || choice.value === "dangerously-skip-permissions";
      const displayName = opt.iconKind === "thinking"
        ? this.localizeThinkingLabel(choice.value, choice.name)
        : choice.name;
      const showRawValue = !isBypassChoice
        && choice.value !== displayName
        && choice.value.toLowerCase() !== displayName.toLowerCase();
      const choiceDesc = isBypassChoice
        ? tr(choice.description || "Skip all permission checks (bypassPermissions)", "跳过所有权限检查 (bypassPermissions)")
        : choice.description;
      const titleColor = isBypassChoice ? (t.blocked ?? "#f59e0b") : t.foreground;
      estListH += choiceDesc ? 48 : 34;
      rows.push(
        h_flex()
          .id(`acp-cfg-choice-${opt.id}-${idx}`)
          .w_full()
          .flex_none()
          .px(9)
          .py(6)
          .gap(8)
          .items_center()
          .justify_between()
          .rounded(8)
          .cursor_pointer()
          .transition("opacity", 90)
          .when(selected || highlighted, (el) => el.bg(selected ? (t.selected ?? t.raised) : t.hover))
          .hover((s) => s.bg(t.hover))
          .active((s) => s.opacity(0.78))
          .on_hover((on, cx) => {
            if (on && this.highlightedChoiceIdx !== idx) {
              this.highlightedChoiceIdx = idx;
              cx.notify();
            }
          })
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => this.setConfigOption(opt, choice.value, cx))
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(1)
              .child(
                h_flex()
                  .w_full()
                  .min_w(0)
                  .gap(6)
                  .items_center()
                  .when(isBypassChoice, (el) => el.child(svg(ICONS.shield).size(13).flex_none().text_color(titleColor)))
                  .child(
                    div()
                      .min_w(0)
                      .flex_initial()
                      .truncate()
                      .text_size(12.5)
                      .when(!isBypassChoice, (el) => el.font_medium())
                      .when(isBypassChoice, (el) => el.font_semibold())
                      .text_color(titleColor)
                      .child(displayName),
                  )
                  .when(showRawValue, (el) =>
                    el.child(div().min_w(0).max_w(135).flex_none().truncate().text_size(11).text_color(t.dim).child(choice.value)),
                  ),
              )
              .when(Boolean(choiceDesc), (el) =>
                el.child(div().w_full().min_w(0).truncate().text_size(11).text_color(t.dim).child(choiceDesc)),
              ),
          )
          .when(selected, (el) => el.child(svg(ICONS.check).size(13).flex_none().text_color(t.accent ?? t.foreground))),
      );
    });

    if (showSearch && trimmed && !hasExact) {
      estListH += 36;
      rows.push(
        h_flex()
          .id("acp-cfg-apply-custom")
          .w_full()
          .flex_none()
          .px(9)
          .py(6)
          .gap(8)
          .items_center()
          .rounded(8)
          .cursor_pointer()
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .hover((s) => s.bg(t.hover))
          .active((s) => s.opacity(0.78))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => this.setConfigOption(opt, trimmed, cx))
          .child(div().text_size(12).font_medium().text_color(t.accent ?? t.foreground).child(`Apply custom value "${trimmed}"`)),
      );
    }

    if (rows.length === 0) {
      rows.push(
        div()
          .px(9)
          .py(10)
          .text_size(12)
          .text_color(t.dim)
          .child(opt.kind === "custom_text" ? tr("Type any custom model identifier and press Enter.", "输入自定义模型名称并按 Enter 确认。") : tr("No matching options.", "无匹配项。")),
      );
    }

    const closing = phase === "closing";
    const targetOpacity = phase === "open" ? 1 : 0;
    const targetTop = phase === "entering" ? 10 : closing ? 8 : 0;
    const headerTitle = this.capabilityTitle(opt);
    const headerDesc = opt.iconKind === "mode" && opt.description
      ? tr(opt.description, "当前 ACP 会话的权限策略")
      : opt.description;

    const maxListH = showSearch ? 320 : 420;
    const listH = Math.min(Math.max(estListH, 40), maxListH);

    return div()
      .pb(6)
      .on_mouse_down("left", stopMouse)
      .on_scroll_wheel((_e, cx) => cx.stop_propagation())
      .child(
        v_flex()
          .id(`acp-cfg-popover-${opt.id}`)
          .relative()
          .top(targetTop)
          .opacity(targetOpacity)
          .transition("opacity", { duration: closing ? 130 : 150, easing: closing ? "ease-in" : "ease-out" })
          .spring("top", closing ? { response: 140, damping: 0.96 } : { response: 175, damping: 0.84 })
          .w(opt.iconKind === "model" ? 380 : opt.iconKind === "mode" ? 360 : 320)
          .p(8)
          .gap(6)
          .rounded(14)
          .border_1()
          .border_color(t.border)
          .bg(t.popover ?? t.surface)
          .shadow_xl()
          .when(!showSearch, (el) => el.track_focus(this.menuFocus))
          .on_key_down((e, cx) => {
            if (e.key === "escape") {
              this.closeConfigPopover(cx);
              cx.stop_propagation();
              return;
            }
            if (!filtered.length) return;
            if (e.key === "down") {
              this.highlightedChoiceIdx = (activeIdx + 1) % filtered.length;
              cx.stop_propagation();
              cx.notify();
            } else if (e.key === "up") {
              this.highlightedChoiceIdx = (activeIdx + filtered.length - 1) % filtered.length;
              cx.stop_propagation();
              cx.notify();
            } else if (e.key === "enter" && !showSearch) {
              const choice = filtered[activeIdx];
              if (choice) this.setConfigOption(opt, choice.value, cx);
              cx.stop_propagation();
            }
          })
          .child(
            h_flex()
              .w_full()
              .px(4)
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                v_flex()
                  .flex_1()
                  .min_w(0)
                  .gap(1)
                  .child(div().text_size(12.5).font_semibold().text_color(t.foreground).child(headerTitle))
                  .when(Boolean(headerDesc), (el) =>
                    el.child(div().truncate().text_size(11).text_color(t.dim).child(headerDesc)),
                  ),
              )
              .child(
                this.pillButton(
                  t,
                  "acp-cfg-close",
                  svg(ICONS.x).size(12),
                  (_e, cx) => this.closeConfigPopover(cx),
                  { h: 24, px: 6 },
                ),
              ),
          )
          .when(showSearch, (el) =>
            el.child(
              h_flex()
                .id("acp-cfg-filter-wrap")
                .w_full()
                .h(32)
                .px(10)
                .gap(7)
                .items_center()
                .rounded(9)
                .border_1()
                .border_color(this.configFilterFocused ? (t.accent ?? t.input) : t.border)
                .bg(t.background)
                .child(svg(ICONS.search).size(12).flex_none().text_color(t.dim))
                .child(Input.new(this.configFilter).flex_1().text_size(12.5).text_color(t.foreground)),
            ),
          )
          .child(
            div()
              .id("acp-cfg-list")
              .w_full()
              .h(listH)
              .max_h(maxListH)
              .overflow_y_scrollbar()
              .child(
                v_flex()
                  .w_full()
                  .gap(2)
                  .children(rows),
              ),
          ),
      );
  }

  renderCapabilityBar(t, opts, k) {
    const isPty = this.isPtyMode();
    const ptyBadges = isPty ? buildPtySessionBadges(this.sessionMeta, k) : [];
    if (!isPty && !opts.length) return null;

    return h_flex()
      .id("acp-cfg-bar")
      .flex_1()
      .min_w(0)
      .flex_wrap()
      .gap(2)
      .items_center()
      .when(isPty, (el) =>
        el
          .child(
            h_flex()
              .id("acp-switch-terminal")
              .h(28)
              .px(8)
              .gap(5)
              .items_center()
              .rounded(8)
              .text_size(12)
              .font_medium()
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(withShortcut(tr("Switch back to raw terminal view", "切回原始终端视图"), "terminal.view:toggle"))
              .on_mouse_down("left", stopMouse)
              .on_click(() => run("terminal.view", { terminal: this.activeTerminal(), view: "terminal" }))
              .child(svg(ICONS.terminal).size(13).flex_none().text_color(t.dim))
              .child(tr("Terminal view", "终端视图")),
          )
          .child(
            div()
              .px(6)
              .py(2)
              .rounded(6)
              .bg(t.raised)
              .text_size(11)
              .text_color(t.dim)
              .child(tr("CLI Agent", "终端智能体")),
          )
          .children(
            ptyBadges.map((b) => {
              const effortNode = b.effortVisual
                ? this.renderEffortGlyph(t, b.effortVisual, t.foreground)
                : null;
              const bIcon = b.iconKind === "model"
                ? (k?.program?.icon && k.program.tint ? k.program.icon : ICONS.sparkles)
                : b.iconKind === "thinking"
                  ? ICONS.brain
                  : ICONS.shield;
              return h_flex()
                .id(`pty-badge-${b.id}`)
                .h(28)
                .px(8)
                .gap(5)
                .items_center()
                .rounded(8)
                .text_size(12)
                .text_color(b.iconKind === "model" ? t.foreground : t.muted)
                .tooltip(b.tooltip)
                .child(effortNode ?? svg(bIcon).size(13).flex_none().text_color(t.dim))
                .child(div().max_w(180).truncate().child(b.label));
            }),
          ),
      )
      .children(
        opts.map((opt) => {
          const isBool = opt.kind === "boolean";
          const isModel = opt.iconKind === "model";
          const isBypassMode = opt.iconKind === "mode"
            && (opt.currentValue === "bypassPermissions" || opt.currentValue === "dangerously-skip-permissions");
          const displayCurrentLabel = opt.iconKind === "thinking"
            ? this.localizeThinkingLabel(opt.currentValue, opt.currentLabel)
            : opt.currentLabel;
          const pillText = opt.iconKind === "thinking" ? displayCurrentLabel : opt.pillLabel;
          const tooltipTitle = this.capabilityTitle(opt);
          const open = this.openConfigId === opt.id;
          const isClosing = !open && Boolean(this.closingConfigs[opt.id]);
          const on = isBool && opt.currentValue === true;
          const active = isBool ? on : open;
          const glyph = this.capabilityIcon(opt);
          const programModelIcon = isModel && k?.program?.icon ? icon(k.program, 13, open ? t.foreground : t.dim) : null;
          const accentText = t.accent ?? t.foreground;
          const warnColor = t.blocked ?? "#f59e0b";
          const glyphColor = isBypassMode
            ? warnColor
            : (isBool && on) || (opt.iconKind === "extra" && opt.isNonDefault)
              ? accentText
              : active
                ? t.foreground
                : t.dim;
          const effortNode = opt.effortVisual
            ? this.renderEffortGlyph(t, opt.effortVisual, active ? t.foreground : (opt.isNonDefault ? accentText : t.muted))
            : null;
          const labelColor = isModel
            ? t.foreground
            : isBypassMode
              ? warnColor
              : (isBool && on) || (opt.iconKind === "extra" && opt.isNonDefault)
                ? accentText
                : active
                  ? t.foreground
                  : t.muted;

          const trigger = h_flex()
            .id(`acp-cfg-${opt.id}`)
            .h(28)
            .px(8)
            .gap(5)
            .items_center()
            .rounded(8)
            .text_size(12.5)
            .transition("opacity", 120)
            .when(isModel || (isBool && on) || isBypassMode, (el) => el.font_medium())
            .when(isBool && on, (el) => el.bg(withAlpha(t.accent ?? "#3b82f6", "1a")))
            .when(isBypassMode && !open, (el) => el.bg(withAlpha(warnColor, "14")))
            .when(!isBool && open, (el) => el.bg(t.selected ?? t.hover))
            .text_color(labelColor)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .active((s) => s.opacity(0.76))
            .tooltip(`${tooltipTitle}: ${displayCurrentLabel}`)
            .on_mouse_down("left", stopMouse)
            .on_click((_e, cx) => {
              if (isBool) {
                this.setConfigOption(opt, !on, cx);
                return;
              }
              if (open) this.closeConfigPopover(cx);
              else this.openConfigPopover(opt, cx);
            })
            .child(programModelIcon ?? effortNode ?? svg(glyph).size(13).flex_none().text_color(glyphColor))
            .child(div().max_w(isBypassMode ? 220 : 190).truncate().child(pillText))
            .when(!isBool, (el) => el.child(svg(open ? ICONS.chevronUp : ICONS.chevronDown).size(11).flex_none().text_color(t.dim)));

          if (isBool) return trigger;

          const phase = open ? (this.enteringConfigId === opt.id ? "entering" : "open") : "closing";
          return Popup.new(`acp-popup-${opt.id}`, trigger)
            .anchor("bottom_left")
            .when(open || isClosing, (el) => el.content(this.renderConfigPopupContent(t, opt, phase)));
        }),
      );
  }

  renderContextStrip(t, k, shortCwd) {
    const addr = this.activeTerminal();
    const ended = k?.state === "finished" || k?.status === "ended";
    const lineageParent = resolvePeerAddress(k?.lineage?.parent || this.agentDetail?.lineage?.parent || "", addr) || null;
    const pInfo = this.framePanes().find((p) => p.terminal === addr) ?? null;
    const aInfo = this.frameAgents().find((a) => a.address === addr) ?? null;
    const allSpaces = this.frameSpaces();
    const sp = allSpaces.find((s) => s.id === (aInfo?.space ?? null))
      ?? allSpaces.find((s) => s.active)
      ?? null;
    const machineName = sp?.machine || tr("Local", "本地");
    const branch = pInfo?.branch || sp?.branch || k?.branch || null;
    const usage = k?.context_usage ?? this.agentDetail?.context_usage ?? this.sessionMeta?.context_usage ?? null;
    const hasUsage = Boolean(usage && typeof usage.size === "number" && usage.size > 0);
    const formatTok = (n) => {
      if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return "0";
      if (n >= 1000000) return `${(n / 1000000).toFixed(1).replace(/\.0$/, "")}M`;
      if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
      return String(Math.round(n));
    };
    const used = usage?.used ?? 0;
    const size = usage?.size ?? 0;
    const pct = hasUsage ? Math.min(100, Math.round((used / size) * 100)) : 0;
    const pctColor = pct >= 85 ? (t.blocked ?? "#ef4444") : pct >= 65 ? (t.done ?? "#f59e0b") : t.dim;
    const accent = t.accent ?? "#3b82f6";

    return h_flex()
      .id("acp-ctx-strip")
      .w_full()
      .max_w(LANE_MAX_W)
      .px(8)
      .pt(6)
      .gap(12)
      .items_center()
      .justify_between()
      .text_size(11.5)
      .text_color(t.dim)
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .gap(10)
          .items_center()
          .child(
            h_flex()
              .gap(4)
              .items_center()
              .flex_none()
              .child(svg(ICONS.server).size(11).text_color(t.dim))
              .child(machineName),
          )
          .when(Boolean(shortCwd), (el) =>
            el.child(
              h_flex()
                .min_w(0)
                .gap(4)
                .items_center()
                .child(svg(ICONS.folder).size(11).flex_none().text_color(t.dim))
                .child(div().min_w(0).truncate().child(shortCwd)),
            ),
          )
          .when(Boolean(branch), (el) =>
            el.child(
              h_flex()
                .gap(4)
                .items_center()
                .flex_none()
                .child(svg(ICONS.gitBranch).size(11).text_color(t.dim))
                .child(div().max_w(140).truncate().child(branch)),
            ),
          )
          .when(Boolean(lineageParent), (el) =>
            el.child(
              h_flex()
                .id("acp-ctx-lineage")
                .gap(4)
                .px(5)
                .py(1)
                .rounded(5)
                .flex_none()
                .items_center()
                .bg(t.raised)
                .border_1()
                .border_color(t.border)
                .text_color(accent)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .tooltip(tr(`Parent session: click to focus ${lineageParent}`, `来源父会话：点击跳转到 ${lineageParent}`))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("agent.focus", { terminal: lineageParent });
                })
                .child(`↳ ${lineageParent}`),
            ),
          ),
      )
      .child(
        h_flex()
          .gap(6)
          .flex_none()
          .items_center()
          .when(Boolean(addr) && !ended && Boolean(lineageParent), (el) =>
            el.child(
              h_flex()
                .id("acp-ctx-merge-back")
                .h(20)
                .px(7)
                .gap(4)
                .items_center()
                .rounded(6)
                .bg(withAlpha(accent, "18") ?? t.raised)
                .border_1()
                .border_color(withAlpha(accent, "66") ?? t.border)
                .text_size(11)
                .font_medium()
                .text_color(accent)
                .cursor_pointer()
                .hover((s) => s.bg(withAlpha(accent, "2a") ?? t.hover))
                .tooltip(tr(`Merge conclusions back into ${lineageParent}`, `将当前旁路/分支结论一键合回 ${lineageParent}`))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("agent.handoff", { source: addr, into: lineageParent, kind: "merge-back" });
                })
                .child(svg(ICONS.cornerUpRight).size(11).flex_none().text_color(accent))
                .child(tr(`Merge → ${lineageParent}`, `合回 ${lineageParent}`)),
            ),
          )
          .when(hasUsage, (el) =>
            el.child(
              h_flex()
                .id("acp-ctx-usage")
                .gap(6)
                .flex_none()
                .items_center()
                .tooltip(tr(`Context window: ${formatTok(used)} / ${formatTok(size)} tokens (${pct}%)`, `上下文占用：${formatTok(used)} / ${formatTok(size)} tokens (${pct}%)`))
                .child(
                  div()
                    .w(28)
                    .h(3.5)
                    .rounded(2)
                    .bg(t.border)
                    .overflow_hidden()
                    .child(div().h_full().w(`${pct}%`).bg(pctColor)),
                )
                .child(div().text_size(11).text_color(pctColor).child(`${pct}% · ${formatTok(used)}/${formatTok(size)}`)),
            ),
          ),
      );
  }

  renderInboxQueue(t, inboxItems, mono) {
    if (!Array.isArray(inboxItems) || inboxItems.length === 0) return null;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const accentSolid = t.accent ?? t.solid ?? "#2563eb";
    const dangerColor = t.blocked ?? "#ef4444";

    return v_flex()
      .id("acp-inbox-queue")
      .w_full()
      .max_w(LANE_MAX_W)
      .mb(6)
      .rounded(14)
      .border_1()
      .border_color(this.editingInboxId != null ? withAlpha(accentSolid, "77") : t.border)
      .bg(t.glass ?? withAlpha(t.popover ?? t.surface, isDark ? "e4" : "f2"))
      .shadow_lg()
      .overflow_hidden()
      .on_mouse_down("left", stopMouse)
      .on_scroll_wheel(stopScroll)
      .child(
        h_flex()
          .id("acp-inbox-header")
          .w_full()
          .min_h(32)
          .px(12)
          .py(5)
          .gap(8)
          .items_center()
          .justify_between()
          .when(!this.queueCollapsed, (el) => el.border_b(1).border_color(withAlpha(t.border, "99")))
          .child(
            h_flex()
              .flex_1()
              .min_w(0)
              .gap(7)
              .items_center()
              .child(svg(ICONS.listTodo).size(13).flex_none().text_color(t.accent ?? t.muted))
              .child(
                div()
                  .flex_none()
                  .text_size(12)
                  .font_medium()
                  .text_color(t.foreground)
                  .child(tr(`Input queue (${inboxItems.length})`, `待发送队列 (${inboxItems.length})`)),
              )
              .child(
                div()
                  .flex_1()
                  .min_w(0)
                  .truncate()
                  .text_size(11)
                  .text_color(t.dim)
                  .child(tr("Sent in order after current turn", "当前对话结束后按序发送")),
              ),
          )
          .child(
            h_flex()
              .id("acp-inbox-toggle")
              .h(22)
              .px(7)
              .gap(4)
              .items_center()
              .rounded(6)
              .flex_none()
              .cursor_pointer()
              .text_size(11)
              .text_color(t.muted)
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                this.queueCollapsed = !this.queueCollapsed;
                cx.notify();
              })
              .child(this.queueCollapsed ? tr("Show", "展开") : tr("Hide", "收起"))
              .child(svg(this.queueCollapsed ? ICONS.chevronUp : ICONS.chevronDown).size(11).text_color(t.dim)),
          ),
      )
      .when(!this.queueCollapsed, (el) =>
        el.child(
          v_flex()
            .id("acp-inbox-list")
            .w_full()
            .max_h(196)
            .overflow_y_scrollbar()
            .p(6)
            .gap(4)
            .children(
              inboxItems.map((item) => {
                const isEditing = Number(this.editingInboxId) === Number(item.id);
                if (isEditing) {
                  const hasEditText = Boolean((this.inboxEditInput?.value?.() ?? "").trim());
                  return v_flex()
                    .id(`acp-inbox-editor-${item.id}`)
                    .w_full()
                    .p(8)
                    .gap(6)
                    .rounded(10)
                    .bg(t.raised)
                    .border_1()
                    .border_color(this.inboxEditFocused ? withAlpha(accentSolid, "90") : t.border)
                    .child(
                      h_flex()
                        .w_full()
                        .gap(6)
                        .items_center()
                        .justify_between()
                        .child(
                          h_flex()
                            .gap(6)
                            .items_center()
                            .child(div().font_family(mono).text_size(11).text_color(t.accent ?? t.muted).child(`#${item.index}`))
                            .child(div().text_size(11.5).font_medium().text_color(t.foreground).child(tr("Editing queued message", "修改队列消息"))),
                        )
                        .child(div().text_size(10.5).text_color(t.dim).child(tr("Enter to save · Esc to cancel", "Enter 保存 · Esc 取消"))),
                    )
                    .child(
                      v_flex()
                        .w_full()
                        .px(9)
                        .py(6)
                        .rounded(8)
                        .bg(t.canvas ?? t.background)
                        .border_1()
                        .border_color(t.border)
                        .cursor_text()
                        .on_click(() => this.inboxEditInput?.focus?.())
                        .child(
                          Textarea.new(this.inboxEditInput)
                            .w_full()
                            .text_size(12.5)
                            .line_height("18px")
                            .text_color(t.foreground),
                        ),
                    )
                    .child(
                      h_flex()
                        .w_full()
                        .gap(6)
                        .justify_end()
                        .items_center()
                        .child(
                          this.pillButton(
                            t,
                            `acp-inbox-cancel-${item.id}`,
                            tr("Cancel", "取消"),
                            (_e, cx) => this.cancelEditInboxItem(cx),
                            { outlined: true, h: 24, px: 9 },
                          ),
                        )
                        .child(
                          this.pillButton(
                            t,
                            `acp-inbox-save-${item.id}`,
                            tr("Save", "保存"),
                            (_e, cx) => this.saveEditInboxItem(cx),
                            { primary: true, h: 24, px: 10, disabled: !hasEditText || this.inboxBusy },
                          ),
                        ),
                    );
                }

                return h_flex()
                  .id(`acp-inbox-item-${item.id}`)
                  .w_full()
                  .min_h(30)
                  .px(8)
                  .py(4)
                  .gap(8)
                  .items_center()
                  .rounded(8)
                  .hover((s) => s.bg(t.hover))
                  .child(
                    div()
                      .flex_none()
                      .px(5)
                      .py(1)
                      .rounded(5)
                      .bg(t.raised)
                      .font_family(mono)
                      .text_size(10.5)
                      .text_color(t.dim)
                      .child(`#${item.index}`),
                  )
                  .when(Boolean(item.fromLabel), (row) =>
                    row.child(
                      div()
                        .flex_none()
                        .max_w(140)
                        .truncate()
                        .px(6)
                        .py(1)
                        .rounded(5)
                        .bg(withAlpha(accentSolid, "16"))
                        .text_size(10.5)
                        .text_color(t.accent ?? t.muted)
                        .child(item.fromLabel),
                    ),
                  )
                  .child(
                    div()
                      .flex_1()
                      .min_w(0)
                      .truncate()
                      .text_size(12.5)
                      .text_color(t.foreground)
                      .cursor_pointer()
                      .tooltip(item.body)
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => this.startEditInboxItem(item, cx))
                      .child(item.preview),
                  )
                  .when(Array.isArray(item.attachments) && item.attachments.length > 0, (row) =>
                    row.child(
                      h_flex()
                        .flex_none()
                        .px(5)
                        .py(1)
                        .gap(3)
                        .items_center()
                        .rounded(5)
                        .bg(t.raised)
                        .text_size(10.5)
                        .text_color(t.muted)
                        .child(svg(ICONS.paperclip).size(10).text_color(t.dim))
                        .child(String(item.attachments.length)),
                    ),
                  )
                  .when(Boolean(item.time), (row) =>
                    row.child(div().flex_none().font_family(mono).text_size(10.5).text_color(t.dim).child(item.time)),
                  )
                  .child(
                    h_flex()
                      .gap(2)
                      .flex_none()
                      .items_center()
                      .child(
                        h_flex()
                          .id(`acp-inbox-send-${item.id}`)
                          .h(22)
                          .px(6)
                          .gap(3)
                          .items_center()
                          .rounded(6)
                          .text_size(11)
                          .font_medium()
                          .text_color(t.accent ?? t.foreground)
                          .when(!this.inboxBusy, (el) => el.cursor_pointer().hover((s) => s.bg(withAlpha(accentSolid, "1a")).text_color(accentSolid)))
                          .tooltip(tr("Interrupt current turn and send immediately", "中断当前输出并立即发送这条消息"))
                          .on_mouse_down("left", stopMouse)
                          .on_click((_e, cx) => this.sendInboxItemNow(item.id, cx))
                          .child(svg(ICONS.arrowUp).size(11))
                          .child(tr("Send now", "立即发送")),
                      )
                      .child(
                        h_flex()
                          .id(`acp-inbox-edit-${item.id}`)
                          .h(22)
                          .px(6)
                          .gap(3)
                          .items_center()
                          .rounded(6)
                          .text_size(11)
                          .text_color(t.muted)
                          .when(!this.inboxBusy, (el) => el.cursor_pointer().hover((s) => s.bg(t.raised).text_color(t.foreground)))
                          .tooltip(tr("Edit queued message", "修改这条队列消息"))
                          .on_mouse_down("left", stopMouse)
                          .on_click((_e, cx) => this.startEditInboxItem(item, cx))
                          .child(svg(ICONS.squarePen).size(11))
                          .child(tr("Edit", "修改")),
                      )
                      .child(
                        h_flex()
                          .id(`acp-inbox-rm-${item.id}`)
                          .h(22)
                          .px(6)
                          .gap(3)
                          .items_center()
                          .rounded(6)
                          .text_size(11)
                          .text_color(t.muted)
                          .when(!this.inboxBusy, (el) => el.cursor_pointer().hover((s) => s.bg(t.raised).text_color(dangerColor)))
                          .tooltip(tr("Remove from queue", "从队列移除"))
                          .on_mouse_down("left", stopMouse)
                          .on_click((_e, cx) => this.removeInboxItem(item.id, cx))
                          .child(svg(ICONS.x).size(11))
                          .child(tr("Remove", "移除")),
                      ),
                  );
              }),
            ),
        ),
      );
  }

  renderDraftAttachments(t, k, mono) {
    const list = Array.isArray(this.draftAttachments) ? this.draftAttachments : [];
    if (list.length === 0) return null;
    const hasImages = list.some((a) => a.kind === "image");
    const caps = k?.prompt_capabilities ?? this.agentDetail?.prompt_capabilities ?? null;
    const imageDowngradeHint = !this.isPtyMode() && hasImages && caps && caps.image === false;
    return v_flex()
      .id("acp-draft-attachments")
      .w_full()
      .px(12)
      .pt(10)
      .pb(4)
      .gap(6)
      .border_b(1)
      .border_color(withAlpha(t.border, "88"))
      .child(
        h_flex()
          .w_full()
          .flex_wrap()
          .gap(8)
          .items_center()
          .children(
            list.map((att, idx) => {
              const sizeStr = formatAttachmentBytes(att.bytes);
              if (att.kind === "image" && att.preview) {
                return div()
                  .id(`acp-draft-att-${idx}`)
                  .relative()
                  .h(56)
                  .min_w(56)
                  .max_w(160)
                  .rounded(8)
                  .overflow_hidden()
                  .border_1()
                  .border_color(t.border)
                  .bg(t.raised)
                  .tooltip(`${att.name}${sizeStr ? ` (${sizeStr})` : ""}`)
                  .child(image(att.preview).h(56))
                  .child(
                    div()
                      .id(`acp-draft-att-rm-${idx}`)
                      .absolute()
                      .top(3)
                      .right(3)
                      .size(18)
                      .rounded(999)
                      .bg(withAlpha(t.popover ?? t.background, "dd"))
                      .border_1()
                      .border_color(t.border)
                      .flex()
                      .items_center()
                      .justify_center()
                      .text_size(11)
                      .text_color(t.foreground)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.blocked ?? "#ef4444").text_color("#ffffff"))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => this.removeDraftAttachment(idx, cx))
                      .child("×"),
                  );
              }
              return h_flex()
                .id(`acp-draft-att-${idx}`)
                .h(30)
                .max_w(220)
                .pl(8)
                .pr(5)
                .gap(5)
                .items_center()
                .rounded(8)
                .border_1()
                .border_color(t.border)
                .bg(t.raised)
                .child(svg(att.kind === "image" ? ICONS.paperclip : ICONS.fileText).size(12).flex_none().text_color(t.accent ?? t.muted))
                .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.foreground).child(att.name))
                .when(Boolean(sizeStr), (el) => el.child(div().flex_none().font_family(mono).text_size(10).text_color(t.dim).child(sizeStr)))
                .child(
                  div()
                    .id(`acp-draft-att-rm-${idx}`)
                    .size(18)
                    .rounded(999)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(11)
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_mouse_down("left", stopMouse)
                    .on_click((_e, cx) => this.removeDraftAttachment(idx, cx))
                    .child("×"),
                );
            }),
          ),
      )
      .when(imageDowngradeHint, (el) =>
        el.child(
          div()
            .text_size(10.5)
            .text_color(t.dim)
            .child(tr("This ACP adapter does not advertise native image input; images will be staged as resource links.", "当前 ACP 适配器未声明原生图片输入能力，图片将自动降级为资源链接并附带文件路径。")),
        ),
      );
  }

  renderPausedInboxBanner(t, pausedItems) {
    if (!Array.isArray(pausedItems) || pausedItems.length === 0) return null;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const accentSolid = t.accent ?? t.solid ?? "#2563eb";
    const preview = String(pausedItems[0]?.body ?? "").replace(/\s+/g, " ").trim();
    const shortPreview = preview.length > 96 ? `${preview.slice(0, 96)}…` : preview;
    const count = pausedItems.length;
    return v_flex()
      .id("acp-paused-inbox")
      .w_full()
      .max_w(LANE_MAX_W)
      .mb(6)
      .px(12)
      .py(8)
      .gap(6)
      .rounded(12)
      .border_1()
      .border_color(withAlpha(accentSolid, "55"))
      .bg(withAlpha(t.popover ?? t.surface, isDark ? "e8" : "f4"))
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap(8)
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(2)
              .child(
                div()
                  .text_size(11.5)
                  .font_semibold()
                  .text_color(t.foreground)
                  .child(
                    tr(
                      `${count} queued prompt${count > 1 ? "s" : ""} paused before restart`,
                      `上次重启前有 ${count} 条排队消息已暂停`,
                    ),
                  ),
              )
              .when(Boolean(shortPreview), (el) =>
                el.child(
                  div()
                    .truncate()
                    .text_size(11)
                    .text_color(t.muted)
                    .child(shortPreview),
                ),
              ),
          )
          .child(
            h_flex()
              .flex_none()
              .gap(4)
              .items_center()
              .child(
                this.pillButton(
                  t,
                  "acp-paused-resume",
                  tr("Continue", "继续发送"),
                  (_e, cx) => this.resumePausedInbox(cx),
                  { h: 24, px: 8, solid: true, accent: accentSolid, disabled: this.inboxBusy },
                ),
              )
              .child(
                this.pillButton(
                  t,
                  "acp-paused-edit",
                  tr("To input", "回填输入框"),
                  (_e, cx) => this.movePausedInboxToInput(pausedItems, cx),
                  { h: 24, px: 8, outlined: true, disabled: this.inboxBusy },
                ),
              )
              .child(
                this.pillButton(
                  t,
                  "acp-paused-discard",
                  tr("Dismiss", "丢弃"),
                  (_e, cx) => this.clearPausedInbox(cx),
                  { h: 24, px: 8, disabled: this.inboxBusy },
                ),
              ),
          ),
      );
  }

  renderComposer(t, k, st, shortCwd, isTab = false, mono = "JetBrains Mono", inboxItems = []) {
    const queued = inboxItems.length > 0 ? inboxItems.length : (k?.inbox_len ?? 0);
    const ended = st === "ended" || k?.state === "finished";
    const pausedItems = !ended && !this.isPtyMode() ? this.pausedInboxItemsFor(k) : [];
    const pausedPanel = pausedItems.length > 0 ? this.renderPausedInboxBanner(t, pausedItems) : null;
    const queuePanel = !ended && inboxItems.length > 0 ? this.renderInboxQueue(t, inboxItems, mono) : null;
    const working = st === "working" && !ended;
    const hasText = Boolean((this.input?.value?.() ?? "").trim());
    const hasAttachments = Array.isArray(this.draftAttachments) && this.draftAttachments.length > 0;
    const hasPayload = hasText || hasAttachments;
    const draftAttachmentsBar = !ended && hasAttachments ? this.renderDraftAttachments(t, k, mono) : null;
    const opts = this.configOptionsFor(k);
    const capBar = ended ? null : this.renderCapabilityBar(t, opts, k);
    const canSend = !ended && hasPayload && !this.busyAction;
    const padX = isTab ? 20 : 28;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const accentSolid = t.accent ?? t.solid ?? "#2563eb";
    const dangerSolid = t.blocked ?? "#ef4444";
    const sendTip = ended
      ? tr("Session ended · transcript preserved", "会话已结束 · 对话记录已保留")
      : working
        ? tr("Queue message (Enter) · Ctrl+Enter steers · Shift+Enter for newline", "加入队列 (Enter) · Ctrl+Enter 插话 · Shift+Enter 换行")
        : tr("Send message (Enter) · Shift+Enter for newline", "发送消息 (Enter) · Shift+Enter 换行");

    return v_flex()
      .id("acp-composer-wrap")
      .w_full()
      .px(padX)
      .pt(2)
      .pb(isTab ? 10 : 14)
      .flex_none()
      .items_center()
      .bg(withAlpha(t.canvas ?? t.background, isDark ? "dc" : "e8"))
      .when(Boolean(pausedPanel), (el) => el.child(pausedPanel))
      .when(Boolean(queuePanel), (el) => el.child(queuePanel))
      .child(
        v_flex()
          .id("acp-composer-card")
          .w_full()
          .max_w(LANE_MAX_W)
          .rounded(18)
          .border_1()
          .border_color(this.inputFocused ? withAlpha(accentSolid, "90") : t.border)
          .bg(t.glass ?? withAlpha(t.popover ?? t.surface, isDark ? "e2" : "f0"))
          .shadow_xl()
          .overflow_hidden()
          .on_mouse_down("left", stopMouse)
          .on_scroll_wheel(stopScroll)
          .child(div().w_full().h(1).bg(isDark ? "#ffffff0e" : "#ffffffcc"))
          .when(this.polishUndoText != null && !ended, (el) =>
            el.child(
              h_flex()
                .id("acp-polish-banner")
                .w_full()
                .px(14)
                .py(5)
                .gap(8)
                .items_center()
                .bg(withAlpha("#a855f7", "14") ?? t.raised)
                .border_b(1)
                .border_color(withAlpha("#a855f7", "38") ?? t.border)
                .child(svg(ICONS.sparkles).size(11.5).flex_none().text_color("#a855f7"))
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .truncate()
                    .text_size(11)
                    .text_color(t.foreground)
                    .child(
                      this.polishSummary
                        ? tr(`Polished by AI · ${this.polishSummary}`, `AI 已润色 · ${this.polishSummary}`)
                        : tr("Polished by AI for clarity", "已由 AI 润色，表达更清晰"),
                    ),
                )
                .child(
                  div()
                    .id("acp-polish-undo")
                    .flex_none()
                    .px(7)
                    .py(2)
                    .rounded(5)
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border)
                    .text_size(10.5)
                    .font_medium()
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .tooltip(withShortcut(tr("Undo last polish and restore original input", "撤销本次润色并恢复润色前的内容"), "prompt.polish_undo"))
                    .on_mouse_down("left", stopMouse)
                    .on_click((_e, cx) => this.revertPolishedPrompt(cx))
                    .child(tr("↩ Undo", "↩ 反悔恢复")),
                )
                .child(
                  div()
                    .id("acp-polish-dismiss")
                    .flex_none()
                    .px(4)
                    .py(1)
                    .rounded(4)
                    .text_size(11)
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_mouse_down("left", stopMouse)
                    .on_click((_e, cx) => {
                      this.polishUndoText = null;
                      this.polishSummary = null;
                      this.polishApplied = false;
                      cx.notify();
                    })
                    .child("×"),
                ),
            ),
          )
          .when(Boolean(draftAttachmentsBar), (el) => el.child(draftAttachmentsBar))
          .child(
            v_flex()
              .id("acp-input-area")
              .w_full()
              .min_h(56)
              .px(16)
              .pt(11)
              .pb(6)
              .cursor_text()
              .on_mouse_down("left", (_e, cx) => {
                if (this.openConfigId) this.closeConfigPopover(cx);
                cx.stop_propagation();
              })
              .on_click((_e, cx) => {
                if (this.openConfigId) this.closeConfigPopover(cx);
                this.input?.focus?.();
              })
              .child(
                Textarea.new(this.input)
                  .w_full()
                  .text_size(14)
                  .line_height("22px")
                  .text_color(t.foreground),
              ),
          )
          .child(
            h_flex()
              .id("acp-composer-footer")
              .w_full()
              .min_h(38)
              .px(10)
              .pb(8)
              .pt(2)
              .gap(8)
              .items_center()
              .justify_between()
              .child(
                capBar ?? h_flex().flex_1().min_w(0).when(ended, (el) =>
                  el.child(div().px(8).py(2).rounded(6).bg(t.raised).text_size(11).text_color(t.muted).child(tr("Session ended · transcript preserved", "会话已结束 · 对话记录已保留"))),
                ),
              )
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .flex_none()
                  .when(queued > 0, (el) =>
                    el.child(
                      div()
                        .id("acp-queued-pill")
                        .px(8)
                        .py(2)
                        .rounded(999)
                        .bg(t.raised)
                        .text_size(11)
                        .text_color(t.accent ?? t.muted)
                        .flex_none()
                        .when(inboxItems.length > 0, (pill) =>
                          pill
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover))
                            .on_mouse_down("left", stopMouse)
                            .on_click((_e, cx) => {
                              this.queueCollapsed = !this.queueCollapsed;
                              cx.notify();
                            }),
                        )
                        .child(tr(`${queued} queued`, `队列 ${queued}`)),
                    ),
                  )
                  .when(!ended, (el) =>
                    el.child(
                      h_flex()
                        .id("acp-attach")
                        .size(28)
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(8)
                        .border_1()
                        .border_color(hasAttachments ? withAlpha(accentSolid, "66") : t.border)
                        .bg(hasAttachments ? withAlpha(accentSolid, "16") : t.raised)
                        .text_color(hasAttachments ? accentSolid : t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover).text_color(t.foreground))
                        .tooltip(tr("Attach image or file (also supports Ctrl+V paste & drag-and-drop)", "附加图片或文件（支持剪贴板粘贴与拖拽）"))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.pickDraftAttachments(cx);
                        })
                        .child(svg(ICONS.paperclip).size(13)),
                    ),
                  )
                  .when(!ended, (el) => {
                    const canPolish = hasText && !this.polishingPrompt && !this.busyAction;
                    const purple = "#a855f7";
                    return el.child(
                      h_flex()
                        .id("acp-polish")
                        .h(28)
                        .px(9)
                        .gap(4)
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(8)
                        .border_1()
                        .border_color(canPolish || this.polishingPrompt ? withAlpha(purple, "66") : t.border)
                        .bg(canPolish || this.polishingPrompt ? withAlpha(purple, "18") : t.raised)
                        .text_size(11.5)
                        .font_medium()
                        .text_color(canPolish || this.polishingPrompt ? purple : t.dim)
                        .when(canPolish, (b) => b.cursor_pointer().hover((s) => s.bg(withAlpha(purple, "2c"))))
                        .tooltip(withShortcut(tr("Polish prompt with Core AI for clarity (one-click undo)", "使用 Core AI 润色提示词，让意图更清晰明确（支持一键还原）"), "prompt.polish"))
                        .on_mouse_down("left", stopMouse)
                        .when(canPolish, (b) =>
                          b.on_click((_e, cx) => {
                            if (this.openConfigId) this.closeConfigPopover(cx);
                            this.polishPrompt(cx);
                          }),
                        )
                        .child(
                          this.polishingPrompt
                            ? svg(ICONS.spinner).size(12).spin(900).text_color(purple)
                            : svg(ICONS.sparkles).size(12).text_color(canPolish ? purple : t.dim),
                        )
                        .child(this.polishingPrompt ? tr("Polishing…", "润色中…") : tr("Polish", "润色")),
                    );
                  })
                  .when(working && hasPayload, (el) =>
                    el.child(
                      h_flex()
                        .id("acp-steer")
                        .h(28)
                        .px(9)
                        .gap(4)
                        .items_center()
                        .justify_center()
                        .rounded(8)
                        .text_size(12)
                        .font_medium()
                        .text_color(t.muted)
                        .when(!this.busyAction, (b) => b.cursor_pointer().hover((s) => s.bg(t.hover).text_color(t.foreground)))
                        .tooltip(tr("Steer current turn immediately (Ctrl+Enter)", "立即插话并重引导 (Ctrl+Enter)"))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.sendPrompt(true);
                        })
                        .child(svg(ICONS.cornerUpRight).size(13))
                        .child(tr("Steer", "插话")),
                    ),
                  )
                  .when(!working || hasPayload, (el) =>
                    el.child(
                      h_flex()
                        .id("acp-send")
                        .size(32)
                        .rounded(999)
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .bg(canSend ? accentSolid : withAlpha(t.foreground, "16"))
                        .text_color(canSend ? "#ffffff" : t.dim)
                        .when(canSend, (btn) => btn.shadow_sm().cursor_pointer().hover((s) => s.opacity(0.9)))
                        .tooltip(sendTip)
                        .when(canSend, (btn) =>
                          btn.on_mouse_down("left", stopMouse).on_click((_e, cx) => {
                            if (this.openConfigId) this.closeConfigPopover(cx);
                            this.sendPrompt(false);
                          }),
                        )
                        .child(svg(working ? ICONS.listPlus : ICONS.arrowUp).size(15)),
                    ),
                  )
                  .when(working, (el) =>
                    el.child(
                      h_flex()
                        .id("acp-stop")
                        .size(32)
                        .rounded(999)
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .bg(dangerSolid)
                        .text_color("#ffffff")
                        .shadow_sm()
                        .when(!this.interrupting, (btn) => btn.cursor_pointer().hover((s) => s.opacity(0.9)))
                        .tooltip(this.interrupting ? tr("Stopping…", "停止中…") : tr("Stop generation (Esc)", "停止生成 (Esc)"))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          if (this.interrupting) return;
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.interruptAgent(this.activeTerminal(), cx);
                        })
                        .child(
                          this.interrupting
                            ? svg(ICONS.spinner).size(13).spin(900)
                            : svg(ICONS.square).size(10),
                        ),
                    ),
                  ),
              ),
          ),
      )
      .child(this.renderContextStrip(t, k, shortCwd));
  }

  renderPtyBlockedBanner(t, terminal, isTab = false) {
    const padX = isTab ? 20 : 28;
    const isDark = Boolean(readSettings()?.dark ?? true);
    const warnColor = t.blocked ?? "#f59e0b";
    return v_flex()
      .id("pty-blocked-banner")
      .w_full()
      .px(padX)
      .mb(6)
      .flex_none()
      .items_center()
      .child(
        h_flex()
          .w_full()
          .max_w(LANE_MAX_W)
          .px(14)
          .py(10)
          .gap(10)
          .items_center()
          .rounded(14)
          .bg(t.glass ?? withAlpha(t.popover ?? t.surface, isDark ? "e4" : "f2"))
          .border_1()
          .border_color(withAlpha(warnColor, "77"))
          .shadow_lg()
          .on_mouse_down("left", stopMouse)
          .on_scroll_wheel(stopScroll)
          .child(svg(ICONS.shield).size(15).flex_none().text_color(warnColor))
          .child(
            div()
              .flex_1()
              .min_w(0)
              .text_size(12.5)
              .font_medium()
              .text_color(t.foreground)
              .child(tr("Terminal is waiting for interactive confirmation", "终端正在等待交互确认")),
          )
          .child(
            this.pillButton(
              t,
              "pty-open-terminal",
              h_flex().gap(5).items_center().child(svg(ICONS.terminal).size(12)).child(tr("Open terminal", "打开终端")),
              () => run("terminal.view", { terminal, view: "terminal" }),
              { outlined: true, h: 26, px: 10 },
            ),
          ),
      );
  }

  renderAcp(t, k, st, pic, parent, cx) {
    const p = this.ownProps();
    const { overlay, tab, pane } = p;
    const isSidePanel = Boolean(p.sidePanel);
    const terminal = this.activeTerminal();
    if (terminal !== this.loadedTerminal) {
      if (this.loadedTerminal) {
        this.writeSavedDraft(this.loadedTerminal, this.input?.value?.() ?? "");
      }
      this.loadedTerminal = terminal;
      this.restoreDraftFor(terminal);
      this.polishUndoText = null;
      this.polishSummary = null;
      this.polishApplied = false;
      this.polishingPrompt = false;
      this.entries = [];
      this.fallbackText = "";
      this.readHow = null;
      this.sessionMeta = null;
      this.agentDetail = null;
      this.lastDetailTerminal = "";
      this.loadedOnce = false;
      this.error = null;
      this.lastKey = "";
      this.lastCkptEpoch = "";
      this.lastReadStdout = "";
      this._itemsCache = null;
      this.visibleItemLimit = DEFAULT_VISIBLE_ITEMS;
      this.expandedStepLimits = {};
      this.reloadEpoch = (this.reloadEpoch || 0) + 1;
    }
    const isTab = Boolean(tab);
    const isPty = this.isPtyMode();
    const isDark = Boolean(readSettings()?.dark ?? true);
    const myPane = pane ?? this.framePanes().find((p) => (p.acp || p.structured) && p.terminal === terminal)?.pane ?? null;
    const ckptEpoch = `${terminal}:${k?.foreground?.pid ?? ""}:${k?.checkpoint_seq ?? 0}:${shared("checkpoint.sync") ?? ""}`;
    let ckptChanged = false;
    if (ckptEpoch !== this.lastCkptEpoch) {
      ckptChanged = this.lastCkptEpoch !== "";
      this.lastCkptEpoch = ckptEpoch;
      this.checkpoints = [];
      this.checkpointDiffs = {};
      this.checkpointDiffOpen = {};
      if (ckptChanged) {
        this.rollbackPromptItem = null;
        this.expandedGroups = {};
        this.expandedStepLimits = {};
        this.autoFollow = true;
        this.revealedSig = "";
        this.optimisticInbox = null;
        this.lastDetailTerminal = "";
        this.lastReadStdout = "";
        this._itemsCache = null;
        this.reloadEpoch = (this.reloadEpoch || 0) + 1;
      }
    }
    const key = `${terminal}:${st}:${k?.turn ?? 0}:${k?.transcript_seq ?? 0}:${k?.foreground?.pid ?? ""}:${k?.checkpoint_seq ?? 0}:${k?.pending_permission?.request_id ?? ""}:${k?.inbox_len ?? 0}:${k?.printed ?? 0}:${k?.status_ms ?? 0}:${shared("checkpoint.sync") ?? ""}`;
    if (!checking() && (ckptChanged || key !== this.lastKey)) {
      this.lastKey = key;
      this.scheduleReload(cx, { forceDetail: ckptChanged });
    }
    if (st === "working") {
      const fromTerm = typeof k?.working_since === "number" && k.working_since > 0 ? k.working_since : null;
      if (fromTerm) {
        this.workingStartedAt = fromTerm;
      } else if (!this.workingStartedAt) {
        this.workingStartedAt = Date.now();
      }
    } else {
      this.workingStartedAt = null;
    }

    const perm = k?.pending_permission;
    const mono = readSettings()?.font_family ?? "JetBrains Mono";
    const inboxItems = isPty ? [] : this.inboxItemsFor(k);
    if (this.editingInboxId != null && !inboxItems.some((it) => Number(it.id) === Number(this.editingInboxId)) && !this.inboxBusy) {
      this.editingInboxId = null;
      this.inboxEditFocused = false;
    }
    const program = k?.program;
    const agentName = k?.labels?.["herdr.name"] || k?.name || k?.template || program?.name || terminal || "ACP Agent";
    const shortCwd = shortenPath(k?.cwd, home());
    const conv = this.getConversationItems(k, st, isPty);
    const items = conv.items;
    this.maxConversationTurn = conv.maxTurn;

    const threadChildren = [];
    if (parent) {
      threadChildren.push(this.renderSubagentDivider(t, parent));
    }
    const visibleItems = conv.visibleItems;
    if (visibleItems.length === 0) {
      threadChildren.push(this.renderEmptyState(t, k, program, agentName, shortCwd, st));
    } else {
      const limit = Math.max(DEFAULT_VISIBLE_ITEMS, this.visibleItemLimit || DEFAULT_VISIBLE_ITEMS);
      const hiddenCount = visibleItems.length > limit ? visibleItems.length - limit : 0;
      const renderedItems = hiddenCount > 0 ? visibleItems.slice(hiddenCount) : visibleItems;
      if (hiddenCount > 0) {
        threadChildren.push(
          h_flex()
            .id("acp-load-earlier")
            .w_full()
            .max_w(LANE_MAX_W)
            .justify_center()
            .py(4)
            .child(
              h_flex()
                .h(26)
                .px(12)
                .gap(6)
                .items_center()
                .rounded(999)
                .bg(t.raised)
                .border_1()
                .border_color(t.border)
                .text_size(11.5)
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, c) => {
                  this.visibleItemLimit = limit + VISIBLE_ITEMS_STEP;
                  c.notify();
                })
                .child(tr(`Show earlier messages (${hiddenCount} hidden)`, `加载更早的对话（已折叠 ${hiddenCount} 条）`)),
            ),
        );
      }
      for (const item of renderedItems) {
        if (item.kind === "user") threadChildren.push(this.renderUserItem(t, item, terminal));
        else if (item.kind === "activity_group") threadChildren.push(this.renderActivityGroup(t, item, mono));
        else if (item.kind === "assistant") threadChildren.push(this.renderAssistantItem(t, item, mono));
        else if (item.kind === "handoff") threadChildren.push(this.renderHandoffBanner(t, item));
        else if (item.kind === "rewind") threadChildren.push(this.renderRewindDivider(t, item));
        else if (item.kind === "terminal_snapshot") threadChildren.push(this.renderTerminalSnapshot(t, item, terminal, mono));
        else if (item.kind === "error") threadChildren.push(this.renderErrorItem(t, item));
      }
    }

    if (st === "working") {
      const lastItem = items[items.length - 1];
      const activeHint = lastItem?.kind === "activity_group" && lastItem.activeTitle
        ? lastItem.activeTitle
        : "";
      const elapsedSec = this.workingStartedAt ? Math.max(0, Math.floor((Date.now() - this.workingStartedAt) / 1000)) : 0;
      const m = Math.floor(elapsedSec / 60);
      const s = String(elapsedSec % 60).padStart(2, "0");
      const workingText = this.interrupting
        ? tr("Stopping…", "正在中断…")
        : activeHint
          ? (elapsedSec >= 1 ? `${activeHint} · ${m}:${s}` : activeHint)
          : (elapsedSec >= 1 ? tr(`Working for ${m}:${s}`, `已运行 ${m}:${s}`) : tr("Working…", "执行中…"));
      threadChildren.push(
        h_flex()
          .id("acp-working-tail")
          .w_full()
          .max_w(LANE_MAX_W)
          .min_w(0)
          .flex_none()
          .px(6)
          .pt(6)
          .pb(6)
          .gap(8)
          .items_center()
          .border_b(1)
          .border_color(withAlpha(t.border, "99"))
          .child(svg(ICONS.spinner).size(13).flex_none().text_color(t.working ?? t.accent).spin(900))
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(12.5)
              .text_color(t.muted)
              .child(workingText),
          )
          .child(div().flex_none().text_size(11).text_color(t.dim).child(tr("Esc to stop", "按 Esc 中断"))),
      );
    }

    const queuePad = inboxItems.length > 0 && st !== "ended" && k?.state !== "finished"
      ? (this.queueCollapsed ? 46 : Math.min(220, 48 + inboxItems.length * 40))
      : 0;
    const overlayPadBottom = (perm || (isPty && st === "blocked") ? 244 : 172) + queuePad;
    threadChildren.push(div().id("acp-bottom-anchor").h(overlayPadBottom).flex_none());

    const last = items[items.length - 1];
    const tailSig = `${items.length}:${last?.id ?? ""}:${last?.text?.length ?? 0}:${last?.steps?.length ?? 0}:${st ?? ""}:${Boolean(perm)}:${inboxItems.length}:${this.queueCollapsed}`;
    if (this.autoFollow && tailSig !== this.revealedSig && threadChildren.length > 0) {
      this.revealedSig = tailSig;
      this.scroll.scroll_to_item(threadChildren.length - 1);
    }

    const padX = isTab ? 20 : 28;
    const isFloating = Boolean(p.floating) || isSidePanel;
    const sideCandidates = isSidePanel ? this.activeSideChatCandidates() : [];
    const modalLineageKind = String(k?.lineage?.kind || this.agentDetail?.lineage?.kind || (isFloating ? "side-chat" : "")).toLowerCase();
    const modalKindBadge = modalLineageKind === "fork"
      ? { label: tr("Fork", "会话分叉"), color: "#a855f7" }
      : (modalLineageKind === "side-chat" || modalLineageKind === "side_chat")
        ? { label: tr("Side Chat", "旁路讨论"), color: "#14b8a6" }
        : modalLineageKind === "handoff"
          ? { label: tr("Handoff", "上下文接力"), color: "#3b82f6" }
          : null;
    const modalWorktree = k?.lineage?.worktree_path || this.agentDetail?.lineage?.worktree_path || null;
    const modalLineageParent = resolvePeerAddress(k?.lineage?.parent || this.agentDetail?.lineage?.parent || p.parent || "", terminal) || null;
    const modalUpstream = modalLineageParent || parent?.address || resolvePeerAddress(k?.parent || "", terminal) || null;
    const modalUpstreamTerm = modalUpstream ? (parent?.address === modalUpstream ? parent : this.frameTerminals().find((x) => x.address === modalUpstream) ?? null) : null;
    const modalUpstreamName = modalUpstreamTerm?.labels?.["herdr.name"] || modalUpstreamTerm?.name || null;
    const modalUpstreamLabel = modalUpstreamName && modalUpstreamName !== modalUpstream ? `${modalUpstreamName} (${modalUpstream})` : modalUpstream;
    const modalEnded = st === "ended" || k?.state === "finished";
    const modalAccent = t.accent ?? "#3b82f6";
    const modalHeader = h_flex()
      .id("acp-header")
      .w_full()
      .min_w(0)
      .min_h(48)
      .px(12)
      .py(6)
      .gap(8)
      .flex_none()
      .items_center()
      .overflow_hidden()
      .bg(t.canvas ?? t.background)
      .border_b(1)
      .border_color(t.border)
      .child(this.avatarTile(t, program, pic, 26, 14))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(
            h_flex()
              .w_full()
              .min_w(0)
              .gap(6)
              .items_center()
              .child(div().min_w(0).truncate().text_size(13).font_semibold().text_color(t.foreground).child(agentName))
              .when(Boolean(modalKindBadge), (el) =>
                el.child(
                  div()
                    .id("acp-modal-kind")
                    .flex_none()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha(modalKindBadge.color, "20") ?? t.raised)
                    .border_1()
                    .border_color(withAlpha(modalKindBadge.color, "66") ?? t.border)
                    .text_size(10)
                    .font_semibold()
                    .text_color(modalKindBadge.color)
                    .child(modalKindBadge.label),
                ),
              )
              .when(modalLineageKind === "side-chat" || modalLineageKind === "side_chat", (el) =>
                el.child(
                  div()
                    .flex_none()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(t.raised)
                    .border_1()
                    .border_color(t.border)
                    .text_size(10)
                    .text_color(t.muted)
                    .tooltip(tr("Read-only discussion mode (will not edit workspace files unless asked)", "只读参谋模式：默认仅分析探讨，不主动修改工作区文件"))
                    .child(tr("Read-only", "只读")),
                ),
              )
              .when(Boolean(modalWorktree), (el) =>
                el.child(
                  div()
                    .flex_none()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha("#f59e0b", "20") ?? t.raised)
                    .border_1()
                    .border_color(withAlpha("#f59e0b", "66") ?? t.border)
                    .text_size(10)
                    .font_semibold()
                    .text_color("#f59e0b")
                    .tooltip(tr(`Isolated Worktree: ${modalWorktree}`, `独立 Worktree：${modalWorktree}`))
                    .child("Worktree"),
                ),
              )
              .when(Boolean(terminal) && !isSidePanel, (el) =>
                el.child(div().font_family(mono).text_size(10.5).text_color(t.dim).flex_none().child(terminal)),
              ),
          )
          .child(
            h_flex()
              .w_full()
              .min_w(0)
              .gap(8)
              .items_center()
              .text_size(11)
              .text_color(t.dim)
              .when(Boolean(modalUpstream), (el) =>
                el.child(
                  h_flex()
                    .id("acp-modal-parent")
                    .flex_none()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha(modalAccent, "14") ?? t.raised)
                    .border_1()
                    .border_color(withAlpha(modalAccent, "44") ?? t.border)
                    .text_color(modalAccent)
                    .cursor_pointer()
                    .hover((s) => s.bg(withAlpha(modalAccent, "28") ?? t.hover).text_color(t.foreground))
                    .tooltip(tr(`Click to return to upstream agent ${modalUpstream}`, `点击返回上游智能体 ${modalUpstream}`))
                    .on_mouse_down("left", stopMouse)
                    .on_click((_e, cx) => {
                      cx.stop_propagation();
                      run("agent.focus", { terminal: modalUpstream });
                      if (isSidePanel) run("layout.hide", { id: "panel" });
                      else if (overlay) closeOverlay(overlay);
                    })
                    .child(tr(`↳ from ${modalUpstreamLabel}`, `↳ 来自 ${modalUpstreamLabel}`)),
                ),
              )
              .when(Boolean(shortCwd) && !isFloating, (el) => el.child(div().min_w(0).truncate().child(shortCwd))),
          ),
      )
      .child(
        h_flex()
          .gap(4)
          .items_center()
          .flex_none()
          .child(this.statusPill(t, st))
          .when(!this.autoFollow, (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-latest",
                tr("↓ Latest", "↓ 最新"),
                (_e, cx) => {
                  this.autoFollow = true;
                  this.revealedSig = "";
                  this.scroll.scroll_to_item(threadChildren.length - 1);
                  cx.notify();
                },
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(!isFloating, (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-refresh",
                h_flex().gap(5).items_center().child(svg(ICONS.refresh).size(12).when(this.loading, (s) => s.spin(900))).child(tr("Refresh", "刷新")),
                (_e, cx) => this.scheduleReload(cx, { forceDetail: true }),
                { h: 26, px: 8 },
              ),
            ),
          )
          .when(isSidePanel && Boolean(terminal) && isPty && !modalEnded, (el) =>
            el.child(
              this.pillButton(
                t,
                "side-to-term",
                h_flex().gap(4).items_center().child(svg(ICONS.terminal).size(12)).child(tr("Terminal", "终端")),
                () => run("terminal.view", { terminal, view: "terminal" }),
                { outlined: true, h: 26, px: 8, tooltip: tr("Switch back to live CLI terminal", "切回实时终端视图") },
              ),
            ),
          )
          .when(Boolean(terminal) && !modalEnded && Boolean(modalLineageParent), (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-merge-back",
                h_flex()
                  .gap(4)
                  .items_center()
                  .child(svg(ICONS.cornerUpRight).size(12))
                  .child(isSidePanel ? tr("Merge", "合回") : isFloating ? tr("Merge & Close", "合回并关闭") : tr(`Merge → ${modalLineageParent}`, `合回 ${modalLineageParent}`)),
                () =>
                  run("agent.handoff", {
                    source: terminal,
                    into: modalLineageParent,
                    kind: "merge-back",
                    ...(!isSidePanel && isFloating && overlay ? { close_overlay: overlay } : {}),
                  }),
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(isFloating && Boolean(modalUpstream), (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-return-main",
                tr("↩ Main", "↩ 回主 Agent"),
                () => {
                  run("agent.focus", { terminal: modalUpstream });
                  if (isSidePanel) run("layout.hide", { id: "panel" });
                  else if (overlay) closeOverlay(overlay);
                },
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(Boolean(terminal) && !modalEnded && !isFloating, (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-handoff",
                h_flex().gap(4).items_center().child(svg(ICONS.gitBranch).size(12)).child(tr("Handoff", "接力")),
                () => run("agent.handoff", { source: terminal, kind: "handoff" }),
                { outlined: true, h: 26, px: 9, tooltip: tr("Context Handoff / Fork Session / Side Chat", "上下文接力 / 会话分叉 / 旁路讨论") },
              ),
            ),
          )
          .when(!isFloating && modalLineageKind !== "side-chat" && modalLineageKind !== "side_chat", (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-mount-tab",
                tr("↗ Open as Tab", "↗ 挂载为标签页"),
                () => {
                  run("terminal.keep", { terminal, place: "tab" });
                  if (overlay) closeOverlay(overlay);
                },
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(!isSidePanel && modalLineageKind !== "side-chat" && modalLineageKind !== "side_chat", (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-split-right",
                isFloating ? tr("Dock", "转分栏") : tr("Split right", "右侧分栏"),
                () => {
                  run("terminal.keep", { terminal, place: "right" });
                  if (overlay) closeOverlay(overlay);
                },
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(st === "working", (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-interrupt",
                h_flex().gap(5).items_center().child(svg(ICONS.square).size(10)).child(tr("Stop", "停止")),
                (_e, cx) => this.interruptAgent(terminal, cx),
                { danger: true, outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(!isSidePanel, (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-kill",
                tr("Delete", "移入回收站"),
                () => {
                  run("agent.trash", { terminal });
                  if (!isTab && overlay) closeOverlay(overlay);
                },
                { danger: true, h: 26, px: 8 },
              ),
            ),
          )
          .child(
            this.pillButton(
              t,
              "acp-close",
              svg(ICONS.x).size(14),
              () => {
                if (isSidePanel) run("layout.hide", { id: "panel" });
                else if (overlay) closeOverlay(overlay);
              },
              { h: 26, px: 6 },
            ),
          ),
      );

    const canvasBase = t.canvas ?? t.background;
    const fadeStops = ["00", "24", "58", "96", "cc"];
    const bottomOverlay = v_flex()
      .absolute()
      .left(0)
      .right(0)
      .bottom(0)
      .w_full()
      .items_center()
      .when(!this.autoFollow, (el) =>
        el.child(
          h_flex()
            .w_full()
            .px(padX)
            .mb(8)
            .justify_center()
            .child(
              h_flex()
                .id("acp-latest-inline")
                .h(28)
                .px(12)
                .gap(5)
                .items_center()
                .rounded(999)
                .border_1()
                .border_color(t.border)
                .bg(t.glass ?? withAlpha(t.popover ?? t.surface, isDark ? "e2" : "f0"))
                .shadow_md()
                .text_size(12)
                .font_medium()
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", stopMouse)
                .on_scroll_wheel(stopScroll)
                .on_click((_e, cx) => {
                  this.autoFollow = true;
                  this.revealedSig = "";
                  this.scroll.scroll_to_item(threadChildren.length - 1);
                  cx.notify();
                })
                .child(svg(ICONS.chevronDown).size(13).text_color(t.dim))
                .child(tr("Scroll to end", "回到底部")),
            ),
        ),
      )
      .when(Boolean(perm), (el) => el.child(this.renderApprovalBanner(t, perm, terminal, k, isTab)))
      .when(isPty && st === "blocked" && !perm, (el) => el.child(this.renderPtyBlockedBanner(t, terminal, isTab)))
      .child(
        v_flex()
          .w_full()
          .children(fadeStops.map((alpha) => div().w_full().h(4).bg(withAlpha(canvasBase, alpha)))),
      )
      .child(this.renderComposer(t, k, st, shortCwd, isTab, mono, inboxItems));

    return v_flex()
      .id("acp-peek")
      .size_full()
      .when(!isTab, (el) => el.rounded(14).border_1().border_color(t.border).shadow_2xl())
      .bg(canvasBase)
      .text_color(t.foreground)
      .overflow_hidden()
      .track_focus(this.handle)
      .on_drop((e, cx) => {
        if (this.handleDropFiles(e?.paths ?? [], cx)) {
          cx.stop_propagation();
        }
      })
      .on_mouse_down("left", (_e, cx) => {
        if (this.openConfigId) this.closeConfigPopover(cx);
        if (!this.inputFocused && !this.configFilterFocused && !this.inboxEditFocused) this.handle?.focus?.();
      })
      .when(isTab && Boolean(myPane), (el) =>
        el.on_mouse_down("right", (_e, cx) => {
          cx.stop_propagation();
          openOverlay({ mod: "menu", kind: "popup", width: 280, props: { tab: "pane", target: { pane: myPane, terminal } } });
        }),
      )
      .on_key_down((e, cx) => {
        const isPaste = String(e?.key ?? "").toLowerCase() === "v"
          && (Boolean(e?.modifiers?.control) || Boolean(e?.modifiers?.platform))
          && !e?.modifiers?.alt;
        if (isPaste && !this.inputFocused && !this.configFilterFocused && !this.inboxEditFocused) {
          if (this.handlePasteAttachments(cx)) {
            cx.stop_propagation();
            return;
          }
        }
        if (e.key === "escape" && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
          if (this.editingInboxId != null) {
            this.cancelEditInboxItem(cx);
            cx.stop_propagation();
            return;
          }
          if (this.openConfigId) {
            this.closeConfigPopover(cx);
            cx.stop_propagation();
            return;
          }
          if (this.status() === "working" && terminal) {
            this.interruptAgent(terminal, cx);
            cx.stop_propagation();
            return;
          }
          if (!isTab && overlay) {
            closeOverlay(overlay);
            cx.stop_propagation();
          }
        }
      })
      .when(!isTab, (el) => el.child(modalHeader))
      .when(isSidePanel && sideCandidates.length > 1, (el) => el.child(this.renderSideChatSwitcher(t, sideCandidates, terminal)))
      .when(Boolean(this.error), (el) =>
        el.child(
          h_flex()
            .id("acp-error-banner")
            .w_full()
            .px(18)
            .py(8)
            .gap(8)
            .flex_none()
            .items_center()
            .justify_between()
            .bg(withAlpha(t.blocked ?? "#ef4444", "12"))
            .border_b(1)
            .border_color(withAlpha(t.blocked ?? "#ef4444", "55"))
            .child(div().flex_1().min_w(0).text_size(12).text_color(t.blocked ?? "#ef4444").child(this.error))
            .child(
              this.pillButton(t, "acp-error-dismiss", svg(ICONS.x).size(12), (_e, cx) => {
                this.error = null;
                cx.notify();
              }, { h: 22, px: 5 }),
            ),
        ),
      )
      .child(
        div()
          .id("acp-stage")
          .relative()
          .flex_1()
          .min_h(0)
          .w_full()
          .overflow_hidden()
          .child(
            v_flex()
              .id("acp-transcript")
              .size_full()
              .min_h(0)
              .overflow_x_hidden()
              .px(padX)
              .pt(isTab ? 16 : 20)
              .pb(8)
              .gap(12)
              .items_center()
              .overflow_y_scrollbar()
              .mode("hover")
              .track_scroll(this.scroll)
              .on_scroll_wheel((e, cx) => {
                if ((e.delta?.y ?? 0) > 6 && this.autoFollow) {
                  this.autoFollow = false;
                  cx.notify();
                }
              })
              .children(threadChildren),
          )
          .child(bottomOverlay),
      );
  }

  renderSidePanelEmpty(t) {
    const mainTarget =
      panes().find((x) => x.focused && x.terminal)?.terminal ||
      agents().find((a) => a.focused)?.address ||
      panes().find((x) => x.terminal)?.terminal ||
      null;
    const accent = "#14b8a6";
    return v_flex()
      .id("side-chat-empty")
      .size_full()
      .bg(t.canvas ?? t.background)
      .border_l(1)
      .border_color(t.border)
      .child(
        h_flex()
          .h(40)
          .px(14)
          .gap(8)
          .flex_none()
          .items_center()
          .justify_between()
          .bg(t.chrome)
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg(ICONS.messagesSquare).size(14).text_color(accent))
              .child(div().text_size(12.5).font_semibold().text_color(t.foreground).child(tr("Side Chat", "旁路讨论")))
              .child(div().px(5).py(1).rounded(4).bg(withAlpha(accent, "20") ?? t.raised).text_size(10).font_semibold().text_color(accent).child(tr("Read-Only", "只读"))),
          )
          .child(
            div()
              .id("side-empty-close")
              .p(4)
              .rounded(4)
              .text_color(t.dim)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click(() => run("layout.hide", { id: "panel" }))
              .child(svg(ICONS.x).size(13)),
          ),
      )
      .child(
        v_flex()
          .flex_1()
          .px(24)
          .gap(12)
          .items_center()
          .justify_center()
          .child(
            div()
              .w(40)
              .h(40)
              .rounded(10)
              .bg(withAlpha(accent, "1c") ?? t.raised)
              .border_1()
              .border_color(withAlpha(accent, "55") ?? t.border)
              .flex()
              .items_center()
              .justify_center()
              .child(svg(ICONS.messagesSquare).size(20).text_color(accent)),
          )
          .child(div().text_size(14).font_semibold().text_color(t.foreground).child(tr("No active Side Chat", "当前暂无旁路讨论")))
          .child(
            div()
              .max_w(320)
              .text_center()
              .text_size(12)
              .line_height("18px")
              .text_color(t.muted)
              .whitespace_normal()
              .child(
                tr(
                  "Open a read-only side channel with current agent context to ask second opinions without polluting main history or modifying files.",
                  "携带当前主会话上下文开启只读旁路讨论，随时找其他模型分析查证，不污染主线记录，也不修改工作区文件。",
                ),
              ),
          )
          .when(Boolean(mainTarget), (el) =>
            el.child(
              h_flex()
                .id("side-empty-start")
                .mt(4)
                .h(32)
                .px(16)
                .gap(6)
                .items_center()
                .rounded(8)
                .bg(accent)
                .text_size(12)
                .font_semibold()
                .text_color("#ffffff")
                .cursor_pointer()
                .hover((s) => s.opacity(0.9))
                .on_click(() => run("agent.handoff", { source: mainTarget, kind: "side-chat" }))
                .child(svg(ICONS.messagesSquare).size(13).text_color("#ffffff"))
                .child(tr("Start Side Chat", "开启旁路讨论")),
            ),
          ),
      );
  }

  renderSideChatSwitcher(t, sideCandidates, terminal) {
    return h_flex()
      .id("side-chat-switcher")
      .w_full()
      .min_w(0)
      .px(12)
      .py(6)
      .gap(6)
      .flex_none()
      .flex_wrap()
      .items_center()
      .bg(t.chrome)
      .border_b(1)
      .border_color(t.border)
      .children(
        sideCandidates.map((sc) => {
          const active = sc.address === terminal;
          const label = sc.labels?.["herdr.name"] || sc.name || sc.program?.name || sc.address;
          const showAddr = label !== sc.address;
          return h_flex()
            .id(`side-sw-${sc.address}`)
            .h(24)
            .px(8)
            .gap(5)
            .flex_none()
            .items_center()
            .rounded(6)
            .border_1()
            .border_color(active ? "#14b8a6" : t.border)
            .bg(active ? (withAlpha("#14b8a6", "1f") ?? t.selected) : t.raised)
            .text_size(11)
            .when(active, (s) => s.font_semibold())
            .text_color(active ? t.foreground : t.muted)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .on_mouse_down("left", stopMouse)
            .on_click((_e, cx) => {
              cx.stop_propagation();
              if (typeof setShared === "function") setShared("side-chat.active", sc.address);
              run("terminal.peek", { terminal: sc.address });
              cx?.notify?.();
            })
            .child(label)
            .when(showAddr, (pill) => pill.child(div().text_size(9.5).text_color(t.dim).child(sc.address)))
            .child(
              div()
                .id(`side-sw-rm-${sc.address}`)
                .p(2)
                .rounded(4)
                .text_color(t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .tooltip(tr("Move side chat to recycle bin", "结束并移入回收站"))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("agent.trash", { terminal: sc.address });
                })
                .child(svg(ICONS.x).size(10)),
            );
        }),
      );
  }

  renderSideTermHeader(t, k, st, pic, parent) {
    const terminal = this.activeTerminal();
    const sideCandidates = this.activeSideChatCandidates();
    const agentName = k?.labels?.["herdr.name"] || k?.name || k?.template || k?.program?.name || terminal || "CLI";
    const modalLineageParent = resolvePeerAddress(k?.lineage?.parent || parent?.address || "", terminal) || null;
    const modalUpstreamLabel = modalLineageParent
      ? (terminals().find((x) => x.address === modalLineageParent)?.labels?.["herdr.name"] || parent?.name || modalLineageParent)
      : "";
    const accent = "#14b8a6";
    const ended = st === "ended" || k?.state === "finished";
    return v_flex()
      .id("side-term-header")
      .w_full()
      .min_w(0)
      .bg(t.surface)
      .child(
        h_flex()
          .w_full()
          .min_w(0)
          .px(12)
          .py(8)
          .gap(8)
          .flex_none()
          .items_center()
          .justify_between()
          .bg(t.surface)
          .border_b(1)
          .border_color(t.border)
          .overflow_hidden()
          .child(
            v_flex()
              .flex_1()
              .min_w(0)
              .gap(2)
              .child(
                h_flex()
                  .w_full()
                  .min_w(0)
                  .gap(6)
                  .items_center()
                  .when(Boolean(pic), (el) => el.child(pic))
                  .child(div().min_w(0).truncate().text_size(12.5).font_semibold().text_color(t.foreground).child(agentName))
                  .child(
                    div()
                      .flex_none()
                      .px(5)
                      .py(1)
                      .rounded(4)
                      .bg(withAlpha(accent, "20") ?? t.raised)
                      .border_1()
                      .border_color(withAlpha(accent, "66") ?? t.border)
                      .text_size(10)
                      .font_semibold()
                      .text_color(accent)
                      .child(tr("Side Chat", "旁路讨论")),
                  )
                  .child(
                    div()
                      .flex_none()
                      .px(5)
                      .py(1)
                      .rounded(4)
                      .bg(t.raised)
                      .border_1()
                      .border_color(t.border)
                      .text_size(10)
                      .text_color(t.muted)
                      .child("CLI"),
                  ),
              )
              .when(Boolean(modalLineageParent), (col) =>
                col.child(
                  h_flex()
                    .w_full()
                    .min_w(0)
                    .gap(6)
                    .items_center()
                    .text_size(11)
                    .child(
                      h_flex()
                        .id("side-term-parent")
                        .flex_none()
                        .px(5)
                        .py(1)
                        .rounded(4)
                        .bg(withAlpha(accent, "14") ?? t.raised)
                        .border_1()
                        .border_color(withAlpha(accent, "44") ?? t.border)
                        .text_color(accent)
                        .cursor_pointer()
                        .hover((s) => s.bg(withAlpha(accent, "28") ?? t.hover).text_color(t.foreground))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          cx.stop_propagation();
                          run("agent.focus", { terminal: modalLineageParent });
                          run("layout.hide", { id: "panel" });
                        })
                        .child(tr(`↳ from ${modalUpstreamLabel}`, `↳ 来自 ${modalUpstreamLabel}`)),
                    ),
                ),
              ),
          )
          .child(
            h_flex()
              .gap(4)
              .items_center()
              .flex_none()
              .when(Boolean(terminal) && !ended, (el) =>
                el.child(
                  this.pillButton(
                    t,
                    "side-to-chat",
                    h_flex().gap(4).items_center().child(svg(ICONS.messagesSquare).size(12)).child(tr("Chat", "对话视图")),
                    () => run("terminal.view", { terminal, view: "peek" }),
                    { outlined: true, h: 26, px: 8, tooltip: tr("Switch to structured chat view", "切换至对话视图") },
                  ),
                ),
              )
              .when(Boolean(terminal) && !ended && Boolean(modalLineageParent), (el) =>
                el.child(
                  this.pillButton(
                    t,
                    "acp-merge-back",
                    h_flex().gap(4).items_center().child(svg(ICONS.cornerUpRight).size(12)).child(tr("Merge", "合回")),
                    () => run("agent.handoff", { source: terminal, into: modalLineageParent, kind: "merge-back" }),
                    { outlined: true, h: 26, px: 8 },
                  ),
                ),
              )
              .when(Boolean(modalLineageParent), (el) =>
                el.child(
                  this.pillButton(
                    t,
                    "acp-return-main",
                    tr("↩ Main", "↩ 回主 Agent"),
                    () => {
                      run("agent.focus", { terminal: modalLineageParent });
                      run("layout.hide", { id: "panel" });
                    },
                    { outlined: true, h: 26, px: 8 },
                  ),
                ),
              ),
          ),
      )
      .when(sideCandidates.length > 1, (el) => el.child(this.renderSideChatSwitcher(t, sideCandidates, terminal)));
  }

  render(cx) {
    _frameSettings = typeof settings === "function" ? settings() : null;
    const p = props() ?? {};
    if (Object.keys(p).length > 0) this.boundProps = p;
    const ownP = Object.keys(p).length > 0 ? p : (this.boundProps ?? p ?? {});
    const allTerminals = terminals();
    const allAgents = agents();
    const allPanes = panes();
    const allSpaces = typeof spaces === "function" ? spaces() : [];
    this._frame = {
      props: ownP,
      terminals: allTerminals,
      agents: allAgents,
      panes: allPanes,
      spaces: allSpaces,
    };
    try {
      const t = theme();
      const { overlay, acp, floating, parent: parentProp, sidePanel, sideTermHeader } = ownP;
      const sideCandidates = sidePanel ? this.activeSideChatCandidates() : null;
      if (sideCandidates) this._frame.sideCandidates = sideCandidates;
      const terminal = this.activeTerminal();
      this._frame.terminal = terminal;
      const k = this.term();
      this._frame.term = k;
      const isPty = this.isPtyMode();
      this._frame.isPty = isPty;
      const st = this.status();
      this._frame.status = st;
      this._frame.isSideChat = this.isSideChatSession(k);
      if (sidePanel) {
        if (!terminal) return this.renderSidePanelEmpty(t);
        const pic = icon(k?.program, 14, t.foreground);
        const parent = k?.parent ? allTerminals.find((x) => x.address === k.parent) : null;
        if (sideTermHeader) {
          return this.renderSideTermHeader(t, k, st, pic, parent);
        }
        return this.renderAcp(t, k, st, pic, parent, cx);
      }
      const pic = icon(k?.program, 14, t.foreground);
      const parent = k?.parent ? allTerminals.find((x) => x.address === k.parent) : null;
      if (acp) {
        return this.renderAcp(t, k, st, pic, parent, cx);
      }
      const home = allPanes.some((p) => p.terminal === terminal);
      const glyph = st === "working" ? SPIN[0] : st === "idle" ? "○" : st ? "●" : "·";
      const lineageKind = String(k?.lineage?.kind || (floating ? "side-chat" : "")).toLowerCase();
      const kindBadge = lineageKind === "fork"
        ? { label: tr("Fork", "会话分叉"), color: "#a855f7" }
        : (lineageKind === "side-chat" || lineageKind === "side_chat")
          ? { label: tr("Side Chat", "旁路讨论"), color: "#14b8a6" }
          : lineageKind === "handoff"
            ? { label: tr("Handoff", "上下文接力"), color: "#3b82f6" }
            : null;
      const upstreamAddr = resolvePeerAddress(k?.lineage?.parent || parent?.address || parentProp || "", terminal) || null;
      return h_flex()
        .size_full()
        .px(12)
        .gap(8)
        .items_center()
        .bg(t.surface)
        .border_b(1)
        .border_color(t.border)
        .text_size(12)
        .child(div().w(12).flex_none().text_color(t[st] ?? t.dim).when(st === "working", (el) => el.frames(SPIN, 90)).child(glyph))
        .when(Boolean(pic), (el) => el.child(pic))
        .child(div().truncate().font_semibold().text_color(t.foreground).child(k?.labels?.["herdr.name"] || k?.name || k?.template || k?.program?.name || terminal || ""))
        .when(Boolean(kindBadge), (el) =>
          el.child(
            div()
              .flex_none()
              .px(5)
              .py(1)
              .rounded(4)
              .bg(withAlpha(kindBadge.color, "20") ?? t.raised)
              .border_1()
              .border_color(withAlpha(kindBadge.color, "66") ?? t.border)
              .text_size(10)
              .font_semibold()
              .text_color(kindBadge.color)
              .child(kindBadge.label),
          ),
        )
        .when(Boolean(terminal), (el) =>
          el.child(div().flex_none().px(5).py(1).rounded(4).bg(t.raised).border_1().border_color(t.border).text_size(10.5).text_color(t.muted).child(terminal)),
        )
        .when(Boolean(st) && !floating, (el) => el.child(div().flex_none().text_color(t[st] ?? t.dim).child(statusWords()[st] ?? st)))
        .when(Boolean(upstreamAddr), (el) =>
          el.child(
            div()
              .id("peek-parent")
              .flex_none()
              .px(5)
              .py(1)
              .rounded(4)
              .bg(withAlpha(t.accent ?? "#3b82f6", "14") ?? t.raised)
              .border_1()
              .border_color(withAlpha(t.accent ?? "#3b82f6", "44") ?? t.border)
              .text_size(10.5)
              .text_color(t.accent ?? "#3b82f6")
              .cursor_pointer()
              .hover((s) => s.bg(withAlpha(t.accent ?? "#3b82f6", "28") ?? t.hover).text_color(t.foreground))
              .tooltip(tr(`Click to return to upstream agent ${upstreamAddr}`, `点击返回上游智能体 ${upstreamAddr}`))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                cx.stop_propagation();
                run("agent.focus", { terminal: upstreamAddr });
                if (overlay) closeOverlay(overlay);
              })
              .child(tr(`↳ from ${parent?.name || upstreamAddr}`, `↳ 来自 ${parent?.name || upstreamAddr}`)),
          ),
        )
        .child(div().flex_1())
        .when(Boolean(floating) && Boolean(upstreamAddr), (el) =>
          el
            .child(
              this.button(t, "merge-close", tr("Merge & Close", "合回并关闭"), () => {
                run("agent.handoff", {
                  source: terminal,
                  into: upstreamAddr,
                  kind: "merge-back",
                  ...(overlay ? { close_overlay: overlay } : {}),
                });
              }),
            )
            .child(
              this.button(t, "back-main", tr("↩ Main", "↩ 回主 Agent"), () => {
                run("agent.focus", { terminal: upstreamAddr });
                if (overlay) closeOverlay(overlay);
              }),
            ),
        )
        .when(lineageKind !== "side-chat" && lineageKind !== "side_chat", (el) =>
          el.child(
            this.button(t, "keep", floating ? tr("Dock", "转分栏") : (home ? tr("↗ go to", "↗ 跳转") : tr("↗ keep", "↗ 挂载")), () => {
              run(home ? "pane.focus" : "terminal.keep", floating ? { terminal, place: "right" } : { terminal });
              closeOverlay(overlay);
            }),
          ),
        )
        .child(this.button(t, "close", "✕", () => closeOverlay(overlay)));
    } finally {
      this._frame = null;
      _frameSettings = null;
    }
  }
}
