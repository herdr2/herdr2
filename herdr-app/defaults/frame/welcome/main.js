// First-run Herdr2 welcome over the live app; records finished/skipped in frame.welcomed.
// Designed after original Herdr: 4 focused, calm pages that each fit in one screen without scrolling.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, Popup } from "gpui-base";
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
  AI_PAGE,
  AI_PROVIDERS,
  DEEPSEEK_PRESET,
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
  normalizeUrl,
  normalizeFetchedModels,
  isAiGateSatisfied,
  canAccessPage,
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
    this.aiTestResult = null;
    this.aiBusy = false;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiSkipped = false;
    this.aiProvider = "deepseek";
    this.aiSelectedModel = "";
    this.aiModels = [];
    this.aiModelsLoading = false;
    this.aiModelsHint = null;
    this.aiModelMenuOpen = false;
    this.aiUrlInput = typeof InputState?.new === "function"
      ? InputState.new({ placeholder: DEEPSEEK_PRESET.baseUrl })
      : null;
    this.aiKeyInput = typeof InputState?.new === "function"
      ? InputState.new({ placeholder: "sk-... (optional)" })
      : null;
    this.aiKeyInput?.set_masked?.(true);
    this.aiUrlInput?.set_value?.(DEEPSEEK_PRESET.baseUrl);
    this.skillStatus = null;
    this.skillInstalling = false;
    this.skillMessage = null;
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
        await Promise.all([this.loadMachines(), this.loadAiStatus(), this.loadSkillStatus()]);
        // If an existing AI endpoint is already saved, silently verify connectivity once.
        if (this.aiStatus?.configured && !this.aiTestResult) {
          await this.verifyExistingAi();
        }
      }
      cx.notify();
    });
    // Live-refresh paired machines while on Page 3.
    cx.timer.every(5000, (cx) => {
      if (isChecking() || this.page !== 3 || this.aiBusy) return;
      cx.spawn(async (cx) => {
        await this.loadMachines();
        cx.notify();
      });
    });
  }

  aiGateBypassed() {
    return (
      isChecking() ||
      Boolean(this.aiSkipped) ||
      Boolean(typeof appWindow === "function" && appWindow()?.skip_ai_gate) ||
      Boolean(typeof shared === "function" && shared("qa.skip_ai_gate"))
    );
  }

  aiReady() {
    return isAiGateSatisfied(this.aiStatus, this.aiTestResult, this.aiGateBypassed());
  }

  skipAi(cx) {
    this.aiSkipped = true;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiModelMenuOpen = false;
    this.go(AI_PAGE + 1, cx);
  }

  async loadSkillStatus() {
    const r = await callCore(["skill", "status", "--json"]);
    if (r?.code === 0 && r.stdout) {
      try {
        this.skillStatus = JSON.parse(r.stdout);
      } catch {}
    }
  }

  installSkillsNow(cx) {
    if (this.skillInstalling) return;
    this.skillInstalling = true;
    this.skillMessage = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const r = await callCore(["skill", "install", "--all", "--json"]);
        if (r?.code === 0 && r.stdout) {
          const parsed = JSON.parse(r.stdout);
          if (parsed?.status) this.skillStatus = parsed.status;
          this.skillMessage = {
            ok: true,
            text: this.tr(
              `Installed ${parsed?.installedCount ?? 0} skill files cleanly.`,
              `已成功安装 ${parsed?.installedCount ?? 0} 个配套 Skill 文件。`,
            ),
          };
        } else {
          this.skillMessage = {
            ok: false,
            text: (r?.stderr || r?.stdout || this.tr("Skill installation failed", "Skill 安装失败")).trim(),
          };
        }
      } catch (e) {
        this.skillMessage = { ok: false, text: String(e?.message ?? e) };
      } finally {
        this.skillInstalling = false;
        cx.notify();
      }
    });
  }

  selectAiProvider(provider, cx) {
    const next = provider === "custom" ? "custom" : "deepseek";
    const changed = this.aiProvider !== next;
    this.aiProvider = next;
    this.aiModelMenuOpen = false;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiModelsHint = null;
    if (next === "deepseek") {
      this.aiUrlInput?.set_value?.(DEEPSEEK_PRESET.baseUrl);
    } else if (normalizeUrl(this.aiUrlInput?.value?.()) === normalizeUrl(DEEPSEEK_PRESET.baseUrl)) {
      this.aiUrlInput?.set_value?.("");
    }
    if (changed) {
      this.aiModels = [];
      this.aiSelectedModel = "";
      this.aiTestResult = null;
    }
    cx.notify();
  }

  selectAiModel(model, cx) {
    this.aiSelectedModel = String(model ?? "").trim();
    this.aiModelMenuOpen = false;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiModelsHint = null;
    this.aiTestResult = null;
    cx.notify();
  }

  async writeAiTempReq(payload) {
    const reqPath = `${appHome()}/.welcome-ai-${Date.now()}-${Math.random().toString(36).slice(2, 7)}.json`;
    await writeTextFile(reqPath, JSON.stringify(payload), null);
    return reqPath;
  }

  async verifyExistingAi() {
    try {
      const r = await callCore(["ai", "test", "--json"]);
      if (r?.code === 0 && r.stdout) {
        const parsed = JSON.parse(r.stdout);
        if (parsed?.ok) {
          this.aiTestResult = parsed;
          if (Array.isArray(parsed.availableModels) && parsed.availableModels.length > 0) {
            this.aiModels = parsed.availableModels;
          }
        }
      }
    } catch {}
  }

  fetchAiModels(cx) {
    if (this.aiModelsLoading || this.aiBusy) return;
    const baseUrl = String(this.aiUrlInput?.value?.() ?? "").trim();
    const apiKey = String(this.aiKeyInput?.value?.() ?? "").trim();
    if (!baseUrl) {
      this.aiError = this.tr("Please enter Service URL first.", "请先填写服务地址（Base URL）。");
      cx.notify();
      return;
    }
    this.aiModelsLoading = true;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiModelsHint = null;
    this.aiModelMenuOpen = false;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const payload = { baseUrl };
        if (apiKey) payload.apiKey = apiKey;
        const reqFile = await this.writeAiTempReq(payload);
        const res = await callCore(["ai", "models", "--file", reqFile, "--consume", "--json"]);
        const parsed = normalizeFetchedModels(res?.stdout || res?.stderr || "");
        if (res?.code !== 0 || !parsed.ok) {
          this.aiError =
            parsed.error ||
            (res?.stderr || res?.stdout || this.tr("Failed to fetch models from server", "从服务端获取模型失败")).trim();
          return;
        }
        this.aiModels = parsed.models;
        if (this.aiSelectedModel && !this.aiModels.includes(this.aiSelectedModel)) {
          this.aiSelectedModel = "";
        }
        this.aiModelsHint = this.tr(
          `Loaded ${parsed.models.length} model(s). Pick one from the dropdown.`,
          `已获取 ${parsed.models.length} 个模型，请从下拉菜单选择。`,
        );
        if (!this.aiSelectedModel) {
          this.aiModelMenuOpen = true;
        }
      } catch (e) {
        this.aiError = String(e?.message ?? e);
      } finally {
        this.aiModelsLoading = false;
        cx.notify();
      }
    });
  }

  saveAndTestAi(cx) {
    if (this.aiBusy || this.aiModelsLoading) return;
    const baseUrl = String(this.aiUrlInput?.value?.() ?? "").trim();
    const apiKey = String(this.aiKeyInput?.value?.() ?? "").trim();
    const model = String(this.aiSelectedModel ?? "").trim();
    if (!baseUrl) {
      this.aiError = this.tr("Please enter Service URL first.", "请先填写服务地址（Base URL）。");
      cx.notify();
      return;
    }
    if (!model) {
      this.aiError = this.tr(
        "Please fetch models and select one from the dropdown first.",
        "请先点击「获取模型」并从下拉菜单选择一个模型。",
      );
      cx.notify();
      return;
    }
    this.aiBusy = true;
    this.aiError = null;
    this.aiGateHint = null;
    this.aiModelsHint = null;
    this.aiModelMenuOpen = false;
    this.aiTestResult = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const testPayload = { baseUrl, model };
        if (apiKey) testPayload.apiKey = apiKey;
        const testFile = await this.writeAiTempReq(testPayload);
        const testRes = await callCore(["ai", "test", "--file", testFile, "--consume", "--json"]);
        let parsedTest = null;
        try {
          parsedTest = JSON.parse(testRes?.stdout || testRes?.stderr || "{}");
        } catch {}
        if (Array.isArray(parsedTest?.availableModels) && parsedTest.availableModels.length > 0) {
          const merged = new Set([...this.aiModels, ...parsedTest.availableModels]);
          this.aiModels = [...merged];
        }
        if (testRes?.code !== 0 || !parsedTest?.ok) {
          this.aiTestResult = { ok: false };
          this.aiError =
            parsedTest?.error?.message ||
            parsedTest?.error ||
            parsedTest?.message ||
            (testRes?.stderr || testRes?.stdout || this.tr("Connection test failed", "连通性测试失败")).trim();
          return;
        }

        const savePayload = { enabled: true, baseUrl, model };
        if (apiKey) {
          savePayload.apiKey = apiKey;
        } else if (
          this.aiStatus?.keyMasked &&
          normalizeUrl(baseUrl) !== normalizeUrl(this.aiStatus?.baseUrl)
        ) {
          savePayload.clearKey = true;
        }
        const saveFile = await this.writeAiTempReq(savePayload);
        const saveRes = await callCore(["ai", "settings-set", "--file", saveFile, "--consume", "--json"]);
        let parsedSave = null;
        try {
          parsedSave = JSON.parse(saveRes?.stdout || saveRes?.stderr || "{}");
        } catch {}
        if (saveRes?.code !== 0) {
          this.aiError =
            parsedSave?.error?.message ||
            parsedSave?.message ||
            (saveRes?.stderr || saveRes?.stdout || this.tr("Failed to save AI settings", "保存 AI 配置失败")).trim();
          return;
        }
        this.aiStatus = summarizeAiStatus(parsedSave) ?? {
          configured: true,
          enabled: true,
          model,
          baseUrl,
          keyMasked: null,
          availableModels: this.aiModels,
        };
        if (this.aiStatus.availableModels?.length > 0) {
          this.aiModels = this.aiStatus.availableModels;
        }
        this.aiTestResult = parsedTest;
        this.aiKeyInput?.set_value?.("");
        this.aiError = null;
        this.aiGateHint = null;
      } catch (e) {
        this.aiError = String(e?.message ?? e);
      } finally {
        this.aiBusy = false;
        cx.notify();
      }
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
    if (this.aiStatus?.baseUrl) {
      this.aiUrlInput?.set_value?.(this.aiStatus.baseUrl);
      this.aiProvider = this.aiStatus.baseUrl.includes("deepseek") ? "deepseek" : "custom";
    }
    if (this.aiStatus?.model) {
      this.aiSelectedModel = this.aiStatus.model;
    }
    if (Array.isArray(this.aiStatus?.availableModels) && this.aiStatus.availableModels.length > 0) {
      this.aiModels = this.aiStatus.availableModels;
    }
  }

  done(outcome, cx = null) {
    if (!this.aiReady()) {
      this.page = AI_PAGE;
      this.aiGateHint = this.tr(
        "Configure & test Core AI first, or click Skip in the bottom bar.",
        "请先配置并测试 Core AI 连通性，或点击右下角「跳过」暂不配置。",
      );
      cx?.notify?.();
      return;
    }
    setShared("frame.welcomed", outcome);
    closeOverlay(props().overlay);
  }

  start(cx = null) {
    if (!this.aiReady()) {
      this.done("finished", cx);
      return;
    }
    this.done("finished", cx);
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
    const requested = clampPage(page);
    const allowed = canAccessPage(requested, this.page, this.aiReady());
    if (requested > allowed) {
      this.page = allowed;
      this.aiGateHint = this.tr(
        "Complete and test Core AI connectivity first, or click Skip to continue.",
        "请先完成 Core AI 连通性测试，或点击右下角「跳过」继续。",
      );
      cx.notify();
      return;
    }
    this.aiGateHint = null;
    this.aiModelMenuOpen = false;
    this.page = allowed;
    if (!isChecking() && this.page === AI_PAGE) {
      cx.spawn(async (cx) => {
        await Promise.all([this.loadMachines(), this.loadAiStatus()]);
        cx.notify();
      });
    }
    if (!isChecking() && this.page === 4) {
      cx.spawn(async (cx) => {
        await this.loadSkillStatus();
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

  modelDropdown(t) {
    const open = Boolean(this.aiModelMenuOpen);
    const models = Array.isArray(this.aiModels) ? this.aiModels : [];
    const selected = String(this.aiSelectedModel ?? "").trim();
    const label = selected
      ? selected
      : models.length > 0
        ? this.tr("Select a model…", "请从下拉菜单选择模型…")
        : this.tr("Click Fetch Models first", "请先点击「获取模型」");
    const rowH = 28;
    const listH = models.length === 0 ? 42 : Math.min(models.length * rowH + 8, 176);

    const trigger = h_flex()
      .id("w-ai-model-trigger")
      .w_full()
      .h(32)
      .px(10)
      .gap(8)
      .items_center()
      .justify_between()
      .rounded(7)
      .border_1()
      .border_color(open ? t.accent : t.input)
      .bg(t.background)
      .cursor_pointer()
      .hover((s) => s.border_color(open ? t.accent : t.ring))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        this.aiModelMenuOpen = !open;
        cx.notify();
      })
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(12.2)
          .text_color(selected ? t.foreground : models.length > 0 ? t.muted : t.dim)
          .child(label),
      )
      .child(div().flex_none().text_size(10.5).text_color(t.dim).child("▾"));

    if (typeof Popup?.new !== "function") {
      return trigger;
    }

    return Popup.new("w-ai-model-popup", trigger)
      .anchor("bottom_left")
      .flex()
      .flex_1()
      .min_w(0)
      .when(open, (el) =>
        el.content(
          v_flex()
            .id("w-ai-model-menu")
            .w(340)
            .rounded(8)
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .shadow_md()
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_mouse_down_out((_e, cx) => {
              this.aiModelMenuOpen = false;
              cx.notify();
            })
            .on_scroll_wheel((_e, cx) => cx.stop_propagation())
            .child(
              div()
                .id("w-ai-model-list")
                .h(listH)
                .p(4)
                .overflow_y_scrollbar()
                .when(models.length === 0, (list) =>
                  list.child(
                    div()
                      .px(9)
                      .py(8)
                      .text_size(11.8)
                      .text_color(t.dim)
                      .child(this.tr("No models loaded yet. Click Fetch Models.", "暂无模型，请先点击「获取模型」。")),
                  ),
                )
                .children(
                  models.map((m, idx) => {
                    const active = m === selected;
                    return h_flex()
                      .id(`w-ai-model-opt-${idx}`)
                      .w_full()
                      .h(rowH)
                      .flex_none()
                      .px(9)
                      .gap(8)
                      .rounded(6)
                      .items_center()
                      .justify_between()
                      .cursor_pointer()
                      .bg(active ? t.accent + "1a" : "#00000000")
                      .hover((s) => s.bg(t.hover))
                      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                      .on_click((_e, cx) => this.selectAiModel(m, cx))
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .truncate()
                          .text_size(12.2)
                          .text_color(active ? t.accent : t.foreground)
                          .when(active, (x) => x.font_medium())
                          .child(m),
                      )
                      .when(active, (row) => row.child(svg(I("check")).size(12).flex_none().text_color(t.accent)));
                  }),
                ),
            ),
        ),
      );
  }

  pageThree(t) {
    const ai = this.aiStatus;
    const aiVerified = this.aiTestResult?.ok === true && Boolean(ai?.configured);
    const aiOn = Boolean(ai?.configured);
    const latency = this.aiTestResult?.latencyMs ?? this.aiTestResult?.latency_ms ?? null;
    const verifiedModel = ai?.model || this.aiSelectedModel || "";
    const aiBadge = aiVerified
      ? this.tr(
          `Verified${verifiedModel ? ` · ${verifiedModel}` : ""}${latency ? ` (${latency}ms)` : ""}`,
          `已验证连通${verifiedModel ? ` · ${verifiedModel}` : ""}${latency ? ` (${latency}ms)` : ""}`,
        )
      : aiOn
        ? this.tr("Saved · click Test to verify", "已保存 · 点击测试验证连通")
        : this.tr("Not configured", "未配置");
    const coreVer = typeof updates === "function" ? fmtVersion(updates()?.core?.version) : "";
    const localNote = coreVer ? `h2 ${coreVer}` : this.tr("here", "本机");
    const shownMachines = this.machines.slice(0, 2);
    const hiddenCount = Math.max(0, this.machines.length - shownMachines.length);
    const sameSavedUrl =
      Boolean(ai?.baseUrl) &&
      normalizeUrl(this.aiUrlInput?.value?.()) === normalizeUrl(ai?.baseUrl);
    const presetChip = (id, label, active, onClick) =>
      div()
        .id(id)
        .px(9)
        .py(3.5)
        .rounded(6)
        .cursor_pointer()
        .text_size(11.8)
        .font_medium()
        .border_1()
        .border_color(active ? t.accent : t.border)
        .bg(active ? t.accent + "1f" : t.background)
        .text_color(active ? t.accent : t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click(onClick)
        .child(label);

    return v_flex()
      .gap(11)
      .child(
        this.title(
          t,
          this.tr("Core AI & other machines", "核心 AI 助手与远程主机"),
          this.tr(
            "Configure Core AI for smart status detection, Prompt Polish, and Context Handoff (DeepSeek Official or any OpenAI-compatible gateway such as local Magpie).",
            "配置核心 AI 以启用智能状态识别、提示词润色与上下文接力（支持 DeepSeek 官方或自定义 OpenAI 格式中转，如本机 Magpie）。",
          ),
        ),
      )
      // Core AI inline setup card
      .child(
        v_flex()
          .gap(9)
          .p(12)
          .rounded(11)
          .border_1()
          .border_color(aiVerified ? t.ansi2 : t.accent)
          .bg(t.raised)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                h_flex()
                  .gap(7)
                  .items_center()
                  .child(svg(S("sparkles")).size(15).text_color(t.accent))
                  .child(div().text_size(13.2).font_semibold().text_color(t.foreground).child(this.tr("Core AI Assistant", "核心 AI 助手")))
                  .child(
                    div()
                      .px(6)
                      .py(1)
                      .rounded(5)
                      .bg(aiVerified ? t.ansi2 + "22" : t.accent + "20")
                      .border_1()
                      .border_color(aiVerified ? t.ansi2 : t.accent)
                      .text_size(11)
                      .font_medium()
                      .text_color(aiVerified ? t.ansi2 : t.accent)
                      .child(aiBadge),
                  ),
              )
              .when(this.aiProvider === "deepseek", (el) =>
                el.child(
                  this.smallButton(
                    t,
                    "w-copy-deepseek-portal",
                    this.copied === "w-copy-deepseek-portal"
                      ? this.tr("Portal URL Copied", "已复制获取地址")
                      : this.tr("Get DeepSeek Key", "获取 DeepSeek Key"),
                    (_e, cx) => {
                      cx.write_to_clipboard(DEEPSEEK_PRESET.portalUrl);
                      this.copied = "w-copy-deepseek-portal";
                      cx.notify();
                    },
                  ),
                ),
              ),
          )
          // Provider bar: DeepSeek Official or Custom OpenAI-compatible only
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .flex_wrap()
              .children(
                AI_PROVIDERS.map((p) =>
                  presetChip(
                    `w-ai-preset-${p.id}`,
                    this.tr(p.en, p.zh),
                    this.aiProvider === p.id,
                    (_e, cx) => this.selectAiProvider(p.id, cx),
                  ),
                ),
              ),
          )
          // Inline inputs: Row 1 (Service URL + optional API Key), Row 2 (Model dropdown + Fetch Models + Save & Test)
          .when(Boolean(this.aiUrlInput && this.aiKeyInput), (el) =>
            el
              .child(
                h_flex()
                  .gap(8)
                  .items_start()
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(3)
                      .child(
                        div()
                          .text_size(11.5)
                          .text_color(t.muted)
                          .child(
                            this.aiProvider === "custom"
                              ? this.tr("Service URL (e.g. http://127.0.0.1:8080/v1)", "服务地址（如 http://127.0.0.1:8080/v1）")
                              : this.tr("Service URL (Base URL)", "服务地址 (Base URL)"),
                          ),
                      )
                      .child(
                        div()
                          .px(8)
                          .py(2)
                          .rounded(7)
                          .border_1()
                          .border_color(t.input)
                          .bg(t.background)
                          .child(Input.new(this.aiUrlInput).h(25).text_size(12.2).text_color(t.foreground)),
                      ),
                  )
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(3)
                      .child(
                        div()
                          .text_size(11.5)
                          .text_color(t.muted)
                          .child(
                            sameSavedUrl && ai?.keyMasked
                              ? this.tr(`API Key (optional · saved ${ai.keyMasked})`, `API Key（选填 · 已保存 ${ai.keyMasked}）`)
                              : this.tr("API Key (optional, leave blank for keyless proxy)", "API Key（选填，本机中转无需填写）"),
                          ),
                      )
                      .child(
                        div()
                          .px(8)
                          .py(2)
                          .rounded(7)
                          .border_1()
                          .border_color(t.input)
                          .bg(t.background)
                          .child(Input.new(this.aiKeyInput).h(25).text_size(12.2).text_color(t.foreground)),
                      ),
                  ),
              )
              .child(
                h_flex()
                  .gap(8)
                  .items_end()
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(3)
                      .child(
                        h_flex()
                          .items_center()
                          .justify_between()
                          .gap(6)
                          .child(div().text_size(11.5).text_color(t.muted).child(this.tr("Model", "模型")))
                          .when(this.aiModels.length > 0, (mEl) =>
                            mEl.child(
                              div()
                                .text_size(11)
                                .text_color(t.dim)
                                .child(this.tr(`${this.aiModels.length} available`, `共 ${this.aiModels.length} 个可选`)),
                            ),
                          ),
                      )
                      .child(this.modelDropdown(t)),
                  )
                  .child(
                    this.button(
                      t,
                      "w-ai-fetch-models",
                      this.aiModelsLoading ? this.tr("Fetching…", "获取中…") : this.tr("Fetch Models", "获取模型"),
                      "outline",
                      (_e, cx) => this.fetchAiModels(cx),
                    ),
                  )
                  .child(
                    this.button(
                      t,
                      "w-ai-save-test",
                      this.aiBusy
                        ? this.tr("Testing…", "测试中…")
                        : aiVerified
                          ? this.tr("Re-test", "重新测试")
                          : this.tr("Save & Test", "保存并测试"),
                      "primary",
                      (_e, cx) => this.saveAndTestAi(cx),
                    ),
                  ),
              ),
          )
          .when(Boolean(this.aiError || this.aiGateHint), (el) =>
            el.child(
              div()
                .px(9)
                .py(6)
                .rounded(7)
                .bg(t.blocked + "18")
                .border_1()
                .border_color(t.blocked)
                .text_size(11.8)
                .line_height("17px")
                .text_color(t.blocked)
                .child(this.aiError || this.aiGateHint),
            ),
          )
          .when(Boolean(this.aiModelsHint && !this.aiError && !this.aiGateHint && !aiVerified), (el) =>
            el.child(
              div()
                .px(9)
                .py(5)
                .rounded(7)
                .bg(t.accent + "14")
                .border_1()
                .border_color(t.accent)
                .text_size(11.5)
                .text_color(t.accent)
                .child(this.aiModelsHint),
            ),
          )
          .when(aiVerified && !this.aiError, (el) =>
            el.child(
              div()
                .px(9)
                .py(6)
                .rounded(7)
                .bg(t.ansi2 + "18")
                .border_1()
                .border_color(t.ansi2)
                .text_size(11.8)
                .text_color(t.ansi2)
                .child(
                  this.tr(
                    "Connection verified! You can now proceed to the next step.",
                    "✓ 连通性测试已通过！可以点击右下角「下一步」继续。",
                  ),
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
              .h(36)
              .px(12)
              .gap(9)
              .items_center()
              .cursor_pointer()
              .text_size(12.5)
              .font_medium()
              .text_color(t.accent)
              .hover((s) => s.bg(t.hover))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click(() => run("mod.open", { name: "machine-add" }))
              .child(svg(W("plus")).size(14))
              .child(this.tr("Add a machine (SSH / WSL)", "添加远程主机 / WSL")),
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
          .gap(7)
          .p(11)
          .rounded(10)
          .border_1()
          .border_color(this.skillMessage?.ok ? t.ansi2 : t.border)
          .bg(t.raised)
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .gap(8)
              .child(
                h_flex()
                  .gap(7)
                  .items_center()
                  .child(svg(S("sparkles")).size(14).text_color(t.accent))
                  .child(div().text_size(13).font_semibold().text_color(t.foreground).child(this.tr("Install Companion Skills", "一键安装配套 Skill"))),
              )
              .child(
                this.smallButton(
                  t,
                  "w-install-skills",
                  this.skillInstalling
                    ? this.tr("Installing…", "安装中…")
                    : this.tr("Install Skills Now", "一键安装到本机 Agent"),
                  (_e, cx) => this.installSkillsNow(cx),
                  !this.skillMessage?.ok,
                ),
              ),
          )
          .child(
            this.para(
              t,
              this.skillMessage?.text
                || this.tr(
                  "Install `herdr2` + `drove` skills into your detected coding agents (Antigravity, Claude Code, Codex, Cursor…) so they can orchestrate `h2` automatically. You can also use AI-assisted install anytime in Settings → Skills.",
                  "自动为本机检测到的编程 Agent（Antigravity、Claude Code、Codex、Cursor 等）安装 `herdr2` 与 `drove` 配套 Skill，解锁无头子 Agent 编排；也可随时在「设置 → Skills」用 AI 智能安装。",
                ),
            ),
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
    const aiVerified = this.aiTestResult?.ok === true && Boolean(this.aiStatus?.configured);
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
        if (k === "escape") {
          if (this.aiModelMenuOpen) {
            this.aiModelMenuOpen = false;
            cx.notify();
            cx.stop_propagation();
            return;
          }
          return this.done("skipped", cx);
        }
        if (m.control || m.alt || m.platform) return;
        if (k === "enter" || k === "right") {
          if (this.page === PAGES) return k === "enter" ? this.start(cx) : undefined;
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
          .when(this.aiReady(), (el) =>
            el.child(
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
                .on_click((_e, cx) => this.done("skipped", cx))
                .child(svg(W("x")).size(14)),
            ),
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
          .when(this.page === AI_PAGE && !aiVerified && !this.aiSkipped, (el) =>
            el.child(
              h_flex()
                .id("w-ai-skip")
                .h(26)
                .px(9)
                .items_center()
                .rounded(6)
                .cursor_pointer()
                .text_size(11.8)
                .text_color(t.dim)
                .hover((s) => s.text_color(t.foreground).bg(t.hover))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => this.skipAi(cx))
                .child(this.tr("Skip", "跳过")),
            ),
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
                  this.page === AI_PAGE && !this.aiReady()
                    ? this.tr("Verify AI to continue", "验证 AI 后继续")
                    : this.tr("Next", "下一步"),
                  this.page === AI_PAGE && !this.aiReady() ? "outline" : "primary",
                  (_e, cx) => this.go(this.page + 1, cx),
                  I("arrow-right"),
                )
              : this.button(
                  t,
                  "w-start",
                  noSpaces ? this.tr("Start working", "开始使用") : this.tr("Done", "完成"),
                  "primary",
                  (_e, cx) => this.start(cx),
                  I("arrow-right"),
                ),
          ),
      );
  }
}
