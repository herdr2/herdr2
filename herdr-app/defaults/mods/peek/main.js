// The bar on top of a peeked agent (or, for headless ACP agents, a T3 Code-style
// structured conversation, work-log, approval, and composer dialog).
// Props: { terminal, overlay, acp }.

import { View, div, image, svg } from "gpui-kit";
import { h_flex, v_flex, Popup, TextareaState, Textarea, InputState, Input, TextView, ScrollHandle } from "gpui-base";
import { agents, terminals, panes, spaces, run, theme, settings, home, props, closeOverlay, openOverlay, checking, h2, shared, setShared } from "herdr";
import { buildConversationItems, buildPtySessionBadges, summarizeActivityGroup, toolIconPath, formatTime, shortenPath, normalizeConfigOptions, filterConfigChoices, normalizeInboxItems, parseToolDetail, resolveEffortVisual, resolvePeerAddress } from "./acp_model.js";

const LANE_MAX_W = 736; // 46rem editorial content column (--chat-content-max-width)
const SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
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
  sparkles: "mods/peek/sparkles.svg",
  wrapText: "mods/peek/wrap-text.svg",
  wrench: "mods/peek/wrench.svg",
  zap: "mods/peek/zap.svg",
  refresh: "frame/settings/refresh.svg",
  shield: "frame/settings/shield.svg",
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
    if (!props()?.acp) return;
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
      cx.notify();
    });
    this.input.on("submit", (e, cx) => {
      if (e?.shift) return;
      const steer = Boolean(e?.secondary) && this.status() === "working";
      const trimmed = (this.input.value() ?? "").trim();
      if (!trimmed) {
        this.input.set_value("");
        cx.notify();
        return;
      }
      this.sendPrompt(steer);
    });
    this.handle = cx.focus_handle();
    if (!checking()) this.handle.focus();
    if (!checking()) {
      this.reloadTranscript();
      cx.timer.every(1000, (tcx) => {
        if (this.status() === "working") tcx?.notify?.();
      });
      if (props()?.pty) {
        cx.timer.every(1200, () => {
          if (this.term()?.state === "live") this.reloadTranscript();
        });
      }
    }
  }

  term() {
    return terminals().find((t) => t.address === props()?.terminal) ?? this.agentDetail ?? null;
  }

  status() {
    const addr = props()?.terminal;
    const fromAgent = agents().find((a) => a.address === addr)?.status;
    if (fromAgent) return fromAgent;
    const fromTerm = terminals().find((t) => t.address === addr)?.status;
    if (typeof fromTerm === "string" && fromTerm) return fromTerm;
    if (fromTerm?.state) return fromTerm.state;
    const fromPane = panes().find((p) => p.terminal === addr)?.status;
    if (fromPane) return fromPane;
    if (this.agentDetail?.status?.state === "working") return "working";
    return this.agentDetail?.state === "finished" ? "ended" : null;
  }

  async reloadTranscript() {
    const terminal = props()?.terminal;
    if (!terminal || checking()) return;
    if (this.loading) {
      this.reloadQueued = true;
      return;
    }
    this.loading = true;
    try {
      const liveTerm = this.term();
      const curTurn = liveTerm?.turn ?? -1;
      const curInboxLen = Number(liveTerm?.inbox_len ?? 0);
      const needDetail = !props()?.pty && (
        !this.agentDetail
        || this.lastDetailTerminal !== terminal
        || !this.agentDetail.config_options?.length
        || this.lastDetailTurn !== curTurn
        || (!Array.isArray(liveTerm?.inbox) && curInboxLen !== this.lastDetailInboxLen)
      );
      const [res, detailRes] = await Promise.all([
        h2(["read", "--json", terminal]),
        needDetail ? h2(["get", "--json", terminal]) : Promise.resolve(null),
      ]);
      if (detailRes && detailRes.code === 0) {
        try {
          const parsedDetail = JSON.parse(detailRes.stdout || "{}");
          if (parsedDetail && typeof parsedDetail === "object") {
            this.agentDetail = parsedDetail;
            this.lastDetailTerminal = terminal;
            this.lastDetailTurn = curTurn;
            this.lastDetailInboxLen = Number(parsedDetail.inbox_len ?? curInboxLen);
          }
        } catch {}
      }
      if (res.code !== 0) {
        this.error = (res.stderr || res.stdout || "Failed to read transcript").trim();
      } else {
        const parsed = JSON.parse(res.stdout || "{}");
        this.entries = Array.isArray(parsed.entries) ? parsed.entries : [];
        this.fallbackText = parsed.text || "";
        this.readHow = parsed.how || null;
        this.sessionMeta = parsed.session_meta ?? null;
        this.loadedOnce = true;
        this.error = null;
      }
    } catch (e) {
      this.error = e?.message ?? String(e);
    } finally {
      this.loading = false;
      this.cx?.notify();
      if (this.reloadQueued) {
        this.reloadQueued = false;
        this.reloadTranscript();
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
    const terminal = props()?.terminal;
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
    const terminal = props()?.terminal;
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

  async executeCheckpointRollback(toTurn, restoreFiles, fallbackPrompt, cx) {
    const terminal = props()?.terminal;
    if (!terminal || checking() || this.rollbackBusy) return;
    this.rollbackBusy = true;
    this.rollbackError = null;
    (cx ?? this.cx)?.notify?.();
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
      setShared("checkpoint.sync", `${terminal}:${toTurn}:${Date.now()}`);
      await this.reloadTranscript();
    } catch (e) {
      this.rollbackError = e?.message ?? String(e);
    } finally {
      this.rollbackBusy = false;
      (cx ?? this.cx)?.notify?.();
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
      this.reloadTranscript();
    }
  }

  async interruptAgent(terminal = props()?.terminal, cx = this.cx) {
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
      this.reloadTranscript();
    }
  }

  async sendPrompt(steer) {
    const terminal = props()?.terminal;
    const st = this.status();
    if (st === "ended" || this.term()?.state === "finished") return;
    const text = (this.input?.value?.() ?? "").trim();
    if (!terminal || !text || checking() || this.busyAction) return;
    this.input.set_value("");
    this.autoFollow = true;
    this.revealedSig = "";
    const args = steer ? ["send", "--json", "--steer", terminal, text] : ["send", "--json", terminal, text];
    await this.runH2(args);
  }

  rawInboxFor(k = this.term()) {
    if (props()?.pty) return [];
    if (Array.isArray(this.optimisticInbox)) return this.optimisticInbox;
    if (Array.isArray(k?.inbox)) return k.inbox;
    if (Array.isArray(this.agentDetail?.inbox)) return this.agentDetail.inbox;
    return [];
  }

  inboxItemsFor(k = this.term()) {
    return normalizeInboxItems(this.rawInboxFor(k), terminals(), props()?.terminal ?? "");
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
    const terminal = props()?.terminal;
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
        this.reloadTranscript();
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
    const terminal = props()?.terminal;
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
    const terminal = props()?.terminal;
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

  configOptionsFor(k) {
    if (props()?.pty) return [];
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
    const terminal = props()?.terminal;
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
    const isDark = settings()?.dark ?? true;
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
    };
    if (cwd) opts.root_dir = cwd;
    return opts;
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
    const isDark = Boolean(settings()?.dark ?? true);
    const time = formatTime(item.ts_ms);
    let fromLabel = item.from ? `from ${item.from}` : null;
    if (item.from) {
      const slash = typeof currentTerminal === "string" ? currentTerminal.lastIndexOf("/") : -1;
      const prefix = slash >= 0 ? currentTerminal.slice(0, slash + 1) : "";
      const sender = terminals().find((x) => x.address === item.from || (prefix && x.address === `${prefix}${item.from}`));
      if (sender?.name && sender.name !== item.from) {
        fromLabel = `from ${sender.name} (${item.from})`;
      }
    }
    const fullBody = item.body || "";
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
    const isRich = displayBody.includes("\n") || displayBody.length > 72 || /[`*_#[\]|>]/.test(displayBody);
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
            .when(Boolean(fromLabel), (b) => b.child(div().flex_none().text_size(11.5).text_color(t.dim).child("·")))
            .child(div().max_w(440).truncate().text_size(12.5).text_color(t.foreground).child(item.foldSummary || fullBody))
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
              .gap(6)
              .child(
                isRich
                  ? TextView.markdown(`acp-u-${item.id}`, displayBody, this.mdOptions(t, this.term()?.cwd)).w_full().min_w(0).selectable(true).scrollable(false)
                  : div().whitespace_normal().child(displayBody),
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
    const isDark = Boolean(settings()?.dark ?? true);
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
    const isDark = Boolean(settings()?.dark ?? true);
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
      });
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
            .child(TextView.markdown(`acp-th-${step.id}`, step.text, this.mdOptions(t, this.term()?.cwd)).w_full().min_w(0).selectable(true).scrollable(false)),
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

  renderChangedFilesCard(t, item, mono) {
    const cf = item?.changedFiles;
    if (!cf || !Array.isArray(cf.files) || cf.files.length === 0) return null;
    const key = `cf-${item.id}`;
    const open = Boolean(this.expandedGroups?.[key]);
    const isDark = Boolean(settings()?.dark ?? true);
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
                  .hover((s) => s.bg(t.hover))
                  .on_mouse_down("left", stopMouse)
                  .on_click(() => {
                    run("layout.select", { stack: "panel-body", child: "changes" });
                    run("layout.show", { id: "panel" });
                  })
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
    const kindLabels = {
      handoff: tr("Handoff", "上下文接力"),
      fork: tr("Fork", "分支探索"),
      "side-chat": tr("Side Chat", "旁路讨论"),
      "merge-back": tr("Merge Back", "合回上下文"),
    };
    const kindLabel = kindLabels[item.handoff_kind] || item.handoff_kind || tr("Handoff", "上下文接力");
    const srcAddr = resolvePeerAddress(item.from_address, props()?.terminal || "");
    const src = srcAddr || "?";
    const prov = item.from_provider ? ` (${item.from_provider})` : "";
    const turns = item.from_turns ? tr(` · turns ${item.from_turns}`, ` · 第 ${item.from_turns} 轮`) : "";
    const label = `${kindLabel} · ${tr(`from ${src}${prov}`, `来自 ${src}${prov}`)}${turns}`;
    const accent = t.accent ?? "#3b82f6";
    return h_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(LANE_MAX_W)
      .gap(10)
      .items_center()
      .py(4)
      .child(div().flex_1().h(1).bg(withAlpha(accent, "44") ?? t.border))
      .child(
        h_flex()
          .gap(6)
          .px(10)
          .py(3)
          .rounded(999)
          .items_center()
          .bg(withAlpha(accent, "16") ?? t.raised)
          .border_1()
          .border_color(withAlpha(accent, "55") ?? t.border)
          .text_size(11)
          .font_medium()
          .text_color(accent)
          .when(Boolean(srcAddr), (el) =>
            el
              .cursor_pointer()
              .hover((s) => s.bg(withAlpha(accent, "26") ?? t.hover))
              .tooltip(tr(`Focus source session ${src}`, `点击跳转到来源会话 ${src}`))
              .on_mouse_down("left", stopMouse)
              .on_click(() => run("agent.focus", { terminal: srcAddr })),
          )
          .child(svg("mods/workspace-chrome/git-branch.svg").size(12).flex_none().text_color(accent))
          .child(label),
      )
      .child(div().flex_1().h(1).bg(withAlpha(accent, "44") ?? t.border));
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
    const stepNodes = (item.latestPlan ? nonPlanSteps : item.steps).map(renderStep).filter(Boolean);
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
    const isDark = Boolean(settings()?.dark ?? true);
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
        .child(TextView.markdown(`acp-md-${item.id}-${bi}`, block.text, mdOpts).w_full().min_w(0).selectable(true).scrollable(false));
    });

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
      .when(Boolean(item.changedFiles), (el) => el.child(this.renderChangedFilesCard(t, item, mono)))
      .when(Boolean(item.isFinalTurnReply) && Boolean(item.text), (el) => {
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
    const starting = st === "starting" || (!props()?.pty && k?.state === "live" && !k?.acp_session_id && !this.agentDetail?.acp_session_id && st !== "idle" && st !== "done");
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
    const isDark = Boolean(settings()?.dark ?? true);
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

    filtered.forEach((choice, idx) => {
      if (choice.group && choice.group !== lastGroup) {
        lastGroup = choice.group;
        rows.push(
          div()
            .id(`acp-cfg-grp-${opt.id}-${idx}`)
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
      const choiceDesc = isBypassChoice
        ? tr(choice.description || "Skip all permission checks (bypassPermissions)", "跳过所有权限检查 (bypassPermissions)")
        : choice.description;
      const titleColor = isBypassChoice ? (t.blocked ?? "#f59e0b") : t.foreground;
      rows.push(
        h_flex()
          .id(`acp-cfg-choice-${opt.id}-${idx}`)
          .w_full()
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
                      .child(choice.name),
                  )
                  .when(choice.value !== choice.name && !isBypassChoice, (el) =>
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
      rows.push(
        h_flex()
          .id("acp-cfg-apply-custom")
          .w_full()
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
    const headerTitle = opt.iconKind === "mode"
      ? tr(opt.name, "权限模式")
      : opt.iconKind === "thinking"
        ? tr(opt.name, "思考强度")
        : opt.name;
    const headerDesc = opt.iconKind === "mode" && opt.description
      ? tr(opt.description, "当前 ACP 会话的权限策略")
      : opt.description;

    return div()
      .pb(6)
      .on_mouse_down("left", stopMouse)
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
            v_flex()
              .id("acp-cfg-list")
              .w_full()
              .max_h(showSearch ? 320 : 420)
              .overflow_y_scrollbar()
              .gap(2)
              .children(rows),
          ),
      );
  }

  renderCapabilityBar(t, opts, k) {
    const isPty = Boolean(props()?.pty);
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
              .tooltip(tr("Switch back to raw terminal view", "切回原始终端视图"))
              .on_mouse_down("left", stopMouse)
              .on_click(() => run("terminal.view", { terminal: props()?.terminal, view: "terminal" }))
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
          const pillText = opt.pillLabel;
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
            .tooltip(`${opt.name}: ${opt.currentLabel}`)
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
    const addr = props()?.terminal;
    const ended = k?.state === "finished" || k?.status === "ended";
    const lineageParent = resolvePeerAddress(k?.lineage?.parent || this.agentDetail?.lineage?.parent || "", addr) || null;
    const pInfo = panes().find((p) => p.terminal === addr) ?? null;
    const aInfo = agents().find((a) => a.address === addr) ?? null;
    const sp = (typeof spaces === "function" ? spaces() : []).find((s) => s.id === (aInfo?.space ?? null))
      ?? (typeof spaces === "function" ? spaces() : []).find((s) => s.active)
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
          .when(Boolean(addr) && !ended, (el) =>
            el.child(
              h_flex()
                .id("acp-ctx-handoff")
                .h(20)
                .px(7)
                .gap(4)
                .items_center()
                .rounded(6)
                .bg(t.raised)
                .border_1()
                .border_color(t.border)
                .text_size(11)
                .font_medium()
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .tooltip(tr("Context handoff / Fork / Side Chat / Merge", "上下文接力 / 分支探索 / 旁路讨论 / 合回"))
                .on_mouse_down("left", stopMouse)
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("agent.handoff", { source: addr, kind: "handoff" });
                })
                .child(svg(ICONS.gitBranch).size(11).flex_none())
                .child(tr("Handoff", "接力")),
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
    const isDark = Boolean(settings()?.dark ?? true);
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

  renderComposer(t, k, st, shortCwd, isTab = false, mono = "JetBrains Mono", inboxItems = []) {
    const queued = inboxItems.length > 0 ? inboxItems.length : (k?.inbox_len ?? 0);
    const ended = st === "ended" || k?.state === "finished";
    const queuePanel = !ended && inboxItems.length > 0 ? this.renderInboxQueue(t, inboxItems, mono) : null;
    const working = st === "working" && !ended;
    const hasText = Boolean((this.input?.value?.() ?? "").trim());
    const opts = this.configOptionsFor(k);
    const capBar = ended ? null : this.renderCapabilityBar(t, opts, k);
    const canSend = !ended && hasText && !this.busyAction;
    const padX = isTab ? 20 : 28;
    const isDark = Boolean(settings()?.dark ?? true);
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
                  .when(working && hasText, (el) =>
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
                  .when(!working || hasText, (el) =>
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
                          this.interruptAgent(props()?.terminal, cx);
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
    const isDark = Boolean(settings()?.dark ?? true);
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

  renderAcp(t, k, st, pic, parent) {
    const { terminal, overlay, tab, pane } = props() ?? {};
    const isTab = Boolean(tab);
    const isPty = Boolean(props()?.pty);
    const isDark = Boolean(settings()?.dark ?? true);
    const myPane = pane ?? panes().find((p) => (p.acp || p.structured) && p.terminal === terminal)?.pane ?? null;
    const ckptEpoch = `${terminal}:${k?.foreground?.pid ?? ""}:${k?.checkpoint_seq ?? 0}:${shared("checkpoint.sync") ?? ""}`;
    if (ckptEpoch !== this.lastCkptEpoch) {
      this.lastCkptEpoch = ckptEpoch;
      this.checkpoints = [];
      this.checkpointDiffs = {};
      this.checkpointDiffOpen = {};
    }
    const key = `${terminal}:${st}:${k?.turn ?? 0}:${k?.transcript_seq ?? 0}:${k?.foreground?.pid ?? ""}:${k?.checkpoint_seq ?? 0}:${k?.pending_permission?.request_id ?? ""}:${k?.inbox_len ?? 0}:${k?.printed ?? 0}:${k?.status_ms ?? 0}:${shared("checkpoint.sync") ?? ""}`;
    if (!checking() && key !== this.lastKey) {
      this.lastKey = key;
      Promise.resolve().then(() => this.reloadTranscript());
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
    const mono = settings()?.font_family ?? "JetBrains Mono";
    const inboxItems = isPty ? [] : this.inboxItemsFor(k);
    if (this.editingInboxId != null && !inboxItems.some((it) => Number(it.id) === Number(this.editingInboxId)) && !this.inboxBusy) {
      this.editingInboxId = null;
      this.inboxEditFocused = false;
    }
    const program = k?.program;
    const agentName = k?.labels?.["herdr.name"] || k?.name || k?.template || program?.name || terminal || "ACP Agent";
    const shortCwd = shortenPath(k?.cwd, home());
    const items = buildConversationItems(this.entries, this.fallbackText, st === "working", {
      isPty,
      how: this.readHow,
      turn: k?.turn ?? 0,
      isAgent: Boolean(program?.agent || k?.status || st),
      cwd: k?.cwd || "",
      checkpointsByTurn: this.checkpoints,
    });
    this.maxConversationTurn = items.reduce((m, it) => Math.max(m, Number(it?.turn) || 0), 0);

    const threadChildren = [];
    if (parent) {
      threadChildren.push(this.renderSubagentDivider(t, parent));
    }
    if (items.length === 0) {
      threadChildren.push(this.renderEmptyState(t, k, program, agentName, shortCwd, st));
    } else {
      for (const item of items) {
        if (item.kind === "user") threadChildren.push(this.renderUserItem(t, item, terminal));
        else if (item.kind === "activity_group") threadChildren.push(this.renderActivityGroup(t, item, mono));
        else if (item.kind === "assistant") threadChildren.push(this.renderAssistantItem(t, item, mono));
        else if (item.kind === "handoff") threadChildren.push(this.renderHandoffBanner(t, item));
        else if (item.kind === "rewind") threadChildren.push(this.renderRewindDivider(t, item));
        else if (item.kind === "terminal_snapshot") threadChildren.push(this.renderTerminalSnapshot(t, item, terminal, mono));
        else if (item.kind === "error") threadChildren.push(this.renderErrorItem(t, item));
        else if (item.kind === "stderr_group") threadChildren.push(this.renderStderrItem(t, item, mono));
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
    const modalLineageParent = resolvePeerAddress(k?.lineage?.parent || this.agentDetail?.lineage?.parent || "", terminal) || null;
    const modalEnded = st === "ended" || k?.state === "finished";
    const modalHeader = h_flex()
      .id("acp-header")
      .min_h(48)
      .px(16)
      .py(6)
      .gap(10)
      .flex_none()
      .items_center()
      .bg(t.canvas ?? t.background)
      .border_b(1)
      .border_color(t.border)
      .child(this.avatarTile(t, program, pic, 28, 15))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(1)
          .child(
            h_flex()
              .gap(7)
              .items_center()
              .child(div().truncate().text_size(13).font_semibold().text_color(t.foreground).child(agentName))
              .when(Boolean(terminal), (el) =>
                el.child(div().font_family(mono).text_size(11).text_color(t.dim).flex_none().child(terminal)),
              ),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .text_size(11)
              .text_color(t.dim)
              .when(Boolean(parent), (el) => el.child(div().flex_none().text_color(t.muted).child(tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`))))
              .when(Boolean(shortCwd), (el) => el.child(div().min_w(0).truncate().child(shortCwd))),
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
          .child(
            this.pillButton(
              t,
              "acp-refresh",
              h_flex().gap(5).items_center().child(svg(ICONS.refresh).size(12).when(this.loading, (s) => s.spin(900))).child(tr("Refresh", "刷新")),
              () => this.reloadTranscript(),
              { h: 26, px: 8 },
            ),
          )
          .when(Boolean(terminal) && !modalEnded && Boolean(modalLineageParent), (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-merge-back",
                h_flex().gap(4).items_center().child(svg(ICONS.cornerUpRight).size(12)).child(tr(`Merge → ${modalLineageParent}`, `合回 ${modalLineageParent}`)),
                () => run("agent.handoff", { source: terminal, into: modalLineageParent, kind: "merge-back" }),
                { outlined: true, h: 26, px: 9 },
              ),
            ),
          )
          .when(Boolean(terminal) && !modalEnded, (el) =>
            el.child(
              this.pillButton(
                t,
                "acp-handoff",
                h_flex().gap(4).items_center().child(svg(ICONS.gitBranch).size(12)).child(tr("Handoff", "接力")),
                () => run("agent.handoff", { source: terminal, kind: "handoff" }),
                { outlined: true, h: 26, px: 9, tooltip: tr("Context handoff / Fork / Side Chat / Merge", "上下文接力 / 分支探索 / 旁路讨论 / 合回") },
              ),
            ),
          )
          .child(
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
          )
          .child(
            this.pillButton(
              t,
              "acp-split-right",
              tr("Split right", "右侧分栏"),
              () => {
                run("terminal.keep", { terminal, place: "right" });
                if (overlay) closeOverlay(overlay);
              },
              { outlined: true, h: 26, px: 9 },
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
          .child(
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
          )
          .child(
            this.pillButton(
              t,
              "acp-close",
              svg(ICONS.x).size(14),
              () => closeOverlay(overlay),
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

  render() {
    const t = theme();
    const { terminal, overlay, acp } = props() ?? {};
    const k = this.term();
    const st = this.status();
    const pic = icon(k?.program, 14, t.foreground);
    const parent = k?.parent ? terminals().find((p) => p.address === k.parent) : null;
    if (acp) {
      return this.renderAcp(t, k, st, pic, parent);
    }
    const home = panes().some((p) => p.terminal === terminal);
    const glyph = st === "working" ? SPIN[0] : st === "idle" ? "○" : st ? "●" : "·";
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
      .when(Boolean(st), (el) => el.child(div().flex_none().text_color(t[st] ?? t.dim).child(statusWords()[st] ?? st)))
      .when(Boolean(parent), (el) => el.child(div().truncate().text_color(t.dim).child(tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`))))
      .child(div().flex_1())
      .child(
        this.button(t, "keep", home ? tr("↗ go to", "↗ 跳转") : tr("↗ keep", "↗ 挂载"), () => {
          run(home ? "pane.focus" : "terminal.keep", { terminal });
          closeOverlay(overlay);
        }),
      )
      .child(this.button(t, "close", "✕", () => closeOverlay(overlay)));
  }
}
