// First-run Herdr2 welcome over the live app; records finished/skipped in frame.welcomed.
// Designed after original Herdr: 4 focused, calm pages that each fit in one screen without scrolling.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import {
  spaces,
  run,
  theme,
  props,
  closeOverlay,
  setShared,
  shared,
  h2,
  hc,
  appHome,
  readTextFile,
  writeTextFile,
  settings,
  layout,
  keys,
  updates,
  window as appWindow,
  pickFolder,
  checking,
} from "herdr";
import {
  PAGES,
  LANGUAGES,
  APPEARANCES,
  PANELS,
  SHORTCUT_HIGHLIGHTS,
  resolveLang,
  clampPage,
  resolveEnabledPanels,
  boundShortcut,
  normalizeMachines,
  summarizeAiStatus,
  agentCustomizePrompt,
  fmtVersion,
  findLayoutNode,
} from "./model.js";

const I = (name) => `frame/welcome/${name}.svg`;
const W = (name) => `mods/workspace-chrome/${name}.svg`;
const S = (name) => `frame/settings/${name}.svg`;

function callCore(args) {
  const fn = typeof h2 === "function" ? h2 : typeof hc === "function" ? hc : null;
  return fn ? fn(args).catch(() => null) : Promise.resolve(null);
}

function isChecking() {
  return typeof checking === "function" && Boolean(checking());
}

export default class Welcome extends View {
  init(_props, cx) {
    this.page = 1;
    this.machines = [];
    this.aiStatus = null;
    this.copied = null;
    this.appearance = null;
    this.languageOverride = null;
    this.lastLiveLang = typeof settings === "function" ? (settings()?.language ?? null) : null;
    this.openingFolder = false;
    this.handle = cx.focus_handle();
    this.handle.focus();
    // It opens while the window is still starting, which takes the keyboard
    // back for its first pane: take it again once that's done.
    cx.timer.after(600, () => this.handle.focus());
    cx.spawn(async (cx) => {
      try {
        const raw = await readTextFile(`${appHome()}/settings.json`);
        this.appearance = JSON.parse(raw ?? "{}").appearance ?? null;
      } catch {}
      if (!isChecking()) {
        await Promise.all([this.loadMachines(), this.loadAiStatus()]);
      }
      cx.notify();
    });
    // Live-refresh AI status and paired machines while on Page 3.
    cx.timer.every(3000, (cx) => {
      if (isChecking() || this.page !== 3) return;
      cx.spawn(async (cx) => {
        await Promise.all([this.loadMachines(), this.loadAiStatus()]);
        cx.notify();
      });
    });
  }

  lang() {
    const live = typeof settings === "function" ? (settings()?.language ?? null) : null;
    if (this.languageOverride && live && live !== this.lastLiveLang) {
      this.languageOverride = null;
    }
    if (live) this.lastLiveLang = live;
    return resolveLang(this.languageOverride ? { language: this.languageOverride } : { language: live }, "en");
  }

  tr(en, zh) {
    return this.lang() === "zh-CN" ? zh : en;
  }

  isMac() {
    return typeof appWindow === "function" && appWindow()?.platform === "macos";
  }

  async loadMachines() {
    const r = await callCore(["machine", "ls", "--json"]);
    this.machines = r?.code === 0 ? normalizeMachines(r.stdout) : [];
  }

  async loadAiStatus() {
    const r = await callCore(["ai", "status", "--json"]);
    this.aiStatus = r?.code === 0 ? summarizeAiStatus(r.stdout) : null;
  }

  done(outcome) {
    setShared("frame.welcomed", outcome);
    closeOverlay(props().overlay);
  }

  start() {
    this.done("finished");
    if (spaces().length === 0) run("space.new");
  }

  openProjectFolder(cx) {
    if (this.openingFolder || typeof pickFolder !== "function") {
      this.start();
      return;
    }
    this.openingFolder = true;
    cx.notify();
    cx.spawn(async (cx) => {
      let folder = null;
      try {
        folder = await pickFolder({ prompt: this.tr("Open space", "打开工作区") });
      } catch {}
      this.openingFolder = false;
      if (folder) {
        this.done("finished");
        run("space.new", { folder });
      } else {
        cx.notify();
      }
    });
  }

  setLanguage(value, cx) {
    this.lastLiveLang = typeof settings === "function" ? (settings()?.language ?? null) : null;
    this.languageOverride = value;
    run("app.language", { language: value });
    cx.notify();
  }

  setAppearance(value, cx) {
    this.appearance = value;
    cx.spawn(async (cx) => {
      const file = `${appHome()}/settings.json`;
      for (let i = 0; i < 3; i++) {
        const raw = await readTextFile(file);
        let p = {};
        try {
          p = raw ? JSON.parse(raw) : {};
        } catch {
          return;
        }
        if (value === null) delete p.appearance;
        else p.appearance = value;
        try {
          await writeTextFile(file, JSON.stringify(p, null, 2) + "\n", raw);
          break;
        } catch {}
      }
      cx.notify();
    });
    cx.notify();
  }

  panels() {
    return resolveEnabledPanels(
      shared("workspace.panels"),
      shared("workspace.panels_has_checkpoints"),
      PANELS.map((p) => p.id),
    );
  }

  togglePanel(id) {
    const current = this.panels();
    const on = current.includes(id);
    const next = on ? current.filter((x) => x !== id) : [...current, id];
    setShared("workspace.panels_has_checkpoints", true);
    setShared("workspace.panels", next);
    const L = layout();
    if (!findLayoutNode(L, "panel")) return;
    if (on) {
      if (findLayoutNode(L, "panel-body")?.active === id || next.length === 0) run("layout.hide", { id: "panel" });
    } else {
      run("layout.select", { stack: "panel-body", child: id });
      run("layout.show", { id: "panel" });
    }
  }

  go(page, cx) {
    this.page = clampPage(page);
    if (!isChecking() && this.page === 3) {
      cx.spawn(async (cx) => {
        await Promise.all([this.loadMachines(), this.loadAiStatus()]);
        cx.notify();
      });
    }
    cx.notify();
  }

  // Pieces -------------------------------------------------------------

  para(t, text) {
    return div().w_full().text_size(12.8).line_height("19px").text_color(t.muted).child(text);
  }

  title(t, text, sub) {
    return v_flex()
      .gap(5)
      .child(div().text_size(17.5).font_semibold().text_color(t.foreground).child(text))
      .when(Boolean(sub), (el) => el.child(this.para(t, sub)));
  }

  button(t, id, label, variant, onClick, icon = null) {
    const primary = variant === "primary";
    const outline = variant === "outline";
    return h_flex()
      .id(id)
      .h(32)
      .px(13)
      .gap(7)
      .items_center()
      .rounded(8)
      .cursor_pointer()
      .text_size(12.8)
      .font_medium()
      .when(primary, (el) => el.bg(t.solid).text_color(t.solid_foreground).hover((s) => s.opacity(0.88)))
      .when(outline, (el) => el.bg(t.raised).border_1().border_color(t.border).text_color(t.foreground).hover((s) => s.bg(t.hover)))
      .when(!primary && !outline, (el) => el.text_color(t.muted).hover((s) => s.text_color(t.foreground).bg(t.hover)))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click(onClick)
      .child(label)
      .when(Boolean(icon), (el) => el.child(svg(icon).size(13.5)));
  }

  smallButton(t, id, label, onClick, accent = false) {
    return h_flex()
      .id(id)
      .h(26)
      .px(10)
      .items_center()
      .rounded(7)
      .cursor_pointer()
      .text_size(12)
      .font_medium()
      .border_1()
      .border_color(accent ? t.accent : t.border)
      .bg(accent ? t.accent + "18" : t.raised)
      .text_color(accent ? t.accent : t.foreground)
      .hover((s) => s.bg(accent ? t.accent + "28" : t.hover))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click(onClick)
      .child(label);
  }

  segment(t, prefix, items, now, onPick) {
    return h_flex()
      .p(2)
      .gap(2)
      .rounded(8)
      .border_1()
      .border_color(t.border)
      .bg(t.chrome)
      .children(
        items.map(([value, label]) =>
          div()
            .id(`w-seg-${prefix}-${value ?? "system"}`)
            .px(10)
            .py(3.5)
            .rounded(6)
            .text_size(12.2)
            .cursor_pointer()
            .when(now === value, (el) => el.bg(t.popover).text_color(t.foreground).border_1().border_color(t.border).shadow_sm())
            .when(now !== value, (el) => el.text_color(t.muted).hover((s) => s.text_color(t.foreground)))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => onPick(value, cx))
            .child(label),
        ),
      );
  }

  sizeControl(t) {
    const zoomPct = Math.round((settings()?.zoom ?? 1) * 100);
    const step = (id, label, cmd) =>
      div()
        .id(id)
        .w(26)
        .h(24)
        .rounded(6)
        .flex()
        .items_center()
        .justify_center()
        .text_size(13.5)
        .text_color(t.muted)
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click(() => run(cmd))
        .child(label);
    return h_flex()
      .p(2)
      .gap(2)
      .items_center()
      .rounded(8)
      .border_1()
      .border_color(t.border)
      .bg(t.chrome)
      .child(step("w-zoom-out", "\u2212", "app.zoom_out"))
      .child(div().w(44).text_size(12.2).text_color(t.foreground).flex().justify_center().child(`${zoomPct}%`))
      .child(step("w-zoom-in", "+", "app.zoom_in"));
  }

  feature(t, iconPath, title, body) {
    return h_flex()
      .gap(12)
      .items_start()
      .child(
        div()
          .flex_none()
          .w(30)
          .h(30)
          .rounded(8)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .flex()
          .items_center()
          .justify_center()
          .child(svg(iconPath).size(15).text_color(t.accent)),
      )
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(title))
          .child(this.para(t, body)),
      );
  }

  copyLine(t, text, id = "w-copy") {
    const copied = this.copied === id;
    return h_flex()
      .gap(10)
      .px(11)
      .py(9)
      .rounded(9)
      .bg(t.background)
      .border_1()
      .border_color(t.input)
      .items_center()
      .child(div().flex_1().min_w(0).text_size(12.4).line_height("18px").text_color(t.foreground).child(text))
      .child(
        div()
          .id(id)
          .flex_none()
          .px(10)
          .py(4)
          .rounded(7)
          .cursor_pointer()
          .text_size(12)
          .font_medium()
          .bg(copied ? t.ansi2 + "22" : t.raised)
          .text_color(copied ? t.ansi2 : t.foreground)
          .border_1()
          .border_color(copied ? t.ansi2 : t.border)
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .on_click((_e, cx) => {
            cx.write_to_clipboard(text);
            this.copied = id;
            cx.notify();
          })
          .child(copied ? this.tr("Copied", "已复制") : this.tr("Copy", "复制")),
      );
  }

  keycap(t, label) {
    return div()
      .flex_none()
      .px(7)
      .py(2)
      .rounded(6)
      .border_1()
      .border_color(t.border)
      .bg(t.background)
      .text_size(11.5)
      .font_semibold()
      .text_color(t.foreground)
      .child(label);
  }

  // Page 1: Core Architecture + Quick Preferences ----------------------

  pageOne(t) {
    const langNow = this.lang();
    const apItems = APPEARANCES.map(([value, en, zh]) => [value, this.tr(en, zh)]);
    const prefRow = (label, control) =>
      h_flex()
        .gap(12)
        .items_center()
        .child(div().w(84).text_size(12.8).text_color(t.muted).child(label))
        .child(control);

    return v_flex()
      .gap(14)
      .child(
        this.title(
          t,
          this.tr("Terminals and agents that stay alive in the background", "真终端与无头智能体，关闭窗口也在后台运行"),
          this.tr(
            "Herdr2 runs your interactive terminals (t*) and headless ACP agents (a*) in h2. Close, update, or reopen this window anytime—your work keeps running.",
            "Herdr2 由 h2 后台守护进程托管交互式终端（t*）与无头 ACP 智能体（a*）。关闭、更新或重开窗口，所有会话都会继续运行。",
          ),
        ),
      )
      .child(
        v_flex()
          .gap(11)
          .child(
            this.feature(
              t,
              I("terminal"),
              this.tr("Dual-mode workspace: real terminals & ACP chat", "双模工作区：Ghostty 真终端与结构化 ACP"),
              this.tr(
                "Use native Ghostty/PTY panes for shells and coding CLIs, or launch headless ACP agents with rich chat and inline approvals.",
                "在 Ghostty 真终端运行 Shell 与编程 CLI，或启动无头 ACP 智能体，获得对话视图、附件与行内审批。",
              ),
            ),
          )
          .child(
            this.feature(
              t,
              I("bot"),
              this.tr("Live agent status & sub-agent tree", "智能体状态感知与子代树"),
              this.tr(
                "Tracks Working, Needs you, and Done automatically—with alerts and sub-agents grouped under their parent.",
                "自动识别运行中、待确认与已完成状态，支持提醒音并在父节点下树状展示派生子智能体。",
              ),
            ),
          )
          .child(
            this.feature(
              t,
              I("layers"),
              this.tr("Your other machines, in the same window", "多台主机，汇集在同一个窗口"),
              this.tr(
                "Pair WSL, Linux servers, or a Mac over SSH so their spaces and agents keep going while this computer sleeps.",
                "通过 SSH 连接 WSL、服务器或 Mac，本机休眠后远程工作区与智能体依然持续运行。",
              ),
            ),
          ),
      )
      .child(div().h(1).bg(t.border))
      .child(
        v_flex()
          .gap(9)
          .child(prefRow(this.tr("Language", "界面语言"), this.segment(t, "lang", LANGUAGES, langNow, (v, cx) => this.setLanguage(v, cx))))
          .child(prefRow(this.tr("Appearance", "外观模式"), this.segment(t, "ap", apItems, this.appearance, (v, cx) => this.setAppearance(v, cx))))
          .child(prefRow(this.tr("Size", "界面缩放"), this.sizeControl(t))),
      );
  }

  // Page 2: Choose Your Panels -----------------------------------------

  panelCard(t, p) {
    const on = this.panels().includes(p.id);
    return h_flex()
      .id(`w-panel-${p.id}`)
      .p(12)
      .gap(12)
      .items_start()
      .rounded(11)
      .border_1()
      .border_color(on ? t.accent : t.input)
      .when(on, (el) => el.bg(t.accent + "10"))
      .cursor_pointer()
      .hover((s) => s.border_color(on ? t.accent : t.ring))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        this.togglePanel(p.id);
        cx.notify();
      })
      .child(
        div()
          .flex_none()
          .w(32)
          .h(32)
          .rounded(8)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .flex()
          .items_center()
          .justify_center()
          .child(svg(W(p.icon)).size(15).text_color(on ? t.accent : t.foreground)),
      )
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(3)
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(this.tr(p.en, p.zh)))
              .child(div().px(6).py(0.5).rounded(5).bg(t.hover).text_size(10.8).text_color(t.dim).child(`mods/${p.id}`)),
          )
          .child(this.para(t, this.tr(p.aboutEn, p.aboutZh))),
      )
      .child(
        div()
          .flex_none()
          .mt(4)
          .w(34)
          .h(20)
          .rounded(10)
          .p(2)
          .bg(on ? t.accent : t.input)
          .flex()
          .when(on, (el) => el.justify_end())
          .child(div().w(16).h(16).rounded(8).bg(on ? t.accent_foreground : t.muted)),
      );
  }

  pageTwo(t) {
    return v_flex()
      .gap(13)
      .child(
        this.title(
          t,
          this.tr("Choose your panels", "选择右侧辅助面板"),
          this.tr(
            "Turn panels on or off to preview them beside your terminals (drag this window aside to see). Switch them anytime from the top-right tab bar.",
            "开关下方右侧面板可即时预览（拖动此窗口即可查看）。之后可随时从顶部标签栏右上角展开或收起。",
          ),
        ),
      )
      .children(PANELS.map((p) => this.panelCard(t, p)))
      .child(
        div()
          .px(12)
          .py(10)
          .rounded(9)
          .bg(t.raised)
          .text_size(12.2)
          .line_height("18px")
          .text_color(t.muted)
          .child(
            this.tr(
              "The left sidebar keeps Spaces, Agents, and Flash Notes ready; Settings → Look offers more themes and layouts.",
              "左侧边栏固定展示工作区（Spaces）、智能体（Agents）与闪念（Flash Notes）；更多主题与布局可在「设置 → 外观」切换。",
            ),
          ),
      );
  }

  // Page 3: Core AI & Remote Machines ----------------------------------

  machineRow(t, icon, name, note, ok) {
    return h_flex()
      .h(38)
      .px(12)
      .gap(10)
      .items_center()
      .border_b(1)
      .border_color(t.border)
      .child(svg(W(icon)).size(14).text_color(ok ? t.accent : t.muted))
      .child(div().flex_1().min_w(0).truncate().text_size(12.8).text_color(t.foreground).child(name))
      .child(div().text_size(11.8).text_color(t.dim).child(note))
      .child(div().w(7).h(7).rounded(4).bg(ok ? t.ansi2 : t.dim));
  }

  pageThree(t) {
    const ai = this.aiStatus;
    const aiOn = Boolean(ai?.configured);
    const aiBadge = aiOn
      ? this.tr(ai.model ? `Configured · ${ai.model}` : "Configured", ai.model ? `已配置 · ${ai.model}` : "已配置")
      : this.tr("Not configured", "未配置");
    const coreVer = typeof updates === "function" ? fmtVersion(updates()?.core?.version) : "";
    const localNote = coreVer ? `h2 ${coreVer}` : this.tr("here", "本机");
    const shownMachines = this.machines.slice(0, 3);
    const hiddenCount = Math.max(0, this.machines.length - shownMachines.length);

    return v_flex()
      .gap(14)
      .child(
        this.title(
          t,
          this.tr("Core AI & other machines", "核心 AI 助手与远程主机"),
          this.tr(
            "Both are optional—set them up now or anytime later in Settings.",
            "两项均为可选配置，现在跳过也可以随时在「设置」中开启。",
          ),
        ),
      )
      // Core AI compact card
      .child(
        v_flex()
          .gap(8)
          .p(12)
          .rounded(11)
          .border_1()
          .border_color(aiOn ? t.ansi2 : t.border)
          .bg(t.raised)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                h_flex()
                  .gap(8)
                  .items_center()
                  .child(svg(S("sparkles")).size(15).text_color(t.accent))
                  .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(this.tr("Core AI Assistant", "核心 AI 助手")))
                  .child(
                    div()
                      .px(6)
                      .py(1)
                      .rounded(5)
                      .bg(aiOn ? t.ansi2 + "20" : t.background)
                      .border_1()
                      .border_color(aiOn ? t.ansi2 : t.border)
                      .text_size(11)
                      .text_color(aiOn ? t.ansi2 : t.dim)
                      .child(aiBadge),
                  ),
              )
              .child(
                this.smallButton(
                  t,
                  "w-open-ai-settings",
                  aiOn ? this.tr("Manage", "管理") : this.tr("Configure AI…", "去配置…"),
                  () => run("app.settings", { section: "ai" }),
                  !aiOn,
                ),
              ),
          )
          .child(
            this.para(
              t,
              this.tr(
                "Connect one OpenAI-compatible endpoint to power Prompt Polish, Context Handoff, Catch-up Briefing, Semantic Titles, and Drove.",
                "配置一个 OpenAI 兼容接口，即可解锁提示词润色、上下文接力、待办简报、语义标题与 Drove 多智能体编排。",
              ),
            ),
          ),
      )
      // Machines compact list
      .child(
        v_flex()
          .rounded(11)
          .border_1()
          .border_color(t.border)
          .bg(t.background)
          .overflow_hidden()
          .child(this.machineRow(t, "monitor", this.tr("This computer", "本机"), localNote, true))
          .children(
            shownMachines.map((m) => {
              const online = m.state === "connected" || m.state === "online";
              const stateText = online
                ? this.tr("connected", "已连接")
                : m.state === "needs_login"
                  ? this.tr("needs login", "需要登录")
                  : String(m.state ?? this.tr("offline", "离线")).replace(/_/g, " ");
              return v_flex()
                .child(this.machineRow(t, "server", m.name, stateText, online))
                .when(m.state === "needs_login", (el) =>
                  el.child(
                    div()
                      .px(10)
                      .py(8)
                      .border_b(1)
                      .border_color(t.border)
                      .bg(t.raised)
                      .child(this.copyLine(t, `h2 machine login ${m.name}`, `w-login-copy-${m.name}`)),
                  ),
                );
            }),
          )
          .when(hiddenCount > 0, (el) =>
            el.child(
              div()
                .px(12)
                .py(7)
                .border_b(1)
                .border_color(t.border)
                .text_size(11.8)
                .text_color(t.dim)
                .child(this.tr(`+${hiddenCount} more in Settings → Machines`, `其余 ${hiddenCount} 台主机可在「设置 → 主机」查看`)),
            ),
          )
          .child(
            h_flex()
              .id("w-add-machine")
              .h(38)
              .px(12)
              .gap(9)
              .items_center()
              .cursor_pointer()
              .text_size(12.8)
              .font_medium()
              .text_color(t.accent)
              .hover((s) => s.bg(t.hover))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click(() => run("mod.open", { name: "machine-add" }))
              .child(svg(W("plus")).size(14))
              .child(this.tr("Add a machine (SSH / WSL)", "添加远程主机 / WSL")),
          ),
      )
      .child(
        div()
          .text_size(12)
          .line_height("18px")
          .text_color(t.dim)
          .child(
            this.tr(
              "Installed coding CLIs (Claude Code, Codex, Gemini, ZCode…) appear automatically in the top + menu.",
              "本机已安装的编程 CLI（Claude Code、Codex、Gemini、ZCode 等）会自动出现在顶部 + 新建菜单中。",
            ),
          ),
      );
  }

  // Page 4: Shortcuts, h2 CLI & Ready to Go ----------------------------

  pageFour(t) {
    const mac = this.isMac();
    const keyList = typeof keys === "function" ? keys() : [];
    const pal = boundShortcut(keyList, "app.palette", mac, mac ? "cmd-shift-p" : "ctrl-shift-p");
    const items = SHORTCUT_HIGHLIGHTS.map((item) => ({
      ...item,
      chord: boundShortcut(keyList, item.action, mac, mac ? item.fallbackMac : item.fallbackLinux),
    })).filter((item) => item.chord);

    const half = Math.ceil(items.length / 2);
    const left = items.slice(0, half);
    const right = items.slice(half);
    const row = (item) =>
      h_flex()
        .py(5)
        .px(9)
        .gap(8)
        .rounded(7)
        .bg(t.raised)
        .border_1()
        .border_color(t.border)
        .items_center()
        .justify_between()
        .child(div().flex_1().min_w(0).truncate().text_size(12.2).text_color(t.foreground).child(this.tr(item.en, item.zh)))
        .child(this.keycap(t, item.chord));

    return v_flex()
      .gap(14)
      .when(Boolean(pal), (el) =>
        el.child(
          h_flex()
            .gap(12)
            .items_center()
            .p(11)
            .rounded(10)
            .border_1()
            .border_color(t.accent)
            .bg(t.accent + "10")
            .child(this.keycap(t, pal))
            .child(
              v_flex()
                .flex_1()
                .min_w(0)
                .gap(2)
                .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(this.tr("One key opens everything", "一个快捷键直达全部功能")))
                .child(
                  this.para(
                    t,
                    this.tr(
                      "Open the command palette for every action, layout, theme, and shortcut.",
                      "打开命令面板即可搜索所有命令、切换布局与主题，或直达设置。",
                    ),
                  ),
                ),
            ),
        ),
      )
      .child(
        v_flex()
          .gap(7)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .child(div().text_size(12.8).font_semibold().text_color(t.foreground).child(this.tr("Essential shortcuts", "常用快捷键")))
              .child(this.smallButton(t, "w-open-help", this.tr("All shortcuts", "全部快捷键"), () => run("app.help"))),
          )
          .child(
            h_flex()
              .gap(8)
              .items_start()
              .child(v_flex().flex_1().min_w(0).gap(6).children(left.map(row)))
              .child(v_flex().flex_1().min_w(0).gap(6).children(right.map(row))),
          ),
      )
      .child(
        v_flex()
          .gap(8)
          .p(12)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.raised)
          .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(this.tr("Make it yours & h2 CLI", "自由定制与 h2 命令行")))
          .child(
            this.para(
              t,
              this.tr(
                "Use `h2 ls` / `h2 run` from any shell (and built-in `h2 mcp` from agents). Every layout and panel is plain files under ~/.config/herdr2-app/:",
                "在任意终端可用 `h2 ls` / `h2 run` 管理会话（Agent 内置 `h2 mcp`）。所有布局与面板都是 ~/.config/herdr2-app/ 下的纯文本文件：",
              ),
            ),
          )
          .child(this.copyLine(t, agentCustomizePrompt(this.lang()), "w-customize-copy")),
      );
  }

  // Frame --------------------------------------------------------------

  render() {
    const t = theme();
    const langNow = this.lang();
    const noSpaces = spaces().length === 0;
    const pages = [this.pageOne, this.pageTwo, this.pageThree, this.pageFour];

    const dots = h_flex()
      .gap(6)
      .items_center()
      .children(
        [...Array(PAGES)].map((_, i) => {
          const num = i + 1;
          const active = num === this.page;
          return div()
            .id(`w-dot-${num}`)
            .w(active ? 18 : 6)
            .h(6)
            .rounded(3)
            .cursor_pointer()
            .bg(active ? t.foreground : t.input)
            .on_click((_e, cx) => this.go(num, cx));
        }),
      );

    return v_flex()
      .id("welcome")
      .track_focus(this.handle)
      .size_full()
      .bg(t.popover)
      .on_key_down((e, cx) => {
        const k = e.key;
        const m = e.modifiers;
        if (k === "escape") return this.done("skipped");
        if (m.control || m.alt || m.platform) return;
        if (k === "enter" || k === "right") {
          if (this.page === PAGES) return k === "enter" ? this.start() : undefined;
          this.go(this.page + 1, cx);
        } else if (k === "left") {
          this.go(this.page - 1, cx);
        } else {
          return;
        }
        cx.stop_propagation();
      })
      // Clean draggable header (faithful to original Herdr)
      .child(
        h_flex()
          .flex_none()
          .h(48)
          .cursor_grab()
          .pl(20)
          .pr(12)
          .gap(10)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .child(image("frame/welcome/herdr.png").w(22).h(22).rounded(6).flex_none())
          .child(div().text_size(14).font_semibold().text_color(t.foreground).child(this.tr("Welcome to Herdr2", "欢迎使用 Herdr2")))
          .child(div().flex_1())
          .child(
            div()
              .id("w-header-lang")
              .px(8)
              .py(3)
              .rounded(6)
              .border_1()
              .border_color(t.border)
              .bg(t.raised)
              .text_size(11.5)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click((_e, cx) => this.setLanguage(langNow === "zh-CN" ? "en" : "zh-CN", cx))
              .child(langNow === "zh-CN" ? "EN" : "中文"),
          )
          .child(div().text_size(11.5).text_color(t.dim).child(this.tr("Drag to move", "拖动移动")))
          .child(
            div()
              .id("w-close")
              .w(26)
              .h(26)
              .rounded(7)
              .flex()
              .items_center()
              .justify_center()
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click(() => this.done("skipped"))
              .child(svg(W("x")).size(14)),
          ),
      )
      // Single-screen body (fits cleanly without vertical scrolling)
      .child(
        div()
          .id("w-body")
          .flex_1()
          .min_h(0)
          .overflow_y_scroll()
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .child(div().px(22).py(18).child(pages[this.page - 1].call(this, t))),
      )
      // Clean footer
      .child(
        h_flex()
          .flex_none()
          .px(16)
          .py(11)
          .gap(8)
          .items_center()
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .border_t(1)
          .border_color(t.border)
          .child(div().pl(6).child(dots))
          .child(div().flex_1())
          .when(this.page > 1, (el) =>
            el.child(this.button(t, "w-back", this.tr("Back", "上一步"), "ghost", (_e, cx) => this.go(this.page - 1, cx))),
          )
          .when(this.page === PAGES && noSpaces && typeof pickFolder === "function", (el) =>
            el.child(
              this.button(
                t,
                "w-open-folder",
                this.openingFolder ? this.tr("Opening…", "打开中…") : this.tr("Open folder…", "选择目录…"),
                "outline",
                (_e, cx) => this.openProjectFolder(cx),
                I("folder"),
              ),
            ),
          )
          .child(
            this.page < PAGES
              ? this.button(
                  t,
                  "w-next",
                  this.tr("Next", "下一步"),
                  "primary",
                  (_e, cx) => this.go(this.page + 1, cx),
                  I("arrow-right"),
                )
              : this.button(
                  t,
                  "w-start",
                  noSpaces ? this.tr("Start working", "开始使用") : this.tr("Done", "完成"),
                  "primary",
                  () => this.start(),
                  I("arrow-right"),
                ),
          ),
      );
  }
}
