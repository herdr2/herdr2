// The bar on top of a peeked agent (or, for headless ACP agents, a T3 Code-style
// structured conversation, work-log, approval, and composer dialog).
// Props: { terminal, overlay, acp }.

import { View, div, image, svg } from "gpui-kit";
import { h_flex, v_flex, Popup, TextareaState, Textarea, InputState, Input, TextView, ScrollHandle } from "gpui-base";
import { agents, terminals, panes, run, theme, settings, home, props, closeOverlay, openOverlay, checking, h2 } from "herdr";
import { buildConversationItems, summarizeActivityGroup, toolIconPath, formatTime, shortenPath, normalizeConfigOptions, filterConfigChoices } from "./acp_model.js";

const SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const statusWords = () => ({
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
  sparkles: "mods/peek/sparkles.svg",
  lightbulb: "mods/peek/lightbulb.svg",
  zap: "mods/peek/zap.svg",
  refresh: "frame/settings/refresh.svg",
  shield: "frame/settings/shield.svg",
  search: "mods/workspace-chrome/search.svg",
  check: "mods/workspace-chrome/check.svg",
  circleCheck: "mods/workspace-chrome/circle-check.svg",
  chevronDown: "mods/workspace-chrome/chevron-down.svg",
  chevronUp: "mods/peek/chevron-up.svg",
  chevronRight: "mods/workspace-chrome/chevron-right.svg",
  layers: "mods/workspace-chrome/layers.svg",
  spinner: "mods/workspace-chrome/spinner.svg",
  square: "mods/workspace-chrome/square.svg",
  terminal: "mods/workspace-chrome/terminal.svg",
  x: "mods/workspace-chrome/x.svg",
};

// The icon from the app's program table: a one-color glyph or a picture.
function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

function withAlpha(color, alphaHex) {
  return typeof color === "string" && /^#[0-9a-fA-F]{6}$/.test(color) ? `${color}${alphaHex}` : color;
}

function stopMouse(_e, cx) {
  cx.stop_propagation();
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
    this.loadedOnce = false;
    this.error = null;
    this.busyAction = false;
    this.interrupting = false;
    this.loading = false;
    this.reloadQueued = false;
    this.expandedGroups = {};
    this.expandedSteps = {};
    this.copiedId = null;
    this.autoFollow = true;
    this.revealedSig = "";
    this.lastKey = "";
    this.inputFocused = false;
    this.agentDetail = null;
    this.lastDetailTerminal = "";
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
      placeholder: tr("Ask anything, or send instructions…", "输入问题或发送指令…"),
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
      const curTurn = this.term()?.turn ?? -1;
      const needDetail = !props()?.pty && (
        !this.agentDetail
        || this.lastDetailTerminal !== terminal
        || !this.agentDetail.config_options?.length
        || this.lastDetailTurn !== curTurn
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

  copyPill(t, key, text, label = tr("Copy", "复制")) {
    const copied = this.copiedId === key;
    return h_flex()
      .id(`copy-${key}`)
      .h(22)
      .px(8)
      .gap(5)
      .items_center()
      .rounded(6)
      .cursor_pointer()
      .text_size(11)
      .text_color(copied ? (t.done ?? t.foreground) : t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", stopMouse)
      .on_click((_e, cx) => this.copyText(key, text, cx))
      .when(copied, (el) => el.child(svg(ICONS.check).size(11).text_color(t.done ?? t.foreground)))
      .child(copied ? tr("Copied", "已复制") : label);
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
    const color = t[st] ?? t.dim;
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
      .when(st === "working", (el) => el.child(svg(ICONS.spinner).size(11).text_color(color).spin(900)))
      .when(st !== "working", (el) => el.child(div().size(6).rounded(999).bg(color)))
      .child(label);
  }

  speakerRow(t, item, program, agentName, copyKey = null, copyContent = null) {
    const time = formatTime(item.ts_ms);
    return h_flex()
      .w_full()
      .items_center()
      .gap(8)
      .child(this.avatarTile(t, program, null, 20, 12))
      .child(div().text_size(12.5).font_semibold().text_color(t.foreground).child(agentName))
      .when(item.turn > 0, (el) => el.child(div().px(6).py(1).rounded(5).bg(t.raised).text_size(10.5).text_color(t.dim).child(`Turn ${item.turn}`)))
      .when(Boolean(time), (el) => el.child(div().text_size(11).text_color(t.dim).child(time)))
      .child(div().flex_1())
      .when(Boolean(copyKey && copyContent), (el) => el.child(this.copyPill(t, copyKey, copyContent, "Copy")));
  }

  renderUserItem(t, item, currentTerminal) {
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
    const body = item.body || "";
    const isRich = body.includes("\n") || body.length > 72 || /[`*_#[\]|>]/.test(body);
    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .items_end()
      .gap(5)
      .child(
        h_flex()
          .gap(6)
          .items_center()
          .when(Boolean(fromLabel), (el) =>
            el.child(div().px(6).py(1).rounded(999).bg(t.raised).border_1().border_color(t.border).text_size(10.5).text_color(t.muted).child(fromLabel)),
          )
          .when(Boolean(time), (el) => el.child(div().text_size(11).text_color(t.dim).child(time))),
      )
      .child(
        isRich
          ? v_flex()
              .w_full()
              .max_w(680)
              .min_w(0)
              .px(16)
              .py(11)
              .rounded(14)
              .bg(t.surface)
              .border_1()
              .border_color(t.border)
              .text_size(13.5)
              .line_height("21px")
              .text_color(t.foreground)
              .child(TextView.markdown(`acp-u-${item.id}`, body).w_full().min_w(0).selectable(true).scrollable(false))
          : div()
              .max_w(640)
              .px(15)
              .py(9)
              .rounded(14)
              .bg(t.surface)
              .border_1()
              .border_color(t.border)
              .text_size(13.5)
              .line_height("21px")
              .text_color(t.foreground)
              .whitespace_normal()
              .child(body),
      );
  }

  renderToolStep(t, step, mono) {
    const running = step.status === "pending" || step.status === "in_progress";
    const failed = step.status === "failed";
    const iconPath = toolIconPath(step.tool_kind, step.title);
    const kindBadge = step.tool_kind && step.tool_kind !== "other" ? step.tool_kind : null;
    const summary = step.summaryTitle || step.title || "Tool call";
    const expandable = Boolean(step.multiline || (step.title && step.title.length > 120));
    const open = Boolean(this.expandedSteps?.[step.id]);
    return v_flex()
      .id(`acp-step-${step.id}`)
      .w_full()
      .min_w(0)
      .rounded(8)
      .bg(t.surface)
      .border_1()
      .border_color(failed ? (t.blocked ?? t.border) : t.border)
      .overflow_hidden()
      .child(
        h_flex()
          .w_full()
          .min_h(30)
          .px(10)
          .py(5)
          .gap(8)
          .items_center()
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
          .child(svg(iconPath).size(13).flex_none().text_color(running ? (t.working ?? t.accent) : t.dim))
          .child(div().flex_1().min_w(0).truncate().font_family(mono).text_size(11.5).text_color(t.foreground).child(summary))
          .when(step.extraLines > 0, (el) =>
            el.child(div().px(5).py(1).rounded(4).bg(t.raised).text_size(10.5).text_color(t.dim).flex_none().child(tr(`+${step.extraLines} lines`, `+${step.extraLines} 行`))),
          )
          .when(Boolean(kindBadge), (el) =>
            el.child(div().px(5).py(1).rounded(4).bg(t.raised).text_size(10.5).text_color(t.dim).flex_none().child(kindBadge)),
          )
          .when(running, (el) => el.child(svg(ICONS.spinner).size(12).flex_none().text_color(t.working ?? t.accent).spin(900)))
          .when(!running && !failed, (el) => el.child(svg(ICONS.check).size(12).flex_none().text_color(t.done ?? t.dim)))
          .when(failed, (el) => el.child(div().flex_none().text_size(11).font_medium().text_color(t.blocked ?? "#ef4444").child(tr("failed", "失败"))))
          .when(expandable, (el) => el.child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim))),
      )
      .when(expandable && open, (el) =>
        el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .border_t(1)
            .border_color(t.border)
            .bg(t.background)
            .overflow_hidden()
            .child(
              div()
                .min_w(0)
                .m(-1)
                .font_family(mono)
                .text_size(11.5)
                .line_height("18px")
                .text_color(t.muted)
                .child(
                  TextView.markdown(`acp-tc-${step.id}`, `${fenceFor(step.title)}\n${step.title}\n${fenceFor(step.title)}`)
                    .w_full()
                    .min_w(0)
                    .selectable(true)
                    .scrollable(false),
                ),
            ),
        ),
      );
  }

  renderActivityGroup(t, item, program, agentName, mono) {
    const open = this.expandedGroups[item.id] ?? item.defaultExpanded;
    const summary = summarizeActivityGroup(item);
    const statusColor = item.runningCount > 0 ? (t.working ?? t.accent) : item.failedCount > 0 ? (t.blocked ?? "#ef4444") : (t.done ?? t.muted);

    const stepNodes = item.steps.map((step) => {
      if (step.kind === "tool_call") {
        return this.renderToolStep(t, step, mono);
      }
      if (step.kind === "thought") {
        return v_flex()
          .id(`acp-step-${step.id}`)
          .w_full()
          .min_w(0)
          .px(12)
          .py(8)
          .gap(4)
          .rounded(8)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .child(div().text_size(10.5).font_semibold().text_color(t.dim).child(tr("REASONING", "推理过程")))
          .child(div().w_full().min_w(0).text_size(12).line_height("18px").text_color(t.muted).child(TextView.markdown(`acp-th-${step.id}`, step.text).w_full().min_w(0).selectable(true).scrollable(false)));
      }
      if (step.kind === "plan") {
        return v_flex()
          .id(`acp-step-${step.id}`)
          .w_full()
          .min_w(0)
          .px(12)
          .py(9)
          .gap(6)
          .rounded(8)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg(ICONS.layers).size(12).text_color(t.accent ?? t.muted))
              .child(div().text_size(11.5).font_semibold().text_color(t.foreground).child(tr("Plan", "执行计划"))),
          )
          .children(
            step.items.map((line, li) =>
              h_flex()
                .gap(7)
                .items_center()
                .child(div().size(5).rounded(999).flex_none().bg(t.accent ?? t.dim))
                .child(div().flex_1().min_w(0).text_size(12).line_height("18px").text_color(t.muted).child(line)),
            ),
          );
      }
      if (step.kind === "permission") {
        return h_flex()
          .id(`acp-step-${step.id}`)
          .w_full()
          .min_h(28)
          .px(10)
          .py(5)
          .gap(8)
          .items_center()
          .rounded(8)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .child(svg(ICONS.shield).size(12).flex_none().text_color(t.accent ?? t.dim))
          .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(t.muted).child(step.title))
          .when(Boolean(step.outcome), (el) =>
            el.child(div().px(6).py(1).rounded(4).bg(t.raised).text_size(10.5).text_color(t.dim).child(step.outcome)),
          );
      }
      return null;
    }).filter(Boolean);

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .gap(8)
      .when(item.showSpeakerHeader, (el) => el.child(this.speakerRow(t, item, program, agentName)))
      .child(
        v_flex()
          .w_full()
          .min_w(0)
          .gap(6)
          .child(
            h_flex()
              .id(`acp-grp-${item.id}`)
              .w_full()
              .h(34)
              .px(11)
              .gap(8)
              .items_center()
              .rounded(9)
              .bg(t.surface)
              .border_1()
              .border_color(t.border)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                this.expandedGroups[item.id] = !open;
                cx.notify();
              })
              .when(item.runningCount > 0, (el) => el.child(svg(ICONS.spinner).size(13).flex_none().text_color(statusColor).spin(900)))
              .when(item.runningCount === 0, (el) => el.child(svg(ICONS.circleCheck).size(13).flex_none().text_color(statusColor)))
              .child(div().flex_1().min_w(0).truncate().text_size(12).font_medium().text_color(t.muted).child(summary))
              .child(div().px(6).py(1).rounded(999).bg(t.raised).text_size(10.5).text_color(t.dim).flex_none().child(String(item.steps.length)))
              .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(13).flex_none().text_color(t.dim)),
          )
          .when(open && stepNodes.length > 0, (el) =>
            el.child(
              v_flex()
                .w_full()
                .min_w(0)
                .pl(12)
                .ml(10)
                .py(2)
                .gap(6)
                .border_l(2)
                .border_color(t.border)
                .children(stepNodes),
            ),
          ),
      );
  }

  renderAssistantItem(t, item, program, agentName, mono) {
    const copyTurnKey = `turn-${item.id}`;
    const blocks = (item.blocks ?? []).map((block, bi) => {
      if (block.type === "code") {
        const key = `${item.id}-code-${bi}`;
        const fence = fenceFor(block.code);
        const fenced = `${fence}\n${block.code}\n${fence}`;
        return v_flex()
          .id(`acp-codecard-${key}`)
          .w_full()
          .min_w(0)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.background)
          .overflow_hidden()
          .child(
            h_flex()
              .h(32)
              .px(12)
              .items_center()
              .justify_between()
              .bg(t.raised)
              .border_b(1)
              .border_color(t.border)
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .child(svg(ICONS.terminal).size(12).text_color(t.dim))
                  .child(div().font_family(mono).text_size(11.5).font_medium().text_color(t.muted).child(block.lang || "code")),
              )
              .child(this.copyPill(t, key, block.code, tr("Copy", "复制"))),
          )
          .child(
            v_flex()
              .w_full()
              .min_w(0)
              .overflow_hidden()
              .child(
                div()
                  .min_w(0)
                  .m(-1)
                  .font_family(mono)
                  .text_size(12.5)
                  .child(TextView.markdown(`acp-cd-${key}`, fenced).w_full().min_w(0).selectable(true).scrollable(false)),
              ),
          );
      }
      return div()
        .w_full()
        .min_w(0)
        .text_size(13.5)
        .line_height("22px")
        .text_color(t.foreground)
        .child(TextView.markdown(`acp-md-${item.id}-${bi}`, block.text).w_full().min_w(0).selectable(true).scrollable(false));
    });

    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .gap(10)
      .when(item.showSpeakerHeader, (el) => el.child(this.speakerRow(t, item, program, agentName, copyTurnKey, item.text)))
      .children(blocks)
      .when(!item.showSpeakerHeader && Boolean(item.isFinalTurnReply) && Boolean(item.text), (el) =>
        el.child(h_flex().w_full().justify_end().child(this.copyPill(t, copyTurnKey, item.text, tr("Copy reply", "复制回复")))),
      );
  }

  renderTerminalSnapshot(t, item, terminal, mono) {
    const src = item.text || "";
    const fence = fenceFor(src);
    const fenced = `${fence}\n${src}\n${fence}`;
    const lineLabel = tr(`${item.lineCount ?? 0} lines`, `${item.lineCount ?? 0} 行`);
    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .rounded(10)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .overflow_hidden()
      .child(
        h_flex()
          .h(32)
          .px(12)
          .items_center()
          .justify_between()
          .bg(t.raised)
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(7)
              .items_center()
              .child(svg(ICONS.terminal).size(12).text_color(t.dim))
              .child(div().text_size(11.5).font_medium().text_color(t.muted).child(tr("Terminal snapshot", "终端画面快照")))
              .child(div().text_size(11).text_color(t.dim).child(lineLabel)),
          )
          .child(
            h_flex()
              .gap(8)
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
      .child(
        v_flex()
          .w_full()
          .min_w(0)
          .overflow_hidden()
          .child(
            div()
              .min_w(0)
              .m(-1)
              .font_family(mono)
              .text_size(12)
              .child(TextView.markdown(`acp-ts-${item.id}`, fenced).w_full().min_w(0).selectable(true).scrollable(false)),
          ),
      );
  }

  renderErrorItem(t, item, program, agentName) {
    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .gap(8)
      .when(item.showSpeakerHeader, (el) => el.child(this.speakerRow(t, item, program, agentName)))
      .child(
        v_flex()
          .w_full()
          .min_w(0)
          .px(14)
          .py(10)
          .gap(4)
          .rounded(10)
          .bg(t.surface)
          .border_1()
          .border_color(t.blocked ?? "#ef4444")
          .child(div().text_size(11.5).font_semibold().text_color(t.blocked ?? "#ef4444").child(tr("Agent error", "智能体错误")))
          .child(div().text_size(12.5).line_height("19px").text_color(t.foreground).child(item.message)),
      );
  }

  renderStderrItem(t, item, mono) {
    const open = this.expandedGroups[item.id] ?? false;
    return v_flex()
      .id(`acp-row-${item.id}`)
      .w_full()
      .max_w(820)
      .min_w(0)
      .flex_none()
      .gap(6)
      .child(
        h_flex()
          .id(`acp-se-${item.id}`)
          .w_full()
          .h(30)
          .px(10)
          .gap(8)
          .items_center()
          .rounded(8)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            this.expandedGroups[item.id] = !open;
            cx.notify();
          })
          .child(svg(ICONS.terminal).size(12).flex_none().text_color(t.dim))
          .child(div().flex_1().text_size(11.5).text_color(t.dim).child(tr(`Diagnostic stderr (${item.lineCount} line${item.lineCount === 1 ? "" : "s"})`, `诊断日志 stderr（${item.lineCount} 行）`)))
          .child(svg(open ? ICONS.chevronDown : ICONS.chevronRight).size(12).flex_none().text_color(t.dim)),
      )
      .when(open, (el) =>
        el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .rounded(8)
            .bg(t.background)
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

  renderEmptyState(t, k, program, agentName, shortCwd) {
    return v_flex()
      .w_full()
      .max_w(520)
      .min_w(0)
      .flex_none()
      .my(24)
      .p(24)
      .gap(12)
      .items_center()
      .rounded(14)
      .bg(t.surface)
      .border_1()
      .border_color(t.border)
      .child(this.avatarTile(t, program, null, 44, 22))
      .child(div().text_size(15).font_semibold().text_color(t.foreground).child(agentName))
      .when(Boolean(shortCwd), (el) => el.child(div().text_size(12).text_color(t.dim).child(shortCwd)))
      .child(
        div()
          .text_size(12.5)
          .line_height("19px")
          .text_color(t.muted)
          .child(this.loadedOnce ? tr("No messages yet. Send a prompt below to begin.", "暂无消息，请在下方输入提示词开始对话。") : tr("Loading conversation…", "正在加载对话…")),
      );
  }

  renderApprovalBanner(t, perm, terminal, k, isTab = false) {
    const modeOpt = this.configOptionsFor(k).find((o) => o.iconKind === "mode" || o.id === "mode");
    const canSkipAll = Boolean(
      modeOpt?.choices?.some((c) => c.value === "bypassPermissions") &&
      modeOpt.currentValue !== "bypassPermissions" &&
      modeOpt.currentValue !== "dangerously-skip-permissions",
    );
    const padX = isTab ? 18 : 28;
    return v_flex()
      .w_full()
      .px(padX)
      .pt(8)
      .flex_none()
      .items_center()
      .child(
        v_flex()
          .w_full()
          .max_w(820)
          .px(16)
          .py(12)
          .gap(10)
          .rounded(12)
          .bg(t.surface)
          .border_1()
          .border_color(t.blocked ?? "#f59e0b")
          .child(
            h_flex()
              .w_full()
              .gap(8)
              .items_center()
              .child(svg(ICONS.shield).size(15).flex_none().text_color(t.blocked ?? "#f59e0b"))
              .child(div().flex_1().min_w(0).text_size(13).font_semibold().text_color(t.foreground).child(perm.title || tr("Permission approval required", "需要权限确认"))),
          )
          .child(
            h_flex()
              .gap(8)
              .flex_wrap()
              .children(
                (perm.options ?? []).map((opt, idx) =>
                  this.pillButton(
                    t,
                    `perm-opt-${idx}`,
                    opt.name || opt.option_id,
                    () => this.runH2(["approve", "--json", terminal, "--option", opt.option_id]),
                    { primary: idx === 0, outlined: idx !== 0, h: 30, px: 12 },
                  ),
                ),
              )
              .when(!(perm.options?.length), (b) =>
                b.child(this.pillButton(t, "perm-allow", tr("Approve", "允许"), () => this.runH2(["approve", "--json", terminal]), { primary: true, h: 30, px: 12 })),
              )
              .when(canSkipAll, (b) =>
                b.child(
                  this.pillButton(
                    t,
                    "perm-skip-all",
                    "dangerously-skip-permissions",
                    (_e, cx) => this.setConfigOption(modeOpt, "bypassPermissions", cx),
                    { outlined: true, h: 30, px: 12 },
                  ),
                ),
              )
              .child(this.pillButton(t, "perm-deny", tr("Deny", "拒绝"), () => this.runH2(["deny", "--json", terminal]), { danger: true, outlined: true, h: 30, px: 12 })),
          ),
      );
  }

  capabilityIcon(opt) {
    if (opt.iconKind === "model") return ICONS.sparkles;
    if (opt.iconKind === "thinking") return ICONS.lightbulb;
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
    const usage = k?.context_usage ?? this.agentDetail?.context_usage ?? null;
    const hasUsage = Boolean(usage && typeof usage.size === "number" && usage.size > 0);
    if (!isPty && !opts.length && !hasUsage) return null;
    const formatTok = (n) => {
      if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return "0";
      if (n >= 1000000) return `${(n / 1000000).toFixed(1).replace(/\.0$/, "")}M`;
      if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
      return String(Math.round(n));
    };
    const used = usage?.used ?? 0;
    const size = usage?.size ?? 0;
    const pct = hasUsage ? Math.min(100, Math.round((used / size) * 100)) : 0;
    const pctColor = pct >= 85 ? (t.blocked ?? "#ef4444") : pct >= 65 ? (t.done ?? "#f59e0b") : t.muted;
    return h_flex()
      .id("acp-cfg-bar")
      .flex_1()
      .min_w(0)
      .flex_wrap()
      .gap(4)
      .items_center()
      .when(isPty, (el) =>
        el
          .child(
            h_flex()
              .id("acp-switch-terminal")
              .h(26)
              .px(9)
              .gap(5)
              .items_center()
              .rounded(999)
              .bg(t.raised)
              .border_1()
              .border_color(t.border)
              .text_size(11.5)
              .font_medium()
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(tr("Switch back to raw terminal view", "切回原始终端视图"))
              .on_mouse_down("left", stopMouse)
              .on_click(() => run("terminal.view", { terminal: props()?.terminal, view: "terminal" }))
              .child(svg(ICONS.terminal).size(12).flex_none().text_color(t.dim))
              .child(tr("Terminal view", "终端视图")),
          )
          .child(
            div()
              .px(7)
              .py(2)
              .rounded(999)
              .bg(t.raised)
              .border_1()
              .border_color(t.border)
              .text_size(10.5)
              .text_color(t.dim)
              .child(tr("CLI Agent", "终端智能体")),
          ),
      )
      .children(
        opts.map((opt) => {
          const isBool = opt.kind === "boolean";
          const isModel = opt.iconKind === "model";
          const isBypassMode = opt.iconKind === "mode"
            && (opt.currentValue === "bypassPermissions" || opt.currentValue === "dangerously-skip-permissions");
          const pillText = opt.iconKind === "mode" && !isBypassMode
            ? `${tr("Permissions", "权限")}: ${opt.pillLabel}`
            : opt.pillLabel;
          const open = this.openConfigId === opt.id;
          const isClosing = !open && Boolean(this.closingConfigs[opt.id]);
          const on = isBool && opt.currentValue === true;
          const active = isBool ? on : open;
          const glyph = this.capabilityIcon(opt);
          const accentText = t.accent ?? t.foreground;
          const warnColor = t.blocked ?? accentText;
          const glyphColor = isModel
            ? (t.accent ?? t.muted)
            : isBypassMode
              ? warnColor
              : (isBool && on) || (opt.iconKind === "extra" && opt.isNonDefault)
                ? accentText
                : active
                  ? t.foreground
                  : t.dim;
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
            .px(isModel ? 10 : 9)
            .gap(isModel ? 6 : isBool ? 4.5 : 5)
            .items_center()
            .rounded(999)
            .text_size(isModel ? 12.5 : 12)
            .transition("opacity", 120)
            .when(isModel || (isBool && on) || isBypassMode, (el) => el.font_medium())
            .when(isModel, (el) =>
              el.bg(open ? (t.selected ?? t.hover) : t.raised)
                .when(open, (b) => b.border_1().border_color(t.accent ?? t.border)),
            )
            .when(!isModel && isBool && on, (el) => el.bg(withAlpha(t.accent ?? "#2563eb", "1f")))
            .when(!isModel && isBypassMode && !open, (el) => el.bg(withAlpha(t.blocked ?? "#f59e0b", "18")))
            .when(!isModel && !isBool && open, (el) => el.bg(t.selected ?? t.hover))
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
            .child(svg(glyph).size(13).flex_none().text_color(glyphColor))
            .child(div().max_w(isBypassMode ? 220 : 200).truncate().child(pillText))
            .when(!isBool, (el) => el.child(svg(open ? ICONS.chevronUp : ICONS.chevronDown).size(10).flex_none().text_color(t.dim)));

          if (isBool) return trigger;

          const phase = open ? (this.enteringConfigId === opt.id ? "entering" : "open") : "closing";
          return Popup.new(`acp-popup-${opt.id}`, trigger)
            .anchor("bottom_left")
            .when(open || isClosing, (el) => el.content(this.renderConfigPopupContent(t, opt, phase)));
        }),
      )
      .when(hasUsage, (el) =>
        el.child(
          h_flex()
            .id("acp-ctx-usage")
            .h(24)
            .px(8)
            .gap(5)
            .flex_none()
            .items_center()
            .rounded(999)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .tooltip(tr(`Context window: ${formatTok(used)} / ${formatTok(size)} tokens (${pct}%)`, `上下文占用：${formatTok(used)} / ${formatTok(size)} tokens (${pct}%)`))
            .child(
              div()
                .w(28)
                .h(4)
                .rounded(2)
                .bg(t.border)
                .overflow_hidden()
                .child(div().h_full().w(`${pct}%`).bg(pctColor)),
            )
            .child(div().text_size(11).font_medium().text_color(pctColor).child(`${pct}% · ${formatTok(used)}/${formatTok(size)}`)),
        ),
      );
  }

  renderComposer(t, k, st, isTab = false) {
    const queued = k?.inbox_len ?? 0;
    const ended = st === "ended" || k?.state === "finished";
    const hasText = Boolean((this.input?.value?.() ?? "").trim());
    const opts = this.configOptionsFor(k);
    const capBar = ended ? null : this.renderCapabilityBar(t, opts, k);
    const canSend = !ended && hasText && !this.busyAction;
    const padX = isTab ? 18 : 28;
    const sendTip = ended
      ? tr("Session ended · transcript preserved", "会话已结束 · 对话记录已保留")
      : st === "working"
        ? tr("Queue message (Enter) · Ctrl+Enter steers · Shift+Enter for newline", "加入队列 (Enter) · Ctrl+Enter 插话 · Shift+Enter 换行")
        : tr("Send message (Enter) · Shift+Enter for newline", "发送消息 (Enter) · Shift+Enter 换行");

    return v_flex()
      .id("acp-composer-wrap")
      .w_full()
      .px(padX)
      .pt(8)
      .pb(isTab ? 16 : 20)
      .flex_none()
      .items_center()
      .bg(t.canvas ?? t.background)
      .child(
        v_flex()
          .id("acp-composer-card")
          .w_full()
          .max_w(820)
          .rounded(18)
          .border_1()
          .border_color(this.inputFocused ? (t.accent ?? t.input) : t.border)
          .bg(t.surface)
          .shadow_md()
          .overflow_hidden()
          .child(
            v_flex()
              .id("acp-input-area")
              .w_full()
              .min_h(72)
              .px(18)
              .pt(14)
              .pb(10)
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
              .min_h(44)
              .px(12)
              .py(6)
              .gap(10)
              .items_center()
              .justify_between()
              .child(
                capBar ?? h_flex().flex_1().min_w(0).when(ended, (el) =>
                  el.child(div().px(8).py(2).rounded(999).bg(t.raised).border_1().border_color(t.border).text_size(11).text_color(t.muted).child(tr("Session ended · transcript preserved", "会话已结束 · 对话记录已保留"))),
                ),
              )
              .child(
                h_flex()
                  .gap(8)
                  .items_center()
                  .flex_none()
                  .when(queued > 0, (el) =>
                    el.child(
                      div()
                        .px(8)
                        .py(2)
                        .rounded(999)
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.border)
                        .text_size(11)
                        .text_color(t.accent ?? t.muted)
                        .flex_none()
                        .child(tr(`${queued} queued`, `队列 ${queued}`)),
                    ),
                  )
                  .when(st === "working" && !ended, (el) =>
                    el.child(
                      this.pillButton(
                        t,
                        "acp-stop",
                        h_flex()
                          .gap(5)
                          .items_center()
                          .child(svg(this.interrupting ? ICONS.spinner : ICONS.square).size(10).when(this.interrupting, (s) => s.spin(900)))
                          .child(this.interrupting ? tr("Stopping…", "停止中…") : tr("Stop · Esc", "停止 · Esc")),
                        (_e, cx) => {
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.interruptAgent(props()?.terminal, cx);
                        },
                        { danger: true, outlined: true, h: 30, px: 11, disabled: this.interrupting },
                      ),
                    ),
                  )
                  .when(st === "working" && hasText && !ended, (el) =>
                    el.child(
                      h_flex()
                        .id("acp-steer")
                        .h(30)
                        .px(12)
                        .items_center()
                        .justify_center()
                        .rounded(999)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.raised)
                        .text_size(12)
                        .font_medium()
                        .text_color(t.foreground)
                        .when(!this.busyAction, (b) => b.cursor_pointer().hover((s) => s.bg(t.hover)))
                        .on_mouse_down("left", stopMouse)
                        .on_click((_e, cx) => {
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.sendPrompt(true);
                        })
                        .child(tr("Steer", "插话")),
                    ),
                  )
                  .child(
                    h_flex()
                      .id("acp-send")
                      .size(32)
                      .rounded(999)
                      .flex_none()
                      .items_center()
                      .justify_center()
                      .bg(canSend ? (t.solid ?? t.foreground) : t.raised)
                      .text_color(canSend ? (t.solid_foreground ?? t.background) : t.dim)
                      .when(canSend, (el) => el.shadow_sm().cursor_pointer().hover((s) => s.opacity(0.88)))
                      .tooltip(sendTip)
                      .when(canSend, (el) =>
                        el.on_mouse_down("left", stopMouse).on_click((_e, cx) => {
                          if (this.openConfigId) this.closeConfigPopover(cx);
                          this.sendPrompt(false);
                        }),
                      )
                      .child(svg(ICONS.arrowUp).size(16)),
                  ),
              ),
          ),
      );
  }

  renderPtyBlockedBanner(t, terminal, isTab = false) {
    const padX = isTab ? 18 : 28;
    return v_flex()
      .id("pty-blocked-banner")
      .w_full()
      .px(padX)
      .pt(8)
      .flex_none()
      .items_center()
      .child(
        h_flex()
          .w_full()
          .max_w(820)
          .px(16)
          .py(10)
          .gap(10)
          .items_center()
          .rounded(12)
          .bg(t.surface)
          .border_1()
          .border_color(t.blocked ?? "#f59e0b")
          .child(svg(ICONS.shield).size(15).flex_none().text_color(t.blocked ?? "#f59e0b"))
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
    const myPane = pane ?? panes().find((p) => (p.acp || p.structured) && p.terminal === terminal)?.pane ?? null;
    const key = `${terminal}:${st}:${k?.turn ?? 0}:${k?.transcript_seq ?? 0}:${k?.pending_permission?.request_id ?? ""}:${k?.printed ?? 0}:${k?.status_ms ?? 0}`;
    if (!checking() && key !== this.lastKey) {
      this.lastKey = key;
      Promise.resolve().then(() => this.reloadTranscript());
    }
    const perm = k?.pending_permission;
    const mono = settings()?.font_family ?? "JetBrains Mono";
    const program = k?.program;
    const agentName = k?.labels?.["herdr.name"] || k?.name || k?.template || program?.name || terminal || "ACP Agent";
    const shortCwd = shortenPath(k?.cwd, home());
    const items = buildConversationItems(this.entries, this.fallbackText, st === "working", {
      isPty,
      how: this.readHow,
      turn: k?.turn ?? 0,
      isAgent: Boolean(program?.agent || k?.status || st),
    });

    const threadChildren = [];
    if (items.length === 0) {
      threadChildren.push(this.renderEmptyState(t, k, program, agentName, shortCwd));
    } else {
      for (const item of items) {
        if (item.kind === "user") threadChildren.push(this.renderUserItem(t, item, terminal));
        else if (item.kind === "activity_group") threadChildren.push(this.renderActivityGroup(t, item, program, agentName, mono));
        else if (item.kind === "assistant") threadChildren.push(this.renderAssistantItem(t, item, program, agentName, mono));
        else if (item.kind === "terminal_snapshot") threadChildren.push(this.renderTerminalSnapshot(t, item, terminal, mono));
        else if (item.kind === "error") threadChildren.push(this.renderErrorItem(t, item, program, agentName));
        else if (item.kind === "stderr_group") threadChildren.push(this.renderStderrItem(t, item, mono));
      }
    }

    if (st === "working") {
      threadChildren.push(
        h_flex()
          .id("acp-working-tail")
          .w_full()
          .max_w(820)
          .min_w(0)
          .flex_none()
          .gap(8)
          .items_center()
          .py(4)
          .child(svg(ICONS.spinner).size(13).text_color(t.working ?? t.accent).spin(900))
          .child(div().text_size(12).text_color(t.muted).child(this.interrupting ? tr("Stopping…", "正在中断…") : tr("Agent is working… · Press Esc to stop", "智能体正在执行… · 按 Esc 中断"))),
      );
    }

    threadChildren.push(div().id("acp-bottom-anchor").h(4).flex_none());

    const last = items[items.length - 1];
    const tailSig = `${items.length}:${last?.id ?? ""}:${last?.text?.length ?? 0}:${last?.steps?.length ?? 0}:${st ?? ""}:${Boolean(perm)}`;
    if (this.autoFollow && tailSig !== this.revealedSig && threadChildren.length > 0) {
      this.revealedSig = tailSig;
      this.scroll.scroll_to_item(threadChildren.length - 1);
    }

    const padX = isTab ? 18 : 28;
    const modalHeader = h_flex()
      .id("acp-header")
      .min_h(54)
      .px(18)
      .py(8)
      .gap(12)
      .flex_none()
      .items_center()
      .bg(t.surface)
      .border_b(1)
      .border_color(t.border)
      .child(this.avatarTile(t, program, pic, 32, 17))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(
            h_flex()
              .gap(7)
              .items_center()
              .child(div().truncate().text_size(13.5).font_semibold().text_color(t.foreground).child(agentName))
              .child(div().px(6).py(1).rounded(5).bg(t.raised).border_1().border_color(t.border).text_size(10.5).font_medium().text_color(t.muted).flex_none().child("ACP"))
              .when(Boolean(terminal), (el) =>
                el.child(div().px(6).py(1).rounded(5).bg(t.raised).font_family(mono).text_size(11).text_color(t.dim).flex_none().child(terminal)),
              ),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .text_size(11.5)
              .text_color(t.dim)
              .when(Boolean(parent), (el) => el.child(div().flex_none().text_color(t.muted).child(tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`))))
              .when(Boolean(shortCwd), (el) => el.child(div().min_w(0).truncate().child(shortCwd))),
          ),
      )
      .child(
        h_flex()
          .gap(6)
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
              { outlined: true, h: 26, px: 9 },
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

    return v_flex()
      .id("acp-peek")
      .size_full()
      .when(!isTab, (el) => el.rounded(14).border_1().border_color(t.border).shadow_2xl())
      .bg(t.canvas ?? t.background)
      .text_color(t.foreground)
      .overflow_hidden()
      .track_focus(this.handle)
      .on_mouse_down("left", (_e, cx) => {
        if (this.openConfigId) this.closeConfigPopover(cx);
        if (!this.inputFocused && !this.configFilterFocused) this.handle?.focus?.();
      })
      .when(isTab && Boolean(myPane), (el) =>
        el.on_mouse_down("right", (_e, cx) => {
          cx.stop_propagation();
          openOverlay({ mod: "menu", kind: "popup", width: 280, props: { tab: "pane", target: { pane: myPane, terminal } } });
        }),
      )
      .on_key_down((e, cx) => {
        if (e.key === "escape" && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
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
            .bg(t.surface)
            .border_b(1)
            .border_color(t.blocked ?? "#ef4444")
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
        v_flex()
          .id("acp-transcript")
          .flex_1()
          .min_h(0)
          .w_full()
          .overflow_x_hidden()
          .px(padX)
          .py(isTab ? 14 : 20)
          .gap(16)
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
      .when(isTab && !this.autoFollow, (el) =>
        el.child(
          h_flex()
            .id("acp-latest-bar")
            .w_full()
            .px(padX)
            .pt(4)
            .flex_none()
            .justify_center()
            .child(
              this.pillButton(
                t,
                "acp-latest-inline",
                tr("↓ Jump to latest", "↓ 跳到最新"),
                (_e, cx) => {
                  this.autoFollow = true;
                  this.revealedSig = "";
                  this.scroll.scroll_to_item(threadChildren.length - 1);
                  cx.notify();
                },
                { outlined: true, h: 26, px: 12 },
              ),
            ),
        ),
      )
      .when(Boolean(perm), (el) => el.child(this.renderApprovalBanner(t, perm, terminal, k, isTab)))
      .when(isPty && st === "blocked" && !perm, (el) => el.child(this.renderPtyBlockedBanner(t, terminal, isTab)))
      .child(this.renderComposer(t, k, st, isTab));
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
