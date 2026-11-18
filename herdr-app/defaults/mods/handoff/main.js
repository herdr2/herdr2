// Cross-provider Context Handoff, Session Fork, Side Chat, and Merge Back dialog.
// Props: { overlay, source, kind? }.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, TextareaState, Textarea } from "gpui-base";
import { terminals, spaces, knownAgents, run, props, closeOverlay, theme, settings, shared, setShared } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

const ACP_PRESETS = ["claude", "codex", "gemini", "agy", "cursor", "opencode", "qwen", "kimi", "copilot", "hermes", "glm", "pi"];
const ACP_ALIAS = { antigravity: "agy" };

const MODE_LIST = () => [
  { id: "handoff", label: tr("Handoff", "上下文接力") },
  { id: "fork", label: tr("Fork", "分支探索") },
  { id: "side-chat", label: tr("Side Chat", "旁路讨论") },
  { id: "merge-back", label: tr("Merge Back", "合回父会话") },
];

const TURN_CHOICES = () => [
  { value: 4, label: tr("4 turns", "最近 4 轮") },
  { value: 8, label: tr("8 turns", "最近 8 轮") },
  { value: 15, label: tr("15 turns", "最近 15 轮") },
  { value: 0, label: tr("All turns", "全部轮次") },
];

function providerPresetOf(rec) {
  if (!rec) return null;
  const raw = String(rec.agent || rec.program?.key || rec.report?.app || rec.template || "").toLowerCase();
  for (const key of ["claude", "codex", "gemini", "agy", "antigravity", "cursor", "opencode", "qwen", "kimi", "copilot", "hermes", "glm", "pi"]) {
    if (raw.includes(key)) return ACP_ALIAS[key] ?? key;
  }
  return null;
}

function sessionLabel(rec) {
  if (!rec) return "";
  const custom = rec.labels?.["herdr.name"];
  const name = custom || rec.name || rec.template || rec.program?.name || rec.title || rec.address;
  return name && name !== rec.address ? `${name} (${rec.address})` : (rec.address || "");
}

export default class Handoff extends View {
  init(_props, cx) {
    this.handle = cx.focus_handle();
    this.promptInput = TextareaState.new({
      value: "",
      placeholder: "",
    });
    this.promptInput.set_auto_grow(3, 7);
    this.promptFocused = false;
    this.promptInput.on("focus", (_e, cx) => { this.promptFocused = true; cx.notify(); });
    this.promptInput.on("blur", (_e, cx) => { this.promptFocused = false; cx.notify(); });
    this.promptInput.on("change", (_e, cx) => { cx.notify(); });
    this.promptInput.on("submit", (e) => {
      if (e?.ctrl || e?.meta || !e?.shift) {
        this.submit();
      }
    });

    const machine = spaces().find((s) => s.active)?.machine ?? "";
    this.knownList = Array.isArray(shared("new-menu.agents")?.[machine]) ? shared("new-menu.agents")[machine] : [];
    this.recentTurns = 8;
    this.preset = "";
    this.runtimeMode = "acp";
    this.openIn = "tab";
    this.intoTarget = "";
    this.mode = "handoff";
    this.lastPropsKey = "";

    this.syncFromProps(true);

    cx.spawn(async (cx) => {
      try {
        const res = await knownAgents({ on: machine || undefined });
        if (Array.isArray(res) && res.length > 0) {
          this.knownList = res;
          setShared("new-menu.agents", { ...(shared("new-menu.agents") ?? {}), [machine]: res });
          this.ensurePresetSelected();
          cx.notify();
        }
      } catch {}
    });
    this.handle.focus();
  }

  sourceRecord(sourceAddr) {
    return terminals().find((x) => x.address === sourceAddr) ?? null;
  }

  availablePresets() {
    const out = [];
    const seen = new Set();
    const pushPreset = (key, label, installed = true) => {
      const k = ACP_ALIAS[key] ?? key;
      if (!k || seen.has(k)) return;
      seen.add(k);
      out.push({ key: k, label: label || k, installed });
    };
    for (const a of this.knownList) {
      if (!a?.installed) continue;
      const mapped = ACP_ALIAS[a.key] ?? a.key;
      if (ACP_PRESETS.includes(mapped)) {
        pushPreset(mapped, a.name || mapped, true);
      }
    }
    for (const k of ACP_PRESETS) {
      pushPreset(k, k, this.knownList.length === 0);
    }
    return out;
  }

  otherLiveSessions(sourceAddr) {
    return terminals().filter(
      (x) =>
        x?.address &&
        x.address !== sourceAddr &&
        x.state === "live" &&
        !x.labels?.["herdr.trashed"],
    );
  }

  ensurePresetSelected() {
    const src = this.sourceRecord(props()?.source);
    const srcPreset = providerPresetOf(src);
    const all = this.availablePresets();
    if (this.mode === "fork" && srcPreset) {
      this.preset = srcPreset;
      return;
    }
    if (!this.preset || !all.some((p) => p.key === this.preset)) {
      const alt = all.find((p) => p.installed && p.key !== srcPreset) ?? all[0];
      this.preset = alt?.key || srcPreset || "claude";
    }
  }

  applyMode(nextMode, resetTargets = false) {
    const p = props() ?? {};
    const sourceAddr = p.source || "";
    const src = this.sourceRecord(sourceAddr);
    this.mode = MODE_LIST().some((m) => m.id === nextMode) ? nextMode : "handoff";

    if (this.mode === "side-chat") {
      this.openIn = "split";
    } else if (this.mode === "handoff" || this.mode === "fork") {
      if (resetTargets) this.openIn = "tab";
    }

    if (this.mode === "fork") {
      const sp = providerPresetOf(src);
      if (sp) this.preset = sp;
    } else if (resetTargets) {
      this.preset = "";
    }
    this.ensurePresetSelected();

    const parentAddr = src?.lineage?.parent || src?.parent || "";
    const others = this.otherLiveSessions(sourceAddr);
    if (resetTargets || !this.intoTarget || !others.some((o) => o.address === this.intoTarget)) {
      if (parentAddr && others.some((o) => o.address === parentAddr)) {
        this.intoTarget = parentAddr;
      } else {
        this.intoTarget = parentAddr || others[0]?.address || "";
      }
    }
  }

  syncFromProps(force = false) {
    const p = props() ?? {};
    const key = `${p.source ?? ""}:${p.kind ?? ""}`;
    if (force || key !== this.lastPropsKey) {
      this.lastPropsKey = key;
      this.applyMode(p.kind || "handoff", true);
    }
  }

  modeCopy(sourceLabel) {
    switch (this.mode) {
      case "fork":
        return {
          title: tr(`Fork Session · ${sourceLabel}`, `分支探索 · ${sourceLabel}`),
          subtitle: tr(
            "Branch from the current session state (uses native fork when same provider supports it).",
            "基于当前会话拉出独立分支并行尝试（同 Provider 支持时优先使用原生 Fork）。",
          ),
          placeholder: tr("Optional direction for this branch…", "可选：输入该分支要尝试的新方向或假设…"),
          submitLabel: tr("Create Fork", "创建分支"),
        };
      case "side-chat":
        return {
          title: tr(`Side Chat · ${sourceLabel}`, `旁路讨论 · ${sourceLabel}`),
          subtitle: tr(
            "Open an isolated side-channel session with current context — explore without polluting main context, then merge back anytime.",
            "带当前上下文开启隔离的旁路会话——放心追问、交叉求证而不污染主线上下文，讨论完可一键合回。",
          ),
          placeholder: tr(
            "What would you like to discuss or verify in this side chat?",
            "输入你想在旁路探讨、求证或让另一个模型帮忙参谋的问题…",
          ),
          submitLabel: tr("Start Side Chat", "开启旁路讨论"),
        };
      case "merge-back":
        return {
          title: tr(
            `Merge Back · ${sourceLabel} → ${this.intoTarget || "?"}`,
            `合回上下文 · ${sourceLabel} → ${this.intoTarget || "?"}`,
          ),
          subtitle: tr(
            "Inject summarized takeaways from this session into the target/parent session and trigger a follow-up turn.",
            "将当前会话的结论与上下文打包注入回父会话（或目标会话），并自动触发主线继续推进。",
          ),
          placeholder: tr(
            "Optional instruction when merging back (leave blank to auto-summarize)…",
            "可选：补充合回后的处理要求（留空则自动提示主会话吸收接力内容并继续）…",
          ),
          submitLabel: tr("Merge Back Now", "确认合回"),
        };
      default:
        return {
          title: tr(`Context Handoff · ${sourceLabel}`, `上下文接力 · ${sourceLabel}`),
          subtitle: tr(
            "Compile recent turns & workspace git state into a clean handoff package for another agent/provider.",
            "将近期对话与当前工作区改动提炼为轻量交接包，无缝接力给另一个 Agent 继续执行。",
          ),
          placeholder: tr(
            "Optional instruction for the receiving agent…",
            "可选：给接力目标 Agent 的启动指令（例如：按上述方案继续实现并跑测试）…",
          ),
          submitLabel: tr("Start Handoff", "开始接力"),
        };
    }
  }

  canSubmit() {
    const p = props() ?? {};
    if (!p.source) return false;
    if (this.mode === "merge-back") return Boolean(this.intoTarget);
    return Boolean(this.preset);
  }

  submit() {
    if (!this.canSubmit()) return;
    const p = props() ?? {};
    const promptText = String(this.promptInput?.value?.() ?? "").trim();
    const args = {
      source: p.source,
      kind: this.mode,
    };
    if (this.recentTurns > 0) {
      args.recent_turns = this.recentTurns;
    }
    if (promptText) {
      args.prompt = promptText;
    }
    if (this.mode === "merge-back") {
      args.into = this.intoTarget;
    } else {
      args.preset = this.preset;
      args.mode = this.runtimeMode;
      args.open = this.openIn;
    }
    closeOverlay(p.overlay);
    run("agent.handoff", args);
  }

  chip(t, id, label, active, onClick, badge = null) {
    const accent = t.accent ?? t.solid ?? "#3b82f6";
    return h_flex()
      .id(id)
      .h(26)
      .px(9)
      .gap(5)
      .items_center()
      .rounded(7)
      .border_1()
      .border_color(active ? accent : t.border)
      .bg(active ? (t.selected ?? t.raised) : t.raised)
      .text_size(11.5)
      .when(active, (el) => el.font_semibold())
      .text_color(active ? t.foreground : t.muted)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        onClick();
        cx.notify();
      })
      .child(label)
      .when(Boolean(badge), (el) =>
        el.child(
          div()
            .px(4)
            .py(1)
            .rounded(4)
            .bg(t.popover ?? t.background)
            .text_size(10)
            .text_color(accent)
            .child(badge),
        ),
      );
  }

  render() {
    this.syncFromProps(false);
    const t = theme();
    const p = props() ?? {};
    const sourceAddr = p.source || "";
    const src = this.sourceRecord(sourceAddr);
    const sourceLabel = sessionLabel(src) || sourceAddr || "?";
    const parentAddr = src?.lineage?.parent || src?.parent || "";
    const copy = this.modeCopy(sourceLabel);
    const presets = this.availablePresets();
    const others = this.otherLiveSessions(sourceAddr);
    const canGo = this.canSubmit();
    const accent = t.accent ?? t.solid ?? "#2563eb";
    const sectionLabel = (txt) => div().pt(2).text_size(11.5).font_medium().text_color(t.dim).child(txt);

    return v_flex()
      .id("handoff-dialog")
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          closeOverlay(p.overlay);
          cx.stop_propagation();
        } else if (e.key === "enter" && (e.modifiers?.control || e.modifiers?.platform)) {
          this.submit();
          cx.stop_propagation();
        }
      })
      .w_full()
      .min_h(0)
      .p(18)
      .gap(12)
      .rounded(10)
      .bg(t.popover)
      .text_color(t.foreground)
      .child(
        h_flex()
          .items_center()
          .justify_between()
          .gap(10)
          .child(div().flex_1().min_w(0).truncate().text_size(15.5).font_semibold().child(copy.title))
          .child(div().flex_none().text_size(11).text_color(t.dim).child("Esc")),
      )
      .child(
        div()
          .px(10)
          .py(7)
          .rounded(7)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .text_size(11.8)
          .line_height("17px")
          .text_color(t.muted)
          .whitespace_normal()
          .child(copy.subtitle),
      )
      .child(
        h_flex()
          .gap(6)
          .flex_wrap()
          .children(
            MODE_LIST().map((m) =>
              this.chip(t, `ho-mode-${m.id}`, m.label, this.mode === m.id, () => {
                this.applyMode(m.id, false);
              }),
            ),
          ),
      )
      .when(this.mode !== "merge-back", (el) =>
        el
          .child(
            v_flex()
              .gap(6)
              .child(sectionLabel(tr("Target Provider", "目标 Agent / 模型提供方")))
              .child(
                h_flex()
                  .gap(6)
                  .flex_wrap()
                  .children(
                    presets.map((pr) =>
                      this.chip(t, `ho-pre-${pr.key}`, pr.label, this.preset === pr.key, () => {
                        this.preset = pr.key;
                      }),
                    ),
                  ),
              ),
          )
          .child(
            h_flex()
              .gap(16)
              .flex_wrap()
              .child(
                v_flex()
                  .gap(6)
                  .child(sectionLabel(tr("Runtime", "运行模式")))
                  .child(
                    h_flex()
                      .gap(6)
                      .child(this.chip(t, "ho-rt-acp", "ACP", this.runtimeMode === "acp", () => { this.runtimeMode = "acp"; }))
                      .child(this.chip(t, "ho-rt-pty", tr("CLI Terminal", "CLI 终端"), this.runtimeMode === "pty", () => { this.runtimeMode = "pty"; })),
                  ),
              )
              .child(
                v_flex()
                  .gap(6)
                  .child(sectionLabel(tr("Open In", "打开位置")))
                  .child(
                    h_flex()
                      .gap(6)
                      .child(this.chip(t, "ho-open-tab", tr("New Tab", "新建标签页"), this.openIn === "tab", () => { this.openIn = "tab"; }))
                      .child(this.chip(t, "ho-open-split", tr("Split Right", "右侧分栏"), this.openIn === "split", () => { this.openIn = "split"; }))
                      .child(this.chip(t, "ho-open-peek", tr("Peek Modal", "浮窗预览"), this.openIn === "peek", () => { this.openIn = "peek"; })),
                  ),
              ),
          ),
      )
      .when(this.mode === "merge-back", (el) =>
        el.child(
          v_flex()
            .gap(6)
            .child(sectionLabel(tr("Target / Parent Session", "合回目标会话（默认选父会话）")))
            .when(others.length > 0, (listEl) =>
              listEl.child(
                h_flex()
                  .gap(6)
                  .flex_wrap()
                  .children(
                    others.map((o) =>
                      this.chip(
                        t,
                        `ho-into-${o.address}`,
                        sessionLabel(o),
                        this.intoTarget === o.address,
                        () => {
                          this.intoTarget = o.address;
                        },
                        o.address === parentAddr ? tr("Parent", "父会话") : null,
                      ),
                    ),
                  ),
              ),
            )
            .when(others.length === 0, (emptyEl) =>
              emptyEl.child(
                div()
                  .px(10)
                  .py(8)
                  .rounded(7)
                  .border_1()
                  .border_color(t.border)
                  .text_size(12)
                  .text_color(t.dim)
                  .child(
                    parentAddr
                      ? tr(`Parent ${parentAddr} is no longer running.`, `父会话 ${parentAddr} 已结束运行，无法合回。`)
                      : tr("No other running session available to merge into.", "暂无其他运行中的会话可供合回。"),
                  ),
              ),
            ),
        ),
      )
      .child(
        v_flex()
          .gap(6)
          .child(sectionLabel(tr("Context Depth", "携带上下文范围")))
          .child(
            h_flex()
              .gap(6)
              .flex_wrap()
              .children(
                TURN_CHOICES().map((c) =>
                  this.chip(t, `ho-turns-${c.value}`, c.label, this.recentTurns === c.value, () => {
                    this.recentTurns = c.value;
                  }),
                ),
              ),
          ),
      )
      .child(
        v_flex()
          .gap(6)
          .child(sectionLabel(tr("Prompt / Notes (optional · Ctrl+Enter to submit)", "补充指令 / 备注（可选 · Ctrl+Enter 确认）")))
          .child(
            div()
              .w_full()
              .px(10)
              .py(7)
              .rounded(8)
              .border_1()
              .border_color(this.promptFocused ? accent : t.input)
              .bg(t.background)
              .child(
                Textarea.new(this.promptInput)
                  .id("ho-prompt-input")
                  .w_full()
                  .min_h(56)
                  .text_size(12.5)
                  .text_color(t.foreground),
              )
              .when(!String(this.promptInput?.value?.() ?? "").trim(), (ph) =>
                ph.child(div().pt(3).text_size(11).text_color(t.dim).child(copy.placeholder)),
              ),
          ),
      )
      .child(
        h_flex()
          .pt(4)
          .gap(8)
          .justify_end()
          .child(
            h_flex()
              .id("ho-cancel")
              .h(32)
              .px(14)
              .items_center()
              .rounded(8)
              .border_1()
              .border_color(t.input)
              .bg(t.raised)
              .text_size(12.5)
              .font_medium()
              .text_color(t.foreground)
              .cursor_pointer()
              .hover((s) => s.opacity(0.88))
              .on_click(() => closeOverlay(p.overlay))
              .child(tr("Cancel", "取消")),
          )
          .child(
            h_flex()
              .id("ho-submit")
              .h(32)
              .px(16)
              .gap(6)
              .items_center()
              .rounded(8)
              .border_1()
              .border_color(canGo ? accent : t.border)
              .bg(canGo ? accent : t.raised)
              .text_size(12.5)
              .font_semibold()
              .text_color(canGo ? (t.solid_foreground ?? "#ffffff") : t.dim)
              .when(canGo, (el) => el.cursor_pointer().hover((s) => s.opacity(0.9)))
              .on_click(() => {
                if (canGo) this.submit();
              })
              .child(svg("mods/workspace-chrome/git-branch.svg").size(13).flex_none())
              .child(copy.submitLabel),
          ),
      );
  }
}
