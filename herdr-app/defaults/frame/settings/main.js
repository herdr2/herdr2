// App-owned settings mod. Writes changes immediately; no Save step.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, Scrollbar, InputState, Input, TextareaState, Textarea, Popup } from "gpui-base";
import { hc, run, theme, themes, fonts, updates, telemetry, settings, layouts, layout, spaces, shared, setShared, appHome, readTextFile, writeTextFile, playSound, props, closeOverlay, notify, window, checking } from "herdr";

const I = (name) => `frame/settings/${name}.svg`;

const SECTIONS = () => [
  { id: "look", title: "Look", zh: "外观", icon: "palette" },
  ...(window()?.platform === "macos" ? [{ id: "keyboard", title: "Keyboard", zh: "键盘", icon: "keyboard" }] : []),
  { id: "machines", title: "Machines", zh: "主机", icon: "server" },
  { id: "ai", title: "AI", zh: "AI 助手", icon: "sparkles" },
  { id: "drove", title: "Drove", zh: "Drove", icon: "layers" },
  { id: "alerts", title: "Alerts", zh: "提醒", icon: "bell" },
  { id: "privacy", title: "Privacy", zh: "隐私", icon: "shield" },
];

const DROVE_BUILTIN_ROLES = ["coder", "tester", "writer", "illustrator", "reviewer", "researcher"];
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
  zcode: { path: "icons/agent.svg", tint: true },
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
    this.showData = false;
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
    this.droveAiConfigMode = null; // null | "all" | "models" | "strengths"
    this.droveAiConfiguring = false;
    this.droveAiRunningMode = null; // null | "all" | "models" | "strengths"
    this.droveAiStage = null; // null | "probing" | "generating"
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
    this.droveUrlInput = InputState.new({ placeholder: "https://api.example.com/v1" });
    this.droveKeyInput = InputState.new({ placeholder: this.tr("API key (stored in 0600 ai.key)", "API Key（保存在 0600 ai.key）") });
    this.droveKeyInput.set_masked?.(true);
    this.droveKeyInput.on("change", () => { this.droveKeyDirty = true; });
    this.droveMagpieUrlInput = InputState.new({ placeholder: "http://127.0.0.1:3425" });
    this.droveNewRoleInput = InputState.new({ placeholder: this.tr("New role id (e.g. architect)", "新角色 ID（如 architect）") });
    this.handle = cx.focus_handle();
    this.handle.focus();
    cx.spawn(async (cx) => {
      await this.load();
      if (!(typeof checking === "function" && checking())) {
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

  keyboard(t) {
    const choices = [
      ["auto", this.tr("Automatic (Alt on US keyboards)", "自动识别（美式键盘作为 Alt）")],
      ["on", "Alt"],
      ["off", this.tr("Types characters", "输入特殊字符")],
      ["left", this.tr("Left Option is Alt", "左 Option 作为 Alt")],
      ["right", this.tr("Right Option is Alt", "右 Option 作为 Alt")],
    ];
    return v_flex()
      .w_full()
      .child(this.heading(t, this.tr("Option key", "Option 键行为")))
      .child(div().pb(12).text_size(13).text_color(t.dim).child(this.tr("Use Option for terminal shortcuts or to type accents and special characters.", "选择将 Option 键用作终端快捷键，还是用于输入重音与特殊字符。")))
      .children(choices.map(([value, label]) => {
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
          .on_click((_e, cx) => this.change((p) => { p.option_as_alt = value; }, cx))
          .child(div().text_color(on ? t.accent : t.muted).child(on ? "◉" : "○"))
          .child(div().text_size(14).text_color(t.foreground).child(label));
      }));
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
    const effMode = mode === "strengths" ? "strengths" : mode === "all" ? "all" : "models";
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
            this.droveRolesBackup = { roles: prevRoles, mode: effMode, at: Date.now() };
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

  restoreDroveRolesBackup(cx) {
    if (!this.droveRolesBackup?.roles || this.droveSaving || this.droveAiConfiguring) return;
    const prev = this.droveRolesBackup.roles;
    this.droveRolesBackup = null;
    this.droveAiConfigResult = { ok: true, restored: true };
    this.saveDrovePatch({ roles: prev }, cx);
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
    }));
  }

  applyRoleChain(role, chain) {
    const clean = (chain ?? [])
      .filter((item) => item && String(item.agent ?? "").trim())
      .map((item) => {
        const strengths = this.normalizeStrengthList(item.strengths ?? []);
        return {
          agent: String(item.agent).trim(),
          model: item.model ? String(item.model) : null,
          effort: item.effort ?? "high",
          ...(item.fast ? { fast: true } : {}),
          ...(item.context ? { context: String(item.context).trim() } : {}),
          ...(strengths.length > 0 ? { strengths } : {}),
        };
      });
    if (clean.length === 0) return;
    role.primary = clean[0];
    role.fallbacks = clean.slice(1);
  }

  droveStrengthPicker(t, id, roleId, currentStrengths, onChange, cx) {
    const key = `drove-str:${id}`;
    const open = this.picking === key;
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
            .w(320)
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
        .child(this.droveStrengthPicker(t, `${roleId}-c${idx}`, roleId, strengths, setStrengths, cx));

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
          .when(Boolean(role.summary), (el) =>
            el.child(div().text_size(11.5).text_color(t.dim).child(role.summary)),
          ),
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
    const handoffDistill = switches.handoffDistill !== false;
    const checkpointLabeling = switches.checkpointLabeling !== false;
    const semanticSummary = switches.semanticSummary !== false;
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
                    (model, cx) => this.saveAiPatch({ model: model || "" }, cx),
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
                this.droveCardRow(
                  t,
                  this.tr("Prompt KV-Cache Optimization", "Prompt 缓存与多轮会话复用"),
                  this.tr("Share a byte-immutable >1024-token system prefix, append-only Run/Attempt sessions, and prompt_cache_key to cut token cost.", "统一静态系统前缀（>1024 tokens）、追加式多轮会话复用与 prompt_cache_key，最大化缓存命中率。"),
                  this.toggle(t, "st-ai-sw-cache", promptCache, (_e, cx) => this.saveAiPatch({ promptCacheEnabled: !promptCache }, cx)),
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
                            "One-click AI auto-configure: enter optional preferences and configure both Role Agents/Models and Candidate Strengths together (auto-probes agents if not cached)",
                            "一键调用智能辅助模型，同时自动配置所有角色的模型与擅长任务（支持输入自然语言需求，未缓存模型将先自动检测刷新）",
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
            const aiReady = Boolean((this.droveUrlInput.value().trim() || cfg.ai?.baseUrl?.trim()));
            const activeAiModelLabel = currentAiModel || aiStatus.model || aiModels[0] || this.tr("Auto-detect from endpoint", "自动从服务端获取");
            const hasCache = this.hasCachedAgentModels();
            const promptPlaceholder = isAllMode
              ? this.tr(
                  "Optional instructions (e.g. coder primary Claude Opus for complex architecture & backend, Gemini for frontend UI, Codex for quick bugfixes; use a different provider for reviewer)…",
                  "可选要求（例如：程序员主力用 Claude Opus 负责复杂架构与后端，备选 Gemini 负责前端 UI、Codex 负责快速修复；审查员使用异构厂商模型；擅长任务用简洁中文标签）…",
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
                            ? this.tr("AI One-Click Configure Role Models & Candidate Strengths", "AI 一键配置各角色的模型与擅长任务")
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
                ? this.tr("role models & strengths", "角色模型与擅长任务")
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

  updates(t) {
    const u = updates();
    const at = (secs) => {
      const d = new Date(secs * 1000);
      return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    };
    const checked = u.checked ? this.tr(` Checked at ${at(u.checked)}.`, ` 上次检查：${at(u.checked)}。`) : "";
    const result = u.checking ? this.tr("Checking\u2026", "检查中…") : u.checked
      ? this.tr(`Up to date.${checked}`, `已是最新版本。${checked}`)
      : this.tr("No successful check. Check your connection and try again.", "检查失败，请检查网络连接后重试。");
    // Pressed: "Checking\u2026" while it looks, then "Up to date" for a moment
    // (a find changes the row instead). One per row: an element shows once.
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
    const app = !u.app.updates
      ? [this.tr("A build from source: it updates with git, not here.", "当前为源码构建版本：请通过 git 拉取更新。"), null]
      : u.app.ready
        ? [this.tr(`${u.app.ready} is ready. Restart to switch now; it also switches when you quit.`, `${u.app.ready} 已就绪，重启后立即生效。`), primary("st-app-update", this.tr("Restart to update", "重启更新"), () => run("app.update"))]
        : [result, check("st-check-app")];
    const c = u.core;
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
              : [this.tr("Not checked: a build from source looks for no updates.", "源码构建版本不检查自动更新。"), null];
    return v_flex()
      .w_full()
      .child(this.heading(t, "Herdr2"))
      .child(this.row(t, `Herdr2 ${fmtVer(u.app.version)}`.trim(), app[0], app[1] ?? div()))
      .child(div().h(18))
      .child(this.heading(t, "herdr2-core"))
      .child(this.row(t, `herdr2-core ${fmtVer(c.version)}`.trim(), core[0], core[1] ?? div()))
      .child(div().pt(12).text_size(12.5).line_height("18px").text_color(t.dim).child(this.tr("Herdr2 looks for both every hour and tells you in the corner. herdr2-core runs your terminals, so they keep running through its updates.", "Herdr2 每小时检查一次更新，并由后台 herdr2-core 守护确保终端会话在更新时不受影响。")));
  }


  // Privacy --------------------------------------------------------------

  // A report as it goes out, one line per line of JSON.
  report(t, id, title, about, report) {
    const lines = JSON.stringify(report, null, 2).split("\n");
    return v_flex()
      .w_full()
      .pt(14)
      .gap(4)
      .child(div().text_size(13.5).text_color(t.foreground).child(title))
      .when(Boolean(about), (el) => el.child(div().text_size(12.5).text_color(t.dim).child(about)))
      .child(
        v_flex()
          .id(id)
          .mt(4)
          .w_full()
          .p(10)
          .rounded(8)
          .border_1()
          .border_color(t.border)
          .bg(t.background)
          .children(lines.map((l) => div().font_family(DEFAULT_FONT).text_size(12).line_height("17px").text_color(t.muted).whitespace_nowrap().child(l || " "))),
      );
  }

  privacy(t) {
    const s = telemetry();
    const on = s ? s.setting : this.person.telemetry !== false;
    const at = (secs) => new Date(secs * 1000).toLocaleString();
    const why = s?.off_because ? this.tr(`Off for now: ${s.off_because} is set where Herdr started.`, `当前已禁用：启动环境中设置了 ${s.off_because}。`) : null;
    const body = v_flex()
      .w_full()
      .child(this.heading(t, this.tr("Usage data", "使用数据")))
      .child(
        this.row(
          t,
          this.tr("Share anonymous usage data", "分享匿名使用数据"),
          why ?? this.tr("Once a day: which agents and features you use, how many spaces and tabs, your system and Herdr's version. Never what's in your terminals, never names of files, folders or projects.", "每天一次：仅统计使用的智能体与功能、工作区与标签页数量、系统及应用版本。绝不采集终端内容、文件名、目录或项目名称。"),
          this.toggle(t, "st-telemetry", on, (_e, cx) =>
            this.change((p) => {
              if (on) p.telemetry = false;
              else delete p.telemetry;
            }, cx),
          ),
        ),
      )
      .child(
        this.row(
          t,
          this.tr("What we send", "上报内容预览"),
          s && !s.sends ? this.tr("This build never sends: it's built from source. You can still see what a release would send.", "源码构建版本不会发送数据，但仍可预览正式版会发送的格式。") : this.tr("Exactly what leaves your computer, word for word.", "逐字展示将离开本机的全部数据内容。"),
          this.button(t, "st-telemetry-show", this.showData ? this.tr("Hide", "隐藏") : this.tr("Show", "查看"), (_e, cx) => {
            this.showData = !this.showData;
            cx.notify();
          }),
        ),
      );
    if (!this.showData) return body;
    if (!s?.enabled) return body.child(div().pt(14).text_size(13).text_color(t.dim).child(this.tr("Nothing is collected or sent while it's off.", "关闭期间不会收集或发送任何数据。")));
    return body
      .children(s.queued.map((r, i) => this.report(t, `st-tq-${i}`, this.tr(`Waiting to send: ${r.day}`, `待发送：${r.day}`), this.tr("Sent as is, soon.", "将按此内容尽快发送。"), r)))
      .when(Boolean(s.draft), (el) => el.child(this.report(t, "st-td", this.tr("Today so far", "今日草稿"), this.tr("A draft: it changes until the day ends (UTC), then goes out once.", "当天结束（UTC）前会持续汇总，之后仅发送一次。"), s.draft)))
      .when(Boolean(s.last_sent), (el) => el.child(this.report(t, "st-tl", this.tr(`Last sent: ${s.last_sent.report.day}`, `上次发送：${s.last_sent.report.day}`), this.tr(`Sent ${at(s.last_sent.at)}.`, `发送于 ${at(s.last_sent.at)}。`), s.last_sent.report)));
  }

  // ----------------------------------------------------------------------

  render(cx) {
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
              this.section = s.id;
              this.picking = null;
              this.machineEdit = null;
              this.handle.focus();
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
                        ? this.keyboard(t)
                      : this.section === "machines"
                        ? this.machines(t)
                      : this.section === "ai"
                        ? this.ai(t, cx)
                      : this.section === "drove"
                        ? this.drove(t, cx)
                      : this.section === "alerts"
                        ? this.alerts(t)
                        : this.section === "privacy"
                          ? this.privacy(t)
                          : this.look(t, cx),
                  ),
              ),
          )
          .child(Scrollbar.vertical("st-body").mode("always").absolute().inset_0()),
      );
  }
}
