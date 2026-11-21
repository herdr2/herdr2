// Cross-provider Context Handoff, Fork Session, Side Chat, and Merge Back dialog.
// Props: { overlay, source, kind?, from_turn?, to_turn? }.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextareaState, Textarea } from "gpui-base";
import { terminals, spaces, knownAgents, run, h2, props, closeOverlay, theme, settings, shared, setShared, checking } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

const ACP_PRESETS = ["claude", "codex", "gemini", "agy", "cursor", "opencode", "qwen", "kimi", "copilot", "hermes", "glm", "pi"];
const ACP_ALIAS = { antigravity: "agy" };

const CREATE_MODES = () => [
  { id: "handoff", label: tr("Handoff", "上下文接力"), color: "#3b82f6" },
  { id: "fork", label: tr("Fork Session", "会话分叉"), color: "#a855f7" },
  { id: "side-chat", label: tr("Side Chat", "旁路讨论"), color: "#14b8a6" },
];

const TURN_CHOICES = () => [
  { value: 4, label: tr("4 turns", "最近 4 轮") },
  { value: 8, label: tr("8 turns", "最近 8 轮") },
  { value: 15, label: tr("15 turns", "最近 15 轮") },
  { value: 0, label: tr("All", "全部") },
];

function withAlpha(hex, alphaHex) {
  if (typeof hex !== "string") return null;
  if (/^#[0-9a-fA-F]{6}$/.test(hex)) return `${hex}${alphaHex}`;
  if (/^#[0-9a-fA-F]{8}$/.test(hex)) return `${hex.slice(0, 7)}${alphaHex}`;
  return null;
}

function providerPresetOf(rec) {
  if (!rec) return null;
  const raw = String(rec.agent || rec.program?.key || rec.report?.app || rec.template || "").toLowerCase();
  for (const key of ["claude", "codex", "gemini", "agy", "antigravity", "cursor", "opencode", "qwen", "kimi", "copilot", "hermes", "glm", "pi"]) {
    if (raw.includes(key)) return ACP_ALIAS[key] ?? key;
  }
  return null;
}

function qualifyPeerAddress(rawAddr, curAddr) {
  const raw = typeof rawAddr === "string" ? rawAddr.trim() : "";
  if (!raw || raw.includes("/")) return raw;
  const cur = typeof curAddr === "string" ? curAddr.trim() : "";
  const slash = cur.lastIndexOf("/");
  return slash > 0 ? `${cur.slice(0, slash + 1)}${raw}` : raw;
}

function sessionLabel(rec) {
  if (!rec) return "";
  const custom = rec.labels?.["herdr.name"];
  const name = custom || rec.name || rec.template || rec.program?.name || rec.title || rec.address;
  return name && name !== rec.address ? `${name} (${rec.address})` : (rec.address || "");
}

export default class Handoff extends View {
  init(_props, cx) {
    this._cx = cx;
    this.handle = cx.focus_handle();
    this._applyingPrefill = false;
    this.promptInput = TextareaState.new({
      value: "",
      placeholder: "",
    });
    this.promptInput.set_auto_grow(2, 5);
    this.promptFocused = false;
    this.promptInput.on("focus", (_e, cx) => { this.promptFocused = true; cx.notify(); });
    this.promptInput.on("blur", (_e, cx) => { this.promptFocused = false; cx.notify(); });
    this.promptInput.on("change", (_e, cx) => {
      if (!this._applyingPrefill) this.userTouchedPrompt = true;
      cx.notify();
    });
    this.promptInput.on("submit", (e) => {
      if (e?.ctrl || e?.meta || !e?.shift) {
        this.submit();
      }
    });

    const machine = spaces().find((s) => s.active)?.machine ?? "";
    this.knownList = Array.isArray(shared("new-menu.agents")?.[machine]) ? shared("new-menu.agents")[machine] : [];
    this.recentTurns = 8;
    this.customTurnsMode = false;
    this.customTurnsFocused = false;
    this.customTurnsInput = InputState.new({
      value: "20",
      placeholder: tr("Turns", "轮数"),
    });
    this.customTurnsInput.on("focus", (_e, cx) => {
      this.customTurnsFocused = true;
      this.customTurnsMode = true;
      cx.notify();
    });
    this.customTurnsInput.on("blur", (_e, cx) => {
      this.customTurnsFocused = false;
      cx.notify();
    });
    this.customTurnsInput.on("change", (_e, cx) => {
      this.customTurnsMode = true;
      cx.notify();
    });
    this.customTurnsInput.on("submit", () => {
      if (this.canSubmit()) this.submit();
    });
    this.preset = "";
    this.runtimeMode = "acp";
    this.openIn = "tab";
    this.intoTarget = "";
    this.mode = "handoff";
    this.worktree = false;
    this.mergeWorktree = true;
    this.archiveSource = false;
    this.showAllProviders = false;
    this.toTurn = null;
    this.fromTurn = null;
    this.lastPropsKey = "";

    this.recommendation = null;
    this.recLoading = false;
    this.recKey = "";
    this.userTouchedMode = false;
    this.userTouchedPreset = false;
    this.userTouchedPrompt = false;

    this.syncFromProps(true);

    if (!checking()) {
      cx.spawn(async (cx) => {
        try {
          const res = await knownAgents({ on: machine || undefined });
          if (Array.isArray(res) && res.length > 0) {
            this.knownList = res;
            setShared("new-menu.agents", { ...(shared("new-menu.agents") ?? {}), [machine]: res });
            this.ensurePresetSelected();
            this.requestRecommendation(cx, true);
            cx.notify();
          }
        } catch {}
      });
      this.requestRecommendation(cx, false);
    }
    this.handle.focus();
  }

  sourceRecord(sourceAddr) {
    return terminals().find((x) => x.address === sourceAddr) ?? null;
  }

  availablePresets() {
    const installed = [];
    const others = [];
    const seen = new Set();
    for (const a of this.knownList) {
      if (!a?.installed) continue;
      const mapped = ACP_ALIAS[a.key] ?? a.key;
      if (!mapped || seen.has(mapped) || !ACP_PRESETS.includes(mapped)) continue;
      seen.add(mapped);
      installed.push({ key: mapped, label: a.name || mapped, installed: true });
    }
    for (const k of ACP_PRESETS) {
      if (seen.has(k)) continue;
      seen.add(k);
      others.push({ key: k, label: k, installed: this.knownList.length === 0 });
    }
    return { installed, others, all: [...installed, ...others] };
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
    const { installed, all } = this.availablePresets();
    const pool = installed.length > 0 ? installed : all;
    if (this.mode === "fork" && srcPreset && all.some((p) => p.key === srcPreset)) {
      this.preset = srcPreset;
      return;
    }
    if (!this.preset || !all.some((p) => p.key === this.preset)) {
      const alt = pool.find((p) => p.key !== srcPreset) ?? pool[0];
      this.preset = alt?.key || srcPreset || "claude";
    }
  }

  applyMode(nextMode, resetTargets = false) {
    const p = props() ?? {};
    const sourceAddr = p.source || "";
    const src = this.sourceRecord(sourceAddr);
    const valid = ["handoff", "fork", "side-chat", "merge-back"];
    this.mode = valid.includes(nextMode) ? nextMode : "handoff";

    if (this.mode === "side-chat") {
      this.openIn = "panel";
      if (resetTargets) this.runtimeMode = "acp";
    } else if (this.mode === "fork") {
      if (resetTargets || this.openIn === "panel" || this.openIn === "window") this.openIn = "split";
    } else if (this.mode === "handoff") {
      if (resetTargets || this.openIn === "panel" || this.openIn === "window") this.openIn = "tab";
    }

    if (this.mode === "fork") {
      const sp = providerPresetOf(src);
      if (sp) this.preset = sp;
      if (resetTargets) this.worktree = false;
    } else if (resetTargets) {
      this.preset = "";
      this.archiveSource = false;
    }
    this.ensurePresetSelected();

    const parentAddr = qualifyPeerAddress(src?.lineage?.parent || src?.parent || "", sourceAddr);
    const others = this.otherLiveSessions(sourceAddr);
    if (resetTargets || !this.intoTarget || !others.some((o) => o.address === this.intoTarget)) {
      if (parentAddr && others.some((o) => o.address === parentAddr)) {
        this.intoTarget = parentAddr;
      } else {
        this.intoTarget = parentAddr || others[0]?.address || "";
      }
    }
    if (resetTargets) {
      this.mergeWorktree = Boolean(src?.lineage?.worktree_path);
    }
  }

  applyPrefillFromRecommendation(rec, includeMode = false) {
    if (!rec) return;
    const { all } = this.availablePresets();
    if (includeMode && rec.recommendedKind && ["handoff", "fork", "side-chat"].includes(rec.recommendedKind)) {
      this.applyMode(rec.recommendedKind, false);
    }
    if (
      (includeMode || !this.userTouchedPreset) &&
      rec.recommendedPreset &&
      all.some((p) => p.key === rec.recommendedPreset)
    ) {
      this.preset = rec.recommendedPreset;
    }
    const existing = String(this.promptInput?.value?.() ?? "").trim();
    if (
      rec.suggestedNote &&
      !this.userTouchedPrompt &&
      !existing
    ) {
      this._applyingPrefill = true;
      try {
        this.promptInput.set_value(rec.suggestedNote);
      } finally {
        this._applyingPrefill = false;
      }
    }
  }

  requestRecommendation(cx, force = false) {
    if (checking()) return;
    const p = props() ?? {};
    const sourceAddr = p.source || "";
    if (!sourceAddr || this.mode === "merge-back") return;
    const { installed } = this.availablePresets();
    const installedKeys = installed.map((x) => x.key);
    const key = `${sourceAddr}:${installedKeys.join(",")}:${isZh() ? "zh" : "en"}`;
    if (!force && this.recKey === key) return;
    this.recKey = key;
    this.recLoading = true;
    const spawner = cx?.spawn ? cx : this._cx;
    if (!spawner?.spawn) {
      this.recLoading = false;
      return;
    }
    spawner.spawn(async (taskCx) => {
      try {
        const args = [
          "ai",
          "handoff-recommend",
          "--json",
          sourceAddr,
          "--lang",
          isZh() ? "zh" : "en",
          ...(installedKeys.length > 0 ? ["--installed", installedKeys.join(",")] : []),
        ];
        const res = await h2(args, { timeout: 6500 });
        if (this.recKey !== key) return;
        if (res.code === 0 && res.stdout) {
          const parsed = JSON.parse(res.stdout);
          if (parsed && typeof parsed === "object" && parsed.recommendedPreset) {
            this.recommendation = parsed;
            if (this.mode !== "merge-back") {
              this.applyPrefillFromRecommendation(parsed, false);
            }
          }
        }
      } catch {}
      finally {
        if (this.recKey === key) this.recLoading = false;
        taskCx?.notify?.();
      }
    });
  }

  syncFromProps(force = false) {
    const p = props() ?? {};
    const key = `${p.source ?? ""}:${p.kind ?? ""}:${p.from_turn ?? ""}:${p.to_turn ?? ""}`;
    if (force || key !== this.lastPropsKey) {
      this.lastPropsKey = key;
      this.fromTurn = typeof p.from_turn === "number" && p.from_turn > 0 ? p.from_turn : null;
      this.toTurn = typeof p.to_turn === "number" && p.to_turn > 0 ? p.to_turn : null;
      this.customTurnsMode = false;
      this.userTouchedMode = false;
      this.userTouchedPreset = false;
      this.userTouchedPrompt = false;
      this.recommendation = null;
      this.recKey = "";
      this.applyMode(p.kind || "handoff", true);
      if (!force) this.requestRecommendation(this._cx, false);
    }
  }

  modeCopy() {
    switch (this.mode) {
      case "fork":
        return {
          title: tr("Fork Session", "会话分叉"),
          accent: "#a855f7",
          subtitle: this.toTurn
            ? tr(
                `Fork an independent conversation line from turn ${this.toTurn} to explore another approach.`,
                `从第 ${this.toTurn} 轮拉出独立平行对话线，尝试另一条实现思路。`,
              )
            : tr(
                "Fork an independent conversation line from the current state (native clone on shared dir; optional Git Worktree).",
                "基于当前对话拉出独立平行对话线（共享目录优先原生克隆，需要并行改代码时可开启 Worktree）。",
              ),
          placeholder: tr("Optional goal for this forked path…", "可选：输入这条分叉要尝试的新方向…"),
          submitLabel: tr("Create Fork", "创建分叉"),
        };
      case "side-chat":
        return {
          title: tr("Side Chat", "旁路讨论"),
          accent: "#14b8a6",
          subtitle: tr(
            "Opens a read-only side discussion in the right panel (beside Files / Changes / Checkpoints) without polluting main context.",
            "在右侧面板（与文件 / 变更 / 快照并列）开启只读旁路探讨，不污染主会话、不改动代码。",
          ),
          placeholder: tr("What would you like to discuss or verify?", "输入你想探讨、核对或求证的问题…"),
          submitLabel: tr("Open Side Chat", "开启旁路讨论"),
        };
      case "merge-back":
        return {
          title: tr("Merge Back", "合回父会话"),
          accent: "#3b82f6",
          subtitle: tr(
            "Deliver takeaways from this session back into the parent session and continue main execution.",
            "将当前会话的结论整理注入回父会话，并触发主线继续推进。",
          ),
          placeholder: tr("Optional follow-up instruction for parent…", "可选：补充合回后的处理要求…"),
          submitLabel: tr("Merge Back", "确认合回"),
        };
      default:
        return {
          title: tr("Context Handoff", "上下文接力"),
          accent: "#3b82f6",
          subtitle: tr(
            "Compile recent turns & git state into a clean handoff package for another agent/model to continue.",
            "提炼近期进展与工作区改动为精简交接包，无缝交棒给其他 Agent / 模型继续执行。",
          ),
          placeholder: tr("Optional instruction for the receiving agent…", "可选：给接力目标 Agent 的启动指令…"),
          submitLabel: tr("Start Handoff", "开始接力"),
        };
    }
  }

  parsedCustomTurns() {
    const raw = String(this.customTurnsInput?.value?.() ?? "").trim();
    if (!/^\d+$/.test(raw)) return null;
    const n = Number.parseInt(raw, 10);
    return Number.isSafeInteger(n) && n >= 1 ? n : null;
  }

  effectiveRecentTurns() {
    if (this.customTurnsMode) return this.parsedCustomTurns();
    return this.recentTurns;
  }

  canSubmit() {
    const p = props() ?? {};
    if (!p.source) return false;
    const pinnedFork = this.mode === "fork" && Boolean(this.toTurn);
    if (!pinnedFork && this.customTurnsMode && this.parsedCustomTurns() === null) {
      return false;
    }
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
    const effectiveTurns = this.effectiveRecentTurns();
    if (this.toTurn && this.mode === "fork") {
      args.to_turn = this.toTurn;
      if (this.fromTurn) args.from_turn = this.fromTurn;
    } else if (typeof effectiveTurns === "number" && effectiveTurns > 0) {
      args.recent_turns = effectiveTurns;
    }
    if (promptText) {
      args.prompt = promptText;
    }
    if (this.mode === "merge-back") {
      args.into = this.intoTarget;
      args.merge_worktree = Boolean(this.mergeWorktree);
    } else {
      args.preset = this.preset;
      args.mode = this.runtimeMode;
      args.open = (this.mode === "fork" && this.worktree) ? "tab" : this.openIn;
      if (this.mode === "fork" && this.worktree) {
        args.worktree = true;
      }
      if (this.mode === "handoff" && this.archiveSource) {
        args.archive_source = true;
      }
    }
    closeOverlay(p.overlay);
    run("agent.handoff", args);
  }

  chip(t, id, label, active, onClick, accentColor, badge = null) {
    return h_flex()
      .id(id)
      .h(26)
      .px(10)
      .gap(5)
      .flex_none()
      .items_center()
      .rounded(6)
      .border_1()
      .border_color(active ? accentColor : t.border)
      .bg(active ? (withAlpha(accentColor, "1f") ?? t.selected ?? t.raised) : t.raised)
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
            .bg(t.background)
            .text_size(9.5)
            .text_color(accentColor)
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
    const parentAddr = qualifyPeerAddress(src?.lineage?.parent || src?.parent || "", sourceAddr);
    const copy = this.modeCopy();
    const accent = copy.accent || t.accent || "#3b82f6";
    const { installed, others: extraPresets, all: allPresets } = this.availablePresets();
    const visiblePresets = (installed.length > 0 && !this.showAllProviders) ? installed : allPresets;
    const otherSessions = this.otherLiveSessions(sourceAddr);
    const canGo = this.canSubmit();
    const hasSrcWorktree = Boolean(src?.lineage?.worktree_path);
    const isMerge = this.mode === "merge-back";

    const sectionLabel = (txt) =>
      div().w_full().min_w(0).text_size(11).font_medium().text_color(t.dim).child(txt);

    const toggleRow = (id, checked, title, desc, onToggle, accentColor = accent) =>
      h_flex()
        .id(id)
        .w_full()
        .min_w(0)
        .px(10)
        .py(8)
        .gap(10)
        .items_start()
        .rounded(8)
        .border_1()
        .border_color(checked ? accentColor : t.border)
        .bg(checked ? (withAlpha(accentColor, "14") ?? t.raised) : t.raised)
        .cursor_pointer()
        .hover((s) => s.bg(t.hover))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => {
          cx.stop_propagation();
          onToggle();
          cx.notify();
        })
        .child(
          div()
            .mt(1)
            .w(15)
            .h(15)
            .flex_none()
            .rounded(4)
            .border_1()
            .border_color(checked ? accentColor : t.input)
            .bg(checked ? accentColor : t.background)
            .flex()
            .items_center()
            .justify_center()
            .text_size(10)
            .font_bold()
            .text_color("#ffffff")
            .child(checked ? "✓" : ""),
        )
        .child(
          v_flex()
            .flex_1()
            .min_w(0)
            .gap(2)
            .child(div().w_full().min_w(0).text_size(12).font_semibold().text_color(t.foreground).child(title))
            .child(div().w_full().min_w(0).text_size(11).line_height("16px").text_color(t.muted).whitespace_normal().child(desc)),
        );

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
      .min_w(0)
      .overflow_hidden()
      .p(18)
      .gap(12)
      .rounded(12)
      .bg(t.popover)
      .text_color(t.foreground)
      // Top header: title + source pill + close
      .child(
        h_flex()
          .w_full()
          .min_w(0)
          .items_center()
          .justify_between()
          .gap(8)
          .child(
            h_flex()
              .flex_1()
              .min_w(0)
              .items_center()
              .gap(8)
              .child(div().w(8).h(8).flex_none().rounded(4).bg(accent))
              .child(div().flex_none().text_size(15).font_semibold().text_color(t.foreground).child(copy.title))
              .child(
                div()
                  .min_w(0)
                  .truncate()
                  .px(6)
                  .py(2)
                  .rounded(5)
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .text_size(10.5)
                  .text_color(t.muted)
                  .child(sourceLabel),
              ),
          )
          .child(
            div()
              .id("ho-close-x")
              .flex_none()
              .p(4)
              .rounded(5)
              .text_color(t.dim)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click(() => closeOverlay(p.overlay))
              .child(svg("mods/pane/close.svg").size(13)),
          ),
      )
      // 3-segment mode switcher (creation modes only — NO Merge Back tab here)
      .when(!isMerge, (el) =>
        el.child(
          h_flex()
            .w_full()
            .min_w(0)
            .p(3)
            .gap(3)
            .rounded(9)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .children(
              CREATE_MODES().map((m) => {
                const on = this.mode === m.id;
                return h_flex()
                  .id(`ho-mode-${m.id}`)
                  .flex_1()
                  .min_w(0)
                  .h(28)
                  .gap(6)
                  .items_center()
                  .justify_center()
                  .rounded(6)
                  .bg(on ? t.background : "#00000000")
                  .border_1()
                  .border_color(on ? (withAlpha(m.color, "77") ?? t.border) : "#00000000")
                  .text_size(12)
                  .when(on, (s) => s.font_semibold())
                  .text_color(on ? t.foreground : t.muted)
                  .cursor_pointer()
                  .hover((s) => s.text_color(t.foreground))
                  .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.userTouchedMode = true;
                    this.applyMode(m.id, false);
                    cx.notify();
                  })
                  .child(div().w(6).h(6).flex_none().rounded(3).bg(on ? m.color : t.dim))
                  .child(div().truncate().child(m.label));
              }),
            ),
        ),
      )
      // Clean wrapping 1-sentence description
      .child(
        div()
          .w_full()
          .min_w(0)
          .text_size(11.5)
          .line_height("17px")
          .text_color(t.muted)
          .whitespace_normal()
          .child(copy.subtitle),
      )
      // AI / heuristic complementary recommendation banner
      .when(!isMerge && Boolean(this.recommendation?.reason), (el) => {
        const rec = this.recommendation;
        const kindLabel =
          rec.recommendedKind === "fork"
            ? tr("Fork Session", "会话分叉")
            : rec.recommendedKind === "side-chat"
              ? tr("Side Chat", "旁路讨论")
              : tr("Handoff", "上下文接力");
        const modeDiffers = Boolean(rec.recommendedKind && rec.recommendedKind !== this.mode && !this.toTurn);
        return el.child(
          h_flex()
            .id("ho-rec-banner")
            .w_full()
            .min_w(0)
            .px(10)
            .py(7)
            .gap(8)
            .items_center()
            .rounded(8)
            .bg(withAlpha(accent, "12") ?? t.raised)
            .border_1()
            .border_color(withAlpha(accent, "55") ?? t.border)
            .child(
              div()
                .flex_none()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(withAlpha(accent, "24") ?? t.background)
                .text_size(10)
                .font_semibold()
                .text_color(accent)
                .child(rec.usedAi ? tr("AI Suggestion", "AI 推荐") : tr("Suggestion", "建议")),
            )
            .child(
              div()
                .flex_1()
                .min_w(0)
                .text_size(11)
                .line_height("15px")
                .text_color(t.foreground)
                .whitespace_normal()
                .child(`${rec.reason} (${rec.recommendedPreset} · ${kindLabel})`),
            )
            .when(modeDiffers, (btnEl) =>
              btnEl.child(
                div()
                  .id("ho-rec-apply-mode")
                  .flex_none()
                  .px(7)
                  .py(3)
                  .rounded(5)
                  .bg(t.background)
                  .border_1()
                  .border_color(withAlpha(accent, "77") ?? t.border)
                  .text_size(10.5)
                  .font_medium()
                  .text_color(accent)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover))
                  .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.userTouchedMode = true;
                    this.applyPrefillFromRecommendation(rec, true);
                    cx.notify();
                  })
                  .child(tr(`Switch to ${kindLabel}`, `切到${kindLabel}`)),
              ),
            ),
        );
      })
      // Creation modes body
      .when(!isMerge, (el) =>
        el
          // Target provider
          .child(
            v_flex()
              .w_full()
              .min_w(0)
              .gap(6)
              .child(
                h_flex()
                  .w_full()
                  .min_w(0)
                  .items_center()
                  .justify_between()
                  .child(sectionLabel(tr("Target Agent", "目标 Agent")))
                  .when(installed.length > 0 && extraPresets.length > 0, (moreEl) =>
                    moreEl.child(
                      div()
                        .id("ho-more-providers")
                        .flex_none()
                        .text_size(10.5)
                        .text_color(t.dim)
                        .cursor_pointer()
                        .hover((s) => s.text_color(t.foreground))
                        .on_click((_e, cx) => {
                          this.showAllProviders = !this.showAllProviders;
                          cx.notify();
                        })
                        .child(this.showAllProviders ? tr("Installed only", "仅看已安装") : tr(`+${extraPresets.length} more`, `+ 展开更多`)),
                    ),
                  ),
              )
              .child(
                h_flex()
                  .w_full()
                  .min_w(0)
                  .gap(6)
                  .flex_wrap()
                  .children(
                    visiblePresets.map((pr) =>
                      this.chip(
                        t,
                        `ho-pre-${pr.key}`,
                        pr.label,
                        this.preset === pr.key,
                        () => {
                          this.userTouchedPreset = true;
                          this.preset = pr.key;
                        },
                        accent,
                        this.recommendation?.recommendedPreset === pr.key ? tr("Suggested", "推荐") : null,
                      ),
                    ),
                  ),
              ),
          )
          // Compact 2-column settings: Runtime + Open Location
          .child(
            h_flex()
              .w_full()
              .min_w(0)
              .gap(14)
              .items_start()
              .child(
                v_flex()
                  .flex_1()
                  .min_w(0)
                  .gap(6)
                  .child(sectionLabel(tr("Runtime", "运行方式")))
                  .child(
                    h_flex()
                      .w_full()
                      .min_w(0)
                      .gap(6)
                      .child(this.chip(t, "ho-rt-acp", tr("ACP Chat", "ACP 对话"), this.runtimeMode === "acp", () => { this.runtimeMode = "acp"; }, accent))
                      .child(this.chip(t, "ho-rt-pty", tr("CLI Terminal", "CLI 终端"), this.runtimeMode === "pty", () => { this.runtimeMode = "pty"; }, accent)),
                  ),
              )
              .child(
                v_flex()
                  .flex_1()
                  .min_w(0)
                  .gap(6)
                  .child(sectionLabel(tr("Open In", "打开位置")))
                  .when(this.mode === "side-chat", (scLoc) =>
                    scLoc.child(
                      h_flex()
                        .h(26)
                        .px(9)
                        .gap(5)
                        .items_center()
                        .rounded(6)
                        .bg(withAlpha("#14b8a6", "1a") ?? t.raised)
                        .border_1()
                        .border_color(withAlpha("#14b8a6", "66") ?? t.border)
                        .text_size(11.5)
                        .font_medium()
                        .text_color("#14b8a6")
                        .child(svg("mods/peek/messages-square.svg").size(12).flex_none().text_color("#14b8a6"))
                        .child(div().truncate().child(tr("Right Side Panel", "右侧「旁路」面板"))),
                    ),
                  )
                  .when(this.mode === "fork" && this.worktree, (wtLoc) =>
                    wtLoc.child(
                      h_flex()
                        .h(26)
                        .px(9)
                        .gap(5)
                        .items_center()
                        .rounded(6)
                        .bg(withAlpha("#f59e0b", "1a") ?? t.raised)
                        .border_1()
                        .border_color(withAlpha("#f59e0b", "66") ?? t.border)
                        .text_size(11.5)
                        .font_medium()
                        .text_color("#f59e0b")
                        .child(svg("mods/workspace-chrome/branch.svg").size(12).flex_none().text_color("#f59e0b"))
                        .child(div().truncate().child(tr("Worktree Space", "独立 Worktree 工作区"))),
                    ),
                  )
                  .when(this.mode !== "side-chat" && !(this.mode === "fork" && this.worktree), (locEl) =>
                    locEl.child(
                      h_flex()
                        .w_full()
                        .min_w(0)
                        .gap(6)
                        .child(this.chip(t, "ho-open-tab", tr("New Tab", "新标签页"), this.openIn === "tab", () => { this.openIn = "tab"; }, accent))
                        .child(this.chip(t, "ho-open-split", tr("Split Right", "右侧分栏"), this.openIn === "split", () => { this.openIn = "split"; }, accent)),
                    ),
                  ),
              ),
          )
          // Mode-specific option row
          .when(this.mode === "fork", (forkEl) =>
            forkEl.child(
              toggleRow(
                "ho-worktree-toggle",
                this.worktree,
                tr("Isolated Git Worktree", "独立 Git Worktree 隔离"),
                this.worktree
                  ? tr("Creates a dedicated Git branch & worktree from HEAD (requires a clean Git repo with at least one commit).", "基于当前 HEAD 创建独立 Git 分支与 Worktree（要求已初始化 Git、至少有一次提交且工作区干净），与主线并行互不冲突。")
                  : tr("Off (shared workspace): fast & keeps caches. Enable only when editing code in parallel.", "默认关闭（共享当前目录，秒开且复用缓存）；需要同时改代码防冲突时勾选。"),
                () => { this.worktree = !this.worktree; },
                "#f59e0b",
              ),
            ),
          )
          .when(this.mode === "handoff", (hoEl) =>
            hoEl.child(
              toggleRow(
                "ho-archive-toggle",
                this.archiveSource,
                tr("Park source pane after handoff", "接力后收起原会话分栏"),
                tr("Keeps the source agent running in the sidebar while the new tab takes focus.", "原会话保留在左下角智能体区，工作区直接切到接力新标签页。"),
                () => { this.archiveSource = !this.archiveSource; },
                accent,
              ),
            ),
          ),
      )
      // Dedicated Merge-Back target picker (only shown when explicitly opened in merge-back mode)
      .when(isMerge, (el) =>
        el
          .child(
            v_flex()
              .w_full()
              .min_w(0)
              .gap(6)
              .child(sectionLabel(tr("Target Parent Session", "合回目标会话")))
              .when(otherSessions.length > 0, (listEl) =>
                listEl.child(
                  h_flex()
                    .w_full()
                    .min_w(0)
                    .gap(6)
                    .flex_wrap()
                    .children(
                      otherSessions.map((o) =>
                        this.chip(
                          t,
                          `ho-into-${o.address}`,
                          sessionLabel(o),
                          this.intoTarget === o.address,
                          () => { this.intoTarget = o.address; },
                          accent,
                          o.address === parentAddr ? tr("Parent", "父会话") : null,
                        ),
                      ),
                    ),
                ),
              )
              .when(otherSessions.length === 0, (emptyEl) =>
                emptyEl.child(
                  div()
                    .w_full()
                    .min_w(0)
                    .px(10)
                    .py(8)
                    .rounded(7)
                    .border_1()
                    .border_color(t.border)
                    .text_size(11.5)
                    .text_color(t.dim)
                    .whitespace_normal()
                    .child(
                      parentAddr
                        ? tr(`Parent ${parentAddr} is no longer running.`, `父会话 ${parentAddr} 已结束运行，无法合回。`)
                        : tr("No other running session available to merge into.", "暂无其他运行中的会话可供合回。"),
                    ),
                ),
              ),
          )
          .when(hasSrcWorktree, (wtEl) =>
            wtEl.child(
              toggleRow(
                "ho-merge-wt-toggle",
                this.mergeWorktree,
                tr("Notify parent agent to merge Worktree branch (keep worktree)", "通知主 Agent 合并 Worktree 分支代码（保留 Worktree 与分支）"),
                tr("Sends a message asking the parent agent to merge the fork branch into the main branch — merge only, without deleting the worktree or branch.", "向主 Agent 发送合并指令，由主 Agent 将分叉分支的代码合并到主线；只合并，不删除 Worktree 目录及分支。"),
                () => { this.mergeWorktree = !this.mergeWorktree; },
                "#f59e0b",
              ),
            ),
          ),
      )
      // Context depth (or pinned fork turn)
      .when(this.mode === "fork" && Boolean(this.toTurn), (el) =>
        el.child(
          h_flex()
            .w_full()
            .min_w(0)
            .px(10)
            .py(6)
            .gap(8)
            .items_center()
            .justify_between()
            .rounded(7)
            .bg(t.raised)
            .border_1()
            .border_color(withAlpha("#a855f7", "66") ?? t.border)
            .child(div().min_w(0).truncate().text_size(11.5).font_medium().text_color(t.foreground).child(tr(`Fork point: up to Turn ${this.toTurn}`, `分叉起点：截至第 ${this.toTurn} 轮`)))
            .child(
              div()
                .id("ho-clear-turn")
                .flex_none()
                .px(6)
                .py(2)
                .rounded(4)
                .text_size(11)
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_click((_e, cx) => {
                  this.toTurn = null;
                  this.fromTurn = null;
                  cx.notify();
                })
                .child(tr("Use latest", "改用最新轮")),
            ),
        ),
      )
      .when(!(this.mode === "fork" && Boolean(this.toTurn)), (el) => {
        const customTurns = this.customTurnsMode ? this.parsedCustomTurns() : null;
        const customInvalid = this.customTurnsMode && customTurns === null;
        const customBorder = customInvalid
          ? (t.danger ?? "#ef4444")
          : (this.customTurnsFocused ? accent : t.input);
        return el.child(
          v_flex()
            .w_full()
            .min_w(0)
            .gap(4)
            .child(
              h_flex()
                .w_full()
                .min_w(0)
                .items_center()
                .justify_between()
                .gap(8)
                .child(div().flex_none().text_size(11).font_medium().text_color(t.dim).child(tr("Context Depth", "携带上下文")))
                .child(
                  h_flex()
                    .gap(5)
                    .items_center()
                    .flex_wrap()
                    .justify_end()
                    .children(
                      TURN_CHOICES().map((c) =>
                        this.chip(t, `ho-turns-${c.value}`, c.label, !this.customTurnsMode && this.recentTurns === c.value, () => {
                          this.customTurnsMode = false;
                          this.recentTurns = c.value;
                        }, accent),
                      ),
                    )
                    .child(
                      this.chip(t, "ho-turns-custom", tr("Custom", "自定义"), this.customTurnsMode, () => {
                        this.customTurnsMode = true;
                        const cur = String(this.customTurnsInput?.value?.() ?? "").trim();
                        if (!cur) {
                          const fallback = this.recentTurns > 0 ? String(this.recentTurns) : "20";
                          this.customTurnsInput.set_value(fallback);
                          this.customTurnsInput.set_selected_range({ start: 0, end: fallback.length });
                        } else {
                          this.customTurnsInput.set_selected_range({ start: 0, end: cur.length });
                        }
                        this.customTurnsInput.focus();
                      }, accent),
                    )
                    .when(this.customTurnsMode, (customEl) =>
                      customEl.child(
                        h_flex()
                          .flex_none()
                          .items_center()
                          .gap(4)
                          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                          .child(
                            Input.new(this.customTurnsInput)
                              .id("ho-turns-custom-input")
                              .w(60)
                              .h(26)
                              .px(7)
                              .rounded(6)
                              .border_1()
                              .border_color(customBorder)
                              .bg(t.background)
                              .text_size(11.5)
                              .text_color(t.foreground),
                          )
                          .child(div().flex_none().text_size(11).text_color(t.muted).child(tr("turns", "轮"))),
                      ),
                    ),
                ),
            )
            .when(customInvalid, (errEl) =>
              errEl.child(
                div()
                  .id("ho-turns-custom-error")
                  .w_full()
                  .min_w(0)
                  .text_right()
                  .text_size(10.5)
                  .text_color(t.danger ?? "#ef4444")
                  .child(tr("Enter a positive integer (≥ 1)", "请输入大于 0 的整数轮次")),
              ),
            ),
        );
      })
      // Prompt input
      .child(
        v_flex()
          .w_full()
          .min_w(0)
          .gap(5)
          .child(sectionLabel(tr("Instruction (optional · Ctrl+Enter)", "补充指令（可选 · Ctrl+Enter 确认）")))
          .child(
            div()
              .w_full()
              .min_w(0)
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
                  .min_h(44)
                  .text_size(12.5)
                  .text_color(t.foreground),
              )
              .when(!String(this.promptInput?.value?.() ?? "").trim(), (ph) =>
                ph.child(div().w_full().min_w(0).pt(3).text_size(11).text_color(t.dim).whitespace_normal().child(copy.placeholder)),
              ),
          ),
      )
      // Footer buttons
      .child(
        h_flex()
          .w_full()
          .min_w(0)
          .pt(2)
          .gap(8)
          .justify_end()
          .child(
            h_flex()
              .id("ho-cancel")
              .h(30)
              .px(14)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(t.input)
              .bg(t.raised)
              .text_size(12)
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
              .h(30)
              .px(16)
              .gap(6)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(canGo ? accent : t.border)
              .bg(canGo ? accent : t.raised)
              .text_size(12)
              .font_semibold()
              .text_color(canGo ? "#ffffff" : t.dim)
              .when(canGo, (el) => el.cursor_pointer().hover((s) => s.opacity(0.9)))
              .on_click(() => {
                if (canGo) this.submit();
              })
              .child(copy.submitLabel),
          ),
      );
  }
}
