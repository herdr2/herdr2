// A text prompt (rename/input) or AI prompt polish composer (mode: "polish").
// Enter saves (or Ctrl/Cmd+Enter sends in polish mode), Esc cancels.
import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextareaState, Textarea } from "gpui-base";
import { run, h2, props, closeOverlay, theme, settings, checking, nextCommand, keys, window as appWindow } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const PURPLE = "#a855f7";

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

function withAlpha(color, alphaHex) {
  if (typeof color !== "string") return color;
  if (/^#[0-9a-fA-F]{6}$/.test(color)) return `${color}${alphaHex}`;
  if (/^#[0-9a-fA-F]{8}$/.test(color)) return `${color.slice(0, 7)}${alphaHex}`;
  return color;
}

export default class Prompt extends View {
  init(_props, cx) {
    const p = props() ?? {};
    this.mode = p.mode === "polish" ? "polish" : "text";
    this.text = p.value ?? "";
    this.polishing = false;
    this.sending = false;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.error = null;

    if (this.mode === "polish") {
      this.textarea = TextareaState.new({
        value: this.text,
        placeholder: tr(
          "Write your rough prompt, click Polish for clarity, then send to the agent…",
          "输入原始提示词，点击「润色」优化表达后直接发给 Agent…",
        ),
      });
      this.textarea.set_auto_grow(5, 11);
      this.textarea.on("change", (_e, c) => {
        this.text = this.textarea.value();
        this.error = null;
        c.notify();
      });
      this.textarea.on("submit", (e, c) => {
        if (e?.shift) return;
        if (e?.secondary || e?.modifiers?.control || e?.modifiers?.platform) {
          this.sendPolished(c);
        }
      });
      this.inputFocused = true;
      this.textarea.on("focus", (_e, c) => { this.inputFocused = true; c.notify(); });
      this.textarea.on("blur", (_e, c) => { this.inputFocused = false; c.notify(); });
      if (!checking()) {
        this.textarea.focus();
        cx.timer.after(200, () => this.textarea?.focus?.());
        if (typeof nextCommand === "function") {
          cx.spawn(async (cmdCx) => {
            for (;;) {
              const cmd = await nextCommand();
              if (cmd === "polish") await this.polish(cmdCx);
              else if (cmd === "polish_undo") this.revertPolish(cmdCx);
            }
          });
        }
      }
    } else {
      this.input = InputState.new({ value: this.text });
      if (this.text) this.input.set_selected_range({ start: 0, end: this.text.length });
      this.input.on("change", (_e, c) => { this.text = this.input.value(); c.notify(); });
      this.input.on("submit", () => this.submit());
      this.inputFocused = true;
      this.input.on("focus", (_e, c) => { this.inputFocused = true; c.notify(); });
      this.input.on("blur", (_e, c) => { this.inputFocused = false; c.notify(); });
      this.input.focus();
      cx.timer.after(600, () => this.input?.focus?.());
    }
    this.handle = cx.focus_handle();
  }

  submit() {
    const p = props();
    closeOverlay(p.overlay);
    run(p.command.name, { ...(p.command.args ?? {}), name: this.text });
  }

  revertPolish(cx) {
    if (this.polishUndoText == null) return;
    const orig = this.polishUndoText;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.text = orig;
    this.textarea?.set_value?.(orig);
    this.textarea?.focus?.();
    cx.notify();
  }

  async polish(cx) {
    const raw = String(this.textarea?.value?.() ?? this.text ?? "").trim();
    if (!raw || this.polishing || this.sending || checking()) return;
    const p = props() ?? {};
    const terminal = p.terminal || null;
    const kind = p.agentKind || "cli";
    const machine = terminal && terminal.includes("/") ? terminal.split("/")[0] : null;
    this.polishing = true;
    this.error = null;
    cx.notify();
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
      if (p.cwd) args.push("--context", `cwd=${p.cwd}`);
      if (machine) args.push("--on", machine);
      args.push("--", raw);
      const res = await h2(args, { timeout: 22000 });
      if (res?.code === 0 && res.stdout) {
        const parsed = JSON.parse(res.stdout);
        const polished = String(parsed?.polished ?? "").trim();
        if (parsed?.usedAi && polished) {
          this.polishUndoText = raw;
          this.polishSummary = parsed?.summary ? String(parsed.summary).trim() : null;
          this.text = polished;
          this.textarea?.set_value?.(polished);
          this.textarea?.focus?.();
        } else {
          const reason = String(parsed?.fallbackReason ?? "");
          this.error = reason === "ai_not_configured"
            ? tr("Core AI is not configured. Enable it in Settings → AI Assistant first.", "Core AI 尚未配置，请先在「设置 → AI 助手」中启用后再使用润色功能。")
            : reason === "ai_circuit_open"
              ? tr("Core AI is temporarily cooling down after recent failures.", "Core AI 暂时熔断冷却中，请稍后重试。")
              : tr("AI polish returned no changes. Please retry.", "AI 润色暂未返回有效结果，请重试。");
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
      this.polishing = false;
      cx.notify();
    }
  }

  async sendPolished(cx) {
    const p = props() ?? {};
    const terminal = p.terminal;
    const text = String(this.textarea?.value?.() ?? this.text ?? "").trim();
    if (!terminal || !text || this.sending || this.polishing || checking()) return;
    this.sending = true;
    this.error = null;
    cx.notify();
    try {
      const res = await h2(["send", "--json", terminal, text]);
      if (res?.code === 0) {
        closeOverlay(p.overlay);
      } else {
        let msg = String(res?.stderr || res?.stdout || "").trim();
        try {
          const parsed = JSON.parse(res?.stderr || res?.stdout || "{}");
          if (parsed?.error?.message) msg = parsed.error.message;
        } catch {}
        this.error = msg || tr("Failed to send prompt", "发送提示词失败");
      }
    } catch (e) {
      this.error = String(e?.message ?? e);
    } finally {
      this.sending = false;
      cx.notify();
    }
  }

  key(e, cx) {
    if (e.key === "escape") {
      closeOverlay(props().overlay);
      cx.stop_propagation();
    }
  }

  renderPolish(t, p) {
    const raw = String(this.textarea?.value?.() ?? this.text ?? "").trim();
    const canPolish = Boolean(raw && !this.polishing && !this.sending);
    const canSend = Boolean(raw && p.terminal && !this.sending && !this.polishing);
    const agentName = p.agentName || p.terminal || "Agent";
    const kindLabel = (p.agentKind || "cli").toUpperCase();

    return v_flex()
      .id("prompt-polish-modal")
      .track_focus(this.handle)
      .on_key_down((e, cx) => this.key(e, cx))
      .size_full()
      .min_h(0)
      .p(16)
      .gap(10)
      .bg(t.popover)
      .text_color(t.foreground)
      .child(
        h_flex()
          .w_full()
          .gap(8)
          .items_center()
          .flex_none()
          .child(svg("mods/peek/sparkles.svg").size(15).flex_none().text_color(PURPLE))
          .child(div().text_size(14.5).font_semibold().child(tr("AI Prompt Polish", "AI 提示词润色")))
          .child(
            div()
              .px(6)
              .py(1)
              .rounded(4)
              .bg(t.raised)
              .border_1()
              .border_color(t.border)
              .text_size(10.5)
              .text_color(t.muted)
              .child(`${kindLabel} · ${agentName}`),
          )
          .child(div().flex_1())
          .child(div().text_size(11).text_color(t.dim).child("Esc")),
      )
      .when(this.polishUndoText != null, (el) =>
        el.child(
          h_flex()
            .id("pm-polish-banner")
            .w_full()
            .px(10)
            .py(6)
            .gap(8)
            .flex_none()
            .items_center()
            .rounded(7)
            .bg(withAlpha(PURPLE, "16") ?? t.raised)
            .border_1()
            .border_color(withAlpha(PURPLE, "44") ?? t.border)
            .child(svg("mods/peek/sparkles.svg").size(12).flex_none().text_color(PURPLE))
            .child(
              div()
                .flex_1()
                .min_w(0)
                .truncate()
                .text_size(11.5)
                .text_color(t.foreground)
                .child(
                  this.polishSummary
                    ? tr(`Polished · ${this.polishSummary}`, `已润色 · ${this.polishSummary}`)
                    : tr("Polished by Core AI for clarity", "已由 Core AI 润色，表达更清晰"),
                ),
            )
            .child(
              div()
                .id("pm-polish-undo")
                .flex_none()
                .px(7)
                .py(2)
                .rounded(5)
                .bg(t.background)
                .border_1()
                .border_color(t.border)
                .text_size(11)
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .tooltip(withShortcut(tr("Undo last polish and restore original input", "撤销本次润色并恢复润色前的内容"), "prompt.polish_undo"))
                .on_click((_e, cx) => this.revertPolish(cx))
                .child(tr("↩ Undo", "↩ 反悔恢复")),
            ),
        ),
      )
      .child(
        div()
          .id("pm-polish-input-wrap")
          .flex_1()
          .min_h(120)
          .w_full()
          .px(10)
          .py(8)
          .rounded(9)
          .bg(t.background)
          .border_1()
          .border_color(this.inputFocused ? PURPLE : t.input)
          .cursor_text()
          .on_click(() => this.textarea?.focus?.())
          .child(
            Textarea.new(this.textarea)
              .size_full()
              .text_size(13)
              .line_height("20px")
              .text_color(t.foreground),
          ),
      )
      .when(Boolean(this.error), (el) =>
        el.child(
          div()
            .id("pm-polish-error")
            .px(9)
            .py(5)
            .rounded(6)
            .bg(withAlpha(t.blocked ?? "#ef4444", "18") ?? t.raised)
            .border_1()
            .border_color(withAlpha(t.blocked ?? "#ef4444", "55") ?? t.border)
            .text_size(11.5)
            .text_color(t.blocked ?? "#ef4444")
            .whitespace_normal()
            .child(this.error),
        ),
      )
      .child(
        h_flex()
          .w_full()
          .gap(8)
          .items_center()
          .flex_none()
          .child(div().text_size(11).text_color(t.dim).child(tr("Ctrl+Enter to send", "Ctrl+Enter 直接发送")))
          .child(div().flex_1())
          .child(
            h_flex()
              .id("pm-polish-cancel")
              .h(30)
              .px(12)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(t.input)
              .bg(t.raised)
              .text_size(12)
              .text_color(t.foreground)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_click(() => closeOverlay(p.overlay))
              .child(tr("Cancel", "取消")),
          )
          .child(
            h_flex()
              .id("pm-polish-run")
              .h(30)
              .px(12)
              .gap(5)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(canPolish || this.polishing ? withAlpha(PURPLE, "66") : t.border)
              .bg(canPolish || this.polishing ? withAlpha(PURPLE, "1c") : t.raised)
              .text_size(12)
              .font_semibold()
              .text_color(canPolish || this.polishing ? PURPLE : t.dim)
              .when(canPolish, (b) => b.cursor_pointer().hover((s) => s.bg(withAlpha(PURPLE, "30"))))
              .tooltip(withShortcut(tr("Polish prompt with Core AI for clarity", "使用 Core AI 润色提示词，让表达更清晰"), "prompt.polish"))
              .on_click((_e, cx) => this.polish(cx))
              .child(
                this.polishing
                  ? svg("mods/workspace-chrome/spinner.svg").size(12).spin(900).text_color(PURPLE)
                  : svg("mods/peek/sparkles.svg").size(12).text_color(canPolish ? PURPLE : t.dim),
              )
              .child(this.polishing ? tr("Polishing…", "润色中…") : tr("AI Polish", "AI 润色")),
          )
          .child(
            h_flex()
              .id("pm-polish-send")
              .h(30)
              .px(14)
              .gap(5)
              .items_center()
              .rounded(7)
              .bg(canSend ? t.solid : t.raised)
              .text_size(12)
              .font_medium()
              .text_color(canSend ? t.solid_foreground : t.dim)
              .when(canSend, (b) => b.cursor_pointer().hover((s) => s.opacity(0.9)).on_click((_e, cx) => this.sendPolished(cx)))
              .child(svg("mods/peek/arrow-up.svg").size(12))
              .child(this.sending ? tr("Sending…", "发送中…") : tr("Send", "发送")),
          ),
      );
  }

  render() {
    const t = theme();
    const p = props() ?? {};
    if (this.mode === "polish") {
      return this.renderPolish(t, p);
    }
    const label = (text) => div().pt(16).pb(7).text_size(11.5).text_color(t.dim).child(text);
    const button = (id, text, primary, action) => h_flex().id(id).h(32).px(14).items_center().rounded(8)
      .border_1().border_color(primary ? t.solid : t.input).bg(primary ? t.solid : t.raised)
      .text_color(primary ? t.solid_foreground : t.foreground).text_size(12.5).font_medium().cursor_pointer()
      .hover(s => s.opacity(0.88)).on_click(action).child(text);
    return v_flex().id("prompt").track_focus(this.handle).on_key_down((e, cx) => this.key(e, cx))
      .w_full().min_h(0).p(18).bg(t.popover).text_color(t.foreground)
      .child(h_flex().items_center().child(div().flex_1().text_size(15).font_semibold().child((p.title ?? "").replace(/^./, (c) => c.toUpperCase())))
        .child(div().text_size(11).text_color(t.dim).child("Esc")))
      .child(label(tr("Name", "名称")))
      .child(Input.new(this.input).h(36).px(10).rounded(9).border_1().border_color(this.inputFocused ? t.accent : t.input).bg(t.background).text_size(12.8).text_color(t.foreground))
      .when(Boolean(p.hint), el => el.child(div().pt(6).text_size(11.5).text_color(t.dim).child(p.hint)))
      .child(h_flex().pt(20).gap(8).justify_end()
        .child(button("pm-cancel", tr("Cancel", "取消"), false, () => closeOverlay(p.overlay)))
        .child(button("pm-save", tr("Save", "保存"), true, () => this.submit())));
  }
}
