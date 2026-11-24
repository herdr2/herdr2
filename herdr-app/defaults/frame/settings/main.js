// App-owned settings mod. Writes changes immediately; no Save step.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, Scrollbar, InputState, Input, TextareaState, Textarea, Popup } from "gpui-base";
import { hc, run, theme, themes, fonts, updates, settings, layouts, layout, spaces, keys, shared, setShared, appHome, readTextFile, writeTextFile, playSound, props, closeOverlay, notify, window, checking } from "herdr";
import {
  SHORTCUT_GROUPS,
  defaultChordFor,
  buildDefaultKeysDoc,
  parseKeysDoc,
  primaryChordFor,
  setActionBinding,
  clearActionBinding,
  formatChord,
  findActionLabel,
} from "./shortcuts_model.js";

const I = (name) => `frame/settings/${name}.svg`;

const SECTIONS = () => [
  { id: "look", title: "Look", zh: "外观", icon: "palette" },
  { id: "keyboard", title: "Shortcuts", zh: "快捷键", icon: "keyboard" },
  { id: "machines", title: "Machines", zh: "主机", icon: "server" },
  { id: "ai", title: "AI", zh: "AI 助手", icon: "sparkles" },
  { id: "drove", title: "Drove", zh: "Drove", icon: "layers" },
  { id: "skills", title: "Skills", zh: "Skills", icon: "sparkles" },
  { id: "alerts", title: "Alerts", zh: "提醒", icon: "bell" },
  { id: "updates", title: "Updates", zh: "更新", icon: "refresh" },
];

const DROVE_BUILTIN_ROLES = ["coder", "tester", "writer", "illustrator", "reviewer", "researcher"];
const DROVE_DEFAULT_ROLE_SUMMARIES_EN = {
  "核心功能实现、模块重构与 Bug 修复": "Core feature implementation, module refactoring, and bug fixes",
  "单元/集成测试编写、故障复现与回归验证": "Unit/integration tests, bug reproduction, and regression verification",
  "技术文档、架构说明与图表撰写": "Technical docs, architecture specs, and diagram writing",
  "架构图、流程图、时序图、SVG 矢量图与可视化配图绘制": "Architecture, flow & sequence diagrams, SVG vectors, and visual illustrations",
  "只读异构代码审查、并发安全与架构审计": "Read-only cross-model code review, concurrency safety, and architecture audits",
  "跨仓源码检索、架构摸底与日志分析": "Cross-repo code search, architecture discovery, and log analysis",
};
const DROVE_FALLBACK_AGENTS = ["claude", "codex", "agy", "gemini", "opencode", "qoder", "cursor", "kimi", "dsh"];
const DROVE_EFFORTS = ["low", "medium", "high", "xhigh"];
const DROVE_ROLE_STRENGTH_PRESETS = {
  coder: [
    ["Backend Architecture", "后端架构"],
    ["Frontend UI", "前端UI"],
    ["Complex Refactor", "复杂重构"],
    ["Bugfix & Debug", "疑难排障"],
    ["Quick Fix", "快速修复"],
    ["Algorithm & Perf", "算法性能"],
    ["System & Native", "底层驱动"],
    ["Scripts & Tooling", "脚本工具"],
  ],
  tester: [
    ["Unit Tests", "单元测试"],
    ["Integration Tests", "集成测试"],
    ["E2E & UI Tests", "端到端测试"],
    ["Bug Repro", "故障复现"],
    ["Regression Check", "回归验证"],
    ["Benchmark & Perf", "性能压测"],
  ],
  writer: [
    ["Tech Docs", "技术文档"],
    ["Architecture Diagrams", "架构图解"],
    ["API Reference", "API说明"],
    ["Blog & Tutorials", "博客教程"],
    ["Changelog", "变更日志"],
  ],
  illustrator: [
    ["Architecture Diagrams", "架构图"],
    ["Flow & Sequence", "流程时序图"],
    ["SVG & Vector", "SVG矢量图"],
    ["Mermaid & D2", "Mermaid/D2"],
    ["UI & Wireframes", "原型线框图"],
    ["Data Charts", "数据图表"],
  ],
  reviewer: [
    ["Security Audit", "安全审计"],
    ["Concurrency & Locks", "并发安全"],
    ["Architecture Review", "架构把关"],
    ["Performance Review", "性能审查"],
    ["Edge Cases", "边界检查"],
  ],
  researcher: [
    ["Codebase Survey", "源码调研"],
    ["Long Context", "长上下文"],
    ["Log Analysis", "日志分析"],
    ["Tech Comparison", "方案对比"],
    ["Dependency Trace", "依赖排查"],
  ],
  default: [
    ["Complex Architecture", "复杂架构"],
    ["Frontend UI", "前端UI"],
    ["Backend API", "后端接口"],
    ["Bugfix & Debug", "疑难排障"],
    ["Quick Routine", "快速日常"],
    ["Long Context", "长上下文"],
  ],
};
const DROVE_AGENT_ICONS = {
  agy: { path: "icons/antigravity.svg", tint: false },
  antigravity: { path: "icons/antigravity.svg", tint: false },
  claude: { path: "icons/claude.svg", tint: false },
  codex: { path: "icons/codex.svg", tint: false },
  gemini: { path: "icons/gemini.svg", tint: true },
  opencode: { path: "icons/opencode.svg", tint: true },
  cursor: { path: "icons/cursor.svg", tint: true },
  "cursor-agent": { path: "icons/cursor.svg", tint: true },
  kimi: { path: "icons/kimi.svg", tint: false },
  qoder: { path: "icons/qoder.svg", tint: true },
  dsh: { path: "icons/dsh-tui.svg", tint: true },
  copilot: { path: "icons/copilot.svg", tint: true },
  hermes: { path: "icons/hermes.svg", tint: true },
  pi: { path: "icons/pi.svg", tint: true },
  zcode: { path: "icons/zcode.svg", tint: true },
  glm: { path: "icons/agent.svg", tint: true },
};

const LOOKS = { main: "Herdr2", terminals: "Terminals", win98: "Herdr 98" };
const PANELS = [
  ["files", "Files", "文件", "The folder you're in as a tree, with highlighted file preview.", "以树形结构浏览当前目录，支持语法高亮只读预览。"],
  ["changes", "Changes", "变更", "Every file changed in git, its diff, and a commit box.", "查看 Git 工作区改动、逐行 diff 并直接提交。"],
  ["checkpoints", "Checkpoints", "快照", "Turn-by-turn snapshots with diff preview and session/workspace restore.", "按对话轮次查看快照时间轴、对比 diff 并一键恢复会话或工作区。"],
];

function resolveEnabledPanels(defaultIds) {
  const saved = shared("workspace.panels");
  if (!Array.isArray(saved)) return defaultIds;
  if (!shared("workspace.panels_has_checkpoints") && !saved.includes("checkpoints")) {
    return [...saved, "checkpoints"];
  }
  return saved;
}
const APPEARANCES = [
  [null, "System", "跟随系统"],
  ["light", "Light", "浅色"],
  ["dark", "Dark", "深色"],
];
const LANGUAGES = [
  ["en", "English"],
  ["zh-CN", "简体中文"],
];

// What the app uses when settings.json names no font (settings.rs).
const DEFAULT_FONT = "JetBrains Mono";
const DEFAULT_UI_FONT = "Geist";
const fmtVer = (v) => (v ? `v${String(v).replace(/^v/i, "")}` : "");

export default class Settings extends View {
  lang() {
    const raw = (typeof settings === "function" ? settings()?.language : undefined) ?? this.person?.language ?? "en";
    return String(raw).toLowerCase().startsWith("zh") ? "zh-CN" : "en";
  }

  tr(en, zh) {
    return this.lang() === "zh-CN" ? zh : en;
  }

  init(_props, cx) {
    const asked = props()?.section;
    this.section = SECTIONS().some((s) => s.id === asked) ? asked : "look";
    this.picking = null; // "font" | "ui_font" while its list is open
    this.filter = "";
    this.fontInput = InputState.new({ placeholder: this.tr("Type to search fonts", "输入字体名称搜索") });
    this.fontInput.on("change", (_e, cx) => { this.filter = this.fontInput.value(); cx.notify(); });
    this.renameInput = InputState.new();
    this.renameInput.on("focus", (_e, cx) => { this.renameFocused = true; cx.notify(); });
    this.renameInput.on("blur", (_e, cx) => { this.renameFocused = false; cx.notify(); });
    this.renameInput.on("change", (_e, cx) => { if (this.machineEdit?.action === "rename") this.machineEdit.name = this.renameInput.value(); cx.notify(); });
    this.renameInput.on("submit", (_e, cx) => {
      const edit = this.machineEdit;
      if (edit?.action === "rename") this.machineAction(edit.machine, "rename", cx, edit.name);
    });
    this.person = {};
    this.raw = null;
    this.previews = {}; // layout name -> its "preview" picture
    this.previewsOf = "";
    this.machineList = null;
    this.machineLoading = false;
    this.machineError = null;
    this.machineBusy = {};
    this.machineResults = {};
    this.machineEdit = null;
    this.machineNotice = null;
    const activeSpaceMachine = typeof spaces === "function" ? (spaces()?.find((s) => s.active)?.machine ?? null) : null;
    this.droveMachine = activeSpaceMachine || null;
    this.aiStatus = null;
    this.aiLoading = false;
    this.aiSaving = false;
    this.aiTesting = false;
    this.aiError = null;
    this.aiTestResult = null;
    this.aiProbingPromptMode = false;
    this.aiPromptProbeResult = null;
    this.droveData = null;
    this.droveLoading = false;
    this.droveSaving = false;
    this.droveProbing = false;
    this.droveProbePromise = null;
    this.droveAutoProbed = {};
    this.droveAiTesting = false;
    this.droveMagpieTesting = false;
    this.droveError = null;
    this.droveTestResult = null;
    this.droveMagpieResult = null;
    this.droveKeyDirty = false;
    this.droveShowAddRole = false;
    this.droveAiConfigMode = null; // null | "all" | "models" | "strengths" | "notes"
    this.droveAiConfiguring = false;
    this.droveAiRunningMode = null; // null | "all" | "models" | "strengths" | "notes"
    this.droveAiStage = null; // null | "probing" | "generating"
    this.droveNoteAiTarget = null; // null | "candidate:<roleId>:<idx>"
    this.droveStrengthAiTarget = null; // null | "candidate:<roleId>:<idx>"
    this.droveAiConfigResult = null;
    this.droveRolesBackup = null;
    this.droveAiPromptFocused = false;
    this.droveModelFilter = "";
    this.droveModelSubmit = null;
    this.droveModelInput = InputState.new({ placeholder: this.tr("Search or enter custom value…", "搜索或输入自定义值…") });
    this.droveModelInput.on("change", (_e, cx) => {
      this.droveModelFilter = this.droveModelInput.value();
      cx.notify();
    });
    this.droveModelInput.on("submit", (_e, cx) => {
      const custom = this.droveModelInput.value().trim();
      if (custom && this.droveModelSubmit) {
        const cb = this.droveModelSubmit;
        this.picking = null;
        this.droveModelSubmit = null;
        this.handle.focus();
        cb(custom, cx);
      }
    });
    this.droveStrengthFilter = "";
    this.droveStrengthSubmit = null;
    this.droveStrengthInput = InputState.new({ placeholder: this.tr("Add work type (comma-separated)…", "输入任务类型，回车或点击添加（支持逗号分隔）…") });
    this.droveStrengthInput.on("change", (_e, cx) => {
      this.droveStrengthFilter = this.droveStrengthInput.value();
      cx.notify();
    });
    this.droveStrengthInput.on("submit", (_e, cx) => {
      const raw = this.droveStrengthInput.value().trim();
      if (raw && this.droveStrengthSubmit) {
        this.droveStrengthSubmit(raw, cx);
        this.droveStrengthFilter = "";
        this.droveStrengthInput.set_value("");
        cx.notify();
      }
    });
    this.droveAiRolePromptInput = TextareaState.new({
      value: "",
      placeholder: "",
    });
    this.droveAiRolePromptInput.set_auto_grow(3, 8);
    this.droveAiRolePromptInput.on("focus", (_e, cx) => { this.droveAiPromptFocused = true; cx.notify(); });
    this.droveAiRolePromptInput.on("blur", (_e, cx) => { this.droveAiPromptFocused = false; cx.notify(); });
    this.droveAiRolePromptInput.on("change", (_e, cx) => { cx.notify(); });
    this.droveAiRolePromptInput.on("submit", (e, cx) => {
      if ((e?.ctrl || e?.meta) && !this.droveAiConfiguring && this.droveAiConfigMode) {
        this.aiConfigureDroveRoles(this.droveAiConfigMode, cx);
      }
    });
    this.droveCandNoteSubmit = null;
    this.droveCandNoteInput = TextareaState.new({
      value: "",
      placeholder: "",
    });
    this.droveCandNoteInput.set_auto_grow(2, 4);
    this.droveCandNoteInput.on("change", (_e, cx) => { cx.notify(); });
    this.droveCandNoteInput.on("submit", (e, cx) => {
      if ((e?.ctrl || e?.meta) && this.droveCandNoteSubmit) {
        const cb = this.droveCandNoteSubmit;
        const val = this.droveCandNoteInput.value().trim();
        this.picking = null;
        this.droveCandNoteSubmit = null;
        this.handle.focus();
        cb(val || null, cx);
      }
    });
    this.droveUrlInput = InputState.new({ placeholder: "https://api.example.com/v1" });
    this.droveKeyInput = InputState.new({ placeholder: this.tr("API key (stored in 0600 ai.key)", "API Key（保存在 0600 ai.key）") });
    this.droveKeyInput.set_masked?.(true);
    this.droveKeyInput.on("change", () => { this.droveKeyDirty = true; });
    this.droveMagpieUrlInput = InputState.new({ placeholder: "http://127.0.0.1:3425" });
    this.droveNewRoleInput = InputState.new({ placeholder: this.tr("New role id (e.g. architect)", "新角色 ID（如 architect）") });
    this.keysDoc = null;
    this.keysRaw = null;
    this.keysLoaded = false;
    this.keysFilter = "";
    this.keysFilterInput = InputState.new({ placeholder: this.tr("Search shortcuts or actions…", "搜索快捷键或操作…") });
    this.keysFilterInput.on("change", (_e, cx) => { this.keysFilter = this.keysFilterInput.value(); cx.notify(); });
    this.releasesUrlInput = InputState.new({ placeholder: "https://github.com/herdr2/herdr2/releases/download/ota-latest" });
    this.releasesUrlSynced = false;
    this.releasesUrlSavedAt = 0;
    this.skillData = null;
    this.skillLoading = false;
    this.skillInstalling = null; // null | "all" | "ai" | "custom" | agentId
    this.skillError = null;
    this.skillResult = null;
    this.skillCustomDirInput = InputState.new({ placeholder: this.tr("~/.claude/skills or /custom/skills/dir", "~/.claude/skills 或自定义技能目录") });
    this.skillAiHintInput = InputState.new({ placeholder: this.tr("Optional: specify an agent or custom directory (leave blank to auto-detect all)…", "可选：输入想适配的 Agent 名称或特殊路径（留空则自动全面识别）…") });
    this.recordingTarget = null;
    this.lastRecordedSeq = Number(shared("settings.recorded_shortcut")?.seq ?? 0);
    this.keysNotice = null;
    this.keysError = null;
    this.handle = cx.focus_handle();
    this.handle.focus();
    cx.spawn(async (cx) => {
      await this.load();
      if (!(typeof checking === "function" && checking())) {
        if (this.section === "keyboard") await this.loadKeys();
        if (this.section === "machines") await this.loadMachines();
        if (this.section === "ai") {
          await this.loadMachines();
          await this.loadAi();
        }
        if (this.section === "drove") {
          await this.loadMachines();
          await this.loadDrove();
          this.maybeAutoProbeDrove(cx);
        }
        if (this.section === "skills") {
          await Promise.all([this.loadSkills(), this.loadAi()]);
        }
      }
      cx.notify();
    });
    cx.timer.every(3000, (cx) => {
      if (this.section !== "machines") return;
      cx.spawn(async (cx) => {
        await this.loadMachines();
        cx.notify();
      });
    });
  }

  file() {
    return `${appHome()}/settings.json`;
  }

  // Each layout file may name a picture of itself (`"preview"` on its root).
  loadPreviews(names, cx) {
    this.previewsOf = names.join("\n");
    cx.spawn(async (cx) => {
      const found = {};
      for (const name of names) {
        try {
          const preview = JSON.parse((await readTextFile(`${appHome()}/layouts/${name}.json`)) ?? "{}").preview;
          if (typeof preview === "string" && preview) found[name] = preview;
        } catch {}
      }
      this.previews = found;
      cx.notify();
    });
  }

  // `broken`: settings.json doesn't parse (mid-edit), or isn't an object, so
  // nothing may be written over it.
  async load() {
    this.raw = await readTextFile(this.file());
    try {
      const person = this.raw ? JSON.parse(this.raw) : {};
      this.broken = !person || typeof person !== "object" || Array.isArray(person);
      this.person = this.broken ? {} : person;
    } catch {
      this.person = {};
      this.broken = true;
    }
    if (!this.releasesUrlSynced) {
      this.releasesUrlInput?.set_value?.(this.person.releases_url ?? "");
      this.releasesUrlSynced = true;
    }
  }

  // Changes settings.json the way an agent would: read, change, write back
  // only if nobody changed it meanwhile (else read again and redo). Says so
  // when it couldn't.
  change(edit, cx) {
    cx.spawn(async (cx) => {
      const failed = await this.save(edit);
      if (failed) notify({ ...failed, level: "error" });
      cx.notify();
    });
  }

  // The toast saying why `edit` wasn't written, or null once it is.
  async save(edit) {
    let error;
    for (let attempt = 0; attempt < 3; attempt++) {
      let next;
      try {
        await this.load();
        if (this.broken) return { title: "settings.json has errors: fix it first" };
        next = JSON.parse(JSON.stringify(this.person));
        edit(next);
      } catch (e) {
        return { title: "Cannot read settings.json", body: String(e?.message ?? e) };
      }
      try {
        await writeTextFile(this.file(), JSON.stringify(next, null, 2) + "\n", this.raw);
        this.person = next;
        return null;
      } catch (e) {
        error = e;
      }
    }
    return { title: "Not saved to settings.json", body: String(error?.message ?? error) };
  }

  close() {
    if (this.recordingTarget) {
      this.recordingTarget = null;
      setShared("settings.recording_shortcut", false);
    }
    closeOverlay(props().overlay);
  }

  // Pieces ---------------------------------------------------------------

  heading(t, text) {
    return div().pt(6).pb(8).text_size(12).font_semibold().text_color(t.dim).child(text.toUpperCase());
  }

  // One setting: what it is on the left, the control on the right.
  row(t, title, about, control) {
    return h_flex()
      .w_full()
      .gap(16)
      .py(12)
      .items_center()
      .border_b(1)
      .border_color(t.border)
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(3)
          .child(div().text_size(14).text_color(t.foreground).child(title))
          .when(Boolean(about), (el) => el.child(div().text_size(12.5).line_height("18px").text_color(t.dim).child(about))),
      )
      .child(div().flex_none().child(control));
  }

  button(t, id, label, onClick) {
    return div()
      .id(id)
      .px(12)
      .py(5)
      .rounded(7)
      .border_1()
      .border_color(t.border)
      .bg(t.raised)
      .cursor_pointer()
      .text_size(13)
      .text_color(t.foreground)
      .hover((s) => s.bg(t.hover))
      .on_click(onClick)
      .child(label);
  }

  // − value +
  stepper(t, id, value, onMinus, onPlus) {
    const b = (k, label, f) =>
      div()
        .id(`${id}-${k}`)
        .w(28)
        .h(28)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(15)
        .text_color(t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_click(f)
        .child(label);
    return h_flex()
      .items_center()
      .rounded(7)
      .border_1()
      .border_color(t.border)
      .bg(t.raised)
      .overflow_hidden()
      .child(b("minus", "\u2212", onMinus))
      .child(div().w(54).text_size(13).text_color(t.foreground).flex().justify_center().child(value))
      .child(b("plus", "+", onPlus));
  }

  toggle(t, id, on, onClick) {
    return div()
      .id(id)
      .w(36)
      .h(20)
      .rounded(10)
      .p(2)
      .cursor_pointer()
      .bg(on ? t.accent : t.hover)
      .flex()
      .when(on, (el) => el.justify_end())
      .on_click(onClick)
      .child(div().w(16).h(16).rounded(8).bg(on ? t.accent_foreground : t.muted));
  }

  // Look -----------------------------------------------------------------

  // A small drawing of each layout, in the theme's colors (as the welcome draws them).
  preview(t, name) {
    const row = () => div().flex().flex_row();
    const col = () => div().flex().flex_col();
    const term = (bg = t.background, edge = t.border) => div().flex_1().rounded(2).bg(bg).border_1().border_color(edge);
    const line = (w, c = t.dim) => div().h(3).w(w).flex_none().rounded(2).bg(c);
    const lines = (n, c) => col().gap(4).p(5).children([...Array(n)].map((_, i) => line(i % 2 ? 22 : 32, c)));
    const frame = (el) => el.size_full().rounded(6).overflow_hidden().border_1().border_color(t.border).bg(t.surface);
    if (name === "main")
      return frame(row())
        .child(col().w(42).border_r(1).border_color(t.border).child(lines(4)))
        .child(
          col()
            .flex_1()
            .child(row().h(11).gap(2).p(2).border_b(1).border_color(t.border).child(div().w(20).rounded(2).bg(t.accent)).child(div().w(16).rounded(2).bg(t.raised)).child(div().w(16).rounded(2).bg(t.raised)))
            .child(row().flex_1().p(4).gap(3).child(term()).child(term())),
        );
    if (name === "terminals") {
      const card = () => col().flex_1().rounded(4).border_1().border_color("#2c2f34").bg("#17191c").child(row().h(7).px(3).items_center().border_b(1).border_color("#25282c").child(line(14, "#5d6168")));
      return frame(col())
        .bg("#131619")
        .p(4)
        .gap(4)
        .child(row().h(10).gap(3).p(1).rounded(4).bg("#1d1f22").child(div().w(30).rounded(3).bg("#34373c").border_1().border_color("#4a4d53")).child(div().w(30)))
        .child(row().flex_1().gap(4).child(card()).child(card()));
    }
    // A layout of the person's own: a plain frame.
    return frame(col()).child(row().h(11).border_b(1).border_color(t.border)).child(row().flex_1().p(4).child(term()));
  }

  layoutPicker(t, cx) {
    const current = shared("layout") ?? "main";
    // Shipped looks in the welcome's order, then the person's own; new files show up by themselves.
    const shipped = Object.keys(LOOKS);
    const rank = (n) => (shipped.includes(n) ? shipped.indexOf(n) : shipped.length);
    const names = [...layouts()].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
    if (names.join("\n") !== this.previewsOf) this.loadPreviews(names, cx);
    return h_flex()
      .w_full()
      .flex_wrap()
      .gap(8)
      .children(
        names.map((name) => {
          const on = name === current;
          return v_flex()
            .id(`st-l-${name}`)
            .w(124)
            .gap(6)
            .p(6)
            .rounded(10)
            .border_2()
            .border_color(on ? t.accent : "#00000000")
            .bg(on ? t.raised : "#00000000")
            .cursor_pointer()
            .hover((s) => s.bg(t.raised))
            .on_click((_e, cx) => {
              run("layout.switch", { name });
              cx.notify();
            })
            .child(
              div()
                .h(62)
                .w_full()
                .overflow_hidden()
                .child(
                  this.previews[name]
                    ? image(this.previews[name]).w_full().h(62).rounded(6).border_1().border_color(t.border)
                    : this.preview(t, name),
                ),
            )
            .child(
              h_flex()
                .gap(5)
                .items_center()
                .child(div().text_size(12.5).text_color(on ? t.foreground : t.muted).child(LOOKS[name] ?? name))
                .when(on, (el) => el.child(svg(I("check")).size(12).text_color(t.accent))),
            );
        }),
      );
  }

  languagePicker(t, cx) {
    const now = this.lang();
    return h_flex()
      .p(2)
      .gap(2)
      .rounded(8)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .children(
        LANGUAGES.map(([value, label]) =>
          div()
            .id(`st-lang-${value}`)
            .px(12)
            .py(4)
            .rounded(6)
            .text_size(13)
            .cursor_pointer()
            .when(now === value, (el) => el.bg(t.raised).text_color(t.foreground).border_1().border_color(t.border))
            .when(now !== value, (el) => el.text_color(t.muted).hover((s) => s.text_color(t.foreground)))
            .on_click((_e, cx) => {
              this.person = { ...this.person };
              if (value === "en") delete this.person.language;
              else this.person.language = value;
              run("app.language", { language: value });
              cx.notify();
            })
            .child(label),
        ),
      );
  }

  appearancePicker(t, cx) {
    const now = this.person.appearance ?? null;
    return h_flex()
      .p(2)
      .gap(2)
      .rounded(8)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .children(
        APPEARANCES.map(([value, label, zh]) =>
          div()
            .id(`st-ap-${label}`)
            .px(12)
            .py(4)
            .rounded(6)
            .text_size(13)
            .cursor_pointer()
            .when(now === value, (el) => el.bg(t.raised).text_color(t.foreground).border_1().border_color(t.border))
            .when(now !== value, (el) => el.text_color(t.muted).hover((s) => s.text_color(t.foreground)))
            .on_click((_e, cx) =>
              this.change((p) => {
                if (value === null) delete p.appearance;
                else p.appearance = value;
              }, cx),
            )
            .child(this.tr(label, zh)),
        ),
      );
  }

  themePicker(t, light) {
    const s = settings();
    const current = light ? s.theme_light : s.theme_dark;
    return h_flex()
      .w_full()
      .flex_wrap()
      .gap(8)
      .children(
        // A theme named like a layout (themes/win98.json) is that layout's own.
        themes()
          .filter((x) => Boolean(x.light) === light && (x.name === shared("layout") || !layouts().includes(x.name)))
          .map(({ name, colors: c }) => {
          const on = name === current;
          const label = name.replace(/-/g, " ");
          const dot = (color) => div().w(8).h(8).rounded(4).bg(color);
          return v_flex()
            .id(`st-t-${name}`)
            .w(124)
            .gap(6)
            .p(6)
            .rounded(10)
            .border_2()
            .border_color(on ? t.accent : "#00000000")
            .cursor_pointer()
            .hover((s) => s.bg(t.raised))
            .on_click((_e, cx) => {
              run("app.theme", { name });
              cx.notify();
            })
            .child(
              v_flex()
                .h(46)
                .w_full()
                .p(7)
                .gap(6)
                .rounded(6)
                .border_1()
                .border_color(t.border)
                .bg(c.background)
                .child(div().h(4).w(44).rounded(2).bg(c.terminal_foreground))
                .child(h_flex().gap(4).child(dot(c.accent)).child(dot(c.ansi1)).child(dot(c.ansi2)).child(dot(c.ansi3)).child(dot(c.ansi5))),
            )
            .child(
              h_flex()
                .w_full()
                .min_w(0)
                .gap(5)
                .items_center()
                .child(div().min_w(0).truncate().tooltip(label).text_size(12.5).text_color(on ? t.foreground : t.muted).child(label))
                .when(on, (el) => el.child(svg(I("check")).size(12).flex_none().text_color(t.accent))),
            );
        }),
      );
  }

  // The font in use, written in itself; a click opens the list.
  fontButton(t, which, family, def) {
    const open = this.picking === which;
    const trigger = div()
      .id(`st-f-${which}`)
      .w(220)
      .px(12)
      .py(6)
      .rounded(7)
      .border_1()
      .border_color(open ? t.accent : t.border)
      .bg(t.raised)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => {
        this.picking = open ? null : which;
        this.filter = "";
        this.fontInput.set_value("");
        if (this.picking) this.fontInput.focus();
        else this.handle.focus();
        cx.notify();
      })
      .child(div().font_family(family).text_size(13.5).text_color(t.foreground).child(family));
    return Popup.new(`st-fp-${which}`, trigger)
      .when(open, (el) => el.content(this.fontList(t, which, def)));
  }

  fontList(t, which, def) {
    const q = this.filter.toLowerCase();
    const matches = fonts().filter((f) => f.toLowerCase().includes(q)).slice(0, 200);
    const choose = (name, cx) => {
      this.change((p) => {
        if (name === null) delete p[which];
        else p[which] = name;
      }, cx);
      this.picking = null;
      this.handle.focus();
      cx.notify();
    };
    const item = (id, label, family, onClick) =>
      div()
        .id(id)
        .px(10)
        .py(5)
        .rounded(6)
        .cursor_pointer()
        .hover((s) => s.bg(t.hover))
        .on_click(onClick)
        .child(div().font_family(family).text_size(13.5).text_color(t.foreground).child(label));
    return v_flex()
      .id(`st-font-list-${which}`)
      .w(360)
      .on_mouse_down_out((_e, cx) => {
        this.picking = null;
        this.handle.focus();
        cx.notify();
      })
      .rounded(8)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      // Let the child's native scroll run before stopping the page's wheel event.
      .on_scroll_wheel((_e, cx) => cx.stop_propagation())
      .child(
        div()
          .px(12)
          .py(8)
          .border_b(1)
          .border_color(t.border)
          .text_size(13)
          .child(Input.new(this.fontInput).h(28).text_color(t.foreground)),
      )
      .child(
        div()
          .id(`st-fl-${which}`)
          .h(200)
          .p(4)
          .overflow_y_scrollbar()
          .when(!q, (el) => el.child(item(`st-fd-${which}`, this.tr(`Default (${def})`, `默认 (${def})`), def, (_e, cx) => choose(null, cx))))
          .children(matches.map((f, i) => item(`st-fi-${which}-${i}`, f, f, (_e, cx) => choose(f, cx)))),
      );
  }

  panels() {
    return resolveEnabledPanels(PANELS.map(([id]) => id));
  }

  togglePanel(id, cx) {
    const cur = this.panels();
    const on = cur.includes(id);
    const next = on ? cur.filter((x) => x !== id) : [...cur, id];
    setShared("workspace.panels_has_checkpoints", true);
    setShared("workspace.panels", next);
    const find = (node, target) => {
      if (!node) return null;
      if (node.id === target) return node;
      for (const c of node.children ?? []) {
        const f = find(c, target);
        if (f) return f;
      }
      return null;
    };
    const L = layout();
    if (on && find(L, "panel") && (find(L, "panel-body")?.active === id || next.length === 0)) {
      run("layout.hide", { id: "panel" });
    }
    cx.notify();
  }

  look(t, cx) {
    const s = settings();
    const p = this.person;
    // Numbers come back from the host as 32-bit floats: 1.1 reads as 1.100000023841858.
    const same = (a, b) => (typeof a === "number" && typeof b === "number" ? Math.abs(a - b) < 1e-4 : a === b);
    const own = (key, composed, def) => !same(p[key] ?? def, composed);
    const size = (key, value, d, cx) => this.change((q) => (q[key] = Math.max(8, Math.min(32, (q[key] ?? value) + d))), cx);
    const step = (key, value, d, lo, hi, cx) => this.change((q) => (q[key] = Math.round(Math.max(lo, Math.min(hi, (q[key] ?? value) + d)) * 100) / 100), cx);
    const ownNote = this.tr("This layout sets its own; yours shows in the others.", "当前布局单独覆盖了该项；其他布局将使用你的设置。");
    const fontRow = (which, sizeKey, title, family, def, sizeNow) =>
      v_flex()
        .w_full()
        .child(
          this.row(
            t,
            title,
            own(which, family, def) ? ownNote : null,
            h_flex()
              .gap(8)
              .items_center()
              .child(this.fontButton(t, which, family, def))
              // The panels' text follows zoom: they size their rows to it.
              .when(Boolean(sizeKey), (el) => el.child(this.stepper(t, `st-${sizeKey}`, `${Math.round(sizeNow)}`, (_e, cx) => size(sizeKey, sizeNow, -1, cx), (_e, cx) => size(sizeKey, sizeNow, 1, cx)))),
          ),
        );
    const isWin = window()?.platform === "windows";
    const shellChoices = isWin
      ? [
          { value: "auto", label: this.tr("Auto (pwsh / PowerShell)", "自动（pwsh / PowerShell）"), meta: "default" },
          { value: "powershell", label: "Windows PowerShell", meta: "powershell.exe" },
          { value: "pwsh", label: "PowerShell 7 (pwsh)", meta: "pwsh.exe" },
          { value: "git-bash", label: "Git Bash", meta: "bash.exe" },
          { value: "cmd", label: this.tr("Command Prompt (cmd)", "命令提示符 (cmd)"), meta: "cmd.exe" },
        ]
      : [
          { value: "auto", label: this.tr("Auto ($SHELL)", "自动（系统 $SHELL）"), meta: "default" },
          { value: "zsh", label: "zsh", meta: "zsh" },
          { value: "bash", label: "bash", meta: "bash" },
          { value: "fish", label: "fish", meta: "fish" },
          { value: "sh", label: "sh", meta: "sh" },
        ];
    const rawShell = String(p.shell ?? s.shell ?? "auto").trim() || "auto";
    const currentShell = isWin && rawShell.toLowerCase() === "wsl" ? "auto" : rawShell;
    const shellItems = shellChoices.map((opt) => ({ ...opt, active: opt.value === currentShell }));
    const currentShellLabel = shellChoices.find((opt) => opt.value === currentShell)?.label ?? currentShell;
    return v_flex()
      .w_full()
      .child(this.heading(t, this.tr("Language", "语言")))
      .child(this.row(t, this.tr("Interface language", "界面语言"), this.tr("Applies immediately across the app.", "在界面各面板、菜单与设置页即时生效。"), this.languagePicker(t, cx)))
      .child(div().h(18))
      .child(this.heading(t, this.tr("Layout", "布局")))
      .child(this.layoutPicker(t, cx))
      .child(div().h(18))
      .child(this.heading(t, this.tr("Panels", "右侧面板")))
      .children(
        PANELS.map(([id, title, zhTitle, about, zhAbout]) =>
          this.row(
            t,
            this.tr(title, zhTitle),
            this.tr(about, zhAbout),
            this.toggle(t, `st-panel-${id}`, this.panels().includes(id), (_e, cx) => this.togglePanel(id, cx)),
          ),
        ),
      )
      .child(div().h(18))
      .child(this.heading(t, this.tr("Theme", "主题")))
      .child(this.row(t, this.tr("Appearance", "外观模式"), this.tr("Light or dark, or as your system is now.", "浅色、深色或自动跟随系统设置。"), this.appearancePicker(t, cx)))
      .child(div().pt(12).pb(8).text_size(12.5).text_color(t.dim).child(this.tr("For the layout you're in: each layout keeps its own light and dark theme.", "仅作用于当前布局：每个布局可分别保留深色与浅色主题。")))
      .child(div().pb(6).text_size(12.5).font_medium().text_color(t.muted).child(this.tr("Dark", "深色")))
      .child(this.themePicker(t, false))
      .child(div().pt(10).pb(6).text_size(12.5).font_medium().text_color(t.muted).child(this.tr("Light", "浅色")))
      .child(this.themePicker(t, true))
      .child(div().h(18))
      .child(this.heading(t, this.tr("Size and fonts", "缩放与字体")))
      .child(
        this.row(
          t,
          this.tr("Zoom", "界面缩放"),
          this.tr("Everything: terminals, panels, text.", "等比缩放终端、侧栏、面板与文字。"),
          h_flex()
            .gap(8)
            .items_center()
            .child(this.stepper(t, "st-zoom", `${Math.round(s.zoom * 100)}%`, () => run("app.zoom_out"), () => run("app.zoom_in")))
            .child(this.button(t, "st-zoom-reset", this.tr("Reset", "重置"), () => run("app.zoom_reset"))),
        ),
      )
      .child(
        this.row(
          t,
          this.tr("Default terminal shell", "默认终端类型"),
          this.tr("Which shell local terminal tabs and splits open with.", "本地新建终端标签页或分栏时默认使用的终端程序。"),
          this.drovePicker(
            t,
            "term-shell",
            currentShellLabel,
            shellItems,
            (val, cx) => {
              this.change((q) => {
                if (!val || val === "auto") delete q.shell;
                else q.shell = val;
              }, cx);
            },
            { width: 220, popupWidth: 280, anchor: "bottom_right" },
            cx,
          ),
        ),
      )
      .child(fontRow("font", "font_size", this.tr("Terminal font", "终端字体"), s.font_family, DEFAULT_FONT, s.font_size))
      .child(
        this.row(
          t,
          this.tr("Terminal font weight", "终端字重"),
          own("font_weight", s.font_weight, 500) ? ownNote : this.tr("Bold text stays at 700.", "粗体文本保持 700 字重。"),
          this.stepper(t, "st-font_weight", `${s.font_weight}`, (_e, cx) => step("font_weight", s.font_weight, -100, 100, 900, cx), (_e, cx) => step("font_weight", s.font_weight, 100, 100, 900, cx)),
        ),
      )
      .children([
        ["terminal_line_height", this.tr("Terminal line height", "终端行高"), 0.8, 2, 0.1],
        ["terminal_cell_width", this.tr("Terminal cell width", "终端字符宽度"), 0.8, 2, 0.05],
        ["terminal_scroll_speed", this.tr("Terminal scroll speed", "终端滚动速度"), 0.1, 5, 0.1],
      ].map(([key, title, min, max, step]) => {
        const by = (d) => (_e, cx) => this.change((q) => (q[key] = Math.max(min, Math.min(max, Math.round(Math.round(((q[key] ?? s[key]) + d) / step) * step * 100) / 100))), cx);
        return this.row(
          t, title,
          own(key, s[key], 1) ? ownNote : null,
          this.stepper(t, `st-${key}`, `${Math.round(s[key] * 100)}%`, by(-step), by(step)),
        );
      }))
      .child(fontRow("ui_font", null, this.tr("Interface font", "界面字体"), s.ui_font_family, DEFAULT_UI_FONT));
  }

  keysFile() {
    return `${appHome()}/keys.json`;
  }

  isMacPlatform() {
    return window()?.platform === "macos";
  }

  effectiveKeysDoc() {
    if (this.keysDoc) return this.keysDoc;
    const list = typeof keys === "function" ? keys() : [];
    return parseKeysDoc(null, list, this.isMacPlatform());
  }

  async loadKeys() {
    const isMac = this.isMacPlatform();
    try {
      const text = await readTextFile(this.keysFile());
      this.keysRaw = text;
      this.keysDoc = parseKeysDoc(text, typeof keys === "function" ? keys() : [], isMac);
      this.keysError = null;
    } catch {
      this.keysRaw = null;
      this.keysDoc = parseKeysDoc(null, typeof keys === "function" ? keys() : [], isMac);
    } finally {
      this.keysLoaded = true;
    }
  }

  startRecordingShortcut(section, action, label, cx) {
    this.recordingTarget = { section, action, label };
    this.keysNotice = null;
    this.keysError = null;
    this.lastRecordedSeq = Number(shared("settings.recorded_shortcut")?.seq ?? this.lastRecordedSeq);
    setShared("settings.recording_shortcut", true);
    this.handle.focus();
    cx.notify();
  }

  cancelRecordingShortcut(cx) {
    this.recordingTarget = null;
    setShared("settings.recording_shortcut", false);
    cx?.notify?.();
  }

  saveKeysDoc(nextDoc, notice, cx) {
    const serialized = JSON.stringify(nextDoc, null, 2) + "\n";
    this.keysDoc = nextDoc;
    this.keysNotice = notice || null;
    this.keysError = null;
    cx?.notify?.();
    if (typeof checking === "function" && checking()) return;
    cx.spawn(async (saveCx) => {
      let err = null;
      for (let attempt = 0; attempt < 2; attempt++) {
        try {
          await writeTextFile(this.keysFile(), serialized, this.keysRaw);
          this.keysRaw = serialized;
          err = null;
          break;
        } catch (e) {
          err = e;
          try {
            this.keysRaw = await readTextFile(this.keysFile());
          } catch {
            this.keysRaw = null;
          }
        }
      }
      if (err) {
        this.keysError = this.tr(
          `Failed to save keys.json: ${err?.message ?? err}`,
          `保存 keys.json 失败：${err?.message ?? err}`,
        );
      }
      saveCx.notify();
    });
  }

  applyRecordedShortcut(chord, cx) {
    const target = this.recordingTarget;
    if (!target || !chord) return;
    this.recordingTarget = null;
    const isMac = this.isMacPlatform();
    const isZh = this.lang() === "zh-CN";
    const { doc: nextDoc, displacedAction } = setActionBinding(
      this.effectiveKeysDoc(),
      target.section,
      target.action,
      chord,
    );
    const pretty = formatChord(chord, isMac);
    const notice = displacedAction
      ? this.tr(
          `Bound ${pretty} to "${target.label}" (reassigned from "${findActionLabel(displacedAction, false)}").`,
          `已将 ${pretty} 绑定到「${target.label}」（已从「${findActionLabel(displacedAction, true)}」移出）。`,
        )
      : this.tr(
          `Bound ${pretty} to "${target.label}". Changes take effect immediately.`,
          `已将 ${pretty} 绑定到「${target.label}」，立即生效。`,
        );
    this.saveKeysDoc(nextDoc, notice, cx);
  }

  clearShortcut(section, action, label, cx) {
    if (this.recordingTarget) this.cancelRecordingShortcut(null);
    const { doc: nextDoc } = clearActionBinding(this.effectiveKeysDoc(), section, action);
    this.saveKeysDoc(
      nextDoc,
      this.tr(`Cleared shortcut for "${label}".`, `已清除「${label}」的快捷键。`),
      cx,
    );
  }

  resetSingleShortcut(item, cx) {
    if (this.recordingTarget) this.cancelRecordingShortcut(null);
    const isMac = this.isMacPlatform();
    const defChord = defaultChordFor(item, isMac);
    const label = this.tr(item.title, item.zh);
    const { doc: nextDoc } = setActionBinding(this.effectiveKeysDoc(), item.section, item.action, defChord);
    this.saveKeysDoc(
      nextDoc,
      this.tr(
        `Reset "${label}" to ${formatChord(defChord, isMac)}.`,
        `已将「${label}」恢复默认：${formatChord(defChord, isMac)}。`,
      ),
      cx,
    );
  }

  resetAllShortcuts(cx) {
    if (this.recordingTarget) this.cancelRecordingShortcut(null);
    const nextDoc = buildDefaultKeysDoc(this.isMacPlatform());
    this.saveKeysDoc(
      nextDoc,
      this.tr("All shortcuts restored to defaults.", "已将全部快捷键恢复为默认配置。"),
      cx,
    );
  }

  keyboard(t, cx) {
    const isMac = this.isMacPlatform();
    const doc = this.effectiveKeysDoc();
    const query = this.keysFilter.trim().toLowerCase();
    const purple = "#a855f7";
    const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);
    const optionChoices = [
      ["auto", this.tr("Automatic (Alt on US keyboards)", "自动识别（美式键盘作为 Alt）")],
      ["on", "Alt"],
      ["off", this.tr("Types characters", "输入特殊字符")],
      ["left", this.tr("Left Option is Alt", "左 Option 作为 Alt")],
      ["right", this.tr("Right Option is Alt", "右 Option 作为 Alt")],
    ];

    const filteredGroups = SHORTCUT_GROUPS.map((g) => {
      const items = g.items.filter((it) => {
        if (!query) return true;
        const cur = primaryChordFor(doc, it, isMac);
        const hay = `${it.title} ${it.zh} ${it.action} ${cur} ${formatChord(cur, isMac)}`.toLowerCase();
        return hay.includes(query);
      });
      return { ...g, items };
    }).filter((g) => g.items.length > 0);

    return v_flex()
      .id("st-shortcuts")
      .w_full()
      .gap(6)
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap(10)
          .child(this.heading(t, this.tr("Keyboard Shortcuts", "快捷键配置")))
          .child(
            div()
              .id("st-keys-reset-all")
              .px(10)
              .py(4)
              .rounded(6)
              .border_1()
              .border_color(t.border)
              .bg(t.raised)
              .text_size(11.5)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, clickCx) => this.resetAllShortcuts(clickCx))
              .child(this.tr("Reset all to defaults", "恢复全部默认")),
          ),
      )
      .child(
        div()
          .pb(6)
          .text_size(12.5)
          .text_color(t.dim)
          .child(
            this.tr(
              "Click any shortcut badge and press a new key combination. Saved live to keys.json.",
              "点击任意快捷键徽章后直接按下新的组合键即可修改，实时写入 keys.json 并立即生效。",
            ),
          ),
      )
      .child(
        div()
          .id("st-keys-filter")
          .w_full()
          .pb(6)
          .child(
            Input.new(this.keysFilterInput)
              .w_full()
              .h(34)
              .px(10)
              .rounded(8)
              .border_1()
              .border_color(t.border)
              .bg(t.background)
              .text_size(12.5)
              .text_color(t.foreground),
          ),
      )
      .when(Boolean(this.recordingTarget), (el) =>
        el.child(
          h_flex()
            .id("st-keys-recording-banner")
            .w_full()
            .px(12)
            .py(8)
            .mb(4)
            .gap(8)
            .items_center()
            .rounded(8)
            .bg(alpha(purple, "1c") ?? t.raised)
            .border_1()
            .border_color(alpha(purple, "77") ?? t.accent)
            .child(
              div()
                .flex_1()
                .text_size(12.5)
                .font_medium()
                .text_color(t.foreground)
                .child(
                  this.tr(
                    `Recording shortcut for "${this.recordingTarget.label}" — press new keys now, or Esc to cancel`,
                    `正在录制「${this.recordingTarget.label}」的快捷键 — 请直接按下组合键，按 Esc 取消`,
                  ),
                ),
            )
            .child(
              div()
                .id("st-keys-cancel-record")
                .px(8)
                .py(3)
                .rounded(5)
                .bg(t.background)
                .border_1()
                .border_color(t.border)
                .text_size(11)
                .text_color(t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_click((_e, clickCx) => this.cancelRecordingShortcut(clickCx))
                .child(this.tr("Cancel", "取消")),
            ),
        ),
      )
      .when(Boolean(this.keysNotice), (el) =>
        el.child(
          div()
            .id("st-keys-notice")
            .px(10)
            .py(6)
            .mb(4)
            .rounded(7)
            .bg(alpha(t.accent ?? "#3b82f6", "18") ?? t.raised)
            .border_1()
            .border_color(alpha(t.accent ?? "#3b82f6", "44") ?? t.border)
            .text_size(12)
            .text_color(t.foreground)
            .child(this.keysNotice),
        ),
      )
      .when(Boolean(this.keysError), (el) =>
        el.child(
          div()
            .id("st-keys-error")
            .px(10)
            .py(6)
            .mb(4)
            .rounded(7)
            .bg(alpha(t.blocked ?? "#ef4444", "1a") ?? t.raised)
            .border_1()
            .border_color(alpha(t.blocked ?? "#ef4444", "55") ?? t.border)
            .text_size(12)
            .text_color(t.blocked ?? "#ef4444")
            .child(this.keysError),
        ),
      )
      .children(
        filteredGroups.map((group) =>
          v_flex()
            .w_full()
            .pb(12)
            .gap(2)
            .child(
              div()
                .pt(8)
                .pb(4)
                .text_size(11.5)
                .font_semibold()
                .text_color(t.dim)
                .child(this.tr(group.title, group.zh)),
            )
            .children(
              group.items.map((item) => {
                const label = this.tr(item.title, item.zh);
                const curChord = primaryChordFor(doc, item, isMac);
                const defChord = defaultChordFor(item, isMac);
                const isModified = curChord !== defChord;
                const isRecording = Boolean(
                  this.recordingTarget
                    && this.recordingTarget.section === item.section
                    && this.recordingTarget.action === item.action,
                );
                const keyId = `st-key-${item.section}-${item.action}`;

                return h_flex()
                  .w_full()
                  .py(8)
                  .px(10)
                  .gap(10)
                  .items_center()
                  .rounded(8)
                  .border_b(1)
                  .border_color(alpha(t.border, "66") ?? t.border)
                  .when(isRecording, (r) => r.bg(alpha(purple, "12") ?? t.raised))
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(1)
                      .child(div().text_size(13).text_color(t.foreground).truncate().child(label))
                      .child(
                        div()
                          .font_family(settings()?.font_family ?? DEFAULT_FONT)
                          .text_size(10.5)
                          .text_color(t.dim)
                          .truncate()
                          .child(item.section === "terminal" ? `${item.action}  (terminal)` : item.action),
                      ),
                  )
                  .child(
                    h_flex()
                      .flex_none()
                      .gap(6)
                      .items_center()
                      .child(
                        div()
                          .id(keyId)
                          .px(10)
                          .py(4)
                          .rounded(6)
                          .border_1()
                          .border_color(isRecording ? purple : curChord ? t.border : (alpha(t.border, "66") ?? t.border))
                          .bg(isRecording ? (alpha(purple, "24") ?? t.raised) : curChord ? t.raised : t.background)
                          .font_family(settings()?.font_family ?? DEFAULT_FONT)
                          .text_size(11.5)
                          .font_medium()
                          .text_color(isRecording ? purple : curChord ? t.foreground : t.dim)
                          .cursor_pointer()
                          .hover((s) => s.bg(isRecording ? (alpha(purple, "30") ?? t.hover) : t.hover))
                          .tooltip(
                            isRecording
                              ? this.tr("Press new key combination (Esc to cancel)", "请按下新组合键（Esc 取消）")
                              : this.tr("Click to record a new shortcut", "点击录制新的快捷键"),
                          )
                          .on_click((_e, clickCx) => {
                            if (isRecording) this.cancelRecordingShortcut(clickCx);
                            else this.startRecordingShortcut(item.section, item.action, label, clickCx);
                          })
                          .child(
                            isRecording
                              ? this.tr("Press keys…", "请按键…")
                              : curChord
                                ? formatChord(curChord, isMac)
                                : this.tr("Unbound", "未设置"),
                          ),
                      )
                      .when(Boolean(curChord) && !isRecording, (el) =>
                        el.child(
                          div()
                            .id(`st-key-clear-${item.section}-${item.action}`)
                            .px(6)
                            .py(4)
                            .rounded(5)
                            .text_size(11)
                            .text_color(t.dim)
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover).text_color(t.foreground))
                            .tooltip(this.tr("Unbind shortcut", "清除快捷键"))
                            .on_click((_e, clickCx) => this.clearShortcut(item.section, item.action, label, clickCx))
                            .child("×"),
                        ),
                      )
                      .when(isModified && !isRecording, (el) =>
                        el.child(
                          div()
                            .id(`st-key-reset-${item.section}-${item.action}`)
                            .px(6)
                            .py(4)
                            .rounded(5)
                            .text_size(10.5)
                            .text_color(t.muted)
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover).text_color(t.foreground))
                            .tooltip(this.tr(`Reset to default (${formatChord(defChord, isMac)})`, `恢复默认 (${formatChord(defChord, isMac)})`))
                            .on_click((_e, clickCx) => this.resetSingleShortcut(item, clickCx))
                            .child(this.tr("Reset", "恢复")),
                        ),
                      ),
                  );
              }),
            ),
        ),
      )
      .when(filteredGroups.length === 0, (el) =>
        el.child(div().py(16).text_size(12.5).text_color(t.dim).child(this.tr("No matching shortcuts.", "没有匹配的快捷键。"))),
      )
      .when(isMac, (el) =>
        el
          .child(div().pt(12).child(this.heading(t, this.tr("Option key", "Option 键行为"))))
          .child(div().pb(12).text_size(13).text_color(t.dim).child(this.tr("Use Option for terminal shortcuts or to type accents and special characters.", "选择将 Option 键用作终端快捷键，还是用于输入重音与特殊字符。")))
          .children(optionChoices.map(([value, label]) => {
            const on = (this.person.option_as_alt ?? "auto") === value;
            return h_flex()
              .id(`st-option-${value}`)
              .gap(10)
              .px(12)
              .py(10)
              .rounded(7)
              .items_center()
              .cursor_pointer()
              .hover((s) => s.bg(t.hover))
              .on_click((_e, clickCx) => this.change((p) => { p.option_as_alt = value; }, clickCx))
              .child(div().text_color(on ? t.accent : t.muted).child(on ? "◉" : "○"))
              .child(div().text_size(14).text_color(t.foreground).child(label));
          })),
      );
  }

  // Machines -------------------------------------------------------------

  async loadMachines() {
    if (this.machineLoading) return;
    this.machineLoading = true;
    try {
      const r = await hc(["machine", "ls", "--json"]);
      if (r.code !== 0) throw new Error(r.stderr || r.stdout || "Couldn't read machines.");
      const list = JSON.parse(r.stdout).machines;
      if (!Array.isArray(list)) throw new Error("Couldn't read machines.");
      this.machineList = list.filter((m) => !m.temporary);
      this.machineError = null;
    } catch (e) {
      this.machineError = String(e);
    } finally {
      this.machineLoading = false;
    }
  }

  machineAction(machine, action, cx, name) {
    if (this.machineBusy[machine]) return;
    if (action === "rename" && (!name?.trim() || name.trim() === machine)) return;
    this.machineBusy[machine] = action;
    delete this.machineResults[machine];
    this.machineEdit = null;
    this.handle.focus();
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const args = ["machine", action, machine];
        if (action === "rename") args.push(name.trim());
        const r = await hc(args);
        let payload = null;
        try { payload = JSON.parse(r.stdout || r.stderr); } catch {}
        if (r.code !== 0) throw new Error(payload?.error?.message || (r.stderr || r.stdout || "It didn't work.").trim());
        const message = action === "update"
          ? `Updated ${machine}${payload?.version ? ` to h2 ${fmtVer(payload.version)}` : ""}; its terminals kept running.`
          : action === "rename" ? `Renamed ${machine} to ${name.trim()}.` : `Removed ${machine}; its terminals keep running there.`;
        if (action === "update") this.machineResults[machine] = { message, ok: true };
        else this.machineNotice = message;
      } catch (e) {
        this.machineResults[machine] = { message: String(e), ok: false };
      } finally {
        delete this.machineBusy[machine];
      }
      await this.loadMachines();
      cx.notify();
    });
  }

  olderCore(remote, local) {
    const parse = (v) => /^(\d+)\.(\d+)\.(\d+)(?:-([^+]+))?(?:\+.*)?$/.exec(v ?? "");
    const a = parse(remote), b = parse(local);
    if (!a || !b) return false;
    for (let i = 1; i <= 3; i++) {
      if (+a[i] !== +b[i]) return +a[i] < +b[i];
    }
    if (!a[4] || !b[4]) return Boolean(a[4] && !b[4]);
    const x = a[4].split("."), y = b[4].split(".");
    for (let i = 0; i < Math.max(x.length, y.length); i++) {
      if (x[i] === y[i]) continue;
      if (x[i] === undefined || y[i] === undefined) return x[i] === undefined;
      const xn = /^\d+$/.test(x[i]), yn = /^\d+$/.test(y[i]);
      return xn && yn ? +x[i] < +y[i] : xn !== yn ? xn : x[i] < y[i];
    }
    return false;
  }

  machineRow(t, m, localVersion) {
    const busy = this.machineBusy[m.name];
    const edit = this.machineEdit?.machine === m.name ? this.machineEdit : null;
    const state = String(m.state ?? "unknown").replace(/[_-]/g, " ");
    const color = ["connected", "online"].includes(m.state) ? t.ansi2 : ["connecting", "reconnecting"].includes(m.state) ? t.working : t.blocked;
    const result = this.machineResults[m.name];
    const controls = h_flex().gap(6).items_center();
    if (busy) controls.child(svg("mods/workspace-chrome/spinner.svg").size(12).text_color(t.working).spin(900)).child(div().text_size(12).text_color(t.muted).child(busy === "update" ? this.tr("Updating…", "更新中…") : busy === "rename" ? this.tr("Renaming…", "重命名中…") : this.tr("Removing…", "移除中…")));
    else {
      for (const [action, label, zh] of [["rename", "Rename", "重命名"], ["rm", "Remove", "移除"]]) {
        controls.child(this.button(t, `st-m-${action}-${m.name}`, this.tr(label, zh), (_e, cx) => {
          this.machineEdit = { machine: m.name, action, name: m.name };
          if (action === "rename") {
            this.renameInput.set_value(m.name);
            this.renameInput.set_selected_range({ start: 0, end: encodeURIComponent(m.name).replace(/%[0-9A-F]{2}/g, "_").length });
            this.renameFocused = true;
            this.renameInput.focus();
          }
          cx.notify();
        }));
      }
    }
    const row = v_flex().w_full().py(12).gap(8).border_b(1).border_color(t.border)
      .child(h_flex().gap(10).items_center()
        .child(div().w(7).h(7).flex_none().rounded(4).bg(color))
        .child(v_flex().flex_1().min_w(0).gap(3)
          .child(div().text_size(14).text_color(t.foreground).child(m.name))
          .child(div().text_size(12.5).text_color(t.dim).child(`${m.target ?? ""} · ${state} · ${m.version ? `h2 ${fmtVer(m.version)}` : this.tr("h2 version unknown", "h2 版本未知")}`)))
        .when(m.keeps_running_after_logout === false, el => el.child(div().id(`st-m-logout-${m.name}`)
          .text_color(t.blocked).cursor_pointer().tooltip(this.tr("Agents stop when you disconnect. Click for the fix.", "断开连接后智能体将停止运行。点击查看修复方案。"))
          .on_click(() => run("mod.open", { name: "machine-add" })).child("⚠")))
        .child(controls.flex_none()));
    if (m.state === "needs_login") {
      const command = `hc machine login ${m.name}`;
      row.child(v_flex().gap(5)
        .child(div().text_size(12.5).text_color(t.blocked).child(this.tr("needs a login: run", "需要登录，请运行")))
        .child(h_flex().gap(8).items_center()
          .child(div().flex_1().min_w(0).text_size(12.5).text_color(t.foreground).child(command))
          .child(this.button(t, `st-m-login-copy-${m.name}`, this.copiedLogin === command ? this.tr("Copied", "已复制") : this.tr("Copy", "复制"), (_e, cx) => {
            cx.write_to_clipboard(command);
            this.copiedLogin = command;
            cx.notify();
          }))));
    }
    if (busy === "update") row.child(div().text_size(12.5).text_color(t.dim).child(this.tr("Copying this computer's herdr-core. Its terminals keep running.", "正在同步本机 herdr-core，远程终端会继续保持运行。")));
    if (edit && !busy) {
      const cancel = this.button(t, `st-m-cancel-${m.name}`, this.tr("Cancel", "取消"), (_e, cx) => { this.machineEdit = null; this.handle.focus(); cx.notify(); });
      if (edit.action === "rm") row.child(v_flex().gap(8)
        .child(div().text_size(12.5).text_color(t.muted).child(this.tr(`Remove ${m.name}? Its terminals keep running there.`, `确定移除 ${m.name}？远程终端仍会继续运行。`)))
        .child(h_flex().gap(6).child(cancel).child(this.button(t, `st-m-confirm-${m.name}`, this.tr("Remove machine", "确认移除"), (_e, cx) => this.machineAction(m.name, "rm", cx)))));
      else row.child(v_flex().gap(6)
        .child(div().text_size(12).text_color(t.dim).child(this.tr("New name (addresses using the old name stop working)", "新名称（使用旧名称的地址将失效）")))
        .child(h_flex().gap(6).items_center()
          .child(Input.new(this.renameInput).flex_1().min_w(0).h(32).px(10).rounded(7).border_1().border_color(this.renameFocused ? t.accent : t.input).bg(t.background).text_size(13).text_color(t.foreground))
          .child(cancel).child(this.button(t, `st-m-save-${m.name}`, this.tr("Save", "保存"), (_e, cx) => this.machineAction(m.name, "rename", cx, edit.name)))));
    }
    if (result) row.child(div().text_size(12.5).text_color(result.ok ? t.ansi2 : t.blocked).child(result.message));
    return row;
  }

  machines(t) {
    const version = updates().core.version;
    return v_flex().w_full()
      .child(this.heading(t, this.tr("Machines", "主机")))
      .child(div().pb(8).text_size(12.5).line_height("18px").text_color(t.dim).child(this.tr("Your computers, reached over ssh. Their terminals keep running when you disconnect.", "通过 SSH 连接的远程主机。断开连接后上面的终端仍保持运行。")))
      .child(this.row(t, this.tr("This computer", "本机"), `${this.tr("Connected", "已连接")} · ${version ? `h2 ${fmtVer(version)}` : this.tr("h2 version unknown", "h2 版本未知")}`, div().w(7).h(7).rounded(4).bg(t.ansi2)))
      .children((this.machineList ?? []).map((m) => this.machineRow(t, m, version)))
      .when(this.machineList === null && !this.machineError, (el) => el.child(h_flex().py(14).gap(8).items_center().child(svg("mods/workspace-chrome/spinner.svg").size(12).text_color(t.working).spin(900)).child(div().text_size(12.5).text_color(t.dim).child(this.tr("Reading machines…", "正在读取主机列表…")))))
      .when(this.machineList?.length === 0, (el) => el.child(div().py(14).text_size(12.5).text_color(t.dim).child(this.tr("No other machines yet.", "暂未配置其他主机。"))))
      .when(Boolean(this.machineError), (el) => el.child(div().py(10).text_size(12.5).text_color(t.blocked).child(this.machineError)))
      .when(Boolean(this.machineNotice), (el) => el.child(div().py(10).text_size(12.5).text_color(t.ansi2).child(this.machineNotice)))
      .child(h_flex().pt(16).child(this.button(t, "st-m-add", this.tr("Add a machine", "添加主机"), () => run("mod.open", { name: "machine-add" }))));
  }

  // Drove ----------------------------------------------------------------

  droveArgs(base) {
    return this.droveMachine ? [...base, "--on", this.droveMachine] : base;
  }

  async droveHcJson(args) {
    if (typeof checking === "function" && checking()) return null;
    const r = await hc([...this.droveArgs(args), "--json"]);
    let parsed = null;
    try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
    if (r.code !== 0) {
      throw new Error(parsed?.error?.message || parsed?.message || (r.stderr || r.stdout || "Drove command failed.").trim());
    }
    return parsed;
  }

  async droveCallWithJsonFile(args, payload) {
    if (typeof checking === "function" && checking()) return null;
    const reqPath = `${appHome()}/.drove-req-${Date.now()}-${Math.random().toString(36).slice(2, 7)}.json`;
    await writeTextFile(reqPath, JSON.stringify(payload), null);
    return this.droveHcJson([...args, "--file", reqPath, "--consume"]);
  }

  syncDroveInputs(data) {
    const cfg = data?.config ?? {};
    this.droveUrlInput.set_value(cfg.ai?.baseUrl ?? "");
    this.droveMagpieUrlInput.set_value(cfg.magpie?.gatewayUrl ?? "");
    this.droveKeyInput.set_value("");
    this.droveKeyDirty = false;
  }

  syncAiInputs(status) {
    this.droveUrlInput.set_value(status?.baseUrl ?? "");
    this.droveKeyInput.set_value("");
    this.droveKeyDirty = false;
  }

  async loadAi() {
    if (this.aiLoading || (typeof checking === "function" && checking())) return;
    this.aiLoading = true;
    this.aiError = null;
    try {
      const data = await this.droveHcJson(["ai", "status"]);
      this.aiStatus = data;
      this.syncAiInputs(data);
    } catch (e) {
      this.aiError = String(e?.message ?? e);
    } finally {
      this.aiLoading = false;
    }
  }

  saveAiPatch(patch, cx, syncInputs = false) {
    if (this.aiSaving) return;
    this.aiSaving = true;
    this.aiError = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const next = await this.droveCallWithJsonFile(["ai", "settings-set"], patch);
        if (next) {
          this.aiStatus = next;
          if (syncInputs) this.syncAiInputs(next);
          if (patch && ("promptMode" in patch || "promptCacheEnabled" in patch || "model" in patch || "baseUrl" in patch)) {
            this.aiPromptProbeResult = null;
          }
          this.droveData = null;
        }
      } catch (e) {
        this.aiError = String(e?.message ?? e);
        notify({ title: this.tr("AI settings not saved", "AI 设置保存失败"), body: this.aiError, level: "error" });
      } finally {
        this.aiSaving = false;
        cx.notify();
      }
    });
  }

  selectAiModel(model, cx) {
    const nextModel = String(model ?? "").trim();
    const prevModel = String(this.aiStatus?.model ?? "").trim();
    if (nextModel === prevModel || this.aiSaving) return;
    this.aiSaving = true;
    this.aiError = null;
    this.aiPromptProbeResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      let shouldProbe = false;
      try {
        const next = await this.droveCallWithJsonFile(["ai", "settings-set"], {
          model: nextModel,
          promptMode: "auto",
        });
        if (next) {
          this.aiStatus = next;
          this.droveData = null;
          shouldProbe = Boolean(next.configured);
        }
      } catch (e) {
        this.aiError = String(e?.message ?? e);
        notify({ title: this.tr("AI settings not saved", "AI 设置保存失败"), body: this.aiError, level: "error" });
      } finally {
        this.aiSaving = false;
        cx.notify();
      }
      if (shouldProbe) {
        await this.runPromptModeRedetect(cx, true);
      }
    });
  }

  redetectPromptMode(cx) {
    if (this.aiProbingPromptMode || this.aiSaving) return;
    cx.spawn(async (cx) => {
      await this.runPromptModeRedetect(cx, false);
    });
  }

  async runPromptModeRedetect(cx, autoTriggered = false) {
    if (this.aiProbingPromptMode) return;
    this.aiProbingPromptMode = true;
    this.aiError = null;
    this.aiPromptProbeResult = null;
    cx.notify();
    try {
      const res = await this.droveHcJson(["ai", "probe-prompt-mode"]);
      if (res) {
        if (res.status) {
          this.aiStatus = res.status;
          this.droveData = null;
        }
        this.aiPromptProbeResult = { ...res, autoTriggered };
      }
    } catch (e) {
      this.aiPromptProbeResult = {
        ok: false,
        detectedMode: "pending",
        reason: String(e?.message ?? e),
        autoTriggered,
      };
    } finally {
      this.aiProbingPromptMode = false;
      cx.notify();
    }
  }

  testCoreAi(cx) {
    if (this.aiTesting) return;
    this.aiTesting = true;
    this.aiTestResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const baseUrl = this.droveUrlInput.value().trim();
        const apiKey = this.droveKeyInput.value().trim();
        const payload = {};
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        const res = await this.droveCallWithJsonFile(["ai", "test"], payload);
        this.aiTestResult = res;
        this.droveTestResult = res;
        if (res?.ok && this.aiStatus && Array.isArray(res.availableModels) && res.availableModels.length > 0) {
          this.aiStatus = { ...this.aiStatus, availableModels: res.availableModels };
        }
      } catch (e) {
        this.aiTestResult = { ok: false, error: String(e?.message ?? e) };
      } finally {
        this.aiTesting = false;
        cx.notify();
      }
    });
  }

  async loadDrove() {
    if (this.droveLoading || (typeof checking === "function" && checking())) return;
    this.droveLoading = true;
    this.droveError = null;
    try {
      const data = await this.droveHcJson(["drove", "settings"]);
      this.droveData = data;
      this.syncDroveInputs(data);
    } catch (e) {
      this.droveError = String(e?.message ?? e);
    } finally {
      this.droveLoading = false;
    }
  }

  saveDrovePatch(patch, cx, syncInputs = false) {
    if (this.droveSaving) return;
    this.droveSaving = true;
    this.droveError = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const next = await this.droveCallWithJsonFile(["drove", "settings-set"], patch);
        if (next) {
          this.droveData = next;
          if (syncInputs) this.syncDroveInputs(next);
        }
      } catch (e) {
        this.droveError = String(e?.message ?? e);
        notify({ title: this.tr("Drove settings not saved", "Drove 设置保存失败"), body: this.droveError, level: "error" });
      } finally {
        this.droveSaving = false;
        cx.notify();
      }
    });
  }

  async runDroveProbe(cx) {
    if (this.droveProbePromise) return this.droveProbePromise;
    const targetMachine = this.droveMachine;
    const key = targetMachine ?? "__local__";
    this.droveProbing = true;
    this.droveError = null;
    cx?.notify?.();
    const task = (async () => {
      try {
        const snap = await this.droveHcJson(["drove", "probe", "--refresh"]);
        if (this.droveMachine !== targetMachine) return false;
        if (snap && this.droveData) this.droveData = { ...this.droveData, agents: snap };
        else await this.loadDrove();
        this.droveAutoProbed = { ...(this.droveAutoProbed ?? {}), [key]: true };
        return true;
      } catch (e) {
        if (this.droveMachine === targetMachine) {
          this.droveError = String(e?.message ?? e);
        }
        return false;
      } finally {
        this.droveProbing = false;
        if (this.droveProbePromise === task) this.droveProbePromise = null;
        cx?.notify?.();
      }
    })();
    this.droveProbePromise = task;
    return task;
  }

  probeDroveAgents(cx) {
    if (this.droveProbing || this.droveProbePromise) return;
    cx.spawn(async (cx) => {
      await this.runDroveProbe(cx);
    });
  }

  testDroveAi(cx) {
    if (this.droveAiTesting) return;
    this.droveAiTesting = true;
    this.droveTestResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const baseUrl = this.droveUrlInput.value().trim();
        const apiKey = this.droveKeyInput.value().trim();
        const payload = {};
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        this.droveTestResult = await this.droveCallWithJsonFile(["drove", "ai-test"], payload);
      } catch (e) {
        this.droveTestResult = { ok: false, error: String(e?.message ?? e) };
      } finally {
        this.droveAiTesting = false;
        cx.notify();
      }
    });
  }

  hasCachedAgentModels() {
    const snap = this.droveData?.agents;
    if (!snap?.fullProbe) return false;
    const agents = snap.agents ?? [];
    if (agents.length === 0) return false;
    return agents.some((a) => a.installed && (a.models?.length ?? 0) > 0);
  }

  aiConfigureDroveRoles(mode, cx) {
    if (this.droveAiConfiguring) return;
    const effMode = mode === "strengths" ? "strengths" : mode === "notes" ? "notes" : mode === "all" ? "all" : "models";
    this.droveAiConfiguring = true;
    this.droveAiRunningMode = effMode;
    this.droveAiStage = null;
    this.droveAiConfigResult = null;
    this.droveError = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        if (!this.droveData) {
          await this.loadDrove();
          cx.notify();
        }
        const prevRoles = this.droveData?.config?.roles
          ? JSON.parse(JSON.stringify(this.droveData.config.roles))
          : null;
        const prevNote = String(this.droveData?.config?.note ?? "");
        let refreshedBeforeAi = false;
        if (this.droveProbePromise) {
          this.droveAiStage = "probing";
          cx.notify();
          if (await this.droveProbePromise) refreshedBeforeAi = true;
        }
        if (!this.hasCachedAgentModels()) {
          this.droveAiStage = "probing";
          cx.notify();
          if (await this.runDroveProbe(cx)) refreshedBeforeAi = true;
        }
        this.droveAiStage = "generating";
        cx.notify();
        const instruction = (this.droveAiRolePromptInput?.value?.() ?? "").trim();
        const baseUrl = (this.droveUrlInput?.value?.() ?? "").trim();
        const apiKey = (this.droveKeyInput?.value?.() ?? "").trim();
        const model = (this.droveData?.config?.ai?.model ?? "").trim();
        const payload = { mode: effMode, apply: true, language: this.lang() };
        if (instruction) payload.instruction = instruction;
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        if (model) payload.model = model;
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload);
        this.droveAiConfigResult = res
          ? { ...res, mode: effMode, probedRefreshed: Boolean(res.probedRefreshed || refreshedBeforeAi) }
          : res;
        if (res?.ok) {
          if (prevRoles) {
            this.droveRolesBackup = { roles: prevRoles, note: prevNote, mode: effMode, at: Date.now() };
          }
          if (res.settings) {
            this.droveData = res.settings;
          } else {
            await this.loadDrove();
          }
        }
      } catch (e) {
        this.droveAiConfigResult = { ok: false, mode: effMode, error: String(e?.message ?? e) };
      } finally {
        this.droveAiConfiguring = false;
        this.droveAiRunningMode = null;
        this.droveAiStage = null;
        cx.notify();
      }
    });
  }

  aiGenerateDroveNote(scope, roleId, candIdx, cx) {
    if (this.droveNoteAiTarget || this.droveStrengthAiTarget || this.droveAiConfiguring) return;
    const targetKey = `candidate:${roleId}:${candIdx}`;
    this.droveNoteAiTarget = targetKey;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        if (!this.droveData) {
          await this.loadDrove();
          cx.notify();
        }
        const draft = (this.droveCandNoteInput?.value?.() ?? "").trim();
        const baseUrl = (this.droveUrlInput?.value?.() ?? "").trim();
        const apiKey = (this.droveKeyInput?.value?.() ?? "").trim();
        const model = (this.droveData?.config?.ai?.model ?? "").trim();
        const payload = {
          mode: "notes",
          noteScope: "candidate",
          targetRole: roleId,
          targetCandidateIndex: candIdx,
          apply: false,
          language: this.lang(),
        };
        if (draft) payload.instruction = draft;
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        if (model) payload.model = model;
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload);
        if (res?.ok && res?.note) {
          const generated = String(res.note).trim();
          if (this.picking === `drove-note:${roleId}-c${candIdx}`) {
            this.droveCandNoteInput.set_value(generated);
            this.droveCandNoteInput.focus();
          }
        } else {
          notify({
            title: this.tr("AI note generation failed", "AI 生成备注失败"),
            body: String(res?.error ?? this.tr("No note returned by AI", "AI 未返回有效备注")),
            level: "error",
          });
        }
      } catch (e) {
        notify({
          title: this.tr("AI note generation failed", "AI 生成备注失败"),
          body: String(e?.message ?? e),
          level: "error",
        });
      } finally {
        if (this.droveNoteAiTarget === targetKey) {
          this.droveNoteAiTarget = null;
        }
        cx.notify();
      }
    });
  }

  aiConfigureCandidateStrengths(roleId, candIdx, cx) {
    if (this.droveStrengthAiTarget || this.droveNoteAiTarget || this.droveAiConfiguring) return;
    const targetKey = `candidate:${roleId}:${candIdx}`;
    this.droveStrengthAiTarget = targetKey;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        if (!this.droveData) {
          await this.loadDrove();
          cx.notify();
        }
        const prevRoles = this.droveData?.config?.roles
          ? JSON.parse(JSON.stringify(this.droveData.config.roles))
          : null;
        const prevNote = String(this.droveData?.config?.note ?? "");
        const hint = (this.droveStrengthInput?.value?.() ?? "").trim();
        const baseUrl = (this.droveUrlInput?.value?.() ?? "").trim();
        const apiKey = (this.droveKeyInput?.value?.() ?? "").trim();
        const model = (this.droveData?.config?.ai?.model ?? "").trim();
        const payload = {
          mode: "strengths",
          targetRole: roleId,
          targetCandidateIndex: candIdx,
          apply: true,
          language: this.lang(),
        };
        if (hint) payload.instruction = hint;
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        if (model) payload.model = model;
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload);
        if (res?.ok && Array.isArray(res?.strengths) && res.strengths.length > 0) {
          if (prevRoles) {
            this.droveRolesBackup = { roles: prevRoles, note: prevNote, mode: "strengths", at: Date.now() };
          }
          if (res.settings) {
            this.droveData = res.settings;
          } else {
            await this.loadDrove();
          }
          if (hint && this.picking === `drove-str:${roleId}-c${candIdx}`) {
            this.droveStrengthFilter = "";
            this.droveStrengthInput.set_value("");
          }
        } else {
          notify({
            title: this.tr("AI strengths configuration failed", "AI 配置擅长任务失败"),
            body: String(res?.error ?? this.tr("No strengths returned by AI", "AI 未返回有效的擅长任务")),
            level: "error",
          });
        }
      } catch (e) {
        notify({
          title: this.tr("AI strengths configuration failed", "AI 配置擅长任务失败"),
          body: String(e?.message ?? e),
          level: "error",
        });
      } finally {
        if (this.droveStrengthAiTarget === targetKey) {
          this.droveStrengthAiTarget = null;
        }
        cx.notify();
      }
    });
  }

  restoreDroveRolesBackup(cx) {
    if (!this.droveRolesBackup?.roles || this.droveSaving || this.droveAiConfiguring) return;
    const prev = this.droveRolesBackup;
    this.droveRolesBackup = null;
    this.droveAiConfigResult = { ok: true, restored: true };
    const patch = { roles: prev.roles };
    if (typeof prev.note === "string") {
      patch.note = prev.note;
    }
    this.saveDrovePatch(patch, cx, true);
  }

  testDroveMagpie(cx) {
    if (this.droveMagpieTesting) return;
    this.droveMagpieTesting = true;
    this.droveMagpieResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const url = this.droveMagpieUrlInput.value().trim();
        const args = ["drove", "magpie-test"];
        if (url) args.push("--url", url);
        this.droveMagpieResult = await this.droveHcJson(args);
      } catch (e) {
        this.droveMagpieResult = { ok: false, error: String(e?.message ?? e) };
      } finally {
        this.droveMagpieTesting = false;
        cx.notify();
      }
    });
  }

  maybeAutoProbeDrove(cx) {
    const key = this.droveMachine ?? "__local__";
    if (this.droveAutoProbed?.[key] || this.droveProbing || (typeof checking === "function" && checking())) return;
    if (!this.droveData?.agents?.agents) return;
    this.droveAutoProbed = { ...(this.droveAutoProbed ?? {}), [key]: true };
    if (this.droveData.agents.fullProbe) return;
    this.probeDroveAgents(cx);
  }

  probedAgents() {
    const probed = this.droveData?.agents?.agents ?? [];
    if (probed.length > 0) return probed;
    return DROVE_FALLBACK_AGENTS.map((id) => ({ id, name: id, installed: false, models: [], efforts: [] }));
  }

  probedModelsFor(agentId) {
    const a = this.probedAgents().find((x) => x.id === agentId);
    const out = [];
    const seen = new Set();
    for (const raw of a?.models ?? []) {
      const id = (typeof raw === "string" ? raw : raw?.id ?? "").trim();
      if (!id || seen.has(id)) continue;
      seen.add(id);
      const name = typeof raw === "string" ? null : raw?.name;
      out.push({ id, label: name && name !== id ? `${id} · ${name}` : id });
    }
    return out;
  }

  aiAvailableModels() {
    const seen = new Set();
    const out = [];
    const add = (m) => {
      const id = String(m ?? "").trim();
      if (id && !seen.has(id)) {
        seen.add(id);
        out.push(id);
      }
    };
    for (const m of this.aiTestResult?.availableModels ?? []) add(m);
    if (this.aiTestResult?.discoveredModel) add(this.aiTestResult.discoveredModel);
    for (const m of this.droveTestResult?.availableModels ?? []) add(m);
    if (this.droveTestResult?.discoveredModel) add(this.droveTestResult.discoveredModel);
    for (const m of this.aiStatus?.availableModels ?? []) add(m);
    for (const m of this.droveData?.aiStatus?.availableModels ?? []) add(m);
    return out;
  }

  agentIconMeta(agentId) {
    const key = String(agentId ?? "").trim().toLowerCase();
    return DROVE_AGENT_ICONS[key] ?? { path: "icons/agent.svg", tint: true };
  }

  agentIconEl(t, agentId, size = 14) {
    const meta = this.agentIconMeta(agentId);
    return meta.tint
      ? svg(meta.path).size(size).flex_none().text_color(t.muted)
      : image(meta.path).w(size).h(size).flex_none();
  }

  roleDisplayTitle(roleId, role) {
    switch (roleId) {
      case "coder": return this.tr("Coder", "程序员");
      case "tester": return this.tr("Tester", "测试员");
      case "writer": return this.tr("Tech Writer", "技术作家");
      case "illustrator": return this.tr("Illustrator", "绘图员");
      case "reviewer": return this.tr("Reviewer", "审查员");
      case "researcher": return this.tr("Researcher", "调研员");
      default: return role?.label || roleId;
    }
  }

  effortLabel(eff) {
    switch (eff) {
      case "low": return this.tr("Low", "低");
      case "medium": return this.tr("Med", "中");
      case "high": return this.tr("High", "高");
      case "xhigh": return this.tr("Max", "极高");
      default: return eff;
    }
  }

  aiEffortLabel(eff) {
    switch (eff) {
      case "auto": return this.tr("Auto", "默认");
      case "none": return this.tr("Off", "关闭");
      case "low": return this.tr("Low", "低");
      case "medium": return this.tr("Med", "中");
      case "high": return this.tr("High", "高");
      default: return eff;
    }
  }

  aiPromptModeLabel(mode) {
    switch (mode) {
      case "shared_prefix": return this.tr("Shared Prefix", "长共享前缀");
      case "compact": return this.tr("Compact", "精简提示词");
      case "auto":
      default: return this.tr("Auto", "自动");
    }
  }

  normalizeAiContextInput(raw) {
    const clean = String(raw ?? "").trim().toLowerCase();
    if (!clean || clean === "auto" || clean === "default") return "";
    return clean;
  }

  parseAiMaxTokensInput(raw) {
    if (typeof raw === "number") {
      if (!Number.isFinite(raw)) return null;
      const n = Math.round(raw);
      return n <= 0 ? 0 : Math.max(64, Math.min(131072, n));
    }
    const clean = String(raw ?? "").trim().toLowerCase();
    if (!clean || clean === "auto" || clean === "default" || clean === "0") return 0;
    let n;
    if (clean.endsWith("k")) {
      const num = Number(clean.slice(0, -1).trim());
      if (!Number.isFinite(num) || num <= 0) return null;
      n = Math.round(num * 1024);
    } else {
      const num = Number(clean);
      if (!Number.isFinite(num) || num <= 0) return null;
      n = Math.round(num);
    }
    return Math.max(64, Math.min(131072, n));
  }

  drovePicker(t, id, label, items, onSelect, opts = 156, cx) {
    const o = typeof opts === "number" ? { width: opts } : (opts ?? {});
    const width = o.width ?? 156;
    const popupWidth = o.popupWidth ?? Math.max(width + 110, 320);
    const anchor = o.anchor ?? "bottom_left";
    const flex1 = Boolean(o.flex1);
    const allowCustom = Boolean(o.allowCustom);
    const key = `drove:${id}`;
    const open = this.picking === key;
    const trigger = h_flex()
      .id(`st-drp-${id}`)
      .when(flex1, (el) => el.flex_1().min_w(0))
      .when(!flex1, (el) => el.w_full().min_w(0))
      .h(28)
      .px(9)
      .rounded(7)
      .border_1()
      .border_color(open ? t.accent : t.border)
      .bg(t.raised)
      .cursor_pointer()
      .items_center()
      .justify_between()
      .gap(6)
      .hover((s) => s.bg(t.hover))
      .on_click((_e, cx) => {
        this.picking = open ? null : key;
        if (this.picking && allowCustom) {
          this.droveModelFilter = "";
          this.droveModelInput.set_value("");
          this.droveModelSubmit = onSelect;
          this.droveModelInput.focus();
        } else {
          this.droveModelSubmit = null;
          this.handle.focus();
        }
        cx.notify();
      })
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .gap(6)
          .items_center()
          .when(Boolean(o.icon), (el) => el.child(this.agentIconEl(t, o.icon, 13)))
          .when(o.dot !== undefined, (el) =>
            el.child(div().w(5).h(5).rounded(3).flex_none().bg(o.dot ? t.ansi2 : t.dim)),
          )
          .child(div().flex_1().min_w(0).truncate().text_size(12.5).text_color(t.foreground).child(label)),
      )
      .child(div().flex_none().text_size(10).text_color(t.dim).child("▾"));

    const rawQ = allowCustom ? (this.droveModelFilter ?? "") : "";
    const qTrim = rawQ.trim();
    const qLower = qTrim.toLowerCase();
    const filtered = qLower
      ? items.filter((it) => String(it.label ?? "").toLowerCase().includes(qLower) || String(it.value ?? "").toLowerCase().includes(qLower))
      : items;
    const hasExact = qTrim && items.some((it) => String(it.value ?? "") === qTrim);
    const listItems = qTrim && !hasExact
      ? [{ value: qTrim, label: this.tr(`Use "${qTrim}"`, `使用「${qTrim}」`), meta: this.tr("custom", "自定义"), active: false }, ...filtered]
      : filtered;
    const rowH = 30;
    const listH = listItems.length === 0 ? 44 : Math.min(listItems.length * rowH + 8, 280);

    return Popup.new(`st-drpp-${id}`, trigger)
      .anchor(anchor)
      .flex()
      .when(flex1, (el) => el.flex_1().min_w(width))
      .when(!flex1, (el) => el.w(width).min_w(width).max_w(width).flex_none())
      .when(open, (el) =>
        el.content(
          v_flex()
            .w(popupWidth)
            .rounded(8)
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .on_mouse_down_out((_e, cx) => {
              this.picking = null;
              this.droveModelSubmit = null;
              this.handle.focus();
              cx.notify();
            })
            .on_scroll_wheel((_e, cx) => cx.stop_propagation())
            .when(allowCustom, (box) =>
              box.child(
                div()
                  .px(10)
                  .py(7)
                  .border_b(1)
                  .border_color(t.border)
                  .text_size(12.5)
                  .child(Input.new(this.droveModelInput).h(24).text_color(t.foreground)),
              ),
            )
            .child(
              div()
                .id(`st-drpm-${id}`)
                .h(listH)
                .p(4)
                .overflow_y_scrollbar()
                .when(listItems.length === 0, (list) =>
                  list.child(div().px(10).py(8).text_size(12).text_color(t.dim).child(this.tr("No matching options", "没有匹配项"))),
                )
                .children(
                  listItems.map((it, idx) =>
                    h_flex()
                      .id(`st-drpi-${id}-${idx}`)
                      .w_full()
                      .h(rowH)
                      .flex_none()
                      .px(9)
                      .gap(7)
                      .rounded(6)
                      .items_center()
                      .cursor_pointer()
                      .bg(it.active ? t.raised : "#00000000")
                      .hover((s) => s.bg(t.hover))
                      .on_click((_e, cx) => {
                        this.picking = null;
                        this.droveModelSubmit = null;
                        this.handle.focus();
                        onSelect(it.value, cx);
                      })
                      .when(Boolean(it.icon), (row) => row.child(this.agentIconEl(t, it.icon, 13)))
                      .when(it.dot !== undefined, (row) =>
                        row.child(div().w(6).h(6).rounded(3).flex_none().bg(it.dot ? t.ansi2 : t.dim)),
                      )
                      .child(div().flex_1().min_w(0).truncate().text_size(12.5).text_color(it.active ? t.foreground : t.muted).child(it.label))
                      .when(Boolean(it.meta), (row) => row.child(div().flex_none().text_size(11).text_color(t.dim).child(it.meta))),
                  ),
                ),
            ),
        ),
      );
  }

  modelHasEmbeddedEffort(modelId) {
    const raw = String(modelId ?? "").trim();
    const lower = raw.toLowerCase();
    if (!lower || lower === "auto" || lower === "default" || lower === "default[]") return false;
    let slug = lower;
    const bracketStart = lower.indexOf("[");
    if (bracketStart >= 0 && lower.endsWith("]")) {
      const inner = lower.slice(bracketStart + 1, -1);
      for (const entry of inner.split(",")) {
        const eq = entry.indexOf("=");
        if (eq < 0) continue;
        const k = entry.slice(0, eq).trim();
        const v = entry.slice(eq + 1).trim();
        if (["effort", "reasoning_effort", "reasoning", "thought_level", "thinking"].includes(k) && v && v !== "auto" && v !== "default") {
          return true;
        }
      }
      slug = lower.slice(0, bracketStart).trim();
    }
    if (slug.endsWith("-fast")) slug = slug.slice(0, -5);
    return ["-none", "-minimal", "-low", "-medium", "-med", "-high", "-xhigh", "-extra-high", "-max", "-thinking"].some((s) =>
      slug.endsWith(s),
    );
  }

  normalizeCursorBaseModel(modelId) {
    let base = String(modelId ?? "").trim();
    if (!base) return "";
    const bracketStart = base.indexOf("[");
    if (bracketStart >= 0 && base.endsWith("]")) {
      base = base.slice(0, bracketStart).trim();
    }
    if (base.toLowerCase().startsWith("cursor-")) base = base.slice(7);
    if (base.toLowerCase().endsWith("-fast")) base = base.slice(0, -5);
    if (base.toLowerCase().endsWith("-thinking")) base = base.slice(0, -9);
    for (const suffix of ["-extra-high", "-xhigh", "-minimal", "-medium", "-none", "-high", "-low", "-med", "-max"]) {
      if (base.toLowerCase().endsWith(suffix)) {
        base = base.slice(0, -suffix.length);
        break;
      }
    }
    if (base.toLowerCase().endsWith("-thinking")) base = base.slice(0, -9);
    return base;
  }

  cursorContextChoicesFor(modelId) {
    const probedCursor = this.probedAgents().find((a) => a.id === "cursor" || a.id === "cursor-agent");
    const map = probedCursor?.modelContexts ?? {};
    const m = String(modelId ?? "").trim();
    const base = this.normalizeCursorBaseModel(m);
    const rawKey = m && Array.isArray(map[m]) ? map[m] : (base && Array.isArray(map[base]) ? map[base] : null);
    let raw = rawKey && rawKey.length > 0 ? rawKey : null;
    if (!raw) {
      const lower = (base || m).toLowerCase();
      if (lower.startsWith("grok-4.7")) raw = ["256k", "500k"];
      else if (lower.startsWith("claude-opus-5") || lower.startsWith("claude-sonnet-5") || lower.startsWith("claude-fable-5") || lower.startsWith("muse-")) {
        raw = ["300k", "1m"];
      } else if (lower.startsWith("gpt-5.")) raw = ["272k", "1m"];
      else if (lower.startsWith("claude-sonnet-4") || lower.startsWith("claude-opus-4-6")) raw = ["200k", "1m"];
      else raw = ["200k", "256k", "272k", "300k", "500k", "1m"];
    }
    const seen = new Set();
    const out = [];
    for (const v of raw) {
      const s = String(v ?? "").trim();
      if (!s || seen.has(s.toLowerCase())) continue;
      seen.add(s.toLowerCase());
      out.push(s);
    }
    return out;
  }

  formatContextLabel(ctx, compact = false) {
    const raw = String(ctx ?? "").trim();
    if (!raw || raw.toLowerCase() === "auto") {
      return compact ? this.tr("Ctx Auto", "Ctx 默认") : this.tr("Auto (default)", "默认（随模型）");
    }
    if (raw.toLowerCase() === "max") {
      return compact ? "Ctx Max" : this.tr("Max (500K / 1M)", "最大上下文 (500K / 1M)");
    }
    const pretty = raw.toUpperCase();
    return compact ? `Ctx ${pretty}` : pretty;
  }

  normalizeStrengthList(items) {
    const out = [];
    const seen = new Set();
    const list = Array.isArray(items) ? items : [items];
    for (const raw of list) {
      for (const part of String(raw ?? "").split(/[,，、;；]+/)) {
        const clean = part.trim().slice(0, 64).trim();
        if (!clean) continue;
        const key = clean.toLowerCase();
        if (!seen.has(key)) {
          seen.add(key);
          out.push(clean);
          if (out.length >= 12) return out;
        }
      }
    }
    return out;
  }

  normalizeEffortValues(rawList) {
    const set = new Set();
    for (const raw of rawList ?? []) {
      const s = String(raw ?? "").trim().toLowerCase();
      if (s === "low" || s === "minimal") set.add("low");
      else if (s === "medium" || s === "med") set.add("medium");
      else if (s === "high") set.add("high");
      else if (s === "xhigh" || s === "extra-high" || s === "extra_high" || s === "max" || s === "ultra") set.add("xhigh");
    }
    return DROVE_EFFORTS.filter((e) => set.has(e));
  }

  supportedEffortsFor(agentId, modelId) {
    const three = ["low", "medium", "high"];
    const four = ["low", "medium", "high", "xhigh"];
    const probed = this.probedAgents().find((a) => a.id === agentId);
    const rawModel = String(modelId ?? "").trim();
    const base = this.normalizeCursorBaseModel(rawModel).toLowerCase();

    // 1. Live probed per-model efforts (e.g. Cursor configOptions / Agy variants).
    const meMap = probed?.modelEfforts ?? {};
    for (const [k, vals] of Object.entries(meMap)) {
      if (k.toLowerCase() === rawModel.toLowerCase() || (base && k.toLowerCase() === base)) {
        const norm = this.normalizeEffortValues(vals);
        if (norm.length > 0) return norm;
      }
    }

    // 2. Model-family reasoning effort capability (3 levels vs 4 levels).
    if (rawModel) {
      let slug = (base || rawModel).toLowerCase();
      const slashIdx = slug.lastIndexOf("/");
      if (slashIdx >= 0) slug = slug.slice(slashIdx + 1);
      const colonIdx = slug.lastIndexOf(":");
      if (colonIdx >= 0) slug = slug.slice(colonIdx + 1);
      if (slug && slug !== "default" && slug !== "auto") {
        const supportsXhigh =
          slug === "opus" ||
          slug === "fable" ||
          slug.includes("opus") ||
          slug.includes("fable") ||
          slug.startsWith("claude-sonnet-5") ||
          slug.startsWith("gpt-6") ||
          /^gpt-5\.[2-9]/.test(slug) ||
          slug.startsWith("o3") ||
          slug.startsWith("o4") ||
          slug.startsWith("grok-4.7") ||
          slug.startsWith("grok-4.8") ||
          slug.startsWith("grok-5") ||
          slug.includes("gemini-4-neon");
        if (supportsXhigh) return four;

        const threeOnly =
          slug === "sonnet" ||
          slug === "haiku" ||
          slug.includes("haiku") ||
          slug.includes("sonnet") ||
          slug.includes("flash") ||
          slug.includes("lite") ||
          slug.includes("mini") ||
          slug.includes("nano") ||
          slug.includes("turbo") ||
          slug.includes("composer") ||
          slug.includes("gemini-2.5") ||
          slug.includes("gemini-3") ||
          slug.includes("gemini-4-carbon") ||
          slug.includes("gemini-4-argon") ||
          slug.includes("grok-4.6") ||
          slug.startsWith("gpt-4") ||
          slug.startsWith("gpt-5.0") ||
          slug.startsWith("gpt-5.1") ||
          slug.includes("glm-") ||
          slug.includes("kimi") ||
          slug.includes("qwen") ||
          slug.includes("deepseek") ||
          slug.includes("doubao");
        if (threeOnly) return three;
      }
    }

    // 3. Probed agent-level efforts fallback.
    const agentNorm = this.normalizeEffortValues(probed?.efforts ?? []);
    if (agentNorm.length > 0) return agentNorm;
    return ["claude", "codex", "cursor", "cursor-agent", "hermes", "agy", "antigravity"].includes(String(agentId ?? "").toLowerCase())
      ? four
      : three;
  }

  clampEffortFor(agentId, modelId, effort) {
    const allowed = this.supportedEffortsFor(agentId, modelId);
    const cur = effort ?? "high";
    if (allowed.includes(cur)) return cur;
    if (cur === "xhigh" && allowed.includes("high")) return "high";
    return allowed[allowed.length - 1] ?? "high";
  }

  customStrengthsMap() {
    const raw = typeof shared === "function" ? shared("drove.custom_strengths") : null;
    return raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
  }

  hiddenStrengthsMap() {
    const raw = typeof shared === "function" ? shared("drove.hidden_strengths") : null;
    return raw && typeof raw === "object" && !Array.isArray(raw) ? raw : {};
  }

  roleStrengthCatalog(roleId) {
    const pairs = DROVE_ROLE_STRENGTH_PRESETS[roleId] ?? DROVE_ROLE_STRENGTH_PRESETS.default;
    const builtins = pairs.map(([en, zh]) => this.tr(en, zh));
    const hidden = new Set((this.hiddenStrengthsMap()[roleId] ?? []).map((s) => String(s).toLowerCase()));
    const customs = Array.isArray(this.customStrengthsMap()[roleId]) ? this.customStrengthsMap()[roleId] : [];
    const seen = new Set();
    const items = [];
    for (const label of builtins) {
      if (hidden.has(label.toLowerCase())) continue;
      seen.add(label.toLowerCase());
      items.push({ label, custom: false });
    }
    for (const raw of customs) {
      const clean = String(raw ?? "").trim();
      if (!clean || seen.has(clean.toLowerCase())) continue;
      seen.add(clean.toLowerCase());
      items.push({ label: clean, custom: true });
    }
    return { items, hasHidden: hidden.size > 0 };
  }

  addStrengthToCatalog(roleId, raw) {
    if (typeof setShared !== "function") return;
    const pairs = DROVE_ROLE_STRENGTH_PRESETS[roleId] ?? DROVE_ROLE_STRENGTH_PRESETS.default;
    const builtinSet = new Set(pairs.map(([en, zh]) => this.tr(en, zh).toLowerCase()));
    const added = this.normalizeStrengthList(raw);
    if (added.length === 0) return;

    const hiddenMap = { ...this.hiddenStrengthsMap() };
    const prevHidden = Array.isArray(hiddenMap[roleId]) ? hiddenMap[roleId] : [];
    const nextHidden = prevHidden.filter((h) => !added.some((a) => a.toLowerCase() === String(h).toLowerCase()));
    if (nextHidden.length !== prevHidden.length) {
      hiddenMap[roleId] = nextHidden;
      setShared("drove.hidden_strengths", hiddenMap);
    }

    const customMap = { ...this.customStrengthsMap() };
    const prevCustom = Array.isArray(customMap[roleId]) ? [...customMap[roleId]] : [];
    let changed = false;
    for (const item of added) {
      const low = item.toLowerCase();
      if (builtinSet.has(low)) continue;
      if (!prevCustom.some((c) => String(c).toLowerCase() === low)) {
        prevCustom.push(item);
        changed = true;
      }
    }
    if (changed) {
      customMap[roleId] = prevCustom;
      setShared("drove.custom_strengths", customMap);
    }
  }

  removeStrengthFromCatalog(roleId, label) {
    if (typeof setShared !== "function") return;
    const low = String(label ?? "").toLowerCase();
    const customMap = { ...this.customStrengthsMap() };
    const prevCustom = Array.isArray(customMap[roleId]) ? customMap[roleId] : [];
    const nextCustom = prevCustom.filter((c) => String(c).toLowerCase() !== low);
    if (nextCustom.length !== prevCustom.length) {
      customMap[roleId] = nextCustom;
      setShared("drove.custom_strengths", customMap);
      return;
    }
    const hiddenMap = { ...this.hiddenStrengthsMap() };
    const prevHidden = Array.isArray(hiddenMap[roleId]) ? [...hiddenMap[roleId]] : [];
    if (!prevHidden.some((h) => String(h).toLowerCase() === low)) {
      prevHidden.push(label);
      hiddenMap[roleId] = prevHidden;
      setShared("drove.hidden_strengths", hiddenMap);
    }
  }

  resetStrengthCatalog(roleId) {
    if (typeof setShared !== "function") return;
    const hiddenMap = { ...this.hiddenStrengthsMap() };
    delete hiddenMap[roleId];
    setShared("drove.hidden_strengths", hiddenMap);
  }

  roleStrengthPresets(roleId) {
    return this.roleStrengthCatalog(roleId).items.map((it) => it.label);
  }

  roleChain(role) {
    const raw = [];
    if (role?.primary?.agent) raw.push(role.primary);
    for (const fb of role?.fallbacks ?? []) {
      if (fb?.agent) raw.push(fb);
    }
    if (raw.length === 0) {
      raw.push({ agent: "claude", model: null, effort: "high", strengths: [] });
    }
    return raw.map((item) => ({
      agent: String(item.agent).trim(),
      model: item.model ? String(item.model) : null,
      effort: item.effort ?? "high",
      ...(item.fast ? { fast: true } : {}),
      ...(item.context ? { context: String(item.context).trim() } : {}),
      strengths: this.normalizeStrengthList(item.strengths ?? []),
      ...(item.note && String(item.note).trim() ? { note: String(item.note).trim() } : {}),
    }));
  }

  applyRoleChain(role, chain) {
    const clean = (chain ?? [])
      .filter((item) => item && String(item.agent ?? "").trim())
      .map((item) => {
        const strengths = this.normalizeStrengthList(item.strengths ?? []);
        const note = String(item.note ?? "").trim();
        return {
          agent: String(item.agent).trim(),
          model: item.model ? String(item.model) : null,
          effort: item.effort ?? "high",
          ...(item.fast ? { fast: true } : {}),
          ...(item.context ? { context: String(item.context).trim() } : {}),
          ...(strengths.length > 0 ? { strengths } : {}),
          ...(note ? { note } : {}),
        };
      });
    if (clean.length === 0) return;
    role.primary = clean[0];
    role.fallbacks = clean.slice(1);
  }

  droveStrengthPicker(t, id, roleId, idx, currentStrengths, onChange, cx) {
    const key = `drove-str:${id}`;
    const open = this.picking === key;
    const aiGenerating = this.droveStrengthAiTarget === `candidate:${roleId}:${idx}`;
    const aiBusy = Boolean(this.droveStrengthAiTarget || this.droveNoteAiTarget || this.droveAiConfiguring);
    const { items: catalogItems, hasHidden } = this.roleStrengthCatalog(roleId);
    const hasTag = (tag) => currentStrengths.some((s) => s.toLowerCase() === tag.toLowerCase());
    const toggleTag = (tag, cx) => {
      if (hasTag(tag)) {
        onChange(currentStrengths.filter((s) => s.toLowerCase() !== tag.toLowerCase()), cx);
      } else {
        onChange(this.normalizeStrengthList([...currentStrengths, tag]), cx);
      }
    };
    const addRaw = (raw, cx) => {
      this.addStrengthToCatalog(roleId, raw);
      const next = this.normalizeStrengthList([...currentStrengths, raw]);
      onChange(next, cx);
    };

    const rawQ = open ? (this.droveStrengthFilter ?? "") : "";
    const qTrim = rawQ.trim();
    const qLower = qTrim.toLowerCase();
    const filteredCatalog = qLower
      ? catalogItems.filter((it) => it.label.toLowerCase().includes(qLower))
      : catalogItems;

    const trigger = div()
      .id(`st-dr-str-add-${id}`)
      .px(6)
      .py(1)
      .rounded(4)
      .border_1()
      .border_color(open ? t.accent : t.border)
      .bg(open ? t.raised : t.background)
      .cursor_pointer()
      .text_size(11)
      .text_color(open ? t.accent : t.muted)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .tooltip(this.tr("Add or remove work types this agent+model excels at", "手动添加或移除此 Agent+模型 擅长的工作类型（编排器会据此选人）"))
      .on_click((_e, cx) => {
        this.picking = open ? null : key;
        if (this.picking) {
          this.droveStrengthFilter = "";
          this.droveStrengthInput.set_value("");
          this.droveStrengthSubmit = addRaw;
          this.droveStrengthInput.focus();
        } else {
          this.droveStrengthSubmit = null;
          this.handle.focus();
        }
        cx.notify();
      })
      .child(currentStrengths.length === 0 ? this.tr("+ Set strengths", "+ 设置擅长任务") : this.tr("+ Manage", "+ 添加 / 管理"));

    return Popup.new(`st-dr-strpp-${id}`, trigger)
      .anchor("bottom_left")
      .flex()
      .flex_none()
      .when(open, (el) =>
        el.content(
          v_flex()
            .w(360)
            .rounded(8)
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .on_mouse_down_out((_e, cx) => {
              this.picking = null;
              this.droveStrengthSubmit = null;
              this.handle.focus();
              cx.notify();
            })
            .on_scroll_wheel((_e, cx) => cx.stop_propagation())
            .child(
              h_flex()
                .px(8)
                .py(7)
                .gap(8)
                .items_center()
                .justify_between()
                .border_b(1)
                .border_color(t.border)
                .child(
                  div()
                    .text_size(11.5)
                    .font_semibold()
                    .text_color(t.foreground)
                    .child(this.tr("Candidate Strengths", "擅长任务管理")),
                )
                .child(
                  h_flex()
                    .id(`st-dr-str-ai-${id}`)
                    .flex_none()
                    .px(8)
                    .py(3)
                    .gap(4)
                    .rounded(5)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.raised)
                    .items_center()
                    .text_size(11)
                    .text_color(t.foreground)
                    .tooltip(
                      this.tr(
                        "Use AI to automatically configure task strengths for this Agent + Model (optionally type hints below first)",
                        "调用 AI 为当前 Agent + 模型自动配置擅长任务（可先在下方输入补充要求）",
                      ),
                    )
                    .when(aiBusy, (b) => b.opacity(0.6))
                    .when(!aiBusy, (b) =>
                      b
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover))
                        .on_click((_e, cx) => this.aiConfigureCandidateStrengths(roleId, idx, cx)),
                    )
                    .when(aiGenerating, (b) =>
                      b.child(svg("mods/workspace-chrome/spinner.svg").size(11).text_color(t.accent).spin(900)),
                    )
                    .child(aiGenerating ? this.tr("Configuring…", "配置中…") : this.tr("✨ AI Configure", "✨ AI 配置")),
                ),
            )
            .child(
              h_flex()
                .px(8)
                .py(7)
                .gap(6)
                .items_center()
                .border_b(1)
                .border_color(t.border)
                .child(div().flex_1().min_w(0).text_size(12).child(Input.new(this.droveStrengthInput).h(24).text_color(t.foreground)))
                .child(
                  div()
                    .id(`st-dr-str-addbtn-${id}`)
                    .flex_none()
                    .px(8)
                    .py(3)
                    .rounded(5)
                    .bg(qTrim ? t.accent : t.raised)
                    .text_size(11.5)
                    .text_color(qTrim ? t.accent_foreground : t.dim)
                    .when(Boolean(qTrim), (b) =>
                      b
                        .cursor_pointer()
                        .on_click((_e, cx) => {
                          addRaw(qTrim, cx);
                          this.droveStrengthFilter = "";
                          this.droveStrengthInput.set_value("");
                          cx.notify();
                        }),
                    )
                    .child(this.tr("Add", "添加")),
                ),
            )
            .child(
              v_flex()
                .p(8)
                .gap(7)
                .when(currentStrengths.length > 0, (box) =>
                  box
                    .child(div().text_size(11).text_color(t.dim).child(this.tr("Assigned (click × to remove)", "已选擅长任务（点击 × 移除）")))
                    .child(
                      h_flex()
                        .flex_wrap()
                        .gap(4)
                        .children(
                          currentStrengths.map((tag, si) =>
                            h_flex()
                              .id(`st-dr-str-sel-${id}-${si}`)
                              .px(6)
                              .py(2)
                              .gap(4)
                              .rounded(5)
                              .border_1()
                              .border_color(t.accent)
                              .bg(t.raised)
                              .items_center()
                              .child(div().text_size(11).text_color(t.foreground).child(tag))
                              .child(
                                div()
                                  .id(`st-dr-str-selrm-${id}-${si}`)
                                  .cursor_pointer()
                                  .text_size(11)
                                  .text_color(t.muted)
                                  .hover((s) => s.text_color(t.blocked))
                                  .on_click((_e, cx) =>
                                    onChange(currentStrengths.filter((_, i) => i !== si), cx),
                                  )
                                  .child("×"),
                              ),
                          ),
                        ),
                    ),
                )
                .child(
                  h_flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(11).text_color(t.dim).child(this.tr("Task types (click to toggle, × to remove)", "任务类型列表（点击切换，× 可移除类型）")))
                    .when(hasHidden, (row) =>
                      row.child(
                        div()
                          .id(`st-dr-str-reset-${id}`)
                          .cursor_pointer()
                          .text_size(10.5)
                          .text_color(t.accent)
                          .on_click((_e, cx) => {
                            this.resetStrengthCatalog(roleId);
                            cx.notify();
                          })
                          .child(this.tr("Reset", "恢复默认")),
                      ),
                    ),
                )
                .child(
                  h_flex()
                    .flex_wrap()
                    .gap(5)
                    .when(filteredCatalog.length === 0, (box) =>
                      box.child(div().text_size(11.5).text_color(t.dim).child(this.tr("No matching task types; type above and click Add", "无匹配项，可在上方输入后点击「添加」"))),
                    )
                    .children(
                      filteredCatalog.map((item, pi) => {
                        const preset = item.label;
                        const active = hasTag(preset);
                        return h_flex()
                          .id(`st-dr-str-pre-${id}-${pi}`)
                          .px(6)
                          .py(3)
                          .gap(5)
                          .rounded(5)
                          .border_1()
                          .border_color(active ? t.accent : t.border)
                          .bg(active ? t.accent : t.raised)
                          .items_center()
                          .child(
                            div()
                              .id(`st-dr-str-pretog-${id}-${pi}`)
                              .cursor_pointer()
                              .text_size(11.5)
                              .text_color(active ? t.accent_foreground : t.foreground)
                              .on_click((_e, cx) => toggleTag(preset, cx))
                              .child(active ? `✓ ${preset}` : preset),
                          )
                          .child(
                            div()
                              .id(`st-dr-str-predel-${id}-${pi}`)
                              .cursor_pointer()
                              .text_size(10.5)
                              .text_color(active ? t.accent_foreground : t.dim)
                              .opacity(0.75)
                              .hover((s) => s.opacity(1.0).text_color(t.blocked))
                              .tooltip(this.tr("Remove this task type from list", "从任务类型列表中移除"))
                              .on_click((_e, cx) => {
                                this.removeStrengthFromCatalog(roleId, preset);
                                if (active) {
                                  onChange(currentStrengths.filter((s) => s.toLowerCase() !== preset.toLowerCase()), cx);
                                } else {
                                  cx.notify();
                                }
                              })
                              .child("×"),
                          );
                      }),
                    ),
                ),
            ),
        ),
      );
  }

  droveCandidateNotePicker(t, id, roleId, idx, currentNote, onChange, cx) {
    const key = `drove-note:${id}`;
    const open = this.picking === key;
    const saved = String(currentNote ?? "").trim();
    const preview = saved.length > 42 ? `${saved.slice(0, 42)}…` : saved;
    const aiGenerating = this.droveNoteAiTarget === `candidate:${roleId}:${idx}`;
    const aiBusy = Boolean(this.droveNoteAiTarget || this.droveStrengthAiTarget || this.droveAiConfiguring);

    const trigger = h_flex()
      .id(`st-dr-cnote-btn-${id}`)
      .px(6)
      .py(1)
      .gap(4)
      .rounded(4)
      .border_1()
      .border_color(open ? t.accent : t.border)
      .bg(open || saved ? t.raised : t.background)
      .items_center()
      .cursor_pointer()
      .text_size(11)
      .text_color(open ? t.accent : (saved ? t.foreground : t.muted))
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .tooltip(
        saved
          ? `${this.tr("Candidate note (click to edit):", "候选备注（点击编辑）：")}\n${saved}`
          : this.tr("Add a routing note for this Agent + Model (read by AI when launching sub-agents)", "为此 Agent + 模型添加备注（供 AI 选择/启动子 Agent 时参考）"),
      )
      .on_click((_e, cx) => {
        this.picking = open ? null : key;
        if (this.picking) {
          this.droveCandNoteInput.set_value(saved);
          this.droveCandNoteSubmit = onChange;
          this.droveCandNoteInput.focus();
        } else {
          this.droveCandNoteSubmit = null;
          this.handle.focus();
        }
        cx.notify();
      })
      .child(saved ? `${this.tr("Note:", "备注：")} ${preview}` : this.tr("+ Note", "+ 备注"));

    return Popup.new(`st-dr-cnotepp-${id}`, trigger)
      .anchor("bottom_left")
      .flex()
      .flex_none()
      .when(open, (el) =>
        el.content(
          v_flex()
            .w(360)
            .p(10)
            .gap(8)
            .rounded(8)
            .border_1()
            .border_color(t.border)
            .bg(t.background)
            .on_mouse_down_out((_e, cx) => {
              this.picking = null;
              this.droveCandNoteSubmit = null;
              this.handle.focus();
              cx.notify();
            })
            .on_scroll_wheel((_e, cx) => cx.stop_propagation())
            .child(
              div()
                .text_size(11.5)
                .font_semibold()
                .text_color(t.foreground)
                .child(this.tr("Candidate Note (for AI sub-agent routing)", "候选备注（供 AI 启动子 Agent 参考）")),
            )
            .child(
              div()
                .w_full()
                .px(8)
                .py(6)
                .rounded(6)
                .border_1()
                .border_color(t.border)
                .bg(t.surface)
                .child(
                  Textarea.new(this.droveCandNoteInput)
                    .id(`st-dr-cnote-ta-${id}`)
                    .w_full()
                    .min_h(54)
                    .text_size(12)
                    .text_color(t.foreground),
                )
                .when(!String(this.droveCandNoteInput?.value?.() ?? "").trim(), (ph) =>
                  ph.child(
                    div()
                      .pt(3)
                      .text_size(11)
                      .text_color(t.dim)
                      .child(
                        this.tr(
                          "e.g. Costs 2x quota during 2:00-6:00 PM; avoid unless no other agent is available.",
                          "例如：下午 2:00–6:00 额度按两倍计算，应避免使用，除非没有其他 agent 可用。",
                        ),
                      ),
                  ),
                ),
            )
            .child(
              h_flex()
                .items_center()
                .justify_between()
                .gap(6)
                .child(div().text_size(10.5).text_color(t.dim).child(this.tr("Ctrl+Enter to save", "Ctrl+Enter 快速保存")))
                .child(
                  h_flex()
                    .gap(6)
                    .items_center()
                    .child(
                      h_flex()
                        .id(`st-dr-cnote-ai-${id}`)
                        .px(8)
                        .py(3)
                        .gap(4)
                        .rounded(5)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.raised)
                        .items_center()
                        .text_size(11)
                        .text_color(t.foreground)
                        .tooltip(
                          this.tr(
                            "Generate or refine a concise routing note for this candidate with AI and fill it into the editor",
                            "调用 AI 为此候选生成或润色路由备注，并填入当前输入框",
                          ),
                        )
                        .when(aiBusy, (el) => el.opacity(0.6))
                        .when(!aiBusy, (el) =>
                          el
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover))
                            .on_click((_e, cx) => this.aiGenerateDroveNote("candidate", roleId, idx, cx)),
                        )
                        .when(aiGenerating, (el) =>
                          el.child(svg("mods/workspace-chrome/spinner.svg").size(11).text_color(t.accent).spin(900)),
                        )
                        .child(aiGenerating ? this.tr("Generating…", "生成中…") : this.tr("✨ AI Note", "✨ AI 备注")),
                    )
                    .when(Boolean(saved), (row) =>
                      row.child(
                        div()
                          .id(`st-dr-cnote-clr-${id}`)
                          .px(8)
                          .py(3)
                          .rounded(5)
                          .border_1()
                          .border_color(t.border)
                          .bg(t.raised)
                          .cursor_pointer()
                          .text_size(11)
                          .text_color(t.muted)
                          .hover((s) => s.bg(t.hover).text_color(t.blocked))
                          .on_click((_e, cx) => {
                            this.picking = null;
                            this.droveCandNoteSubmit = null;
                            this.handle.focus();
                            onChange(null, cx);
                          })
                          .child(this.tr("Clear", "清空")),
                      ),
                    )
                    .child(
                      div()
                        .id(`st-dr-cnote-save-${id}`)
                        .px(10)
                        .py(3)
                        .rounded(5)
                        .bg(t.accent)
                        .cursor_pointer()
                        .text_size(11)
                        .font_semibold()
                        .text_color(t.accent_foreground)
                        .on_click((_e, cx) => {
                          const val = this.droveCandNoteInput.value().trim();
                          this.picking = null;
                          this.droveCandNoteSubmit = null;
                          this.handle.focus();
                          onChange(val || null, cx);
                        })
                        .child(this.tr("Save", "保存")),
                    ),
                ),
            ),
        ),
      );
  }

  nextCandidateAgent(chain) {
    const probed = this.probedAgents();
    const used = new Set((chain ?? []).map((c) => c.agent));
    const unusedInstalled = probed.find((a) => a.installed && !used.has(a.id));
    if (unusedInstalled) return unusedInstalled.id;
    const unusedAny = probed.find((a) => !used.has(a.id));
    if (unusedAny) return unusedAny.id;
    const last = chain?.[chain.length - 1]?.agent;
    return probed.find((a) => a.id !== last)?.id ?? "codex";
  }

  droveUpdateRole(roleId, updater, cx) {
    const roles = JSON.parse(JSON.stringify(this.droveData?.config?.roles ?? {}));
    const current = roles[roleId];
    if (!current) return;
    updater(current, roles);
    this.saveDrovePatch({ roles }, cx);
  }

  droveRoleCard(t, roleId, role, cx) {
    const builtin = DROVE_BUILTIN_ROLES.includes(roleId);
    const chain = this.roleChain(role);
    const probed = this.probedAgents();
    const access = role.access ?? "writable";
    const accessLabel = access === "readonly"
      ? this.tr("Read-only", "只读")
      : access === "scoped_write"
        ? this.tr("Scoped", "受限写")
        : this.tr("Writable", "可写");
    const accessColor = access === "readonly" ? t.dim : t.ansi2;

    const commitChain = (nextChain, cx) => {
      this.droveUpdateRole(roleId, (r) => this.applyRoleChain(r, nextChain), cx);
    };

    const candidateRow = (target, idx) => {
      const isPrimary = idx === 0;
      const curAgentInfo = probed.find((a) => a.id === target.agent);
      const agentItems = probed.map((a) => ({
        value: a.id,
        label: a.name && a.name !== a.id ? `${a.id} · ${a.name}` : a.id,
        icon: a.id,
        dot: Boolean(a.installed),
        meta: !a.installed ? this.tr("missing", "未安装") : a.models?.length ? `${a.models.length} ${this.tr("models", "模型")}` : "✓",
        active: target.agent === a.id,
      }));
      const models = this.probedModelsFor(target.agent);
      const modelItems = [
        { value: null, label: this.tr("Auto (agent default)", "自动（Agent 默认）"), active: !target.model },
        ...models.map((m) => ({
          value: m.id,
          label: m.label,
          active: target.model === m.id,
        })),
      ];
      if (target.model && !models.some((m) => m.id === target.model)) {
        modelItems.splice(1, 0, { value: target.model, label: target.model, meta: this.tr("custom", "自定义"), active: true });
      }
      const canUp = idx > 0;
      const canDown = idx < chain.length - 1;
      const canRemove = chain.length > 1;
      const stepBtn = (btnId, glyph, enabled, title, onClick, hoverColor = t.foreground) =>
        div()
          .id(btnId)
          .w(20)
          .h(26)
          .flex_none()
          .flex()
          .items_center()
          .justify_center()
          .rounded(5)
          .text_size(12)
          .text_color(enabled ? t.muted : t.dim)
          .opacity(enabled ? 1.0 : 0.35)
          .when(enabled, (el) =>
            el
              .cursor_pointer()
              .tooltip(title)
              .hover((s) => s.bg(t.hover).text_color(hoverColor))
              .on_click(onClick),
          )
          .child(glyph);

      const strengths = Array.isArray(target.strengths) ? target.strengths : [];
      const setStrengths = (nextStrengths, cx) => {
        const next = chain.map((c, i) => (i === idx ? { ...c, strengths: nextStrengths } : c));
        commitChain(next, cx);
      };
      const candNote = String(target.note ?? "").trim();
      const setCandNote = (nextNote, cx) => {
        const clean = String(nextNote ?? "").trim();
        const next = chain.map((c, i) => {
          if (i !== idx) return c;
          const copy = { ...c };
          if (clean) copy.note = clean;
          else delete copy.note;
          return copy;
        });
        commitChain(next, cx);
      };

      const mainRow = h_flex()
        .id(`st-dr-cand-${roleId}-${idx}`)
        .w_full()
        .gap(4)
        .items_center()
        .child(
          div()
            .w(54)
            .min_w(54)
            .max_w(54)
            .h(28)
            .flex_none()
            .px(4)
            .rounded(5)
            .bg(isPrimary ? t.raised : t.background)
            .border_1()
            .border_color(t.border)
            .flex()
            .items_center()
            .justify_center()
            .text_size(11)
            .when(isPrimary, (el) => el.font_semibold())
            .text_color(isPrimary ? t.accent : t.dim)
            .child(isPrimary ? this.tr("#1 Main", "#1 主选") : this.tr(`#${idx + 1}`, `#${idx + 1} 备`)),
        )
        .child(
          this.drovePicker(
            t,
            `${roleId}-c${idx}-agent`,
            target.agent,
            agentItems,
            (agent, cx) => {
              const next = chain.map((c, i) =>
                i === idx
                  ? {
                      ...c,
                      agent,
                      model: null,
                      effort: this.clampEffortFor(agent, null, c.effort),
                      ...(agent === "codex" ? {} : { fast: false }),
                      ...(agent === "cursor" || agent === "cursor-agent" ? {} : { context: null }),
                    }
                  : c,
              );
              commitChain(next, cx);
            },
            { width: 118, popupWidth: 280, icon: target.agent, dot: curAgentInfo ? Boolean(curAgentInfo.installed) : undefined },
            cx,
          ),
        )
        .child(
          this.drovePicker(
            t,
            `${roleId}-c${idx}-model`,
            target.model || this.tr("Auto (default)", "自动（默认）"),
            modelItems,
            (model, cx) => {
              const next = chain.map((c, i) =>
                i === idx
                  ? { ...c, model, effort: this.clampEffortFor(c.agent, model, c.effort) }
                  : c,
              );
              commitChain(next, cx);
            },
            { flex1: true, width: 115, popupWidth: 380, allowCustom: true },
            cx,
          ),
        )
        .child(
          (() => {
            const isAgy = target.agent === "agy" || target.agent === "antigravity";
            const isCursor = target.agent === "cursor" || target.agent === "cursor-agent";
            const cursorHasEffort = isCursor && this.modelHasEmbeddedEffort(target.model);
            const disableEffort = isAgy || cursorHasEffort;
            const efforts = this.supportedEffortsFor(target.agent, target.model);
            const curEff = this.clampEffortFor(target.agent, target.model, target.effort ?? "high");
            const disabledEffortTip = isAgy
              ? this.tr("Antigravity model names already include effort", "Antigravity 模型名已自带思考强度，不可单独选择")
              : this.tr("Selected Cursor model already includes reasoning effort", "当前选中的 Cursor 模型已自带思考强度，不可单独选择");
            return h_flex()
              .id(`st-dr-eff-group-${roleId}-${idx}`)
              .w(128)
              .min_w(128)
              .max_w(128)
              .h(28)
              .flex_none()
              .p(2)
              .gap(1)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(t.border)
              .bg(t.background)
              .when(disableEffort, (el) =>
                el
                  .opacity(0.35)
                  .tooltip(disabledEffortTip),
              )
              .children(
                efforts.map((eff) => {
                  const active = !disableEffort && curEff === eff;
                  return div()
                    .id(`st-dr-eff-${roleId}-${idx}-${eff}`)
                    .flex_1()
                    .min_w(0)
                    .h(22)
                    .px(2)
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(5)
                    .bg(active ? t.accent : "#00000000")
                    .text_size(11)
                    .when(active, (el) => el.font_semibold())
                    .text_color(disableEffort ? t.dim : (active ? t.accent_foreground : t.muted))
                    .when(!disableEffort, (el) =>
                      el
                        .cursor_pointer()
                        .when(!active, (el2) => el2.hover((s) => s.bg(t.hover).text_color(t.foreground)))
                        .on_click((_e, cx) => {
                          const next = chain.map((c, i) => (i === idx ? { ...c, effort: eff } : c));
                          commitChain(next, cx);
                        }),
                    )
                    .child(this.effortLabel(eff));
                }),
              );
          })(),
        )
        .child(
          (() => {
            const isCursor = target.agent === "cursor" || target.agent === "cursor-agent";
            if (isCursor) {
              const rawCtxs = this.cursorContextChoicesFor(target.model);
              const curCtx = target.context ? String(target.context).trim() : null;
              const ctxItems = [
                { value: null, label: this.formatContextLabel(null, false), meta: "auto", active: !curCtx },
                { value: "max", label: this.formatContextLabel("max", false), meta: this.tr("largest", "最大"), active: curCtx?.toLowerCase() === "max" },
                ...rawCtxs.map((v) => ({
                  value: v,
                  label: this.formatContextLabel(v, false),
                  meta: v,
                  active: curCtx?.toLowerCase() === v.toLowerCase(),
                })),
              ];
              if (curCtx && curCtx.toLowerCase() !== "max" && !rawCtxs.some((v) => v.toLowerCase() === curCtx.toLowerCase())) {
                ctxItems.splice(2, 0, { value: curCtx, label: this.formatContextLabel(curCtx, false), meta: this.tr("custom", "自定义"), active: true });
              }
              return this.drovePicker(
                t,
                `${roleId}-c${idx}-ctx`,
                this.formatContextLabel(curCtx, true),
                ctxItems,
                (context, cx) => {
                  const cleanCtx = context && String(context).trim() && String(context).trim().toLowerCase() !== "auto"
                    ? String(context).trim()
                    : null;
                  const next = chain.map((c, i) => (i === idx ? { ...c, context: cleanCtx } : c));
                  commitChain(next, cx);
                },
                { width: 86, popupWidth: 230, anchor: "bottom_right", allowCustom: true },
                cx,
              );
            }
            const canFast = target.agent === "codex";
            const fastActive = canFast && Boolean(target.fast);
            return h_flex()
              .id(`st-dr-fast-${roleId}-${idx}`)
              .w(86)
              .h(28)
              .flex_none()
              .px(4)
              .rounded(7)
              .border_1()
              .border_color(fastActive ? t.accent : t.border)
              .bg(fastActive ? t.accent : t.background)
              .items_center()
              .justify_center()
              .text_size(11)
              .when(fastActive, (el) => el.font_semibold())
              .text_color(fastActive ? t.accent_foreground : (canFast ? t.muted : t.dim))
              .when(!canFast, (el) => el.opacity(0.32).tooltip(this.tr("Fast mode is only supported for Codex", "Fast 加速模式仅支持 Codex")))
              .when(canFast, (el) =>
                el
                  .cursor_pointer()
                  .when(!fastActive, (s) => s.hover((h) => h.bg(t.hover).text_color(t.foreground)))
                  .tooltip(this.tr("Codex Fast mode (1.5x speed, increased usage)", "Codex Fast 加速模式（约 1.5x 速度，消耗更多额度）"))
                  .on_click((_e, cx) => {
                    const next = chain.map((c, i) => (i === idx ? { ...c, fast: !target.fast } : c));
                    commitChain(next, cx);
                  }),
              )
              .child("⚡ Fast");
          })(),
        )
        .child(
          h_flex()
            .flex_none()
            .gap(1)
            .items_center()
            .child(stepBtn(`st-dr-up-${roleId}-${idx}`, "↑", canUp, this.tr("Move up", "上移"), (_e, cx) => {
              const next = [...chain];
              [next[idx - 1], next[idx]] = [next[idx], next[idx - 1]];
              commitChain(next, cx);
            }))
            .child(stepBtn(`st-dr-dn-${roleId}-${idx}`, "↓", canDown, this.tr("Move down", "下移"), (_e, cx) => {
              const next = [...chain];
              [next[idx], next[idx + 1]] = [next[idx + 1], next[idx]];
              commitChain(next, cx);
            }))
            .child(stepBtn(`st-dr-rm-${roleId}-${idx}`, "×", canRemove, this.tr("Remove", "移除"), (_e, cx) => {
              commitChain(chain.filter((_, i) => i !== idx), cx);
            }, t.blocked)),
        );

      const strengthsRow = h_flex()
        .id(`st-dr-cand-str-${roleId}-${idx}`)
        .w_full()
        .pl(58)
        .gap(4)
        .items_center()
        .flex_wrap()
        .child(div().text_size(11).text_color(t.dim).child(this.tr("Best for:", "擅长任务：")))
        .children(
          strengths.map((tag, si) =>
            h_flex()
              .id(`st-dr-str-chip-${roleId}-${idx}-${si}`)
              .px(6)
              .py(1)
              .gap(4)
              .rounded(4)
              .border_1()
              .border_color(t.border)
              .bg(t.raised)
              .items_center()
              .child(div().text_size(11).text_color(t.foreground).child(tag))
              .child(
                div()
                  .id(`st-dr-str-rm-${roleId}-${idx}-${si}`)
                  .cursor_pointer()
                  .text_size(11)
                  .text_color(t.dim)
                  .hover((s) => s.text_color(t.blocked))
                  .on_click((_e, cx) => setStrengths(strengths.filter((_, i) => i !== si), cx))
                  .child("×"),
              ),
          ),
        )
        .child(this.droveStrengthPicker(t, `${roleId}-c${idx}`, roleId, idx, strengths, setStrengths, cx))
        .child(this.droveCandidateNotePicker(t, `${roleId}-c${idx}`, roleId, idx, candNote, setCandNote, cx));

      return v_flex()
        .id(`st-dr-cand-wrap-${roleId}-${idx}`)
        .w_full()
        .p(8)
        .gap(6)
        .rounded(8)
        .border_1()
        .border_color(t.border)
        .bg(t.background)
        .child(mainRow)
        .child(strengthsRow);
    };

    return v_flex()
      .id(`st-dr-role-${roleId}`)
      .rounded(10)
      .border_1()
      .border_color(t.border)
      .bg(t.surface)
      .overflow_hidden()
      .child(
        v_flex()
          .px(14)
          .py(10)
          .gap(4)
          .border_b(1)
          .border_color(t.border)
          .bg(t.background)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                h_flex()
                  .gap(7)
                  .items_center()
                  .min_w(0)
                  .child(div().text_size(13.5).font_semibold().text_color(t.foreground).child(this.roleDisplayTitle(roleId, role)))
                  .child(div().px(6).py(1).rounded(4).bg(t.raised).font_family(DEFAULT_FONT).text_size(11).text_color(t.dim).child(roleId))
                  .child(div().px(6).py(1).rounded(4).bg(t.raised).text_size(11).text_color(accessColor).child(accessLabel)),
              )
              .child(
                h_flex()
                  .flex_none()
                  .gap(8)
                  .items_center()
                  .child(div().text_size(11.5).text_color(t.dim).child(this.tr(`${chain.length} candidate(s)`, `${chain.length} 个候选`)))
                  .when(!builtin, (el) =>
                    el.child(
                      div()
                        .id(`st-dr-del-${roleId}`)
                        .flex_none()
                        .px(7)
                        .py(2)
                        .rounded(5)
                        .cursor_pointer()
                        .text_size(11.5)
                        .text_color(t.dim)
                        .hover((s) => s.bg(t.hover).text_color(t.blocked))
                        .on_click((_e, cx) => {
                          this.droveUpdateRole(roleId, (_cur, all) => { delete all[roleId]; }, cx);
                        })
                        .child(this.tr("Remove", "删除")),
                    ),
                  ),
              ),
          )
          .when(Boolean(role.summary), (el) => {
            const summaryText = this.tr(DROVE_DEFAULT_ROLE_SUMMARIES_EN[role.summary] ?? role.summary, role.summary);
            return el.child(div().text_size(11.5).text_color(t.dim).child(summaryText));
          }),
      )
      .child(
        v_flex()
          .p(10)
          .gap(8)
          .children(chain.map((target, idx) => candidateRow(target, idx))),
      )
      .child(
        h_flex()
          .px(12)
          .py(6)
          .border_t(1)
          .border_color(t.border)
          .bg(t.background)
          .items_center()
          .child(
            div()
              .id(`st-dr-fb-add-${roleId}`)
              .cursor_pointer()
              .px(7)
              .py(2)
              .rounded(5)
              .text_size(11.5)
              .text_color(t.muted)
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, cx) =>
                commitChain(
                  [...chain, { agent: this.nextCandidateAgent(chain), model: null, effort: chain[0]?.effort ?? "high", strengths: [] }],
                  cx,
                ),
              )
              .child(this.tr("+ Add candidate agent", "+ 添加候选 Agent")),
          ),
      );
  }

  droveCardRow(t, title, desc, control, first = false) {
    return h_flex()
      .w_full()
      .px(14)
      .py(11)
      .gap(16)
      .items_center()
      .justify_between()
      .when(!first, (el) => el.border_t(1).border_color(t.border))
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(div().text_size(13).text_color(t.foreground).child(title))
          .when(Boolean(desc), (el) =>
            el.child(div().text_size(11.5).line_height("16px").text_color(t.dim).child(desc)),
          ),
      )
      .child(div().flex_none().child(control));
  }

  ai(t, cx) {
    const machines = (this.machineList ?? []).filter((m) => !m.temporary);
    const st = this.aiStatus ?? {};
    const switches = st.switches ?? {};
    const usage = st.usage ?? {};
    const aiOn = st.enabled !== false;
    const ptyArb = switches.ptyStatusArbitration !== false;
    const notesDigest = switches.notesDigest !== false;
    const promptCache = switches.promptCacheEnabled !== false;
    const promptModeChoices = ["shared_prefix", "compact", "auto"];
    const rawPromptMode = String(switches.promptMode ?? (promptCache ? "auto" : "compact")).trim().toLowerCase();
    const promptMode = promptModeChoices.includes(rawPromptMode) ? rawPromptMode : (promptCache ? "auto" : "compact");
    const detectedPromptMode = usage.promptCacheDetectedMode ?? (usage.promptCacheAutoDegraded ? "compact" : null);
    const probeMisses = Number(usage.promptCacheProbeMisses ?? 0);
    const probeThreshold = Number(usage.promptCacheProbeThreshold ?? 5) || 5;
    const handoffDistill = switches.handoffDistill !== false;
    const checkpointLabeling = switches.checkpointLabeling !== false;
    const semanticSummary = switches.semanticSummary !== false;
    const redactSecrets = switches.redactSecrets !== false;
    const aiModels = this.aiAvailableModels();
    const currentAiModel = (st.model ?? "").trim();
    const aiModelItems = [
      { value: "", label: this.tr("Auto (first available model)", "自动选择（首个可用模型）"), active: !currentAiModel },
      ...aiModels.map((m) => ({ value: m, label: m, active: currentAiModel === m })),
    ];
    if (currentAiModel && !aiModels.includes(currentAiModel)) {
      aiModelItems.splice(1, 0, { value: currentAiModel, label: currentAiModel, meta: this.tr("custom", "自定义"), active: true });
    }
    const aiEffortChoices = ["auto", "none", "low", "medium", "high"];
    const rawAiEffort = (st.effort ?? "low").trim().toLowerCase() || "low";
    const currentAiEffort = aiEffortChoices.includes(rawAiEffort) ? rawAiEffort : "low";
    const currentAiContext = this.normalizeAiContextInput(st.contextWindow ?? "");
    const currentAiMaxTokens = Number(st.maxTokens ?? 0) || 0;
    const aiContextPresets = ["8k", "16k", "32k", "64k", "128k", "256k", "1m"];
    const aiContextItems = [
      { value: "", label: this.tr("Auto (model default)", "默认（随模型）"), active: !currentAiContext },
      ...aiContextPresets.map((v) => ({ value: v, label: v.toUpperCase(), active: currentAiContext === v })),
    ];
    if (currentAiContext && !aiContextPresets.includes(currentAiContext)) {
      aiContextItems.splice(1, 0, {
        value: currentAiContext,
        label: currentAiContext.toUpperCase(),
        meta: this.tr("custom", "自定义"),
        active: true,
      });
    }
    const aiMaxTokenPresets = [512, 1024, 2048, 4096, 8192, 16384];
    const aiMaxTokenItems = [
      { value: 0, label: this.tr("Auto (by tier/effort)", "自动（按任务分级）"), active: !currentAiMaxTokens },
      ...aiMaxTokenPresets.map((v) => ({ value: v, label: `${v} tokens`, active: currentAiMaxTokens === v })),
    ];
    if (currentAiMaxTokens && !aiMaxTokenPresets.includes(currentAiMaxTokens)) {
      aiMaxTokenItems.splice(1, 0, {
        value: currentAiMaxTokens,
        label: `${currentAiMaxTokens} tokens`,
        meta: this.tr("custom", "自定义"),
        active: true,
      });
    }
    const machineChip = (id, label, value) => {
      const on = this.droveMachine === value;
      return div()
        .id(`st-ai-m-${id}`)
        .px(10)
        .py(4)
        .rounded(6)
        .border_1()
        .border_color(on ? t.accent : t.border)
        .bg(on ? t.accent : t.raised)
        .cursor_pointer()
        .text_size(12.5)
        .text_color(on ? t.accent_foreground : t.foreground)
        .on_click((_e, cx) => {
          this.droveMachine = value;
          this.aiStatus = null;
          this.aiTestResult = null;
          this.aiPromptProbeResult = null;
          this.droveData = null;
          this.picking = null;
          cx.spawn(async (cx) => {
            await this.loadAi();
            cx.notify();
          });
        })
        .child(label);
    };
    const totalCalls = Number(usage.totalCalls ?? 0);
    const localHits = Number(usage.localCacheHits ?? 0);
    const sessionsReused = Number(usage.sessionsReused ?? 0);
    const activeSessions = Number(usage.activeSessions ?? 0);
    const inTokens = Number(usage.inputTokens ?? 0);
    const cachedTokens = Number(usage.cachedInputTokens ?? 0);
    const outTokens = Number(usage.outputTokens ?? 0);
    const hitPct = `${Math.round(Number(usage.cacheHitRatio ?? 0) * 1000) / 10}%`;
    const cacheAutoDegraded = Boolean(usage.promptCacheAutoDegraded);
    const opMetrics = usage.opMetrics && typeof usage.opMetrics === "object" ? usage.opMetrics : {};
    const opSummaryItems = Object.entries(opMetrics)
      .map(([op, m]) => {
        const calls = Number(m?.calls ?? 0);
        const hits = Number(m?.localCacheHits ?? 0);
        const fails = Number(m?.failures ?? 0);
        const parseErrs = Number(m?.parseErrors ?? 0);
        const lowConf = Number(m?.lowConfidenceRejects ?? 0);
        const avgMs = calls > 0 ? Math.round(Number(m?.totalLatencyMs ?? 0) / calls) : 0;
        return { op, calls, hits, fails, parseErrs, lowConf, avgMs, weight: calls + hits + fails };
      })
      .filter((x) => x.weight > 0)
      .sort((a, b) => b.weight - a.weight)
      .slice(0, 6);
    const statPill = (label, value, accent = false) =>
      v_flex()
        .flex_1()
        .min_w(110)
        .px(11)
        .py(8)
        .rounded(8)
        .border_1()
        .border_color(t.border)
        .bg(t.background)
        .gap(2)
        .child(div().text_size(11).text_color(t.dim).child(label))
        .child(div().text_size(14).font_semibold().text_color(accent ? t.accent : t.foreground).child(String(value)));

    return v_flex()
      .w_full()
      .gap(16)
      // 0. Target Machine Bar
      .child(
        h_flex()
          .w_full()
          .px(14)
          .py(10)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.surface)
          .items_center()
          .justify_between()
          .gap(10)
          .flex_wrap()
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .flex_wrap()
              .child(div().text_size(12).text_color(t.dim).child(this.tr("Machine:", "目标主机：")))
              .child(machineChip("local", this.tr("This computer", "本机"), null))
              .children(machines.map((m) => machineChip(m.name, m.name, m.name))),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(
                div()
                  .px(7)
                  .py(3)
                  .rounded(5)
                  .bg(t.background)
                  .border_1()
                  .border_color(t.border)
                  .text_size(11)
                  .text_color(st.configured ? t.ansi2 : t.dim)
                  .child(st.configured ? this.tr("Connected", "已配置") : this.tr("Not configured", "未配置")),
              )
              .child(this.button(t, "st-ai-refresh", this.aiLoading ? this.tr("Refreshing…", "刷新中…") : this.tr("Refresh", "刷新"), (_e, cx) => {
                cx.spawn(async (cx) => { await this.loadAi(); cx.notify(); });
              })),
          ),
      )
      .when(Boolean(this.aiError), (el) =>
        el.child(div().px(12).py(8).rounded(8).border_1().border_color(t.blocked).bg(t.surface).text_size(12.5).text_color(t.blocked).child(this.aiError)),
      )
      // 1. Core AI Endpoint & Model Card
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Core AI Assistant", "核心 AI 助手")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Enable Core AI assistant", "启用全局 AI 助手"),
                  this.tr("Shared lightweight OpenAI-compatible runtime used by PTY status arbitration, parent note digests, and Drove orchestration.", "全局共享的轻量 OpenAI 兼容接口，同时服务于终端状态仲裁、子 Agent 摘要与 Drove 编排。"),
                  this.toggle(t, "st-ai-on", aiOn, (_e, cx) => this.saveAiPatch({ enabled: !aiOn }, cx)),
                  true,
                ),
              )
              .child(
                h_flex()
                  .w_full()
                  .px(14)
                  .py(12)
                  .gap(12)
                  .border_t(1)
                  .border_color(t.border)
                  .items_start()
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(4)
                      .child(div().text_size(12).text_color(t.foreground).child(this.tr("Service URL", "远端服务地址")))
                      .child(div().px(8).py(2).rounded(7).border_1().border_color(t.border).bg(t.background).child(Input.new(this.droveUrlInput).h(26).text_size(12.5).text_color(t.foreground))),
                  )
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(4)
                      .child(
                        h_flex()
                          .justify_between()
                          .items_center()
                          .gap(6)
                          .child(div().text_size(12).text_color(t.foreground).child(this.tr("API Key", "API Key")))
                          .when(Boolean(st.keyMasked), (el) =>
                            el.child(div().truncate().text_size(11).text_color(t.dim).child(this.tr(`Saved: ${st.keyMasked}`, `已保存：${st.keyMasked}`))),
                          ),
                      )
                      .child(div().px(8).py(2).rounded(7).border_1().border_color(t.border).bg(t.background).child(Input.new(this.droveKeyInput).h(26).text_size(12.5).text_color(t.foreground))),
                  ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Model", "使用模型"),
                  aiModels.length > 0
                    ? this.tr(`${aiModels.length} model(s) discovered from endpoint`, `已从服务端拉取 ${aiModels.length} 个可用模型`)
                    : this.tr("Save or test connection to load models, or type a custom model ID", "保存或测试连通性后自动拉取模型列表，也可直接输入自定义模型"),
                  this.drovePicker(
                    t,
                    "core-ai-model",
                    currentAiModel || this.tr("Auto (first available)", "自动选择（首个可用）"),
                    aiModelItems,
                    (model, cx) => this.selectAiModel(model || "", cx),
                    { width: 240, popupWidth: 380, anchor: "bottom_right", allowCustom: true },
                    cx,
                  ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Reasoning Effort", "思考强度"),
                  this.tr("Controls default reasoning_effort; Nano tasks automatically use minimal thinking when set to Auto/Low.", "控制默认思考深度；设为自动或低时，超轻量分类任务会自动关闭长思考以省 Token。"),
                  h_flex()
                    .id("st-ai-eff-group")
                    .w(240)
                    .h(28)
                    .flex_none()
                    .p(2)
                    .gap(1)
                    .items_center()
                    .rounded(7)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .children(
                      aiEffortChoices.map((eff) => {
                        const active = currentAiEffort === eff;
                        return div()
                          .id(`st-ai-eff-${eff}`)
                          .flex_1()
                          .h(22)
                          .px(2)
                          .flex()
                          .items_center()
                          .justify_center()
                          .rounded(5)
                          .bg(active ? t.accent : "#00000000")
                          .text_size(11)
                          .when(active, (el) => el.font_semibold())
                          .text_color(active ? t.accent_foreground : t.muted)
                          .cursor_pointer()
                          .when(!active, (el) => el.hover((s) => s.bg(t.hover).text_color(t.foreground)))
                          .on_click((_e, cx) => this.saveAiPatch({ effort: eff }, cx))
                          .child(this.aiEffortLabel(eff));
                      }),
                    ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Context Size", "上下文大小"),
                  this.tr("Caps prompt input budget and sets local num_ctx (e.g. Ollama).", "限制输入预算并为本地服务（如 Ollama）下发 num_ctx。"),
                  this.drovePicker(
                    t,
                    "core-ai-context",
                    currentAiContext ? currentAiContext.toUpperCase() : this.tr("Auto (model default)", "默认（随模型）"),
                    aiContextItems,
                    (val, cx) => this.saveAiPatch({ contextWindow: this.normalizeAiContextInput(val) }, cx),
                    { width: 240, popupWidth: 320, anchor: "bottom_right", allowCustom: true },
                    cx,
                  ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Max Output Tokens", "最大输出长度"),
                  this.tr("Max reply tokens; Auto keeps classification/digest responses compact.", "最大输出长度；设为自动时对分类与摘要任务自动收敛长度。"),
                  this.drovePicker(
                    t,
                    "core-ai-max-tokens",
                    currentAiMaxTokens ? `${currentAiMaxTokens} tokens` : this.tr("Auto (by tier/effort)", "自动（按任务分级）"),
                    aiMaxTokenItems,
                    (val, cx) => {
                      const parsed = this.parseAiMaxTokensInput(val);
                      if (parsed !== null) {
                        this.saveAiPatch({ maxTokens: parsed }, cx);
                      }
                    },
                    { width: 240, popupWidth: 320, anchor: "bottom_right", allowCustom: true },
                    cx,
                  ),
                ),
              )
              .child(
                h_flex()
                  .w_full()
                  .px(14)
                  .py(9)
                  .gap(8)
                  .border_t(1)
                  .border_color(t.border)
                  .bg(t.background)
                  .items_center()
                  .justify_between()
                  .flex_wrap()
                  .child(
                    h_flex()
                      .gap(8)
                      .items_center()
                      .child(
                        div()
                          .id("st-ai-save")
                          .px(12)
                          .py(5)
                          .rounded(6)
                          .bg(t.accent)
                          .cursor_pointer()
                          .text_size(12.5)
                          .font_semibold()
                          .text_color(t.accent_foreground)
                          .hover((s) => s.opacity(0.9))
                          .on_click((_e, cx) => {
                            const patch = { baseUrl: this.droveUrlInput.value().trim(), enabled: aiOn };
                            const k = this.droveKeyInput.value().trim();
                            if (this.droveKeyDirty && k) patch.apiKey = k;
                            this.saveAiPatch(patch, cx, true);
                          })
                          .child(this.aiSaving ? this.tr("Saving…", "保存中…") : this.tr("Save", "保存")),
                      )
                      .child(this.button(t, "st-ai-test", this.aiTesting ? this.tr("Testing…", "测试中…") : this.tr("Test Connection", "测试连通性"), (_e, cx) => this.testCoreAi(cx)))
                      .when(Boolean(st.keyMasked), (el) =>
                        el.child(this.button(t, "st-ai-clear", this.tr("Clear Key", "清除 Key"), (_e, cx) => {
                          this.droveKeyInput.set_value("");
                          this.droveKeyDirty = false;
                          this.saveAiPatch({ clearKey: true }, cx);
                        })),
                      ),
                  )
                  .when(Boolean(this.aiTestResult), (el) => {
                    const trRes = this.aiTestResult;
                    const n = (trRes.availableModels ?? []).length;
                    return el.child(
                      div()
                        .text_size(12)
                        .text_color(trRes.ok ? t.ansi2 : t.blocked)
                        .child(
                          trRes.ok
                            ? `✓ OK (${trRes.latencyMs ?? 0}ms) · ${trRes.discoveredModel ?? ""}${n > 0 ? ` · ${n} ${this.tr("models", "个模型")}` : ""}`
                            : `✗ ${trRes.error ?? "Failed"}${n > 0 ? ` (${n} ${this.tr("models listed", "个模型可选")})` : ""}`,
                        ),
                    );
                  }),
              ),
          ),
      )
      // 2. Core AI Feature Switches
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Core Semantic Capabilities", "核心语义能力开关")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .child(
                this.droveCardRow(
                  t,
                  this.tr("PTY Terminal State Arbitration", "非协议终端语义状态仲裁"),
                  this.tr("Fallback semantic classification for non-OSC-7501 CLI agents when quiet, surfacing approval prompts or errors in the sidebar.", "当非 OSC 7501 终端静默且启发式不确定时，异步兜底识别审批提问、错误或思考中状态。"),
                  this.toggle(t, "st-ai-sw-pty", ptyArb, (_e, cx) => this.saveAiPatch({ ptyStatusArbitration: !ptyArb }, cx)),
                  true,
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Child Note Error Digest", "父子通知异常智能摘要"),
                  this.tr("Append a compact root-cause digest to parent <h2> envelopes when a child agent errors or exits abnormally.", "子 Agent 报错或异常退出时，向父 Agent 的 <h2> 通知信封附加根因与建议摘要，避免盲目读取大日志。"),
                  this.toggle(t, "st-ai-sw-notes", notesDigest, (_e, cx) => this.saveAiPatch({ notesDigest: !notesDigest }, cx)),
                ),
              )
              .child(
                v_flex()
                  .w_full()
                  .px(14)
                  .py(11)
                  .gap(8)
                  .border_t(1)
                  .border_color(t.border)
                  .child(
                    h_flex()
                      .w_full()
                      .gap(12)
                      .items_center()
                      .justify_between()
                      .child(
                        v_flex()
                          .flex_1()
                          .min_w(0)
                          .gap(2)
                          .child(
                            div()
                              .text_size(13)
                              .text_color(t.foreground)
                              .child(this.tr("Prompt KV-Cache Optimization", "长共享前缀 / 精简提示词")),
                          )
                          .child(
                            div()
                              .text_size(11.5)
                              .line_height("16px")
                              .text_color(t.dim)
                              .child(
                                this.tr(
                                  "Choose between Shared Prefix (>1024-token KV-cache reuse), Compact short prompts, or Auto detection. Switching models resets this to Auto.",
                                  "选择使用长共享前缀（>1024 tokens 复用 KV 缓存）、精简提示词，或由程序自动识别；切换模型后将自动切回“自动”并重新识别。",
                                ),
                              ),
                          ),
                      )
                      .child(
                        h_flex()
                          .id("st-ai-prompt-mode-group")
                          .w(276)
                          .h(28)
                          .flex_none()
                          .p(2)
                          .gap(1)
                          .items_center()
                          .rounded(7)
                          .border_1()
                          .border_color(t.border)
                          .bg(t.background)
                          .children(
                            promptModeChoices.map((mode) => {
                              const active = promptMode === mode;
                              return div()
                                .id(`st-ai-pm-${mode}`)
                                .flex_1()
                                .h(22)
                                .px(4)
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(5)
                                .bg(active ? t.accent : "#00000000")
                                .text_size(11)
                                .when(active, (el) => el.font_semibold())
                                .text_color(active ? t.accent_foreground : t.muted)
                                .cursor_pointer()
                                .when(!active, (el) => el.hover((s) => s.bg(t.hover).text_color(t.foreground)))
                                .on_click((_e, cx) => {
                                  if (mode === promptMode) return;
                                  this.saveAiPatch({ promptMode: mode }, cx);
                                })
                                .child(this.aiPromptModeLabel(mode));
                            }),
                          ),
                      ),
                  )
                  .child(
                    h_flex()
                      .w_full()
                      .px(10)
                      .py(6)
                      .gap(8)
                      .rounded(7)
                      .border_1()
                      .border_color(t.border)
                      .bg(t.background)
                      .items_center()
                      .justify_between()
                      .flex_wrap()
                      .child(
                        div()
                          .id("st-ai-pm-status")
                          .flex_1()
                          .min_w(160)
                          .text_size(11.5)
                          .text_color(
                            this.aiPromptProbeResult && this.aiPromptProbeResult.ok === false
                              ? t.blocked
                              : this.aiPromptProbeResult?.detectedMode === "shared_prefix"
                                ? t.ansi2
                                : t.dim,
                          )
                          .child(
                            this.aiProbingPromptMode
                              ? this.tr(
                                  "Clearing prior record and running automatic KV-cache detection…",
                                  "正在清除此前识别记录并重新执行自动识别…",
                                )
                              : this.aiPromptProbeResult
                                ? this.aiPromptProbeResult.ok === false
                                  ? this.tr(
                                      `Detection cleared (probe failed: ${this.aiPromptProbeResult.reason ?? "error"})`,
                                      `已清除旧记录（探测失败：${this.aiPromptProbeResult.reason ?? "请求异常"}）`,
                                    )
                                  : this.aiPromptProbeResult.detectedMode === "shared_prefix"
                                    ? this.tr(
                                        `Detected: Shared Prefix (${Number(this.aiPromptProbeResult.cachedInputTokens ?? 0).toLocaleString()} cached tokens hit)`,
                                        `自动识别完成：长共享前缀（命中 KV 缓存 ${Number(this.aiPromptProbeResult.cachedInputTokens ?? 0).toLocaleString()} tokens）`,
                                      )
                                    : this.aiPromptProbeResult.detectedMode === "compact"
                                      ? this.tr(
                                          "Detected: Compact prompts (endpoint returned 0 cached tokens)",
                                          "自动识别完成：精简提示词（当前端点连续探测未命中 KV 缓存）",
                                        )
                                      : this.tr(
                                          `Detection record cleared${this.aiPromptProbeResult.reason ? ` (${this.aiPromptProbeResult.reason})` : ""}`,
                                          `已清除识别记录，将在后续调用时自动识别${this.aiPromptProbeResult.reason ? `（${this.aiPromptProbeResult.reason}）` : ""}`,
                                        )
                                : promptMode === "auto"
                                  ? detectedPromptMode === "shared_prefix"
                                    ? this.tr(
                                        "Auto-detected: Shared Prefix (KV cache verified active)",
                                        "当前自动识别结果：长共享前缀（已验证命中 KV 缓存）",
                                      )
                                    : detectedPromptMode === "compact"
                                      ? this.tr(
                                          "Auto-detected: Compact prompts (endpoint did not hit KV cache)",
                                          "当前自动识别结果：精简提示词（端点未命中 KV 缓存，已自动降级）",
                                        )
                                      : probeMisses > 0
                                        ? this.tr(
                                            `Auto-detecting (${probeMisses}/${probeThreshold} uncached probes so far)`,
                                            `自动识别中（已连续 ${probeMisses}/${probeThreshold} 次未命中缓存）`,
                                          )
                                        : this.tr(
                                            "Auto-detection pending (will probe on next call or click Re-detect)",
                                            "自动识别状态：待识别（将在下次调用或点击右侧按钮时识别）",
                                          )
                                  : this.tr(
                                      `Manually pinned to ${this.aiPromptModeLabel(promptMode)} (resets to Auto when model changes)`,
                                      `当前已手动选择“${this.aiPromptModeLabel(promptMode)}”（切换模型后将自动切回“自动”）`,
                                    ),
                          ),
                      )
                      .child(
                        this.button(
                          t,
                          "st-ai-pm-redetect",
                          this.aiProbingPromptMode
                            ? this.tr("Detecting…", "识别中…")
                            : this.tr("Re-detect Automatically", "重新自动识别"),
                          (_e, cx) => this.redetectPromptMode(cx),
                        ),
                      ),
                  ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Handoff Semantic Distillation", "接力与合回语义记忆蒸馏"),
                  this.tr("Distill omitted middle turns and side-chat merge-backs into structured memory (rejected approaches, conventions, blockers).", "在上下文接力与旁路合回时，将截断的中间轮次与长讨论提炼为高密度避坑记忆，避免新 Agent 重蹈覆辙。"),
                  this.toggle(t, "st-ai-sw-handoff", handoffDistill, (_e, cx) => this.saveAiPatch({ handoffDistill: !handoffDistill }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Checkpoint Semantic Labeling", "快照语义里程碑命名"),
                  this.tr("Asynchronously summarize each turn's diff into a 10-15 char milestone label without touching manual bookmarks.", "每轮快照落盘后异步根据改动生成 10–15 字语义标签（替代“继续 / 修报错”），且绝不覆盖手动书签。"),
                  this.toggle(t, "st-ai-sw-ckpt-label", checkpointLabeling, (_e, cx) => this.saveAiPatch({ checkpointLabeling: !checkpointLabeling }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Semantic Naming & Hover Micro-Summary", "语义命名与悬停微摘要"),
                  this.tr("Generate short task titles for unnamed tabs/agents, one-sentence hover progress tooltips, and Catch-up Briefing executive summaries.", "为未命名标签页与智能体生成简短任务标题、悬停一句话进展，以及离开重连简报顶栏总览。"),
                  this.toggle(t, "st-ai-sw-semantic", semanticSummary, (_e, cx) => this.saveAiPatch({ semanticSummary: !semanticSummary }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Outbound Secret Redaction", "外发内容敏感信息脱敏"),
                  this.tr("Automatically scrub API keys, Bearer tokens, PEM private keys, and URL credentials before sending terminal logs or diffs to external LLMs.", "向外部模型发送终端日志、代码 Diff 或对话片段前，自动将常见密钥、Bearer Token、私钥与 URL 凭据替换为 <REDACTED>。"),
                  this.toggle(t, "st-ai-sw-redact", redactSecrets, (_e, cx) => this.saveAiPatch({ redactSecrets: !redactSecrets }, cx)),
                ),
              ),
          ),
      )
      // 3. Live Cache Efficiency & Token Usage Card
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Cache Efficiency & Token Usage", "缓存命中率与 Token 统计")))
          .child(
            v_flex()
              .w_full()
              .p(14)
              .gap(10)
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .child(
                h_flex()
                  .w_full()
                  .gap(8)
                  .flex_wrap()
                  .child(statPill(this.tr("KV Cache Hit Rate", "KV 缓存命中率"), hitPct, true))
                  .child(statPill(this.tr("Local 0-Token Hits", "本地零消耗命中"), localHits))
                  .child(statPill(this.tr("Sessions Reused", "多轮会话复用"), `${sessionsReused} (${activeSessions} ${this.tr("active", "活跃")})`))
                  .child(statPill(this.tr("API Calls", "远端调用次数"), totalCalls)),
              )
              .when(promptMode === "auto" && cacheAutoDegraded, (el) =>
                el.child(
                  div()
                    .id("st-ai-cache-degraded")
                    .px(10)
                    .py(6)
                    .rounded(7)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .text_size(11.5)
                    .text_color(t.dim)
                    .child(
                      this.tr(
                        "Compact prompt mode active: this endpoint returned 0 cached tokens repeatedly, so Herdr2 switched stateless ops to short prompts to save input tokens.",
                        "已自动切换为精简提示词模式：当前端点连续多次未命中 KV 缓存，已自动停发长共享前缀以节省输入 Token。",
                      ),
                    ),
                ),
              )
              .child(
                div()
                  .text_size(11.5)
                  .text_color(t.dim)
                  .child(
                    this.tr(
                      `Tokens — Input: ${inTokens.toLocaleString()} (Cached: ${cachedTokens.toLocaleString()}) · Output: ${outTokens.toLocaleString()} · Config: ${st.configPath ?? "~/.config/herdr2/ai.json"}`,
                      `Token 消耗 — 输入：${inTokens.toLocaleString()}（缓存命中：${cachedTokens.toLocaleString()}）· 输出：${outTokens.toLocaleString()} · 配置：${st.configPath ?? "~/.config/herdr2/ai.json"}`,
                    ),
                  ),
              )
              .when(opSummaryItems.length > 0, (el) =>
                el.child(
                  v_flex()
                    .id("st-ai-op-metrics")
                    .w_full()
                    .pt(8)
                    .gap(4)
                    .border_t(1)
                    .border_color(t.border)
                    .child(
                      div()
                        .text_size(11)
                        .font_semibold()
                        .text_color(t.dim)
                        .child(this.tr("Per-Operation Quality & Latency", "各能力质量与耗时")),
                    )
                    .children(
                      opSummaryItems.map((it) => {
                        const issues = [];
                        if (it.fails > 0) issues.push(this.tr(`${it.fails} fail`, `${it.fails} 失败`));
                        if (it.parseErrs > 0) issues.push(this.tr(`${it.parseErrs} parse err`, `${it.parseErrs} 解析错`));
                        if (it.lowConf > 0) issues.push(this.tr(`${it.lowConf} low-conf`, `${it.lowConf} 低置信`));
                        const tail = issues.length > 0 ? ` · ${issues.join(", ")}` : "";
                        return h_flex()
                          .w_full()
                          .justify_between()
                          .items_center()
                          .gap(8)
                          .text_size(11.5)
                          .child(div().text_color(t.foreground).child(it.op))
                          .child(
                            div()
                              .text_color(issues.length > 0 ? t.blocked : t.dim)
                              .child(
                                this.tr(
                                  `${it.calls} calls · ${it.hits} cached · avg ${it.avgMs}ms${tail}`,
                                  `${it.calls} 次调用 · ${it.hits} 次命中 · 均耗时 ${it.avgMs}ms${tail}`,
                                ),
                              ),
                          );
                      }),
                    ),
                ),
              ),
          ),
      );
  }

  drove(t, cx) {
    const machines = (this.machineList ?? []).filter((m) => !m.temporary);
    const data = this.droveData;
    const cfg = data?.config ?? {};
    const aiStatus = data?.aiStatus ?? {};
    const roles = cfg.roles ?? {};
    const roleIds = Object.keys(roles).sort((a, b) => {
      const ia = DROVE_BUILTIN_ROLES.indexOf(a);
      const ib = DROVE_BUILTIN_ROLES.indexOf(b);
      if (ia !== -1 || ib !== -1) return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib);
      return a.localeCompare(b);
    });
    const machineChip = (id, label, value) => {
      const on = this.droveMachine === value;
      return div()
        .id(`st-dr-m-${id}`)
        .px(10)
        .py(4)
        .rounded(6)
        .border_1()
        .border_color(on ? t.accent : t.border)
        .bg(on ? t.accent : t.raised)
        .cursor_pointer()
        .text_size(12.5)
        .text_color(on ? t.accent_foreground : t.foreground)
        .on_click((_e, cx) => {
          this.droveMachine = value;
          this.droveData = null;
          this.droveProbePromise = null;
          this.picking = null;
          this.droveModelSubmit = null;
          this.droveStrengthSubmit = null;
          this.droveTestResult = null;
          this.droveAiConfigResult = null;
          this.droveRolesBackup = null;
          this.droveAiConfigMode = null;
          this.droveMagpieResult = null;
          cx.spawn(async (cx) => {
            await this.loadDrove();
            this.maybeAutoProbeDrove(cx);
            cx.notify();
          });
        })
        .child(label);
    };
    const magpieOn = Boolean(cfg.magpie?.enabled);
    const aiOn = cfg.ai?.enabled !== false;
    const aiModels = this.aiAvailableModels();
    const currentAiModel = (cfg.ai?.model ?? "").trim();
    const aiModelItems = [
      { value: "", label: this.tr("Auto (first available model)", "自动选择（首个可用模型）"), active: !currentAiModel },
      ...aiModels.map((m) => ({ value: m, label: m, active: currentAiModel === m })),
    ];
    if (currentAiModel && !aiModels.includes(currentAiModel)) {
      aiModelItems.splice(1, 0, { value: currentAiModel, label: currentAiModel, meta: this.tr("custom", "自定义"), active: true });
    }
    const aiEffortChoices = ["auto", "none", "low", "medium", "high"];
    const rawAiEffort = (cfg.ai?.effort ?? aiStatus.effort ?? "low").trim().toLowerCase() || "low";
    const currentAiEffort = aiEffortChoices.includes(rawAiEffort) ? rawAiEffort : "low";
    const currentAiContext = this.normalizeAiContextInput(cfg.ai?.contextWindow ?? aiStatus.contextWindow ?? "");
    const currentAiMaxTokens = Number(cfg.ai?.maxTokens ?? aiStatus.maxTokens ?? 0) || 0;
    const aiContextPresets = ["8k", "16k", "32k", "64k", "128k", "256k", "1m"];
    const aiContextItems = [
      { value: "", label: this.tr("Auto (model default)", "默认（随模型）"), active: !currentAiContext },
      ...aiContextPresets.map((v) => ({ value: v, label: v.toUpperCase(), active: currentAiContext === v })),
    ];
    if (currentAiContext && !aiContextPresets.includes(currentAiContext)) {
      aiContextItems.splice(1, 0, {
        value: currentAiContext,
        label: currentAiContext.toUpperCase(),
        meta: this.tr("custom", "自定义"),
        active: true,
      });
    }
    const aiMaxTokenPresets = [512, 1024, 2048, 4096, 8192, 16384];
    const aiMaxTokenItems = [
      { value: 0, label: this.tr("Auto (by effort)", "自动（随思考强度）"), active: !currentAiMaxTokens },
      ...aiMaxTokenPresets.map((v) => ({ value: v, label: `${v} tokens`, active: currentAiMaxTokens === v })),
    ];
    if (currentAiMaxTokens && !aiMaxTokenPresets.includes(currentAiMaxTokens)) {
      aiMaxTokenItems.splice(1, 0, {
        value: currentAiMaxTokens,
        label: `${currentAiMaxTokens} tokens`,
        meta: this.tr("custom", "自定义"),
        active: true,
      });
    }
    const magpieQuotas = data?.magpieQuotas ?? [];
    const switches = aiStatus.switches ?? {};
    const verifyDigest = (cfg.ai?.verifyDigest ?? switches.verifyDigest) !== false;
    const handoverCompact = (cfg.ai?.handoverCompact ?? switches.handoverCompact) !== false;
    const autoTitle = (cfg.ai?.autoTitle ?? switches.autoTitle) !== false;
    const taskEnrich = (cfg.ai?.taskEnrich ?? switches.taskEnrich) !== false;
    const conflictPolicy = cfg.ai?.conflictPolicy ?? switches.conflictPolicy ?? "warn";
    const rawQuestionMode = cfg.ai?.questionMode ?? switches.questionMode ?? "draft";
    const questionMode = rawQuestionMode === "auto" ? "auto_when_offline" : rawQuestionMode;
    const loopDetect = cfg.ai?.loopDetect ?? switches.loopDetect ?? "off";
    const conflictOptions = [
      { value: "off", label: this.tr("Off", "关闭 (off)") },
      { value: "warn", label: this.tr("Warn", "告警提示 (warn)") },
      { value: "auto_worktree", label: this.tr("Auto Worktree", "自动隔离工作树 (auto_worktree)") },
    ].map((it) => ({ ...it, active: it.value === conflictPolicy }));
    const questionOptions = [
      { value: "off", label: this.tr("Off", "关闭 (off)") },
      { value: "draft", label: this.tr("Draft Reply", "生成草稿 (draft)") },
      { value: "auto_when_offline", label: this.tr("Auto Reply (High Confidence)", "高置信自动回复 (auto_when_offline)") },
    ].map((it) => ({ ...it, active: it.value === questionMode }));
    const loopOptions = [
      { value: "off", label: this.tr("Off", "关闭 (off)") },
      { value: "warn", label: this.tr("Warn Only", "仅告警 (warn)") },
      { value: "steer_once", label: this.tr("Steer Once", "自动纠偏一次 (steer_once)") },
    ].map((it) => ({ ...it, active: it.value === loopDetect }));
    const conflictLabel = conflictOptions.find((o) => o.active)?.label ?? conflictPolicy;
    const questionLabel = questionOptions.find((o) => o.active)?.label ?? questionMode;
    const loopLabel = loopOptions.find((o) => o.active)?.label ?? loopDetect;

    const probedList = this.probedAgents();
    const installedCount = probedList.filter((a) => a.installed).length;

    return v_flex()
      .w_full()
      .gap(16)
      // 0. Top Environment Bar Card
      .child(
        h_flex()
          .w_full()
          .px(14)
          .py(10)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.surface)
          .items_center()
          .justify_between()
          .gap(10)
          .flex_wrap()
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .flex_wrap()
              .child(div().text_size(12).text_color(t.dim).child(this.tr("Machine:", "目标主机：")))
              .child(machineChip("local", this.tr("This computer", "本机"), null))
              .children(machines.map((m) => machineChip(m.name, m.name, m.name))),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .when(installedCount > 0, (el) =>
                el.child(
                  div()
                    .px(7)
                    .py(3)
                    .rounded(5)
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border)
                    .text_size(11)
                    .text_color(t.muted)
                    .child(this.tr(`${installedCount} agents ready`, `${installedCount} 个 Agent 可用`)),
                ),
              )
              .child(this.button(t, "st-dr-probe", this.droveProbing ? this.tr("Refreshing…", "检测中…") : this.tr("Refresh Agents", "刷新检测"), (_e, cx) => this.probeDroveAgents(cx)))
              .child(this.button(t, "st-dr-web", this.tr("Open Drove Web", "打开 Drove 网页"), () => {
                if (!(typeof checking === "function" && checking())) hc(this.droveArgs(["drove", "web"]));
              })),
          ),
      )
      .when(Boolean(this.droveError), (el) =>
        el.child(
          div()
            .px(12)
            .py(8)
            .rounded(8)
            .border_1()
            .border_color(t.blocked)
            .bg(t.surface)
            .text_size(12)
            .text_color(t.blocked)
            .child(this.droveError),
        ),
      )
      .when(this.droveLoading && !data, (el) =>
        el.child(h_flex().py(10).gap(8).items_center().child(svg("mods/workspace-chrome/spinner.svg").size(12).text_color(t.working).spin(900)).child(div().text_size(12.5).text_color(t.dim).child(this.tr("Loading Drove configuration…", "正在读取 Drove 配置…")))),
      )
      // 1. Roles Section
      .child(
        v_flex()
          .w_full()
          .gap(10)
          .child(
            v_flex()
              .gap(3)
              .child(this.heading(t, this.tr("Roles", "角色配置")))
              .child(
                div()
                  .text_size(12)
                  .line_height("17px")
                  .text_color(t.dim)
                  .child(
                    this.tr(
                      "Assign one or more Agent + Model candidates and their strengths per role. Matches best-fit candidate first, falling back #1 → #2 → #3 on tie, quota exhaustion, or retry.",
                      "为每个角色配置一个或多个 Agent + 模型及擅长任务；编排者优先按擅长类型选人，未命中、额度耗尽或重试时按 #1 → #2 → #3… 顺延。",
                    ),
                  ),
              ),
          )
          .child(
            h_flex()
              .w_full()
              .items_center()
              .justify_between()
              .gap(8)
              .flex_wrap()
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .flex_wrap()
                  .child(
                    (() => {
                      const active = this.droveAiConfigMode === "all";
                      const runningAll = this.droveAiConfiguring && this.droveAiRunningMode === "all";
                      return h_flex()
                        .id("st-dr-ai-all-btn")
                        .px(10)
                        .py(5)
                        .gap(5)
                        .rounded(6)
                        .border_1()
                        .border_color(active ? t.accent : t.border)
                        .bg(active ? t.accent : t.raised)
                        .items_center()
                        .cursor_pointer()
                        .text_size(12)
                        .font_semibold()
                        .text_color(active ? t.accent_foreground : t.foreground)
                        .when(!active, (el) => el.hover((s) => s.bg(t.hover)))
                        .tooltip(
                          this.tr(
                            "One-click AI auto-configure: enter optional preferences and configure Role Agents/Models, Candidate Strengths, and Routing Notes together (auto-probes agents if not cached)",
                            "一键调用智能辅助模型，同时自动配置所有角色的模型、擅长任务与选人备注（支持输入自然语言需求，未缓存模型将先自动检测刷新）",
                          ),
                        )
                        .on_click((_e, cx) => {
                          this.droveAiConfigMode = active ? null : "all";
                          if (this.droveAiConfigMode) {
                            this.droveShowAddRole = false;
                            this.droveAiRolePromptInput.focus();
                            if (!this.hasCachedAgentModels() && !this.droveProbing && !this.droveProbePromise && !(typeof checking === "function" && checking())) {
                              this.probeDroveAgents(cx);
                            }
                          }
                          cx.notify();
                        })
                        .when(runningAll, (el) =>
                          el.child(
                            svg("mods/workspace-chrome/spinner.svg")
                              .size(12)
                              .text_color(active ? t.accent_foreground : t.foreground)
                              .spin(900),
                          ),
                        )
                        .child(
                          runningAll
                            ? this.tr("Auto-configuring…", "一键配置中…")
                            : this.tr("⚡ AI One-Click", "⚡ AI 一键配置"),
                        );
                    })(),
                  )
                  .child(
                    (() => {
                      const active = this.droveAiConfigMode === "models";
                      const runningModels = this.droveAiConfiguring && this.droveAiRunningMode === "models";
                      return div()
                        .id("st-dr-ai-models-btn")
                        .px(10)
                        .py(5)
                        .rounded(6)
                        .border_1()
                        .border_color(active ? t.accent : t.border)
                        .bg(active ? t.accent : t.raised)
                        .cursor_pointer()
                        .text_size(12)
                        .text_color(active ? t.accent_foreground : t.foreground)
                        .when(!active, (el) => el.hover((s) => s.bg(t.hover)))
                        .tooltip(this.tr("Use AI Assistant to auto-configure each role's Agent, Model, and Effort (auto-probes agents if not cached)", "调用智能辅助模型自动配置各角色的 Agent 与模型（若未缓存可用模型将先自动检测刷新）"))
                        .on_click((_e, cx) => {
                          this.droveAiConfigMode = active ? null : "models";
                          if (this.droveAiConfigMode) {
                            this.droveShowAddRole = false;
                            this.droveAiRolePromptInput.focus();
                            if (!this.hasCachedAgentModels() && !this.droveProbing && !this.droveProbePromise && !(typeof checking === "function" && checking())) {
                              this.probeDroveAgents(cx);
                            }
                          }
                          cx.notify();
                        })
                        .child(runningModels ? this.tr("✨ Configuring…", "✨ 配模型中…") : this.tr("✨ AI Models", "✨ AI 配模型"));
                    })(),
                  )
                  .child(
                    (() => {
                      const active = this.droveAiConfigMode === "strengths";
                      const runningStrengths = this.droveAiConfiguring && this.droveAiRunningMode === "strengths";
                      return div()
                        .id("st-dr-ai-strengths-btn")
                        .px(10)
                        .py(5)
                        .rounded(6)
                        .border_1()
                        .border_color(active ? t.accent : t.border)
                        .bg(active ? t.accent : t.raised)
                        .cursor_pointer()
                        .text_size(12)
                        .text_color(active ? t.accent_foreground : t.foreground)
                        .when(!active, (el) => el.hover((s) => s.bg(t.hover)))
                        .tooltip(this.tr("Use AI Assistant to configure work-type strengths for each existing Agent + Model candidate", "保持现有 Agent 与模型不变，调用智能辅助模型自动为每个候选分配擅长的工作类型"))
                        .on_click((_e, cx) => {
                          this.droveAiConfigMode = active ? null : "strengths";
                          if (this.droveAiConfigMode) {
                            this.droveShowAddRole = false;
                            this.droveAiRolePromptInput.focus();
                            if (!this.hasCachedAgentModels() && !this.droveProbing && !this.droveProbePromise && !(typeof checking === "function" && checking())) {
                              this.probeDroveAgents(cx);
                            }
                          }
                          cx.notify();
                        })
                        .child(runningStrengths ? this.tr("✨ Configuring…", "✨ 配任务中…") : this.tr("✨ AI Strengths", "✨ AI 配擅长任务"));
                    })(),
                  )
                  .child(
                    (() => {
                      const active = this.droveAiConfigMode === "notes";
                      const runningNotes = this.droveAiConfiguring && this.droveAiRunningMode === "notes";
                      return div()
                        .id("st-dr-ai-notes-btn")
                        .px(10)
                        .py(5)
                        .rounded(6)
                        .border_1()
                        .border_color(active ? t.accent : t.border)
                        .bg(active ? t.accent : t.raised)
                        .cursor_pointer()
                        .text_size(12)
                        .text_color(active ? t.accent_foreground : t.foreground)
                        .when(!active, (el) => el.hover((s) => s.bg(t.hover)))
                        .tooltip(
                          this.tr(
                            "Use AI Assistant to generate candidate routing notes (keeps current Agents, Models, and Strengths)",
                            "保持现有 Agent、模型与擅长任务不变，调用智能辅助模型自动生成各候选的选人备注",
                          ),
                        )
                        .on_click((_e, cx) => {
                          this.droveAiConfigMode = active ? null : "notes";
                          if (this.droveAiConfigMode) {
                            this.droveShowAddRole = false;
                            this.droveAiRolePromptInput.focus();
                          }
                          cx.notify();
                        })
                        .child(runningNotes ? this.tr("✨ Generating…", "✨ 配备注中…") : this.tr("✨ AI Notes", "✨ AI 备注"));
                    })(),
                  )
                  .when(Boolean(this.droveRolesBackup), (row) =>
                    row.child(
                      div()
                        .id("st-dr-ai-roles-undo")
                        .px(10)
                        .py(5)
                        .rounded(6)
                        .border_1()
                        .border_color(t.border)
                        .bg(t.raised)
                        .cursor_pointer()
                        .text_size(12)
                        .text_color(t.foreground)
                        .hover((s) => s.bg(t.hover))
                        .tooltip(this.tr("Restore role configuration from before the last AI auto-configure", "还原到上次 AI 自动配置前的角色设置"))
                        .on_click((_e, cx) => this.restoreDroveRolesBackup(cx))
                        .child(this.tr("↩ Restore", "↩ 还原配置")),
                    ),
                  ),
              )
              .child(
                this.button(t, "st-dr-add-role-btn", this.droveShowAddRole ? this.tr("Cancel", "取消") : this.tr("+ Add role", "+ 添加角色"), (_e, cx) => {
                  this.droveShowAddRole = !this.droveShowAddRole;
                  if (this.droveShowAddRole) this.droveAiConfigMode = null;
                  this.droveNewRoleInput.set_value("");
                  cx.notify();
                }),
              ),
          )
          .when(Boolean(this.droveAiConfigMode), (el) => {
            const mode = this.droveAiConfigMode;
            const isAllMode = mode === "all";
            const isStrengthsMode = mode === "strengths";
            const isNotesMode = mode === "notes";
            const aiReady = Boolean((this.droveUrlInput.value().trim() || cfg.ai?.baseUrl?.trim()));
            const activeAiModelLabel = currentAiModel || aiStatus.model || aiModels[0] || this.tr("Auto-detect from endpoint", "自动从服务端获取");
            const hasCache = this.hasCachedAgentModels();
            const promptPlaceholder = isAllMode
              ? this.tr(
                  "Optional instructions (e.g. coder primary Claude Opus for complex architecture, Gemini for frontend UI, avoid GLM 2:00-6:00 PM due to 2x quota; auto-generates models, strengths & notes)…",
                  "可选要求（例如：程序员主力用 Claude Opus 负责复杂架构，备选 Gemini 负责前端 UI；GLM 相关模型下午 2:00–6:00 双倍额度尽量避开；将一并生成模型、擅长任务与备注）…",
                )
              : isNotesMode
                ? this.tr(
                    "Optional instructions (e.g. GLM models cost 2x quota from 2:00-6:00 PM so avoid unless no other agent is available; use heterogeneous provider for reviewer)…",
                    "可选要求（例如：GLM 相关模型在下午 2:00–6:00 额度按两倍计算，应避免使用除非没有其他 agent 可用；审查优先异构模型）…",
                  )
                : isStrengthsMode
                  ? this.tr(
                      "Optional instructions (e.g. Claude Opus for complex architecture & backend, Gemini for frontend UI, Codex for quick bugfixes & algorithms)…",
                      "可选要求（例如：Claude Opus 专注复杂架构与后端重构，Gemini 专注前端 UI 与样式，Codex 专注快速修复与算法，使用简洁中文标签）…",
                    )
                  : this.tr(
                      "Optional instructions (e.g. coder primary Claude + Codex fallback, reviewer use a different provider from coder, keep token costs low)…",
                      "可选要求（例如：程序员主力用 Claude Opus、备选用 Codex 和 Gemini；审查员使用不同厂商模型做异构审查；优先控制额度消耗）…",
                    );
            return el.child(
              v_flex()
                .id("st-dr-ai-roles-panel")
                .p(12)
                .gap(8)
                .rounded(10)
                .border_1()
                .border_color(t.accent)
                .bg(t.surface)
                .child(
                  h_flex()
                    .items_center()
                    .justify_between()
                    .gap(8)
                    .flex_wrap()
                    .child(
                      div()
                        .text_size(12.5)
                        .font_semibold()
                        .text_color(t.foreground)
                        .child(
                          isAllMode
                            ? this.tr("AI One-Click Configure Role Models, Strengths & Notes", "AI 一键配置各角色的模型、擅长任务与备注")
                            : isNotesMode
                              ? this.tr("AI Configure Candidate Notes (keeps current Agents, Models & Strengths)", "AI 生成各候选选人备注（保持现有模型与擅长任务不变）")
                              : isStrengthsMode
                                ? this.tr("AI Configure Candidate Strengths (keeps current Agents & Models)", "AI 配置各候选擅长任务（保持现有 Agent 与模型不变）")
                                : this.tr("AI Configure Role Agents & Models", "AI 配置各角色的 Agent 与模型"),
                        ),
                    )
                    .child(
                      div()
                        .text_size(11.5)
                        .text_color(aiReady ? t.dim : t.blocked)
                        .child(
                          aiReady
                            ? this.tr(`Assistant model: ${activeAiModelLabel}`, `智能辅助模型：${activeAiModelLabel}`)
                            : this.tr("Configure Service URL in AI Assistant below first", "请先在下方「智能辅助模型」配置服务地址"),
                        ),
                    ),
                )
                .when(!hasCache, (box) =>
                  box.child(
                    div()
                      .text_size(11.5)
                      .text_color(t.muted)
                      .child(this.tr("No cached agent/model list detected yet — will auto-probe available agents & models first.", "尚未缓存可用的 Agent 与模型列表，点击生成时将先自动检测刷新。")),
                  ),
                )
                .child(
                  div()
                    .w_full()
                    .px(10)
                    .py(7)
                    .rounded(7)
                    .border_1()
                    .border_color(this.droveAiPromptFocused ? t.accent : t.border)
                    .bg(t.background)
                    .child(
                      Textarea.new(this.droveAiRolePromptInput)
                        .id("st-dr-ai-roles-prompt")
                        .w_full()
                        .min_h(76)
                        .text_size(12.5)
                        .text_color(t.foreground),
                    )
                    .when(!String(this.droveAiRolePromptInput?.value?.() ?? "").trim(), (ph) =>
                      ph.child(div().pt(3).text_size(11).text_color(t.dim).child(promptPlaceholder)),
                    ),
                )
                .child(
                  h_flex()
                    .items_center()
                    .justify_between()
                    .gap(8)
                    .child(
                      div()
                        .text_size(11)
                        .text_color(t.dim)
                        .child(
                          this.droveAiConfiguring && (this.droveAiStage === "probing" || !hasCache)
                            ? this.tr("Probing installed agents & models before AI configuration…", "正在刷新检测可用 Agent 与模型，完成后将自动调用 AI 配置…")
                            : this.droveAiConfiguring
                              ? this.tr("Generating role configuration with AI…", "正在调用 AI 生成角色配置…")
                              : this.tr("Multi-line instructions supported · Ctrl+Enter to apply", "支持多行自然语言要求 · Ctrl+Enter 快速生成"),
                        ),
                    )
                    .child(
                      h_flex()
                        .gap(8)
                        .items_center()
                        .child(
                          h_flex()
                            .id("st-dr-ai-roles-run")
                            .px(12)
                            .py(5)
                            .gap(6)
                            .rounded(6)
                            .bg(t.accent)
                            .items_center()
                            .text_size(12.5)
                            .font_semibold()
                            .text_color(t.accent_foreground)
                            .when(this.droveAiConfiguring, (b) => b.opacity(0.65))
                            .when(!this.droveAiConfiguring, (b) => b.cursor_pointer().on_click((_e, cx) => this.aiConfigureDroveRoles(mode, cx)))
                            .when(this.droveAiConfiguring, (b) => b.child(svg("mods/workspace-chrome/spinner.svg").size(12).text_color(t.accent_foreground).spin(900)))
                            .child(
                              this.droveAiConfiguring
                                ? (this.droveAiStage === "probing"
                                    ? this.tr("Probing agents…", "检测 Agent 中…")
                                    : this.tr("Generating…", "生成中…"))
                                : isAllMode
                                  ? this.tr("⚡ Generate & Apply All", "⚡ 一键生成并应用")
                                  : isNotesMode
                                    ? this.tr("✨ Generate Notes", "✨ 生成并应用备注")
                                    : isStrengthsMode
                                      ? this.tr("✨ Generate Strengths", "✨ 生成并应用擅长任务")
                                      : this.tr("✨ Generate Models", "✨ 生成并应用模型"),
                            ),
                        )
                        .child(
                          this.button(t, "st-dr-ai-roles-close", this.tr("Close", "收起"), (_e, cx) => {
                            this.droveAiConfigMode = null;
                            cx.notify();
                          }),
                        ),
                    ),
                ),
            );
          })
          .when(Boolean(this.droveAiConfigResult), (el) => {
            const res = this.droveAiConfigResult;
            const ok = Boolean(res?.ok);
            const restored = Boolean(res?.restored);
            const modeLabel =
              res?.mode === "all"
                ? this.tr("role models, strengths & notes", "角色模型、擅长任务与备注")
                : res?.mode === "notes"
                  ? this.tr("routing notes", "选人备注")
                  : res?.mode === "strengths"
                    ? this.tr("candidate strengths", "擅长任务")
                    : this.tr("role agents & models", "角色与模型");
            const refreshNote = res?.probedRefreshed ? this.tr(" · refreshed agents", " · 已自动刷新检测") : "";
            return el.child(
              h_flex()
                .id("st-dr-ai-roles-result")
                .px(12)
                .py(8)
                .gap(8)
                .rounded(8)
                .border_1()
                .border_color(ok ? t.ansi2 : t.blocked)
                .bg(t.surface)
                .items_start()
                .justify_between()
                .child(
                  v_flex()
                    .flex_1()
                    .min_w(0)
                    .gap(2)
                    .child(
                      div()
                        .text_size(12)
                        .font_semibold()
                        .text_color(ok ? t.ansi2 : t.blocked)
                        .child(
                          restored
                            ? this.tr("✓ Restored role configuration to before AI auto-config", "✓ 已还原至 AI 配置前的角色设置")
                            : ok
                              ? this.tr(
                                  `✓ AI updated ${modeLabel} (${res.modelUsed || "model"} · ${res.latencyMs ?? 0}ms${refreshNote})`,
                                  `✓ AI 已自动更新${modeLabel}（${res.modelUsed || "model"} · ${res.latencyMs ?? 0}ms${refreshNote}）`,
                                )
                              : this.tr(
                                  `AI role configuration failed: ${res?.error || "unknown error"}`,
                                  `AI 角色配置失败：${res?.error || "未知错误"}`,
                                ),
                        ),
                    )
                    .when(ok && !restored && Boolean(res?.rationale), (b) =>
                      b.child(div().text_size(11.5).text_color(t.muted).child(res.rationale)),
                    ),
                )
                .child(
                  h_flex()
                    .flex_none()
                    .gap(6)
                    .items_center()
                    .when(ok && !restored && Boolean(this.droveRolesBackup), (row) =>
                      row.child(
                        div()
                          .id("st-dr-ai-roles-res-undo")
                          .px(8)
                          .py(3)
                          .rounded(5)
                          .border_1()
                          .border_color(t.border)
                          .bg(t.raised)
                          .cursor_pointer()
                          .text_size(11.5)
                          .text_color(t.foreground)
                          .hover((s) => s.bg(t.hover))
                          .on_click((_e, cx) => this.restoreDroveRolesBackup(cx))
                          .child(this.tr("↩ Restore", "↩ 还原配置")),
                      ),
                    )
                    .child(
                      div()
                        .id("st-dr-ai-roles-res-close")
                        .cursor_pointer()
                        .px(4)
                        .text_size(12)
                        .text_color(t.dim)
                        .hover((s) => s.text_color(t.foreground))
                        .on_click((_e, cx) => {
                          this.droveAiConfigResult = null;
                          cx.notify();
                        })
                        .child("×"),
                    ),
                ),
            );
          })
          .when(this.droveShowAddRole, (el) =>
            el.child(
              h_flex()
                .p(10)
                .gap(8)
                .rounded(8)
                .border_1()
                .border_color(t.border)
                .bg(t.surface)
                .items_center()
                .child(div().flex_1().px(8).py(3).rounded(6).border_1().border_color(t.border).bg(t.background).child(Input.new(this.droveNewRoleInput).h(26).text_size(13).text_color(t.foreground)))
                .child(this.button(t, "st-dr-add-role-ok", this.tr("Create", "创建"), (_e, cx) => {
                  const id = this.droveNewRoleInput.value().trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-");
                  if (!id) return;
                  const nextRoles = JSON.parse(JSON.stringify(roles));
                  if (!nextRoles[id]) {
                    nextRoles[id] = { label: id, access: "writable", primary: { agent: "claude", model: null, effort: "high" }, fallbacks: [] };
                    this.droveShowAddRole = false;
                    this.saveDrovePatch({ roles: nextRoles }, cx);
                  }
                })),
            ),
          )
          .child(
            v_flex()
              .gap(10)
              .children(roleIds.map((id) => this.droveRoleCard(t, id, roles[id], cx))),
          ),
      )
      // 2. Shared Core AI Assistant Summary Card (configured in Settings -> AI)
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("AI Assistant (Shared Core)", "智能辅助模型（全局共享）")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .child(
                this.droveCardRow(
                  t,
                  aiStatus.configured
                    ? this.tr(
                        `Ready · ${currentAiModel || aiModels[0] || "Auto"}`,
                        `已就绪 · ${currentAiModel || aiModels[0] || "自动模型"}`,
                      )
                    : this.tr("AI Assistant not configured", "尚未配置全局 AI 助手"),
                  aiStatus.configured
                    ? this.tr(
                        `Shared across Core & Drove (${cfg.ai?.baseUrl || aiStatus.baseUrl || "local"}). Edit endpoint, API key, cache & core switches in Settings → AI.`,
                        `由 Core 与 Drove 共享（${cfg.ai?.baseUrl || aiStatus.baseUrl || "本地"}）。前往「设置 → AI 助手」管理接口地址、Key 与缓存统计。`,
                      )
                    : this.tr(
                        "Configure an OpenAI-compatible helper once in Settings → AI to unlock role auto-config, semantic conflict/loop checks, and PTY state arbitration.",
                        "在「设置 → AI 助手」中配置一次轻量 OpenAI 兼容模型，即可同时启用 Drove 编排辅助、死循环/冲突分析与终端状态仲裁。",
                      ),
                  h_flex()
                    .gap(8)
                    .items_center()
                    .child(
                      this.toggle(t, "st-dr-ai-on", aiOn, (_e, cx) => this.saveDrovePatch({ ai: { enabled: !aiOn } }, cx)),
                    )
                    .child(
                      this.button(t, "st-dr-open-ai", this.tr("Open AI Settings →", "前往 AI 设置 →"), (_e, cx) => {
                        this.section = "ai";
                        this.picking = null;
                        cx.spawn(async (cx) => {
                          await this.loadAi();
                          cx.notify();
                        });
                        cx.notify();
                      }),
                    ),
                  true,
                ),
              ),
          ),
      )
      // 3. Control-Plane AI Policies Section Card
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Control-Plane AI Policies", "控制面 AI 策略开关")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Verify Digest", "Verify 失败摘要"),
                  this.tr("Summarize root failure causes when verification commands fail.", "验证命令失败时自动提炼根因摘要。"),
                  this.toggle(t, "st-dr-pol-verify", verifyDigest, (_e, cx) => this.saveDrovePatch({ ai: { verifyDigest: !verifyDigest } }, cx)),
                  true,
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Handover Compact", "交接上下文压缩"),
                  this.tr("Compact context summaries when handing work across tasks or retries.", "跨任务交接或重试时自动压缩上下文摘要。"),
                  this.toggle(t, "st-dr-pol-handover", handoverCompact, (_e, cx) => this.saveDrovePatch({ ai: { handoverCompact: !handoverCompact } }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Auto Title", "Run 自动命名"),
                  this.tr("Automatically generate a concise title for newly created runs.", "为新建的 Drove Run 自动生成简短标题。"),
                  this.toggle(t, "st-dr-pol-title", autoTitle, (_e, cx) => this.saveDrovePatch({ ai: { autoTitle: !autoTitle } }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Task Enrich", "任务语义补全"),
                  this.tr("Enrich task specifications and metadata with semantic hints.", "自动补全任务描述与语义标签信息。"),
                  this.toggle(t, "st-dr-pol-enrich", taskEnrich, (_e, cx) => this.saveDrovePatch({ ai: { taskEnrich: !taskEnrich } }, cx)),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Conflict Policy", "写冲突策略"),
                  this.tr("How to handle concurrent file edits across tasks in the same workspace.", "多任务并发修改同一工作区文件时的冲突处理方式。"),
                  this.drovePicker(
                    t,
                    "pol-conflict",
                    conflictLabel,
                    conflictOptions,
                    (val, cx) => this.saveDrovePatch({ ai: { conflictPolicy: val } }, cx),
                    { width: 240, popupWidth: 280, anchor: "bottom_right" },
                    cx,
                  ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Question Triage", "提问分诊模式"),
                  this.tr("Draft answers for worker questions or auto-reply at high confidence.", "Worker 提问时自动生成草稿或高置信自动回复。"),
                  this.drovePicker(
                    t,
                    "pol-question",
                    questionLabel,
                    questionOptions,
                    (val, cx) => this.saveDrovePatch({ ai: { questionMode: val } }, cx),
                    { width: 240, popupWidth: 280, anchor: "bottom_right" },
                    cx,
                  ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Loop Detect", "死循环检测"),
                  this.tr("Detect repetitive stalling behavior and optionally steer once.", "检测重复空转死循环并决定告警或自动纠偏一次。"),
                  this.drovePicker(
                    t,
                    "pol-loop",
                    loopLabel,
                    loopOptions,
                    (val, cx) => this.saveDrovePatch({ ai: { loopDetect: val } }, cx),
                    { width: 240, popupWidth: 280, anchor: "bottom_right" },
                    cx,
                  ),
                ),
              ),
          ),
      )
      // 4. Magpie Quota Section Card
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Magpie Quota (Optional)", "Magpie 额度（可选）")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Enable Magpie quota integration", "启用 Magpie 额度联动"),
                  this.tr("Off by default. Shows live quota cards in Settings/Web and skips exhausted primary agents.", "默认关闭。开启后将在设置与网页端展示实时额度，并在主 Agent 额度耗尽时自动回退。"),
                  this.toggle(t, "st-dr-magpie-on", magpieOn, (_e, cx) => this.saveDrovePatch({ magpie: { enabled: !magpieOn } }, cx)),
                  true,
                ),
              )
              .when(magpieOn, (el) =>
                el.child(
                  v_flex()
                    .px(14)
                    .py(12)
                    .gap(10)
                    .border_t(1)
                    .border_color(t.border)
                    .child(
                      v_flex()
                        .gap(4)
                        .child(div().text_size(12).text_color(t.dim).child(this.tr(`Gateway URL (blank = ${data?.magpieGatewayResolved ?? "http://127.0.0.1:3425"})`, `网关地址（留空则自动探测 ${data?.magpieGatewayResolved ?? "http://127.0.0.1:3425"}）`)))
                        .child(
                          h_flex()
                            .gap(8)
                            .items_center()
                            .child(div().flex_1().px(8).py(2).rounded(7).border_1().border_color(t.border).bg(t.background).child(Input.new(this.droveMagpieUrlInput).h(26).text_size(12.5).text_color(t.foreground)))
                            .child(this.button(t, "st-dr-mag-save", this.tr("Save URL", "保存地址"), (_e, cx) => {
                              this.saveDrovePatch({ magpie: { enabled: true, gatewayUrl: this.droveMagpieUrlInput.value().trim() } }, cx);
                            }))
                            .child(this.button(t, "st-dr-mag-test", this.droveMagpieTesting ? this.tr("Testing…", "测试中…") : this.tr("Test", "测试连接"), (_e, cx) => this.testDroveMagpie(cx)))
                            .child(this.button(t, "st-dr-mag-refresh", this.droveLoading ? this.tr("Refreshing…", "刷新中…") : this.tr("Refresh Quotas", "刷新额度"), (_e, cx) => {
                              cx.spawn(async (cx) => { await this.loadDrove(); cx.notify(); });
                            })),
                        ),
                    )
                    .when(Boolean(this.droveMagpieResult), (r) => {
                      const mr = this.droveMagpieResult;
                      const prov = (mr.providers ?? []).slice(0, 6).join(", ");
                      return r.child(
                        div()
                          .text_size(12)
                          .text_color(mr.ok ? t.ansi2 : t.blocked)
                          .child(
                            mr.ok
                              ? `✓ ${mr.cardsCount ?? 0} ${this.tr("quota card(s)", "张额度卡片")}${prov ? ` (${prov})` : ""}`
                              : `✗ ${mr.error ?? "Unavailable"}`,
                          ),
                      );
                    })
                    .when(Boolean(data?.magpieError), (r) =>
                      r.child(div().text_size(12).text_color(t.blocked).child(`✗ ${data.magpieError}`)),
                    )
                    .when(magpieQuotas.length > 0, (r) =>
                      r.child(
                        v_flex()
                          .gap(6)
                          .children(
                            magpieQuotas.slice(0, 8).map((card, idx) => {
                              const wins = (card.windows ?? []).filter((w) => !w.text).slice(0, 3);
                              return h_flex()
                                .id(`st-dr-mq-${idx}`)
                                .px(10)
                                .py(7)
                                .rounded(7)
                                .border_1()
                                .border_color(t.border)
                                .bg(t.background)
                                .items_center()
                                .justify_between()
                                .gap(10)
                                .child(
                                  h_flex()
                                    .gap(6)
                                    .items_center()
                                    .min_w(0)
                                    .child(div().text_size(12.5).font_semibold().text_color(t.foreground).child(card.name || card.provider))
                                    .when(Boolean(card.plan), (el) => el.child(div().px(5).py(1).rounded(4).bg(t.raised).text_size(10.5).text_color(t.dim).child(card.plan)))
                                    .when(Boolean(card.balance), (el) => el.child(div().text_size(11.5).text_color(t.muted).child(card.balance))),
                                )
                                .child(
                                  h_flex()
                                    .gap(8)
                                    .items_center()
                                    .children(
                                      wins.map((w) => {
                                        const pct = w.unlimited ? "∞" : `${Math.round(Math.max(0, Math.min(100, w.remaining ?? 0)))}%`;
                                        const color = w.unlimited ? t.ansi2 : (w.remaining ?? 0) <= 0 ? t.blocked : (w.remaining ?? 0) <= 20 ? t.working : t.ansi2;
                                        return div().text_size(11.5).text_color(color).child(`${w.name}: ${pct}`);
                                      }),
                                    ),
                                );
                            }),
                          ),
                      ),
                    ),
                ),
              ),
          ),
      );
  }

  // Skills ---------------------------------------------------------------

  async loadSkills() {
    if (this.skillLoading || (typeof checking === "function" && checking())) return;
    this.skillLoading = true;
    this.skillError = null;
    try {
      const r = await hc(["skill", "status", "--json"]);
      let parsed = null;
      try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
      if (r.code !== 0) {
        throw new Error(parsed?.error?.message || (r.stderr || r.stdout || "Failed to inspect skills.").trim());
      }
      this.skillData = parsed;
    } catch (e) {
      this.skillError = String(e?.message ?? e);
    } finally {
      this.skillLoading = false;
    }
  }

  installSkills(opts, cx) {
    if (this.skillInstalling) return;
    const tag = opts?.target ? "custom" : opts?.agent ? opts.agent : "all";
    this.skillInstalling = tag;
    this.skillError = null;
    this.skillResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const args = ["skill", "install", "--json"];
        if (opts?.target) args.push("--target", opts.target);
        else if (opts?.agent) args.push("--agent", opts.agent);
        else args.push("--all");
        const r = await hc(args);
        let parsed = null;
        try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
        if (r.code !== 0) {
          throw new Error(parsed?.error?.message || (r.stderr || r.stdout || "Skill installation failed.").trim());
        }
        if (parsed?.status) this.skillData = parsed.status;
        else await this.loadSkills();
        this.skillResult = {
          ok: true,
          usedAi: false,
          summary: this.tr(
            `Installed ${parsed?.installedCount ?? 0} skill file(s) across ${(parsed?.targets ?? []).length} target(s).`,
            `已为 ${(parsed?.targets ?? []).length} 个目标目录安装 ${parsed?.installedCount ?? 0} 个 Skill 文件。`,
          ),
          targets: parsed?.targets ?? [],
          tips: [],
        };
      } catch (e) {
        this.skillError = String(e?.message ?? e);
      } finally {
        this.skillInstalling = false;
        cx.notify();
      }
    });
  }

  aiInstallSkills(cx) {
    if (this.skillInstalling) return;
    this.skillInstalling = "ai";
    this.skillError = null;
    this.skillResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const hint = String(this.skillAiHintInput?.value?.() ?? "").trim();
        const args = ["skill", "ai-install", "--lang", this.lang(), "--json"];
        if (hint) args.push("--query", hint);
        const r = await hc(args);
        let parsed = null;
        try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
        if (r.code !== 0) {
          throw new Error(parsed?.error?.message || (r.stderr || r.stdout || "AI skill installation failed.").trim());
        }
        if (parsed?.status) this.skillData = parsed.status;
        else await this.loadSkills();
        this.skillResult = {
          ok: true,
          usedAi: Boolean(parsed?.usedAi),
          model: parsed?.model ?? null,
          aiError: parsed?.aiError ?? null,
          summary: parsed?.summary || this.tr("Companion skills installed.", "配套 Skill 已安装完成。"),
          targets: parsed?.targets ?? [],
          tips: Array.isArray(parsed?.tips) ? parsed.tips : [],
        };
      } catch (e) {
        this.skillError = String(e?.message ?? e);
      } finally {
        this.skillInstalling = false;
        cx.notify();
      }
    });
  }

  skills(t, cx) {
    const data = this.skillData ?? {};
    const bundled = Array.isArray(data.bundledSkills) ? data.bundledSkills : [
      { id: "herdr2", summaryEn: "Core h2 CLI orchestration & ACP sub-agents", summaryZh: "核心 h2 CLI 编排与无头 ACP 子智能体" },
      { id: "drove", summaryEn: "Multi-agent DAG orchestration", summaryZh: "Drove 多智能体 DAG 编排" },
      { id: "drove-add", summaryEn: "Append tasks to active Drove Runs", summaryZh: "向 Drove Run 追加新任务" },
      { id: "drove-setup", summaryEn: "Configure Drove roles & AI", summaryZh: "配置 Drove 角色与 AI" },
    ];
    const agents = Array.isArray(data.agents) ? data.agents : [];
    const aiReady = Boolean(this.aiStatus?.configured);
    const busy = Boolean(this.skillInstalling);
    const primaryBtn = (id, label, onClick, disabled = false) =>
      div()
        .id(id)
        .px(12)
        .py(6)
        .rounded(7)
        .text_size(12.5)
        .font_semibold()
        .bg(t.accent)
        .text_color(t.accent_foreground)
        .when(disabled, (el) => el.opacity(0.55))
        .when(!disabled, (el) => el.cursor_pointer().hover((s) => s.opacity(0.9)).on_click(onClick))
        .child(label);

    const statusBadge = (st) => {
      if (st === "up_to_date") return [this.tr("Up to date", "已是最新"), t.ansi2];
      if (st === "outdated") return [this.tr("Update available", "有新版可更新"), t.working];
      if (st === "partial") return [this.tr("Partial", "部分已安装"), t.working];
      return [this.tr("Not installed", "未安装"), t.dim];
    };

    return v_flex()
      .w_full()
      .gap(16)
      // 1. Companion Skills Overview & One-Click Install
      .child(
        v_flex()
          .w_full()
          .gap(10)
          .p(14)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.surface)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(10)
              .child(
                v_flex()
                  .gap(2)
                  .child(div().text_size(14).font_semibold().text_color(t.foreground).child(this.tr("Herdr2 Companion Skills", "Herdr2 配套 Skill 一键安装")))
                  .child(
                    div()
                      .text_size(12.2)
                      .text_color(t.dim)
                      .child(
                        this.tr(
                          "Install bundled `herdr2` + `drove` skills so your coding agents know how to orchestrate `h2` CLI, ACP sub-agents, and Drove DAGs.",
                          "一键安装内置的 `herdr2` 与 `drove` 配套 Skill，让本机各类编程 Agent 自动掌握 `h2` CLI、无头子智能体编排与 Drove DAG 协作。",
                        ),
                      ),
                  ),
              )
              .child(
                h_flex()
                  .gap(8)
                  .flex_none()
                  .items_center()
                  .child(
                    this.button(
                      t,
                      "st-skill-refresh",
                      this.skillLoading ? this.tr("Scanning…", "扫描中…") : this.tr("Rescan", "重新扫描"),
                      (_e, cx) => { cx.spawn(async (cx) => { await this.loadSkills(); cx.notify(); }); },
                    ),
                  )
                  .child(
                    primaryBtn(
                      "st-skill-install-all",
                      this.skillInstalling === "all"
                        ? this.tr("Installing…", "安装中…")
                        : this.tr("Install to All Detected", "一键安装到已检测 Agent"),
                      (_e, cx) => this.installSkills({ all: true }, cx),
                      busy,
                    ),
                  ),
              ),
          )
          .child(
            h_flex()
              .gap(6)
              .flex_wrap()
              .children(
                bundled.map((sk) =>
                  div()
                    .px(8)
                    .py(3)
                    .rounded(6)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .text_size(11.5)
                    .text_color(t.foreground)
                    .tooltip(this.tr(sk.summaryEn || "", sk.summaryZh || ""))
                    .child(sk.id),
                ),
              ),
          ),
      )
      .when(Boolean(this.skillError), (el) =>
        el.child(div().px(12).py(8).rounded(8).border_1().border_color(t.blocked).bg(t.surface).text_size(12.5).text_color(t.blocked).child(this.skillError)),
      )
      .when(Boolean(this.skillResult), (el) =>
        el.child(
          v_flex()
            .gap(6)
            .p(12)
            .rounded(9)
            .border_1()
            .border_color(t.ansi2)
            .bg(t.ansi2 + "14")
            .child(
              h_flex()
                .items_center()
                .justify_between()
                .gap(8)
                .child(div().text_size(12.8).font_semibold().text_color(t.ansi2).child(this.skillResult.summary))
                .when(Boolean(this.skillResult.usedAi && this.skillResult.model), (b) =>
                  b.child(div().text_size(11).text_color(t.dim).child(`AI · ${this.skillResult.model}`)),
                ),
            )
            .when(Boolean(this.skillResult.aiError), (b) =>
              b.child(div().text_size(11.8).text_color(t.working).child(this.skillResult.aiError)),
            )
            .children(
              (this.skillResult.targets ?? []).map((item) =>
                div()
                  .text_size(11.8)
                  .text_color(t.foreground)
                  .child(`• ${item.agent}: ${item.target}${item.reason ? ` — ${item.reason}` : ""}`),
              ),
            )
            .children(
              (this.skillResult.tips ?? []).map((tip) =>
                div().text_size(11.5).text_color(t.muted).child(`→ ${tip}`),
              ),
            ),
        ),
      )
      // 2. AI-Assisted Skill Installer Card
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("AI Skill Installation Assistant", "✨ 自带 AI 智能安装助手")))
          .child(
            v_flex()
              .w_full()
              .gap(10)
              .p(14)
              .rounded(10)
              .border_1()
              .border_color(t.accent)
              .bg(t.surface)
              .child(
                div()
                  .text_size(12.5)
                  .line_height("18px")
                  .text_color(t.muted)
                  .child(
                    this.tr(
                      "Use Herdr2's built-in Core AI to inspect local CLIs, config directories, and non-standard agent tools, then automatically install and verify all Herdr2 skills.",
                      "调用 Herdr2 内置核心 AI 能力，自动扫描本机编程 CLI、配置目录及非标准工具，智能规划目标路径并完成全套 Skill 安装与校验。",
                    ),
                  ),
              )
              .child(
                h_flex()
                  .gap(8)
                  .items_center()
                  .child(
                    div()
                      .flex_1()
                      .px(8)
                      .py(3)
                      .rounded(6)
                      .border_1()
                      .border_color(t.border)
                      .bg(t.background)
                      .child(Input.new(this.skillAiHintInput).h(26).text_size(12.5).text_color(t.foreground)),
                  )
                  .child(
                    aiReady
                      ? primaryBtn(
                          "st-skill-ai-install",
                          this.skillInstalling === "ai"
                            ? this.tr("AI Installing…", "AI 分析安装中…")
                            : this.tr("✨ AI Auto-Install", "✨ AI 智能安装"),
                          (_e, cx) => this.aiInstallSkills(cx),
                          busy,
                        )
                      : this.button(
                          t,
                          "st-skill-goto-ai",
                          this.tr("Configure AI first…", "先配置 AI 助手…"),
                          (_e, cx) => {
                            this.section = "ai";
                            cx.spawn(async (cx) => { await this.loadAi(); cx.notify(); });
                            cx.notify();
                          },
                        ),
                  ),
              ),
          ),
      )
      // 3. Detected Local Agent Targets
      .child(
        v_flex()
          .w_full()
          .gap(6)
          .child(this.heading(t, this.tr("Local Coding Agents", "本机编程 Agent 检测列表")))
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .children(
                agents.map((a, idx) => {
                  const [stLabel, stColor] = statusBadge(a.status);
                  const actLabel =
                    this.skillInstalling === a.id
                      ? this.tr("Installing…", "安装中…")
                      : a.status === "up_to_date"
                        ? this.tr("Reinstall", "重装")
                        : a.status === "outdated" || a.status === "partial"
                          ? this.tr("Update", "更新")
                          : this.tr("Install", "安装");
                  return h_flex()
                    .w_full()
                    .px(14)
                    .py(10)
                    .gap(12)
                    .items_center()
                    .when(idx > 0, (el) => el.border_t(1).border_color(t.border))
                    .child(div().w(7).h(7).flex_none().rounded(4).bg(a.detected ? t.ansi2 : t.dim))
                    .child(
                      v_flex()
                        .flex_1()
                        .min_w(0)
                        .gap(2)
                        .child(
                          h_flex()
                            .gap(8)
                            .items_center()
                            .child(div().text_size(13).font_semibold().text_color(t.foreground).child(a.name))
                            .when(a.detected, (el) =>
                              el.child(
                                div()
                                  .px(6)
                                  .py(1)
                                  .rounded(4)
                                  .bg(t.ansi2 + "1c")
                                  .text_size(10.5)
                                  .text_color(t.ansi2)
                                  .child(this.tr("Detected", "已检测到")),
                              ),
                            )
                            .child(
                              div()
                                .px(6)
                                .py(1)
                                .rounded(4)
                                .bg(t.background)
                                .border_1()
                                .border_color(t.border)
                                .text_size(10.5)
                                .text_color(stColor)
                                .child(stLabel),
                            ),
                        )
                        .child(div().truncate().text_size(11.5).text_color(t.dim).child(a.skillsDir)),
                    )
                    .child(
                      this.button(t, `st-skill-inst-${a.id}`, actLabel, (_e, cx) => {
                        if (!busy) this.installSkills({ agent: a.id }, cx);
                      }),
                    );
                }),
              ),
          ),
      )
      // 4. Custom Directory Install
      .child(
        v_flex()
          .w_full()
          .gap(8)
          .child(this.heading(t, this.tr("Custom Skills Directory", "安装到自定义目录")))
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(
                div()
                  .flex_1()
                  .px(8)
                  .py(3)
                  .rounded(6)
                  .border_1()
                  .border_color(t.border)
                  .bg(t.background)
                  .child(Input.new(this.skillCustomDirInput).h(26).text_size(12.5).text_color(t.foreground)),
              )
              .child(
                this.button(
                  t,
                  "st-skill-install-custom",
                  this.skillInstalling === "custom" ? this.tr("Installing…", "安装中…") : this.tr("Install Here", "安装到此目录"),
                  (_e, cx) => {
                    const dir = String(this.skillCustomDirInput?.value?.() ?? "").trim();
                    if (!dir) {
                      this.skillError = this.tr("Please enter a target directory path.", "请先输入目标目录路径。");
                      cx.notify();
                      return;
                    }
                    this.installSkills({ target: dir }, cx);
                  },
                ),
              ),
          ),
      );
  }

  // Alerts ---------------------------------------------------------------

  alerts(t) {
    const sounds = this.person.sounds ?? {};
    const on = sounds.enabled !== false;
    const file = (f) => (f ? f.split(/[\\/]/).pop() : this.tr("Herdr's sound", "默认提示音"));
    return v_flex()
      .w_full()
      .child(this.heading(t, this.tr("Sounds", "提示音")))
      .child(
        this.row(
          t,
          this.tr("Play sounds", "播放提示音"),
          this.tr("When an agent needs you, and when one finishes while you're not looking. A note shows in the corner either way.", "当智能体需要你的确认，或在后台完成任务时播放提示音（右下角通知始终显示）。"),
          this.toggle(t, "st-sounds", on, (_e, cx) => this.change((p) => (p.sounds = { ...(p.sounds ?? {}), enabled: !on }), cx)),
        ),
      )
      .child(this.row(t, this.tr("An agent needs you", "智能体需要确认时"), file(sounds.blocked), this.button(t, "st-play-blocked", this.tr("Play", "试听"), () => playSound("blocked"))))
      .child(this.row(t, this.tr("An agent finished", "智能体完成任务时"), file(sounds.done), this.button(t, "st-play-done", this.tr("Play", "试听"), () => playSound("done"))))
      .child(div().pt(12).text_size(12.5).line_height("18px").text_color(t.dim).child(this.tr("Want your own sounds? Ask your agent to set sounds.done and sounds.blocked in settings.json to an MP3.", "如需自定义提示音，可在 settings.json 中配置 sounds.done 和 sounds.blocked 为 MP3 文件路径。")));
  }

  // Updates --------------------------------------------------------------

  updates(t, cx) {
    const u = updates();
    const ui = u.ui ?? {};
    const c = u.core ?? {};
    const at = (secs) => {
      const d = new Date(secs * 1000);
      return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    };
    const checked = u.checked ? this.tr(` Checked at ${at(u.checked)}.`, ` 上次检查：${at(u.checked)}。`) : "";
    const result = u.checking ? this.tr("Checking\u2026", "检查中…") : u.checked
      ? this.tr(`Up to date.${checked}`, `已是最新版本。${checked}`)
      : u.checks
        ? this.tr("Ready to check. Click Check now to query the release server.", "就绪，点击「立即检查」查询更新源。")
        : this.tr("Configure a releases URL below to enable updates.", "请在下方配置更新源地址以开启自动检查。");
    const asked = this.asked && Date.now() - this.asked < 3000;
    const check = (id) =>
      !u.checks
        ? null
        : u.checking && this.asked
          ? div().px(12).py(5).text_size(13).text_color(t.dim).child(this.tr("Checking\u2026", "检查中…"))
          : asked && u.checked && !u.checking
            ? div().px(12).py(5).text_size(13).text_color(t.ansi2).child(this.tr("\u2713 Up to date", "✓ 已是最新"))
            : this.button(t, id, this.tr("Check now", "立即检查"), (_e, cx) => {
                this.asked = Date.now();
                run("app.check_updates");
                cx.notify();
              });
    const primary = (id, label, f) =>
      div().id(id).px(12).py(5).rounded(7).cursor_pointer().bg(t.accent).text_color(t.accent_foreground).text_size(13).font_semibold().hover((s) => s.opacity(0.9)).on_click(f).child(label);

    const uiMode = this.person.ui_hot_update ?? ui.mode ?? "prompt";
    const uiCard = u.dev_defaults
      ? [
          this.tr(`Dev live-reload active: ${u.dev_defaults}`, `开发源码直连热重载已开启：${u.dev_defaults}`),
          this.button(t, "st-reload-mods", this.tr("Reload mods", "重载 Mods"), () => run("app.reload_mods")),
        ]
      : ui.applying
        ? [this.tr("Applying UI hot-update\u2026 terminals stay running.", "正在免重启热更新 UI… 终端不会中断。"), null]
        : ui.error
          ? [this.tr(`Last UI update failed: ${ui.error}`, `上次 UI 热更新失败：${ui.error}`), check("st-check-ui")]
          : ui.ready
            ? [
                this.tr(`${ui.ready} is ready. Applies live in ~200ms without restarting.`, `${ui.ready} 已就绪，约 200ms 免重启热生效，不中断终端。`),
                primary("st-ui-apply", this.tr("Hot-update UI", "立即热更新 UI"), () => run("app.apply_ui_update")),
              ]
            : [
                ui.active_override
                  ? this.tr(`Hot-updated bundle active (base app ${fmtVer(ui.app_version)}).${checked}`, `当前运行热更新 UI 包（底座 ${fmtVer(ui.app_version)}）。${checked}`)
                  : result,
                check("st-check-ui"),
              ];

    const app = !u.app.updates
      ? [
          u.releases_url
            ? this.tr("Source / portable binary: app self-swap applies only to installed packages.", "源码/便携运行模式：App 二进制替换仅对正式安装目录生效。")
            : this.tr("No releases URL configured. Configure one below or build from source.", "未配置更新源地址：可在下方填写更新源或通过源码更新。"),
          this.button(t, "st-app-restart", this.tr("Restart window", "热重启窗口"), () => run("app.restart")),
        ]
      : u.app.ready
        ? [this.tr(`${u.app.ready} is ready. Restart to switch now; it also switches when you quit.`, `${u.app.ready} 已就绪，重启后立即生效。`), primary("st-app-update", this.tr("Restart to update", "重启更新"), () => run("app.update"))]
        : [result, check("st-check-app")];

    const core = c.managed
      ? [this.tr(`Managed by ${c.managed}: update it there.${c.ready ? ` ${c.ready} is available.` : ""}`, `由 ${c.managed} 托管：请通过包管理器更新。${c.ready ? ` 已有新版本 ${c.ready}。` : ""}`), u.checks ? check("st-check-core") : null]
      : c.updating
        ? [this.tr("Updating\u2026 your terminals keep running.", "更新中… 终端会话不会中断。"), null]
        : c.error
          ? [this.tr(`The update failed: ${c.error}`, `更新失败：${c.error}`), primary("st-core-retry", this.tr("Try again", "重试"), () => run("app.update_core"))]
          : c.ready
            ? [this.tr(`${c.ready} is ready. Your terminals keep running through it.`, `${c.ready} 已就绪，更新过程不会中断现有终端。`), primary("st-core-update", this.tr("Update herdr2-core", "更新 herdr2-core"), () => run("app.update_core"))]
            : u.checks
              ? [result, check("st-check-core")]
              : [this.tr("Not checked: configure a releases URL below to check core updates.", "未检查：配置更新源地址后可一键热升级 herdr2-core。"), null];

    const modeOptions = [
      ["prompt", this.tr("Prompt", "提示后应用")],
      ["auto", this.tr("Auto", "静默自动")],
      ["off", this.tr("Off", "关闭")],
    ];
    const savedNotice = this.releasesUrlSavedAt && Date.now() - this.releasesUrlSavedAt < 3000;

    return v_flex()
      .w_full()
      .child(this.heading(t, this.tr("UI Hot-Update (No Restart)", "UI 热更新（免重启）")))
      .child(this.row(t, `UI ${fmtVer(ui.version || u.app.version)}`.trim(), uiCard[0], uiCard[1] ?? div()))
      .child(
        this.row(
          t,
          this.tr("UI hot-update mode", "UI 热更新策略"),
          this.tr("Choose whether verified UI bundles prompt in the corner or apply live automatically.", "选择校验通过的 UI 资源包是弹出胶囊确认，还是自动热生效。"),
          h_flex()
            .gap(4)
            .children(
              modeOptions.map(([val, label]) => {
                const active = uiMode === val;
                return div()
                  .id(`st-ui-mode-${val}`)
                  .px(10)
                  .py(5)
                  .rounded(6)
                  .cursor_pointer()
                  .text_size(12.5)
                  .border_1()
                  .border_color(active ? t.accent : t.border)
                  .bg(active ? t.accent : t.surface)
                  .text_color(active ? t.accent_foreground : t.foreground)
                  .on_click((_e, cx) => {
                    this.change((p) => {
                      if (val === "prompt") delete p.ui_hot_update;
                      else p.ui_hot_update = val;
                    }, cx);
                  })
                  .child(label);
              }),
            ),
        ),
      )
      .child(div().h(18))
      .child(this.heading(t, "Herdr2"))
      .child(this.row(t, `Herdr2 ${fmtVer(u.app.version)}`.trim(), app[0], app[1] ?? div()))
      .child(div().h(18))
      .child(this.heading(t, "herdr2-core"))
      .child(this.row(t, `herdr2-core ${fmtVer(c.version)}`.trim(), core[0], core[1] ?? div()))
      .child(div().h(18))
      .child(this.heading(t, this.tr("Release Server", "更新源配置")))
      .child(
        v_flex()
          .gap(8)
          .pt(6)
          .child(
            div()
              .text_size(12.5)
              .text_color(t.dim)
              .child(
                this.tr(
                  "Base URL serving app/latest.json, ui/latest.json, and core/latest.json (HERDR2_RELEASES_URL env overrides this).",
                  "提供 app/latest.json、ui/latest.json、core/latest.json 的静态服务地址（环境变量 HERDR2_RELEASES_URL 优先级更高）。",
                ),
              ),
          )
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(
                div()
                  .flex_1()
                  .px(8)
                  .py(3)
                  .rounded(6)
                  .border_1()
                  .border_color(t.border)
                  .bg(t.background)
                  .child(Input.new(this.releasesUrlInput).h(26).text_size(13).text_color(t.foreground)),
              )
              .child(
                this.button(t, "st-gh-releases-url", this.tr("GitHub OTA", "使用 GitHub 热更源"), (_e, cx) => {
                  const ghUrl = "https://github.com/herdr2/herdr2/releases/download/ota-latest";
                  this.releasesUrlInput.set_value(ghUrl);
                  this.change((p) => { p.releases_url = ghUrl; }, cx);
                  this.releasesUrlSavedAt = Date.now();
                  this.asked = Date.now();
                  run("app.check_updates");
                  cx.notify();
                }),
              )
              .child(
                this.button(t, "st-save-releases-url", savedNotice ? this.tr("\u2713 Saved", "✓ 已保存") : this.tr("Save & Check", "保存并检查"), (_e, cx) => {
                  const val = this.releasesUrlInput.value().trim().replace(/\/+$/, "");
                  this.change((p) => {
                    if (val) p.releases_url = val;
                    else delete p.releases_url;
                  }, cx);
                  this.releasesUrlSavedAt = Date.now();
                  if (val) {
                    this.asked = Date.now();
                    run("app.check_updates");
                  }
                  cx.notify();
                }),
              ),
          ),
      )
      .child(div().pt(12).text_size(12.5).line_height("18px").text_color(t.dim).child(this.tr("Herdr2 checks for App, UI bundle, and Core updates every hour. herdr2-core runs your terminals, so they keep running through UI and Core updates.", "Herdr2 每小时检查一次桌面端、UI 资源包与 Core 更新；UI 与 Core 热更新全程不中断现有终端会话。")));
  }

  // ----------------------------------------------------------------------

  syncRecordedShortcut(cx) {
    if (!this.recordingTarget) return;
    const rec = shared("settings.recorded_shortcut");
    const seq = Number(rec?.seq ?? 0);
    if (seq > this.lastRecordedSeq && rec?.chord) {
      this.lastRecordedSeq = seq;
      this.applyRecordedShortcut(String(rec.chord), cx);
      return;
    }
    if (shared("settings.recording_shortcut") === false) {
      this.recordingTarget = null;
    }
  }

  render(cx) {
    this.syncRecordedShortcut(cx);
    const t = theme();
    const nav = v_flex()
      .flex_none()
      .w(190)
      .p(10)
      .gap(2)
      .border_r(1)
      .border_color(t.border)
      .child(div().px(10).pt(6).pb(12).text_size(16).font_semibold().text_color(t.foreground).child(this.tr("Settings", "设置")))
      .children(
        SECTIONS().map((s) => {
          const on = s.id === this.section;
          return h_flex()
            .id(`st-s-${s.id}`)
            .gap(10)
            .px(10)
            .py(7)
            .items_center()
            .rounded(7)
            .cursor_pointer()
            .bg(on ? t.raised : "#00000000")
            .hover((st) => st.bg(t.raised))
            .on_click((_e, cx) => {
              if (this.recordingTarget) this.cancelRecordingShortcut(null);
              this.section = s.id;
              this.picking = null;
              this.machineEdit = null;
              this.handle.focus();
              if (s.id === "keyboard") cx.spawn(async (cx) => { await this.loadKeys(); cx.notify(); });
              if (s.id === "machines") cx.spawn(async (cx) => { await this.loadMachines(); cx.notify(); });
              if (s.id === "ai") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await this.loadAi();
                cx.notify();
              });
              if (s.id === "drove") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await this.loadDrove();
                this.maybeAutoProbeDrove(cx);
                cx.notify();
              });
              if (s.id === "skills") cx.spawn(async (cx) => {
                await Promise.all([this.loadSkills(), this.loadAi()]);
                cx.notify();
              });
              cx.notify();
            })
            .child(svg(I(s.icon)).size(15).text_color(on ? t.accent : t.dim))
            .child(div().flex_1().text_size(13.5).text_color(on ? t.foreground : t.muted).child(this.tr(s.title, s.zh)));
        }),
      );
    return h_flex()
      .id("settings")
      .track_focus(this.handle)
      .when(Boolean(this.picking), (el) => el.on_action("overlay.close", (_e, cx) => {
        this.picking = null;
        this.handle.focus();
        cx.stop_propagation();
        cx.notify();
      }))
      .w_full()
      .h(620)
      .min_h(0)
      .items_start()
      .on_key_down((e, cx) => {
        const k = e.key;
        if (this.recordingTarget) {
          if (k === "escape") {
            this.cancelRecordingShortcut(cx);
            cx.stop_propagation();
          }
          return;
        }
        if (this.machineEdit) {
          if (k === "escape") this.machineEdit = null;
          else return;
          this.handle.focus();
          cx.stop_propagation();
          cx.notify();
          return;
        }
        if (k === "escape") {
          if (this.picking) this.picking = null;
          else return this.close();
          this.handle.focus();
        } else return;
        cx.stop_propagation();
        cx.notify();
      })
      .child(nav.h_full())
      .child(
        // A bar that always shows: the pages are taller than the dialog and
        // people should see that they scroll.
        div()
          .relative()
          .flex_1()
          .min_w(0)
          .h_full()
          .child(
            div()
              .id("st-body")
              .size_full()
              .overflow_y_scroll()
              // Padding inside, not on the scroll area: there it counts as
              // content and a page that fits still shows a bar.
              .child(
                div()
                  .px(28)
                  .py(22)
                  .child(
                    this.section === "look"
                      ? this.look(t, cx)
                      : this.section === "keyboard"
                        ? this.keyboard(t, cx)
                      : this.section === "machines"
                        ? this.machines(t)
                      : this.section === "ai"
                        ? this.ai(t, cx)
                      : this.section === "drove"
                        ? this.drove(t, cx)
                      : this.section === "skills"
                        ? this.skills(t, cx)
                      : this.section === "alerts"
                        ? this.alerts(t)
                      : this.section === "updates"
                        ? this.updates(t, cx)
                        : this.look(t, cx),
                  ),
              ),
          )
          .child(Scrollbar.vertical("st-body").mode("always").absolute().inset_0()),
      );
  }
}
