// App-owned settings mod. Writes changes immediately; no Save step.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, Scrollbar, InputState, Input, TextareaState, Textarea, Popup } from "gpui-base";
import { hc, run, theme, themes, fonts, updates, settings, layouts, layout, spaces, keys, shared, setShared, home, appHome, readTextFile, writeTextFile, playSound, props, closeOverlay, notify, window, checking, knownAgents } from "herdr";
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
  { id: "agents", title: "Agents & ACP", zh: "智能体与 ACP", icon: "bot" },
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
  trae: { path: "icons/trae.svg", tint: true },
  glm: { path: "icons/agent.svg", tint: true },
  qwen: { path: "icons/agent.svg", tint: true },
  kiro: { path: "icons/kiro.svg", tint: false },
  amp: { path: "icons/amp.svg", tint: false },
  grok: { path: "icons/grok.svg", tint: true },
  goose: { path: "icons/goose.svg", tint: true },
  crush: { path: "icons/crush.svg", tint: false },
  omp: { path: "icons/omp.svg", tint: true },
  pig: { path: "icons/pig.svg", tint: false },
  empryo: { path: "icons/empryo.svg", tint: false },
};

// Canonical catalog of all Herdr2-supported CLI & ACP agents so the Agents & ACP
// settings page always displays every supported agent with accurate CLI / ACP capabilities
// and one-click install metadata.
const SUPPORTED_AGENTS_CATALOG = [
  {
    id: "claude",
    name: "Claude Code",
    cli: "claude",
    acpMode: "managed",
    acpCommand: "claude-agent-acp",
    install: {
      argv: ["npm", "install", "-g", "@anthropic-ai/claude-code"],
      display: "npm install -g @anthropic-ai/claude-code",
    },
  },
  {
    id: "codex",
    name: "Codex",
    cli: "codex",
    acpMode: "managed",
    acpCommand: "codex-acp",
    install: {
      argv: ["npm", "install", "-g", "@openai/codex"],
      display: "npm install -g @openai/codex",
    },
  },
  {
    id: "agy",
    name: "Antigravity",
    cli: "agy",
    acpMode: "builtin",
    acpCommand: "agy acp",
  },
  {
    id: "cursor",
    name: "Cursor",
    cli: "cursor-agent",
    acpMode: "builtin",
    acpCommand: "cursor-agent acp",
    install: {
      argv: ["sh", "-c", "curl https://cursor.com/install -fsS | bash"],
      winArgv: ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "irm 'https://cursor.com/install?win32=true' | iex"],
      display: "curl https://cursor.com/install -fsS | bash",
    },
  },
  {
    id: "gemini",
    name: "Gemini CLI",
    cli: "gemini",
    acpMode: "builtin",
    acpCommand: "gemini --acp",
    install: {
      argv: ["npm", "install", "-g", "@google/gemini-cli"],
      display: "npm install -g @google/gemini-cli",
    },
  },
  {
    id: "opencode",
    name: "OpenCode",
    cli: "opencode",
    acpMode: "builtin",
    acpCommand: "opencode acp",
    install: {
      argv: ["npm", "install", "-g", "opencode-ai"],
      display: "npm install -g opencode-ai",
    },
  },
  {
    id: "pi",
    name: "Pi",
    cli: "pi",
    acpMode: "managed",
    acpCommand: "pi-acp",
    install: {
      argv: ["npm", "install", "-g", "@mariozechner/pi-coding-agent"],
      display: "npm install -g @mariozechner/pi-coding-agent",
    },
  },
  {
    id: "kimi",
    name: "Kimi",
    cli: "kimi",
    acpMode: "builtin",
    acpCommand: "kimi acp",
    install: {
      argv: ["uv", "tool", "install", "kimi-cli"],
      display: "uv tool install kimi-cli",
    },
  },
  {
    id: "copilot",
    name: "GitHub Copilot",
    cli: "copilot",
    acpMode: "builtin",
    acpCommand: "copilot --acp --stdio",
    install: {
      argv: ["npm", "install", "-g", "@github/copilot"],
      display: "npm install -g @github/copilot",
    },
  },
  {
    id: "qwen",
    name: "Qwen Code",
    cli: "qwen",
    acpMode: "builtin",
    acpCommand: "qwen --acp",
    install: {
      argv: ["npm", "install", "-g", "@qwen-code/qwen-code"],
      display: "npm install -g @qwen-code/qwen-code",
    },
  },
  {
    id: "hermes",
    name: "Hermes",
    cli: "hermes",
    acpMode: "separate",
    acpCommand: "hermes-acp",
  },
  {
    id: "zcode",
    name: "ZCode",
    cli: "zcode",
    acpMode: "separate",
    acpCommand: "zcode-acp-server",
  },
  {
    id: "trae",
    name: "TRAE CLI",
    cli: "traecli",
    acpMode: "builtin",
    acpCommand: "traecli --acp",
  },
  {
    id: "glm",
    name: "GLM",
    cli: null,
    standaloneAcp: true,
    acpMode: "managed",
    acpCommand: "glm-acp-agent",
  },
  {
    id: "dsh",
    name: "DeepSeek Harness",
    cli: "dsh-tui",
    acpMode: "builtin",
    acpCommand: "dsh --profile acp",
  },
  {
    id: "qoder",
    name: "Qoder",
    cli: "qodercli",
    acpMode: "none",
  },
  {
    id: "kiro",
    name: "Kiro",
    cli: "kiro-cli",
    acpMode: "none",
  },
  {
    id: "amp",
    name: "Amp",
    cli: "amp",
    acpMode: "none",
    install: {
      argv: ["npm", "install", "-g", "@sourcegraph/amp"],
      display: "npm install -g @sourcegraph/amp",
    },
  },
  {
    id: "grok",
    name: "Grok",
    cli: "grok",
    acpMode: "none",
  },
  {
    id: "goose",
    name: "goose",
    cli: "goose",
    acpMode: "none",
  },
  {
    id: "crush",
    name: "Crush",
    cli: "crush",
    acpMode: "none",
    install: {
      argv: ["npm", "install", "-g", "@charmland/crush"],
      display: "npm install -g @charmland/crush",
    },
  },
  {
    id: "omp",
    name: "oh-my-pi",
    cli: "omp",
    acpMode: "none",
    install: {
      argv: ["npm", "install", "-g", "@oh-my-pi/pi-coding-agent"],
      display: "npm install -g @oh-my-pi/pi-coding-agent",
    },
  },
  {
    id: "pig",
    name: "PiG",
    cli: "pig",
    acpMode: "none",
  },
  {
    id: "empryo",
    name: "Empryo",
    cli: "empryo",
    acpMode: "none",
  },
];

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
const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);

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
    this.adapterStatus = null;
    this.adapterBusy = false;
    this.adapterConfigSaving = false;
    this.adapterError = null;
    this.adapterActionFeedback = {};
    this.agentFilter = "all"; // "all" | "installed" | "missing" | "issues"
    this.adapterRegistryUrlInput = InputState.new({
      placeholder: "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json",
    });
    this.droveAutoProbed = {};
    this.droveProbeAgentLaunching = false;
    this.droveProbeFixOpen = false;
    this.droveProbeFixFocusId = null;
    this.droveProbeFixAgent = null;
    this.droveProbeFixModel = null;
    this.droveProbeFixEffort = null;
    this.agentAiInstallTargetId = null;
    this.agentAiInstallAgent = null;
    this.agentAiInstallModel = null;
    this.agentAiInstallEffort = null;
    this.droveFixKnownAgents = {};
    this.droveFixKnownLoading = false;
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
    this.releasesUrlInput.on("blur", (_e, cx) => {
      this.saveReleasesUrl(cx);
    });
    this.releasesUrlInput.on("submit", (_e, cx) => {
      this.triggerUpdateCheck(cx);
    });
    this.waitingUpdateCheck = false;
    this.pendingUpdateCheckSeq = 0;
    this.updateModal = null;
    this.aiUpdateLaunching = false;
    this.aiUpdateOpen = false;
    this.aiUpdateAgent = null;
    this.aiUpdateModel = null;
    this.aiUpdateEffort = null;
    this.aiUpdateExtraError = null;
    this.aiUpdateLocalProbed = null;
    this.aiUpdateCatalogLoading = false;
    this.skillData = null;
    this.skillMachine = null; // null = local, else SSH machine name
    this.skillLoading = false;
    this.skillInstalling = null; // null | "all" | "ai" | "custom" | agentId
    this.skillError = null;
    this.skillResult = null;
    this.skillAgentFilter = "detected"; // "detected" | "all"
    this.skillCustomDirInput = InputState.new({ placeholder: this.tr("~/.claude/skills or /custom/skills/dir", "~/.claude/skills 或自定义技能目录") });
    this.skillAiHintInput = InputState.new({ placeholder: this.tr("Optional: specify an agent or custom directory (leave blank to auto-detect all)…", "可选：输入想适配的 Agent 名称或特殊路径（留空则自动全面识别）…") });
    this.recordingTarget = null;
    this.lastRecordedSeq = Number(shared("settings.recorded_shortcut")?.seq ?? 0);
    this.lastKeysReassignedSeq = Number(shared("keys.reassigned")?.seq ?? 0);
    this.keysNotice = null;
    this.keysError = null;
    this.handle = cx.focus_handle();
    this.handle.focus();
    cx.spawn(async (cx) => {
      await this.load();
      if (!(typeof checking === "function" && checking())) {
        if (this.section === "keyboard") await this.loadKeys();
        if (this.section === "machines" || this.section === "updates") await this.loadMachines();
        if (this.section === "ai") {
          await Promise.all([this.loadMachines(), this.loadAi()]);
        }
        if (this.section === "agents") {
          await Promise.all([this.loadMachines(), this.loadDrove()]);
          this.loadDroveFixKnownAgents(cx);
          this.maybeAutoProbeDrove(cx);
        }
        if (this.section === "drove") {
          await Promise.all([this.loadMachines(), this.loadAi(), this.loadDrove()]);
          this.maybeAutoProbeDrove(cx);
        }
        if (this.section === "skills") {
          await Promise.all([this.loadMachines(), this.loadSkills(), this.loadAi(), this.droveData ? Promise.resolve() : this.loadDrove()]);
        }
      }
      cx.notify();
    });
    cx.timer.every(3000, (cx) => {
      if (this.section !== "machines" && this.section !== "updates") return;
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
    const winInfo = typeof window === "function" ? window() : null;
    const isWin = winInfo?.platform === "windows";
    const installedWinShells = isWin && Array.isArray(winInfo?.shells)
      ? new Set(winInfo.shells.map((sh) => String(sh).trim().toLowerCase()).filter(Boolean))
      : null;
    const allWinShellChoices = [
      { value: "auto", label: this.tr("Auto (pwsh / PowerShell)", "自动（pwsh / PowerShell）"), meta: "default" },
      { value: "powershell", label: "Windows PowerShell", meta: "powershell.exe" },
      { value: "pwsh", label: "PowerShell 7 (pwsh)", meta: "pwsh.exe" },
      { value: "git-bash", label: "Git Bash", meta: "bash.exe" },
      { value: "cmd", label: this.tr("Command Prompt (cmd)", "命令提示符 (cmd)"), meta: "cmd.exe" },
    ];
    const shellChoices = isWin
      ? allWinShellChoices.filter(
          (opt) => opt.value === "auto" || !installedWinShells || installedWinShells.size === 0 || installedWinShells.has(opt.value),
        )
      : [
          { value: "auto", label: this.tr("Auto ($SHELL)", "自动（系统 $SHELL）"), meta: "default" },
          { value: "zsh", label: "zsh", meta: "zsh" },
          { value: "bash", label: "bash", meta: "bash" },
          { value: "fish", label: "fish", meta: "fish" },
          { value: "sh", label: "sh", meta: "sh" },
        ];
    const rawShell = String(p.shell ?? s.shell ?? "auto").trim() || "auto";
    const currentShell = isWin && rawShell.toLowerCase() === "wsl" ? "auto" : rawShell;
    const canonicalWinShell = isWin
      ? (currentShell.toLowerCase() === "powershell7" ? "pwsh" : currentShell.toLowerCase() === "gitbash" || currentShell.toLowerCase() === "bash" ? "git-bash" : currentShell.toLowerCase())
      : currentShell;
    const missingWinShell = Boolean(
      isWin &&
      installedWinShells &&
      installedWinShells.size > 0 &&
      canonicalWinShell !== "auto" &&
      ["powershell", "pwsh", "git-bash", "cmd"].includes(canonicalWinShell) &&
      !installedWinShells.has(canonicalWinShell),
    );
    const shellItems = shellChoices.map((opt) => ({ ...opt, active: opt.value === (missingWinShell ? "auto" : canonicalWinShell) }));
    const matchedShellLabel = allWinShellChoices.find((opt) => opt.value === canonicalWinShell)?.label
      ?? shellChoices.find((opt) => opt.value === currentShell)?.label
      ?? currentShell;
    const currentShellLabel = missingWinShell
      ? `${matchedShellLabel} · ${this.tr("missing (auto)", "未安装 · 自动回退")}`
      : matchedShellLabel;
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
          this.tr("Fixed shell used for local terminals, splits, and CLI agents.", "本地新建终端、分栏以及启动 CLI 智能体时固定使用的终端类型。"),
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

  async droveHcJson(args, opts) {
    if (typeof checking === "function" && checking()) return null;
    const localOnly = Boolean(opts?.local);
    const cmd = [...(localOnly ? args : this.droveArgs(args)), "--json"];
    const hcOpts = opts && "timeout" in opts ? { timeout: opts.timeout } : undefined;
    const r = hcOpts ? await hc(cmd, hcOpts) : await hc(cmd);
    let parsed = null;
    try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
    if (r.code !== 0) {
      throw new Error(parsed?.error?.message || parsed?.message || (r.stderr || r.stdout || "Drove command failed.").trim());
    }
    return parsed;
  }

  async droveCallWithJsonFile(args, payload, opts) {
    if (typeof checking === "function" && checking()) return null;
    const reqPath = `${appHome()}/.drove-req-${Date.now()}-${Math.random().toString(36).slice(2, 7)}.json`;
    await writeTextFile(reqPath, JSON.stringify(payload), null);
    return this.droveHcJson([...args, "--file", reqPath, "--consume"], opts);
  }

  async aiHcJson(args) {
    if (typeof checking === "function" && checking()) return null;
    const r = await hc([...args, "--json"]);
    let parsed = null;
    try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
    if (r.code !== 0) {
      throw new Error(parsed?.error?.message || parsed?.message || (r.stderr || r.stdout || "AI command failed.").trim());
    }
    return parsed;
  }

  async aiCallWithJsonFile(args, payload) {
    if (typeof checking === "function" && checking()) return null;
    const reqPath = `${appHome()}/.ai-req-${Date.now()}-${Math.random().toString(36).slice(2, 7)}.json`;
    await writeTextFile(reqPath, JSON.stringify(payload), null);
    return this.aiHcJson([...args, "--file", reqPath, "--consume"]);
  }

  syncDroveInputs(data) {
    const cfg = data?.config ?? {};
    this.droveMagpieUrlInput.set_value(cfg.magpie?.gatewayUrl ?? "");
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
      const data = await this.aiHcJson(["ai", "status"]);
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
        const next = await this.aiCallWithJsonFile(["ai", "settings-set"], patch);
        if (next) {
          this.aiStatus = next;
          if (syncInputs) this.syncAiInputs(next);
          if (patch && ("promptMode" in patch || "promptCacheEnabled" in patch || "model" in patch || "baseUrl" in patch)) {
            this.aiPromptProbeResult = null;
          }
          if (patch && ("baseUrl" in patch || "apiKey" in patch || "clearKey" in patch)) {
            this.aiTestResult = null;
          }
          if (this.section === "drove") {
            await this.loadDrove();
          } else {
            this.droveData = null;
          }
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
        const next = await this.aiCallWithJsonFile(["ai", "settings-set"], {
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
      const res = await this.aiHcJson(["ai", "probe-prompt-mode"]);
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
        const res = await this.aiCallWithJsonFile(["ai", "test"], payload);
        this.aiTestResult = res;
        this.droveTestResult = res;
        if (res?.ok && this.aiStatus) {
          const patchStatus = { ...this.aiStatus };
          if (Array.isArray(res.availableModels) && res.availableModels.length > 0) {
            patchStatus.availableModels = res.availableModels;
          }
          if (res.protocol) {
            patchStatus.protocol = res.protocol;
          }
          this.aiStatus = patchStatus;
        }
      } catch (e) {
        this.aiTestResult = { ok: false, error: String(e?.message ?? e) };
      } finally {
        this.aiTesting = false;
        cx.notify();
      }
    });
  }

  syncAdapterInputs(status) {
    if (!this.adapterRegistryUrlInput) return;
    this.adapterRegistryUrlInput.set_value(status?.custom_registry_url ?? "");
  }

  async loadDrove() {
    if (this.droveLoading || (typeof checking === "function" && checking())) return;
    this.droveLoading = true;
    this.droveError = null;
    this.adapterError = null;
    try {
      const data = await this.droveHcJson(["drove", "settings"], { timeout: 60000 });
      this.droveData = data;
      this.syncDroveInputs(data);
      try {
        this.adapterStatus = await this.droveHcJson(["adapter", "status"], { timeout: 15000 });
        this.syncAdapterInputs(this.adapterStatus);
      } catch {}
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
        const snap = await this.droveHcJson(["drove", "probe", "--refresh"], { timeout: 180000 });
        if (this.droveMachine !== targetMachine) return false;
        if (snap && this.droveData) this.droveData = { ...this.droveData, agents: snap };
        else {
          await this.loadDrove();
          if (snap && this.droveData) this.droveData = { ...this.droveData, agents: snap };
        }
        try {
          this.adapterStatus = await this.droveHcJson(["adapter", "status", "--refresh"], { timeout: 30000 });
          this.syncAdapterInputs(this.adapterStatus);
        } catch {}
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

  refreshAgentsDetection(cx) {
    this.loadDroveFixKnownAgents(cx, true);
    this.probeDroveAgents(cx);
  }

  selectTargetMachine(value, cx) {
    this.droveMachine = value;
    this.droveData = null;
    this.droveProbePromise = null;
    this.adapterStatus = null;
    this.adapterError = null;
    this.adapterActionFeedback = {};
    this.picking = null;
    this.droveModelSubmit = null;
    this.droveStrengthSubmit = null;
    this.droveTestResult = null;
    this.droveAiConfigResult = null;
    this.droveRolesBackup = null;
    this.droveAiConfigMode = null;
    this.droveMagpieResult = null;
    this.droveProbeFixOpen = false;
    this.droveProbeFixFocusId = null;
    this.droveProbeFixAgent = null;
    this.droveProbeFixModel = null;
    this.droveProbeFixEffort = null;
    this.agentAiInstallTargetId = null;
    this.agentAiInstallAgent = null;
    this.agentAiInstallModel = null;
    this.agentAiInstallEffort = null;
    this.syncAdapterInputs(null);
    cx.spawn(async (cx) => {
      await this.loadDrove();
      if (this.section === "agents") {
        this.loadDroveFixKnownAgents(cx);
      }
      this.maybeAutoProbeDrove(cx);
      cx.notify();
    });
  }

  openAgentsSection(cx) {
    if (this.recordingTarget) this.cancelRecordingShortcut(null);
    this.section = "agents";
    this.picking = null;
    this.machineEdit = null;
    this.handle?.focus?.();
    cx.spawn(async (cx) => {
      if (this.machineList === null) await this.loadMachines();
      if (!this.droveData || !this.adapterStatus) await this.loadDrove();
      this.loadDroveFixKnownAgents(cx);
      this.maybeAutoProbeDrove(cx);
      cx.notify();
    });
    cx.notify();
  }

  saveAcpConfig(patch, cx) {
    if (this.adapterConfigSaving || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine;
    const args = ["adapter", "config"];
    if ("autoInstall" in patch) {
      args.push("--auto-install", patch.autoInstall ? "true" : "false");
    }
    if ("registryUrl" in patch) {
      const trimmed = String(patch.registryUrl ?? "").trim();
      if (trimmed) args.push("--registry-url", trimmed);
      else args.push("--clear-registry-url");
    }
    if ("defaultPermissions" in patch && patch.defaultPermissions) {
      args.push("--default-permissions", String(patch.defaultPermissions));
    }
    if ("injectMcp" in patch) {
      args.push("--inject-mcp", patch.injectMcp ? "true" : "false");
    }
    if (args.length <= 2) return;
    this.adapterConfigSaving = true;
    this.adapterError = null;
    cx?.notify?.();
    cx.spawn(async (cx) => {
      try {
        const res = await this.droveHcJson(args, { timeout: 20000 });
        if (this.droveMachine !== targetMachine) return;
        if (res) {
          this.adapterStatus = res;
          this.syncAdapterInputs(res);
        }
      } catch (e) {
        if (this.droveMachine === targetMachine) {
          this.adapterError = String(e?.message ?? e);
          notify({
            title: this.tr("ACP configuration not saved", "ACP 配置保存失败"),
            body: this.adapterError,
            level: "error",
          });
        }
      } finally {
        this.adapterConfigSaving = false;
        cx?.notify?.();
      }
    });
  }

  checkAdapterUpdates(cx) {
    if (this.adapterBusy || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine;
    this.adapterBusy = "check";
    this.adapterError = null;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const res = await this.droveHcJson(["adapter", "status", "--refresh"], { timeout: 30000 });
        if (this.droveMachine !== targetMachine) return;
        if (res) {
          this.adapterStatus = res;
          this.syncAdapterInputs(res);
        }
        if (res?.registry_error) {
          this.adapterError = String(res.registry_error);
          notify({
            title: this.tr("Failed to check ACP updates", "检查 ACP 更新失败"),
            body: this.adapterError,
            level: "error",
          });
          return;
        }
        const adapters = Array.isArray(res?.adapters) ? res.adapters : [];
        const managed = adapters.filter((a) => a?.source === "managed" && a?.installed);
        const updatable = managed.filter((a) => Boolean(a.update_available));
        const now = Date.now();
        const nextFeedback = { ...(this.adapterActionFeedback ?? {}) };
        if (updatable.length > 0) {
          for (const item of updatable) {
            delete nextFeedback[item.preset_id];
          }
          this.adapterActionFeedback = nextFeedback;
          const summary = updatable
            .map((u) => `${u.preset_id}${u.version ? ` v${u.version}` : ""} → v${u.latest_version}`)
            .join(", ");
          notify({
            title: this.tr(
              `${updatable.length} ACP adapter update${updatable.length > 1 ? "s" : ""} available`,
              `发现 ${updatable.length} 个 ACP 适配器可更新`,
            ),
            body: summary,
            level: "info",
          });
        } else {
          for (const item of managed) {
            nextFeedback[item.preset_id] = {
              action: "up_to_date",
              version: item.version ?? null,
              error: null,
              at: now,
            };
          }
          this.adapterActionFeedback = nextFeedback;
          notify({
            title: this.tr("ACP adapters are up to date", "ACP 适配器已是最新版本"),
            body: managed.length > 0
              ? managed.map((m) => `${m.preset_id}${m.version ? ` v${m.version}` : ""}`).join(", ")
              : this.tr("No managed ACP adapters installed yet.", "当前尚未安装托管 ACP 适配器。"),
            level: "info",
          });
        }
      } catch (e) {
        if (this.droveMachine === targetMachine) {
          this.adapterError = String(e?.message ?? e);
          notify({
            title: this.tr("Failed to check ACP updates", "检查 ACP 更新失败"),
            body: this.adapterError,
            level: "error",
          });
        }
      } finally {
        this.adapterBusy = false;
        cx.notify();
      }
    });
  }

  runAdapterAction(cx, mode, presetId = null) {
    if (this.adapterBusy || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine;
    const isUpdate = mode === "update";
    this.adapterBusy = presetId ? `${mode}:${presetId}` : mode;
    this.droveError = null;
    this.adapterError = null;
    if (presetId) {
      const nextFeedback = { ...(this.adapterActionFeedback ?? {}) };
      delete nextFeedback[presetId];
      this.adapterActionFeedback = nextFeedback;
    } else {
      this.adapterActionFeedback = {};
    }
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        let args;
        if (isUpdate) {
          args = presetId ? ["adapter", "update", presetId] : ["adapter", "update"];
        } else if (mode === "reinstall") {
          args = presetId ? ["adapter", "install", presetId, "--force"] : ["adapter", "install", "--detected", "--force"];
        } else if (mode === "install-all") {
          args = ["adapter", "install", "--all"];
        } else {
          args = presetId ? ["adapter", "install", presetId] : ["adapter", "install", "--detected"];
        }
        const res = await this.droveHcJson(args, { timeout: 600000 });
        if (this.droveMachine !== targetMachine) return;
        const results = Array.isArray(res?.results) ? res.results : [];
        const now = Date.now();
        const nextFeedback = { ...(this.adapterActionFeedback ?? {}) };
        for (const item of results) {
          if (!item?.preset_id) continue;
          nextFeedback[item.preset_id] = {
            action: item.action,
            version: item.version ?? null,
            error: item.error ?? null,
            at: now,
          };
        }
        this.adapterActionFeedback = nextFeedback;

        const failed = results.filter((r) => r.action === "failed");
        const changed = results.filter((r) => r.action === "installed" || r.action === "updated");
        const upToDate = results.filter((r) => r.action === "up_to_date" || r.action === "already_ready");
        if (failed.length > 0) {
          const partial = changed.length > 0 || upToDate.length > 0;
          notify({
            title: isUpdate
              ? partial
                ? this.tr("ACP adapter update partially failed", "ACP 适配器部分更新失败")
                : this.tr("ACP adapter update failed", "ACP 适配器更新失败")
              : partial
                ? this.tr("ACP adapter install partially failed", "ACP 适配器部分安装失败")
                : this.tr("ACP adapter install failed", "ACP 适配器安装失败"),
            body: failed.map((f) => `${f.preset_id}: ${f.error || "failed"}`).join("; "),
            level: partial ? "warning" : "error",
          });
        } else if (changed.length > 0) {
          const changedSummary = changed.map((c) => `${c.preset_id}${c.version ? ` v${c.version}` : ""}`).join(", ");
          const extraUpToDate =
            upToDate.length > 0
              ? this.tr(
                  ` (${upToDate.map((u) => u.preset_id).join(", ")} already up to date)`,
                  `（${upToDate.map((u) => u.preset_id).join("、")} 已是最新）`,
                )
              : "";
          notify({
            title: isUpdate
              ? this.tr("ACP adapters updated", "ACP 适配器已更新")
              : this.tr("ACP adapters ready", "ACP 适配器已就绪"),
            body: `${changedSummary}${extraUpToDate}`,
            level: "success",
          });
        } else if (upToDate.length > 0) {
          notify({
            title: this.tr("ACP adapters already up to date", "ACP 适配器已是最新版本"),
            body: upToDate.map((u) => `${u.preset_id}${u.version ? ` v${u.version}` : ""}`).join(", "),
            level: "info",
          });
        } else {
          notify({
            title: isUpdate
              ? this.tr("No managed ACP adapters to update", "没有需要更新的托管 ACP 适配器")
              : this.tr("No ACP adapters needed installation", "没有需要安装的 ACP 适配器"),
            level: "info",
          });
        }
        try {
          this.adapterStatus = await this.droveHcJson(["adapter", "status"], { timeout: 15000 });
          this.syncAdapterInputs(this.adapterStatus);
        } catch {}
        if (changed.length > 0) {
          await this.runDroveProbe(cx);
        }
      } catch (e) {
        if (this.droveMachine === targetMachine) {
          this.adapterError = String(e?.message ?? e);
          if (presetId) {
            this.adapterActionFeedback = {
              ...(this.adapterActionFeedback ?? {}),
              [presetId]: {
                action: "failed",
                version: null,
                error: this.adapterError,
                at: Date.now(),
              },
            };
          }
          notify({
            title: isUpdate
              ? this.tr("ACP adapter update failed", "ACP 适配器更新失败")
              : this.tr("ACP adapter install failed", "ACP 适配器安装失败"),
            body: this.adapterError,
            level: "error",
          });
        }
      } finally {
        this.adapterBusy = false;
        cx.notify();
      }
    });
  }

  buildDroveProbeFixPrompt(errorAgents) {
    const machineLabel = this.droveMachine || this.tr("this computer (local)", "本机（local）");
    const errLines = errorAgents.map((a) => {
      const cmd = Array.isArray(a.command) && a.command.length > 0 ? a.command.join(" ") : a.id;
      const err = String(a.error ?? "").trim();
      const kind = a.issueKind === "adapter"
        ? "managed_acp_adapter_install"
        : a.issueKind === "env"
          ? "environment_detection"
          : a.issueKind === "auth"
            ? "acp_auth_required"
            : "acp_probe";
      return `- agent: ${a.id}\n  kind: ${kind}\n  command: ${cmd}\n  path: ${a.path ?? "(unknown)"}\n  error: ${this.stripAcpAuthRawNoise(err)}`;
    });

    if (this.lang() === "zh-CN") {
      return [
        `请帮我排查 Herdr2 在目标主机「${machineLabel}」上检测到的智能体与 ACP 异常：`,
        "",
        ...errLines,
        "",
        "【重要约束】不要直接开始修复！在我明确确认修复方案前，禁止执行任何修改配置/文件、安装/升级软件、登录或重启服务等变更操作，仅允许进行只读排查。",
        "",
        "请严格按以下阶段推进：",
        "1. **只读排查与解读错误原因**：",
        "   - 对比守护进程 `herdr2 serve` 与当前交互式 Shell 的环境差异（尤其是 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY、PATH、Node/npm、认证凭据文件如 `~/.pi/agent/auth.json` / `models.json`、Windows 注册表用户环境变量 vs PowerShell `$PROFILE` / `~/.bashrc`、以及 ACP 适配器是否已安装）。",
        "   - 区分哪些是“工作区 CLI 正常但守护进程无头 ACP 探测环境缺失”导致的假阳性，哪些是需要登录、修配置、设代理镜像或安装适配器的真实问题。",
        "   - 逐项向我清楚解读每个异常 Agent 的报错含义与根本原因。",
        "2. **制定修复方案并等待确认**：",
        "   - 针对每个问题给出具体、可执行的修复方案（说明拟执行的命令、会改动的配置或文件、是否需要我手动交互登录如 `claude` / `cursor-agent login` 或重启服务，以及潜在影响）。",
        "   - 输出错误原因解读与修复方案后立即停下，等待我明确确认（或指定只修复其中部分问题），切勿直接开始修复。",
        "3. **用户确认后再开始修复与验证**：",
        "   - 收到我的确认后，再按确认的方案逐步执行修复；需要我手动交互的步骤给出清晰指引。",
        "   - 修复完成后执行 `h2 adapter status` 与 `h2 drove probe --refresh` 验证异常是否已消除。",
      ].join("\n");
    }
    return [
      `Please help investigate the Herdr2 Agents & ACP errors on "${machineLabel}":`,
      "",
      ...errLines,
      "",
      "[IMPORTANT] Do NOT start fixing directly! Until I explicitly confirm the fix plan, only perform read-only diagnostics—do not modify files or configs, install/upgrade packages, run logins, or restart services.",
      "",
      "Please follow these phases strictly:",
      "1. **Read-only diagnosis & explain root causes**:",
      "   - Compare the `herdr2 serve` daemon environment against the interactive shell (HTTP_PROXY/HTTPS_PROXY/ALL_PROXY, PATH, Node/npm, persistent auth files like `~/.pi/agent/auth.json` / `models.json`, Windows Registry user env vars vs PowerShell `$PROFILE` / `~/.bashrc`, and installed ACP adapters).",
      "   - Distinguish workspace-CLI vs headless-ACP false positives from genuine missing-adapter, network/mirror, config, or login issues.",
      "   - Clearly explain what each error means and its root cause.",
      "2. **Propose a fix plan & wait for confirmation**:",
      "   - Draft a concrete, actionable fix plan for each issue (commands to run, files/configs to change, any manual interactive login steps such as `claude` / `cursor-agent login` or daemon restarts, and potential impact).",
      "   - Stop after presenting the root-cause explanation and fix plan, and wait for my explicit confirmation before making any changes.",
      "3. **Start fixing only after confirmation & verify**:",
      "   - Once I confirm (all or selected items), execute the approved plan step by step and guide me through any interactive steps.",
      "   - After fixing, run `h2 adapter status` and `h2 drove probe --refresh` to verify the errors are resolved.",
    ].join("\n");
  }

  buildAgentInstallPrompt(row) {
    const machineLabel = this.droveMachine || this.tr("this computer (local)", "本机（local）");
    const needSeparateAcp = Boolean(row.cliInstalled && row.acpMode === "separate" && !row.acpInstalled);
    if (this.lang() === "zh-CN") {
      return [
        needSeparateAcp
          ? `请帮我在目标主机「${machineLabel}」上安装并配置 ${row.name}（${row.id}）对应的 ACP 组件（${row.acpCommand || row.id}）。`
          : `请帮我在目标主机「${machineLabel}」上安装 ${row.name}（${row.id}${row.cli ? `，CLI 命令：${row.cli}` : ""}${row.acpCommand ? `，ACP 命令：${row.acpCommand}` : ""}）。`,
        "",
        "请按以下步骤处理：",
        "1. 先检查当前操作系统、包管理器（npm / uv / brew / curl / winget 等）与 PATH/代理环境。",
        "2. 给出最适合本机环境的官方安装方案并说明将执行的命令；在我确认后执行安装。",
        "3. 如需登录或配置 API Key，请给出清晰指引；安装完成后运行 `h2 drove probe --refresh` 验证。",
      ].join("\n");
    }
    return [
      needSeparateAcp
        ? `Please help install and configure the ACP component (${row.acpCommand || row.id}) for ${row.name} (${row.id}) on "${machineLabel}".`
        : `Please help install ${row.name} (${row.id}${row.cli ? `, CLI: ${row.cli}` : ""}${row.acpCommand ? `, ACP: ${row.acpCommand}` : ""}) on "${machineLabel}".`,
      "",
      "Please follow these steps:",
      "1. Check the OS, available package managers (npm / uv / brew / curl / winget), PATH, and proxy environment first.",
      "2. Propose the cleanest official installation command for this machine and run it once I confirm.",
      "3. Guide me through any required login or API key setup, then verify with `h2 drove probe --refresh`.",
    ].join("\n");
  }

  agentLoginSpec(agentId, row = null) {
    const id = this.canonicalFixAgentId(agentId ?? row?.id);
    switch (id) {
      case "pi":
        return {
          argv: ["pi"],
          display: "pi (/login)",
          hintEn: "Run `pi` and execute `/login` (or configure `~/.pi/agent/models.json`; Windows env vars must be in User/System env, not only $PROFILE)",
          hintZh: "运行 `pi` 并执行 `/login` 登录或配置 `~/.pi/agent/models.json`（Windows 环境变量需设在用户/系统变量，勿仅写在 $PROFILE）",
        };
      case "claude":
        return {
          argv: ["claude"],
          display: "claude",
          hintEn: "Run `claude` in a terminal to complete setup & sign in",
          hintZh: "在终端运行 `claude` 选择主题并完成登录（或执行 `/login`）",
        };
      case "cursor":
        return {
          argv: ["cursor-agent", "login"],
          display: "cursor-agent login",
          hintEn: "Run `cursor-agent login` in a terminal to sign in",
          hintZh: "在终端运行 `cursor-agent login` 完成登录",
        };
      case "codex":
        return {
          argv: ["codex", "login"],
          display: "codex login",
          hintEn: "Run `codex login` in a terminal to sign in",
          hintZh: "在终端运行 `codex login` 完成登录",
        };
      case "copilot":
        return {
          argv: ["copilot", "login"],
          display: "copilot login",
          hintEn: "Run `copilot login` in a terminal to sign in",
          hintZh: "在终端运行 `copilot login` 完成登录",
        };
      case "kimi":
        return {
          argv: ["kimi", "login"],
          display: "kimi login",
          hintEn: "Run `kimi login` in a terminal to sign in",
          hintZh: "在终端运行 `kimi login` 完成登录",
        };
      case "gemini":
        return {
          argv: ["gemini"],
          display: "gemini",
          hintEn: "Run `gemini` in a terminal to sign in or configure API key",
          hintZh: "在终端运行 `gemini` 完成登录或配置 API Key",
        };
      case "qwen":
        return {
          argv: ["qwen"],
          display: "qwen",
          hintEn: "Run `qwen` in a terminal to sign in or configure API key",
          hintZh: "在终端运行 `qwen` 完成登录或配置 API Key",
        };
      case "opencode":
        return {
          argv: ["opencode", "auth", "login"],
          display: "opencode auth login",
          hintEn: "Run `opencode auth login` in a terminal to configure credentials",
          hintZh: "在终端运行 `opencode auth login` 配置模型凭据",
        };
      default:
        if (row?.cliInstalled && row?.cli) {
          return {
            argv: [row.cli],
            display: row.cli,
            hintEn: `Sign in via \`${row.cli}\` in a terminal`,
            hintZh: `请在终端运行 \`${row.cli}\` 完成登录认证`,
          };
        }
        return null;
    }
  }

  launchAgentLoginCli(row, cx) {
    if (!row || (typeof checking === "function" && checking())) return;
    const spec = this.agentLoginSpec(row.id, row);
    if (!spec?.argv?.length) return;
    const targetMachine = this.droveMachine ?? null;
    const tabName = this.tr(`${row.name} Sign In`, `${row.name} 登录`);
    const targetSpace = this.targetHomeSpaceForMachine(targetMachine);
    const baseLaunchArgs = {
      folder: this.defaultHomeFolderForMachine(targetMachine),
      argv: [...spec.argv],
    };
    if (targetSpace) {
      run("tab.new", { space: targetSpace.id, name: tabName, ...baseLaunchArgs });
    } else {
      const args = { ...baseLaunchArgs, tab: tabName };
      if (targetMachine) args.machine = targetMachine;
      run("space.new", args);
    }
    this.close();
    const machineText = targetMachine || this.tr("this computer", "本机");
    notify({
      title: this.tr(`Opened ${row.name} sign-in terminal`, `已打开 ${row.name} 终端登录`),
      body: this.tr(
        `Complete sign-in (${spec.display}) on ${machineText}, then return to Settings → Agents & ACP and click Detect Environment.`,
        `请在「${machineText}」终端完成登录或配置 API Key（${spec.display}），完成后返回「设置 → 智能体与 ACP」点击「一键检测环境」。`,
      ),
    });
    cx?.notify?.();
  }

  installAgentCli(row, cx) {
    if (!row?.install || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine ?? null;
    const isLocalWindows = targetMachine === null && typeof window === "function" && String(window()?.platform ?? "").toLowerCase() === "windows";
    const argv = isLocalWindows && Array.isArray(row.install.winArgv) && row.install.winArgv.length > 0
      ? [...row.install.winArgv]
      : [...row.install.argv];
    const tabName = this.tr(`Install ${row.name}`, `安装 ${row.name}`);
    const targetSpace = this.targetHomeSpaceForMachine(targetMachine);
    const baseLaunchArgs = {
      folder: this.defaultHomeFolderForMachine(targetMachine),
      argv,
    };
    if (targetSpace) {
      run("tab.new", { space: targetSpace.id, name: tabName, ...baseLaunchArgs });
    } else {
      const args = { ...baseLaunchArgs, tab: tabName };
      if (targetMachine) args.machine = targetMachine;
      run("space.new", args);
    }
    this.close();
    const machineText = targetMachine || this.tr("this computer", "本机");
    notify({
      title: this.tr(`Installing ${row.name} in terminal`, `已在终端启动 ${row.name} 安装`),
      body: this.tr(
        `Running \`${row.install.display}\` on ${machineText}. Once finished, return to Settings → Agents & ACP and click Detect Environment.`,
        `正在「${machineText}」执行 \`${row.install.display}\`。安装完成后，返回「设置 → 智能体与 ACP」点击「一键检测环境」即可更新状态。`,
      ),
    });
    cx?.notify?.();
  }

  ensureAgentAiInstallSelection(targetRowId = null) {
    const options = this.droveFixAgentOptions();
    const excludeId = this.canonicalFixAgentId(targetRowId ?? this.agentAiInstallTargetId);
    const curId = this.canonicalFixAgentId(this.agentAiInstallAgent);
    let chosen = curId ? options.find((o) => o.id === curId) : null;
    if (!chosen) {
      chosen =
        options.find((o) => o.ready) ??
        options.find((o) => o.installed) ??
        options.find((o) => !excludeId || o.id !== excludeId) ??
        options[0] ??
        null;
      this.agentAiInstallAgent = chosen?.id ?? null;
      this.agentAiInstallModel = null;
    } else if (this.agentAiInstallAgent !== chosen.id) {
      this.agentAiInstallAgent = chosen.id;
    }
    const agent = chosen?.id ?? null;
    const model = String(this.agentAiInstallModel ?? "").trim() || null;
    const disabledReason = agent ? this.droveFixEffortDisabledReason(agent, model) : null;
    if (agent && this.agentAiInstallEffort && !disabledReason) {
      this.agentAiInstallEffort = this.clampEffortFor(agent, model, this.agentAiInstallEffort);
    }
    return {
      agent,
      model,
      effort: this.agentAiInstallEffort ?? null,
      disabledReason,
      option: chosen,
      options,
    };
  }

  setAgentAiInstallAgent(agentId, cx) {
    const next = this.canonicalFixAgentId(agentId);
    if (!next) return;
    this.agentAiInstallAgent = next;
    this.agentAiInstallModel = null;
    if (this.agentAiInstallEffort && !this.droveFixEffortDisabledReason(next, null)) {
      this.agentAiInstallEffort = this.clampEffortFor(next, null, this.agentAiInstallEffort);
    }
    cx?.notify?.();
  }

  setAgentAiInstallModel(modelId, cx) {
    const next = String(modelId ?? "").trim() || null;
    this.agentAiInstallModel = next;
    const agent = this.canonicalFixAgentId(this.agentAiInstallAgent);
    if (agent && this.agentAiInstallEffort && !this.droveFixEffortDisabledReason(agent, next)) {
      this.agentAiInstallEffort = this.clampEffortFor(agent, next, this.agentAiInstallEffort);
    }
    cx?.notify?.();
  }

  setAgentAiInstallEffort(effort, cx) {
    const raw = String(effort ?? "").trim().toLowerCase();
    if (!raw || raw === "auto" || raw === "default") {
      this.agentAiInstallEffort = null;
    } else {
      const agent = this.canonicalFixAgentId(this.agentAiInstallAgent);
      this.agentAiInstallEffort = agent
        ? this.clampEffortFor(agent, this.agentAiInstallModel, raw)
        : raw;
    }
    cx?.notify?.();
  }

  openAgentAiInstall(rowOrId, cx) {
    const targetId = typeof rowOrId === "string" ? rowOrId : rowOrId?.id;
    if (!targetId) return;
    if (
      this.agentAiInstallTargetId !== targetId &&
      typeof this.picking === "string" &&
      this.picking.startsWith("drove:agent-install-")
    ) {
      this.picking = null;
      this.droveModelSubmit = null;
    }
    this.agentAiInstallTargetId = targetId;
    this.ensureAgentAiInstallSelection(targetId);
    const machineKey = this.droveMachine ?? "__local__";
    if (!Array.isArray(this.droveFixKnownAgents?.[machineKey])) {
      this.loadDroveFixKnownAgents(cx);
    }
    cx?.notify?.();
  }

  closeAgentAiInstall(cx) {
    this.agentAiInstallTargetId = null;
    if (typeof this.picking === "string" && this.picking.startsWith("drove:agent-install-")) {
      this.picking = null;
      this.droveModelSubmit = null;
    }
    cx?.notify?.();
  }

  toggleAgentAiInstall(rowOrId, cx) {
    const targetId = typeof rowOrId === "string" ? rowOrId : rowOrId?.id;
    if (!targetId) return;
    if (this.agentAiInstallTargetId === targetId) {
      this.closeAgentAiInstall(cx);
    } else {
      this.openAgentAiInstall(targetId, cx);
    }
  }

  launchAgentToInstall(row, cx, override = null) {
    if (!row || this.droveProbeAgentLaunching || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine ?? null;
    const summary = this.droveProbeSummary();
    const selectedAgent = override?.agent !== undefined ? override.agent : this.agentAiInstallAgent;
    const selectedModel = override?.model !== undefined ? override.model : this.agentAiInstallModel;
    const selectedEffort = override?.effort !== undefined ? override.effort : this.agentAiInstallEffort;
    const needSeparateAcp = Boolean(row.cliInstalled && row.acpMode === "separate" && !row.acpInstalled);
    this.droveProbeAgentLaunching = true;
    cx?.notify?.();
    cx.spawn(async (cx) => {
      try {
        const picked = await this.resolveFixAgentLaunch(targetMachine, summary.ready, selectedAgent);
        if (!picked) {
          notify({
            title: this.tr("No installed AI agent available", "当前主机未检测到可用的 AI Agent"),
            body: this.tr(
              "Install one CLI agent first (for example Claude Code, Codex, or Gemini CLI using the Install button) to enable AI-assisted setup.",
              "请先使用「一键安装」安装至少一个 CLI Agent（如 Claude Code、Codex 或 Gemini CLI），之后即可由 AI 协助安装其他智能体。",
            ),
            level: "error",
          });
          return;
        }
        const pickedId = this.canonicalFixAgentId(picked.id || selectedAgent || picked.name);
        let cleanModel = pickedId === "zcode" ? null : (String(selectedModel ?? "").trim() || null);
        const effortDisabled = Boolean(this.droveFixEffortDisabledReason(pickedId, cleanModel));
        let cleanEffort = !effortDisabled && selectedEffort
          ? this.clampEffortFor(pickedId, cleanModel, selectedEffort)
          : null;
        if (pickedId === "cursor") {
          const resolvedCursor = this.resolveCursorCliModelForLaunch(cleanModel, cleanEffort, this.probedAgents());
          cleanModel = resolvedCursor.model;
          cleanEffort = resolvedCursor.effort;
        }
        const prompt = this.buildAgentInstallPrompt(row);
        const tabName = needSeparateAcp
          ? this.tr(`Install ${row.name} ACP`, `AI 安装 ${row.name} ACP`)
          : this.tr(`Install ${row.name}`, `AI 安装 ${row.name}`);
        const targetSpace = this.targetHomeSpaceForMachine(targetMachine);
        const baseLaunchArgs = {
          folder: this.defaultHomeFolderForMachine(targetMachine),
          argv: picked.argv,
          task: prompt,
        };
        if (cleanModel) baseLaunchArgs.model = cleanModel;
        if (cleanEffort) baseLaunchArgs.effort = cleanEffort;
        if (targetSpace) {
          run("tab.new", { space: targetSpace.id, name: tabName, ...baseLaunchArgs });
        } else {
          const args = { ...baseLaunchArgs, tab: tabName };
          if (targetMachine) args.machine = targetMachine;
          run("space.new", args);
        }
        this.agentAiInstallTargetId = null;
        if (typeof this.picking === "string" && this.picking.startsWith("drove:agent-install-")) {
          this.picking = null;
          this.droveModelSubmit = null;
        }
        this.close();
        const metaParts = [cleanModel, cleanEffort ? this.effortLabel(cleanEffort) : null].filter(Boolean);
        const metaEn = metaParts.length > 0 ? ` (${metaParts.join(" · ")})` : "";
        const metaZh = metaParts.length > 0 ? `（${metaParts.join(" · ")}）` : "";
        const targetLabel = needSeparateAcp ? `${row.name} ACP` : row.name;
        notify({
          title: this.tr(
            `Launched ${picked.name}${metaEn} to install ${targetLabel}`,
            `已启动 ${picked.name}${metaZh} 协助安装 ${targetLabel}`,
          ),
          body: this.tr(
            "After installation finishes, return to Settings → Agents & ACP and click Detect Environment.",
            "安装完成后，返回「设置 → 智能体与 ACP」点击「一键检测环境」即可更新状态。",
          ),
        });
      } catch (e) {
        notify({
          title: this.tr("Failed to launch AI agent", "启动 AI 协助失败"),
          body: String(e?.message ?? e),
          level: "error",
        });
      } finally {
        this.droveProbeAgentLaunching = false;
        cx.notify();
      }
    });
  }

  isLikelyRemoteHomePath(rawPath) {
    const p = String(rawPath ?? "").trim().replace(/[\\/]+$/, "");
    if (!p || p === "~" || p === "/root") return true;
    if (/^\/(?:home|Users)\/[^/\\]+$/.test(p)) return true;
    if (/^[A-Za-z]:[\\/]Users[\\/][^/\\]+$/i.test(p)) return true;
    return false;
  }

  isHomeSpace(space, targetMachine = null) {
    if (!space || (space.machine ?? null) !== (targetMachine ?? null)) return false;
    const spName = String(space.name ?? "").trim();
    const spFolder = String(space.folder ?? "").trim();
    if (spName === "~") return true;
    if (spName && !spFolder) return false;
    if (!spFolder || spFolder === "~") return true;
    if (targetMachine) {
      return this.isLikelyRemoteHomePath(spFolder);
    }
    const norm = (v) => String(v ?? "").trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
    const localHome = typeof home === "function" ? norm(home()) : "";
    return Boolean(localHome && norm(spFolder) === localHome);
  }

  targetHomeSpaceForMachine(targetMachine = null) {
    const wantMachine = targetMachine ?? null;
    const all = typeof spaces === "function" ? (spaces() ?? []) : [];
    const activeHome = all.find((s) => s.active && this.isHomeSpace(s, wantMachine));
    if (activeHome) return activeHome;
    return all.find((s) => this.isHomeSpace(s, wantMachine)) ?? null;
  }

  targetSpaceForDroveMachine() {
    return this.targetHomeSpaceForMachine(this.droveMachine ?? null);
  }

  defaultHomeFolderForMachine(targetMachine = null) {
    if (targetMachine) return "~";
    const localHome = typeof home === "function" ? String(home() ?? "").trim() : "";
    return localHome || "~";
  }

  resolveCursorCliModelForLaunch(modelId, effort, probedList = null) {
    const rawModel = String(modelId ?? "").trim();
    if (!rawModel || /^auto$/i.test(rawModel) || /^default$/i.test(rawModel)) {
      return { model: null, effort: null };
    }
    // Strip ACP-style bracket overrides (`grok-4.7[effort=high,fast=true]`) into CLI base + effort.
    let base = rawModel;
    let eff = String(effort ?? "").trim().toLowerCase() || null;
    if (eff === "auto" || eff === "default") eff = null;
    const bracketMatch = rawModel.match(/^([^\[]+)\[([^\]]*)\]$/);
    if (bracketMatch) {
      base = bracketMatch[1].trim();
      for (const pair of bracketMatch[2].split(",")) {
        const [kRaw, vRaw] = pair.split("=");
        const k = String(kRaw ?? "").trim().toLowerCase();
        const v = String(vRaw ?? "").trim().toLowerCase();
        if (["effort", "reasoning_effort", "reasoning", "thought_level", "thinking"].includes(k) && v && v !== "auto" && v !== "default") {
          eff = eff || v;
        } else if (k === "fast" && ["true", "1", "yes"].includes(v) && !base.toLowerCase().endsWith("-fast")) {
          base = `${base}-fast`;
        }
      }
    }

    const list = Array.isArray(probedList) ? probedList : this.probedAgents();
    const probedIds = this.probedModelsFromList(list, "cursor").map((m) => m.id);
    const findExact = (cand) => probedIds.find((x) => x.toLowerCase() === String(cand).toLowerCase()) ?? null;

    const isFast = base.toLowerCase().endsWith("-fast");
    const stem = isFast ? base.slice(0, -5) : base;
    const fastSuf = isFast ? "-fast" : "";
    const withTier = (tier) => `${stem}-${tier}${fastSuf}`;

    // Already carries an embedded effort suffix (e.g. `grok-4.7-high`, `grok-4.7-high-fast`).
    if (this.modelHasEmbeddedEffort(base)) {
      return { model: findExact(base) ?? base, effort: null };
    }

    if (eff) {
      const exactTier = findExact(withTier(eff));
      if (exactTier) return { model: exactTier, effort: null };
      // Some Cursor models (e.g. `gpt-5.3-codex`) use the bare slug for medium effort.
      if (eff === "medium" && findExact(base) && !findExact(withTier("medium"))) {
        return { model: findExact(base), effort: null };
      }
      if (probedIds.length > 0) {
        const anyVariant = ["high", "medium", "low", "xhigh", "max"]
          .map((t) => findExact(withTier(t)))
          .find(Boolean);
        if (anyVariant) return { model: anyVariant, effort: null };
        const exactBase = findExact(base);
        if (exactBase) return { model: exactBase, effort: null };
      }
      // Custom/unprobed Cursor model: let Rust format hyphenated CLI slug.
      return { model: base, effort: eff };
    }

    // No explicit effort selected: if probed catalog exposes `-medium` as a distinct CLI slug
    // (e.g. `grok-4.7-medium`, `claude-opus-5-5-medium`), the bare ACP slug (`grok-4.7`) is not
    // accepted by `cursor-agent --model` directly — promote to its CLI high/medium slug.
    if (probedIds.length > 0 && findExact(withTier("medium"))) {
      const preferred = findExact(withTier("high")) ?? findExact(withTier("medium"));
      if (preferred) return { model: preferred, effort: null };
    }
    return { model: findExact(base) ?? base, effort: null };
  }

  canonicalFixAgentId(raw) {
    const id = String(raw ?? "").trim().toLowerCase();
    if (!id) return "";
    if (id === "antigravity") return "agy";
    if (id === "cursor-agent") return "cursor";
    if (id === "dsh-tui") return "dsh";
    return id;
  }

  fixAgentFallbackArgv(agentId) {
    const id = this.canonicalFixAgentId(agentId);
    const cliFallback = {
      claude: ["claude", "--dangerously-skip-permissions"],
      agy: ["agy", "--dangerously-skip-permissions"],
      codex: ["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"],
      cursor: ["cursor-agent", "--yolo", "--trust"],
      opencode: ["opencode", "--auto"],
      gemini: ["gemini", "--yolo"],
      kimi: ["kimi", "--auto"],
      hermes: ["hermes", "--yolo", "--accept-hooks"],
      zcode: ["zcode", "--mode", "yolo"],
      dsh: ["dsh-tui"],
      copilot: ["copilot"],
      qoder: ["qodercli"],
    };
    return cliFallback[id] ? [...cliFallback[id]] : null;
  }

  normalizeFixAgentArgv(agentId, argv) {
    if (!Array.isArray(argv) || argv.length === 0) return null;
    const id = this.canonicalFixAgentId(agentId);
    if (id === "cursor" && argv[0] === "agent") {
      return ["cursor-agent", ...argv.slice(1)];
    }
    return [...argv];
  }

  agentOptionsFrom(probedList, machineKey) {
    const prefOrder = [
      "claude",
      "agy",
      "codex",
      "cursor",
      "opencode",
      "gemini",
      "hermes",
      "zcode",
      "kimi",
      "copilot",
      "qoder",
      "dsh",
    ];
    const knownList = Array.isArray(this.droveFixKnownAgents?.[machineKey])
      ? this.droveFixKnownAgents[machineKey]
      : [];
    const byId = new Map();

    for (const a of probedList ?? []) {
      const id = this.canonicalFixAgentId(a?.id);
      if (!id) continue;
      const argv = this.fixAgentFallbackArgv(id);
      const err = this.agentProbeError(a);
      const models = this.probedModelsFromList(probedList, a.id);
      const prev = byId.get(id);
      if (!prev) {
        byId.set(id, {
          id,
          name: a.name && a.name !== a.id ? a.name : id,
          installed: Boolean(a.installed),
          probeError: err,
          models,
          argv,
        });
      } else {
        if (a.installed) prev.installed = true;
        if (!prev.probeError && err) prev.probeError = err;
        if (prev.models.length === 0 && models.length > 0) prev.models = models;
        if (!prev.argv && argv) prev.argv = argv;
      }
    }

    for (const p of knownList) {
      const id = this.canonicalFixAgentId(p?.key);
      if (!id) continue;
      const runArgv = this.normalizeFixAgentArgv(id, p?.run);
      const prev = byId.get(id);
      if (prev) {
        if (p?.name) prev.name = p.name;
        if (p?.installed) prev.installed = true;
        if (runArgv) prev.argv = runArgv;
      } else if (runArgv) {
        byId.set(id, {
          id,
          name: p?.name || id,
          installed: Boolean(p?.installed),
          probeError: null,
          models: this.probedModelsFromList(probedList, id),
          argv: runArgv,
        });
      }
    }

    const rank = (id) => {
      const i = prefOrder.indexOf(id);
      return i === -1 ? 99 : i;
    };
    const group = (item) => {
      if (item.installed && !item.probeError) return 0;
      if (item.installed) return 1;
      return 2;
    };

    return [...byId.values()]
      .filter((item) => Array.isArray(item.argv) && item.argv.length > 0)
      .map((item) => ({ ...item, ready: Boolean(item.installed && !item.probeError) }))
      .sort((a, b) => {
        const ga = group(a);
        const gb = group(b);
        if (ga !== gb) return ga - gb;
        const ra = rank(a.id);
        const rb = rank(b.id);
        if (ra !== rb) return ra - rb;
        return a.id.localeCompare(b.id);
      });
  }

  droveFixAgentOptions() {
    const machineKey = this.droveMachine ?? "__local__";
    return this.agentOptionsFrom(this.probedAgents(), machineKey);
  }

  localProbedAgents() {
    if (Array.isArray(this.aiUpdateLocalProbed) && this.aiUpdateLocalProbed.length > 0) {
      return this.aiUpdateLocalProbed;
    }
    if ((this.droveMachine ?? null) === null && Array.isArray(this.droveData?.agents?.agents) && this.droveData.agents.agents.length > 0) {
      return this.droveData.agents.agents;
    }
    return DROVE_FALLBACK_AGENTS.map((id) => ({ id, name: id, installed: false, models: [], efforts: [] }));
  }

  aiUpdateAgentOptions() {
    return this.agentOptionsFrom(this.localProbedAgents(), "__local__");
  }

  droveFixEffortDisabledReason(agentId, modelId) {
    const id = this.canonicalFixAgentId(agentId);
    const cleanModel = String(modelId ?? "").trim();
    if (id === "zcode") {
      return this.tr(
        "ZCode does not accept startup --model/--effort flags; switch inside its TUI",
        "ZCode 不支持通过启动参数指定模型或思考强度，需进入后在 TUI 内切换",
      );
    }
    if ((id === "agy" || id === "cursor") && cleanModel && this.modelHasEmbeddedEffort(cleanModel)) {
      return this.tr(
        "Selected model already includes reasoning effort",
        "当前所选模型已自带思考强度，不可单独选择",
      );
    }
    if (id === "cursor" && !cleanModel) {
      return this.tr(
        "Select a Cursor model first to apply custom reasoning effort",
        "Cursor 需先选择具体模型才能指定自定义思考强度",
      );
    }
    return null;
  }

  ensureDroveFixSelection() {
    const options = this.droveFixAgentOptions();
    const curId = this.canonicalFixAgentId(this.droveProbeFixAgent);
    let chosen = curId ? options.find((o) => o.id === curId) : null;
    if (!chosen) {
      chosen = options.find((o) => o.ready) ?? options.find((o) => o.installed) ?? options[0] ?? null;
      this.droveProbeFixAgent = chosen?.id ?? null;
      this.droveProbeFixModel = null;
    } else if (this.droveProbeFixAgent !== chosen.id) {
      this.droveProbeFixAgent = chosen.id;
    }
    const agent = chosen?.id ?? null;
    const model = String(this.droveProbeFixModel ?? "").trim() || null;
    const disabledReason = agent ? this.droveFixEffortDisabledReason(agent, model) : null;
    if (agent && this.droveProbeFixEffort && !disabledReason) {
      this.droveProbeFixEffort = this.clampEffortFor(agent, model, this.droveProbeFixEffort);
    }
    return {
      agent,
      model,
      effort: this.droveProbeFixEffort ?? null,
      disabledReason,
      option: chosen,
      options,
    };
  }

  setDroveProbeFixAgent(agentId, cx) {
    const next = this.canonicalFixAgentId(agentId);
    if (!next) return;
    this.droveProbeFixAgent = next;
    this.droveProbeFixModel = null;
    if (this.droveProbeFixEffort && !this.droveFixEffortDisabledReason(next, null)) {
      this.droveProbeFixEffort = this.clampEffortFor(next, null, this.droveProbeFixEffort);
    }
    cx?.notify?.();
  }

  setDroveProbeFixModel(modelId, cx) {
    const next = String(modelId ?? "").trim() || null;
    this.droveProbeFixModel = next;
    const agent = this.canonicalFixAgentId(this.droveProbeFixAgent);
    if (agent && this.droveProbeFixEffort && !this.droveFixEffortDisabledReason(agent, next)) {
      this.droveProbeFixEffort = this.clampEffortFor(agent, next, this.droveProbeFixEffort);
    }
    cx?.notify?.();
  }

  setDroveProbeFixEffort(effort, cx) {
    const raw = String(effort ?? "").trim().toLowerCase();
    if (!raw || raw === "auto" || raw === "default") {
      this.droveProbeFixEffort = null;
    } else {
      const agent = this.canonicalFixAgentId(this.droveProbeFixAgent);
      this.droveProbeFixEffort = agent
        ? this.clampEffortFor(agent, this.droveProbeFixModel, raw)
        : raw;
    }
    cx?.notify?.();
  }

  loadDroveFixKnownAgents(cx, forceRefresh = false) {
    if (typeof knownAgents !== "function" || (typeof checking === "function" && checking())) return;
    const targetMachine = this.droveMachine ?? null;
    const machineKey = targetMachine ?? "__local__";
    if (this.droveFixKnownLoading && !forceRefresh) return;
    this.droveFixKnownLoading = true;
    cx?.notify?.();
    cx.spawn(async (spawnCx) => {
      try {
        const opts = {};
        if (targetMachine) opts.on = targetMachine;
        if (forceRefresh) opts.refresh = true;
        const list = await knownAgents(opts);
        if (Array.isArray(list)) {
          this.droveFixKnownAgents = { ...(this.droveFixKnownAgents ?? {}), [machineKey]: list };
          if ((this.droveMachine ?? null) === targetMachine) {
            this.ensureDroveFixSelection();
            if (this.agentAiInstallTargetId) {
              this.ensureAgentAiInstallSelection(this.agentAiInstallTargetId);
            }
          }
        }
      } catch {}
      finally {
        this.droveFixKnownLoading = false;
        spawnCx?.notify?.();
      }
    });
  }

  toggleDroveProbeFixPanel(cx) {
    this.droveProbeFixOpen = !this.droveProbeFixOpen;
    if (this.droveProbeFixOpen) {
      this.droveProbeFixFocusId = null;
      this.ensureDroveFixSelection();
      const machineKey = this.droveMachine ?? "__local__";
      if (!Array.isArray(this.droveFixKnownAgents?.[machineKey])) {
        this.loadDroveFixKnownAgents(cx);
      }
    } else {
      this.droveProbeFixFocusId = null;
      if (typeof this.picking === "string" && this.picking.startsWith("drove:probe-fix-")) {
        this.picking = null;
        this.droveModelSubmit = null;
      }
    }
    cx?.notify?.();
  }

  openAgentAiFix(cx, agentId = null) {
    this.droveProbeFixFocusId = agentId ? this.canonicalFixAgentId(agentId) : null;
    this.droveProbeFixOpen = true;
    this.ensureDroveFixSelection();
    const machineKey = this.droveMachine ?? "__local__";
    if (!Array.isArray(this.droveFixKnownAgents?.[machineKey])) {
      this.loadDroveFixKnownAgents(cx);
    }
    cx?.notify?.();
  }

  async resolveFixAgentLaunch(targetMachine, readyAgents, preferredAgentId = null) {
    const prefOrder = ["claude", "agy", "codex", "cursor", "opencode", "gemini", "hermes", "zcode", "kimi", "copilot", "qoder", "dsh"];
    const want = this.canonicalFixAgentId(preferredAgentId);
    const machineKey = targetMachine ?? "__local__";
    let knownList = null;

    if (typeof knownAgents === "function") {
      try {
        const list = await knownAgents(targetMachine ? { on: targetMachine } : {});
        if (Array.isArray(list)) {
          knownList = list;
          this.droveFixKnownAgents = { ...(this.droveFixKnownAgents ?? {}), [machineKey]: list };
        }
      } catch {}
    }

    if (want) {
      const opts = targetMachine === null ? this.aiUpdateAgentOptions() : this.droveFixAgentOptions();
      if (Array.isArray(knownList)) {
        const matched = knownList.find(
          (p) => this.canonicalFixAgentId(p?.key) === want && Array.isArray(p?.run) && p.run.length > 0,
        );
        if (matched?.installed) {
          const argv = this.normalizeFixAgentArgv(want, matched.run);
          if (argv) return { id: want, name: matched.name || want, argv };
        }
        const opt = opts.find((o) => o.id === want);
        if (opt?.argv) {
          return { id: want, name: matched?.name || opt.name || want, argv: opt.argv };
        }
      }
      const opt = opts.find((o) => o.id === want);
      if (opt?.argv) return { id: want, name: opt.name || want, argv: opt.argv };
      const fallbackArgv = this.fixAgentFallbackArgv(want);
      if (fallbackArgv) return { id: want, name: want, argv: fallbackArgv };
      return null;
    }

    const readyKeys = new Set(
      (readyAgents ?? []).map((a) => this.canonicalFixAgentId(a?.id)).filter(Boolean),
    );

    if (Array.isArray(knownList)) {
      const installed = knownList.filter((p) => p?.installed && Array.isArray(p.run) && p.run.length > 0);
      const rank = (p) => {
        const i = prefOrder.indexOf(this.canonicalFixAgentId(p.key));
        return i === -1 ? 99 : i;
      };
      const sorted = [...installed].sort((a, b) => rank(a) - rank(b));
      const picked = sorted.find((p) => readyKeys.has(this.canonicalFixAgentId(p.key))) ?? sorted[0];
      if (picked) {
        const id = this.canonicalFixAgentId(picked.key);
        const argv = this.normalizeFixAgentArgv(id, picked.run) ?? picked.run;
        return { id, name: picked.name || id || picked.key, argv };
      }
    }

    const candidates = [
      ...(readyAgents ?? []).map((a) => this.canonicalFixAgentId(a?.id)),
      ...this.droveProbeSummary().installed.map((a) => this.canonicalFixAgentId(a?.id)),
    ].filter(Boolean);
    for (const id of prefOrder) {
      const argv = this.fixAgentFallbackArgv(id);
      if (candidates.includes(id) && argv) {
        return { id, name: id, argv };
      }
    }
    for (const id of candidates) {
      const argv = this.fixAgentFallbackArgv(id);
      if (argv) {
        return { id, name: id, argv };
      }
    }
    return null;
  }

  agentIssues(rows = null) {
    const list = Array.isArray(rows) ? rows : this.detectedAgentRows();
    const out = [];
    const seen = new Set();
    for (const r of list) {
      if (r.acpError || r.adapterError) {
        const combined = [r.acpError, r.adapterError && r.adapterError !== r.acpError ? r.adapterError : null]
          .filter(Boolean)
          .join("\n");
        out.push({
          id: r.id,
          name: r.name,
          command: Array.isArray(r.command) && r.command.length > 0 ? r.command : (r.acpCommand ? r.acpCommand.split(" ") : [r.id]),
          path: r.path ?? r.adapter?.path ?? null,
          error: combined,
          issueKind: r.adapterError && !r.acpError ? "adapter" : r.acpAuthRequired ? "auth" : "probe",
        });
        seen.add(r.id);
      }
    }
    const topErr = String(this.adapterError || this.droveError || "").trim();
    if (topErr && !out.some((it) => String(it.error ?? "").includes(topErr))) {
      out.push({
        id: "environment",
        name: this.tr("Environment / ACP", "环境与 ACP 检测"),
        command: ["h2", "drove", "probe", "--refresh"],
        path: null,
        error: topErr,
        issueKind: "env",
      });
    }
    return out;
  }

  formatProbeTimestamp(iso) {
    const raw = String(iso ?? "").trim();
    if (!raw) return null;
    const d = new Date(raw);
    if (Number.isNaN(d.getTime())) return null;
    const pad = (n) => String(n).padStart(2, "0");
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }

  launchAgentToFixDroveProbeErrors(cx, override = null) {
    if (this.droveProbeAgentLaunching) return;
    const summary = this.droveProbeSummary();
    const allIssues = this.agentIssues();
    const focusId = this.droveProbeFixFocusId ? this.canonicalFixAgentId(this.droveProbeFixFocusId) : null;
    const targetIssues = focusId
      ? allIssues.filter((it) => this.canonicalFixAgentId(it.id) === focusId)
      : allIssues;
    const effectiveIssues = targetIssues.length > 0 ? targetIssues : summary.errored;
    if (effectiveIssues.length === 0) return;
    const targetMachine = this.droveMachine ?? null;
    const selectedAgent = override?.agent !== undefined ? override.agent : this.droveProbeFixAgent;
    const selectedModel = override?.model !== undefined ? override.model : this.droveProbeFixModel;
    const selectedEffort = override?.effort !== undefined ? override.effort : this.droveProbeFixEffort;
    this.droveProbeAgentLaunching = true;
    cx.notify();
    cx.spawn(async (cx) => {
      try {
        const picked = await this.resolveFixAgentLaunch(targetMachine, summary.ready, selectedAgent);
        if (!picked) {
          notify({
            title: this.tr("No available CLI agent found", "未找到可启动的 CLI Agent"),
            body: this.tr(
              "Install or configure at least one agent (e.g. claude, agy, codex) on this machine first.",
              "请先在该主机安装至少一个可用的 CLI Agent（如 claude、agy、codex）。",
            ),
            level: "error",
          });
          return;
        }
        const pickedId = this.canonicalFixAgentId(picked.id || selectedAgent || picked.name);
        let cleanModel = pickedId === "zcode" ? null : (String(selectedModel ?? "").trim() || null);
        const effortDisabled = Boolean(this.droveFixEffortDisabledReason(pickedId, cleanModel));
        let cleanEffort = !effortDisabled && selectedEffort
          ? this.clampEffortFor(pickedId, cleanModel, selectedEffort)
          : null;
        if (pickedId === "cursor") {
          const resolvedCursor = this.resolveCursorCliModelForLaunch(cleanModel, cleanEffort, this.probedAgents());
          cleanModel = resolvedCursor.model;
          cleanEffort = resolvedCursor.effort;
        }
        const prompt = this.buildDroveProbeFixPrompt(effectiveIssues);
        const tabName = this.tr("Fix Agent / ACP Issues", "AI 排查智能体异常");
        const targetSpace = this.targetHomeSpaceForMachine(targetMachine);
        const baseLaunchArgs = {
          folder: this.defaultHomeFolderForMachine(targetMachine),
          argv: picked.argv,
          task: prompt,
        };
        if (cleanModel) baseLaunchArgs.model = cleanModel;
        if (cleanEffort) baseLaunchArgs.effort = cleanEffort;
        if (targetSpace) {
          run("tab.new", { space: targetSpace.id, name: tabName, ...baseLaunchArgs });
        } else {
          const args = { ...baseLaunchArgs, tab: tabName };
          if (targetMachine) args.machine = targetMachine;
          run("space.new", args);
        }
        this.droveProbeFixOpen = false;
        this.droveProbeFixFocusId = null;
        this.close();
        const metaParts = [cleanModel, cleanEffort ? this.effortLabel(cleanEffort) : null].filter(Boolean);
        const metaEn = metaParts.length > 0 ? ` (${metaParts.join(" · ")})` : "";
        const metaZh = metaParts.length > 0 ? `（${metaParts.join(" · ")}）` : "";
        notify({
          title: this.tr(`Launched ${picked.name}${metaEn} to troubleshoot`, `已启动 ${picked.name}${metaZh} 协助排查`),
          body: this.tr(
            "After fixing, return to Settings → Agents & ACP and click Detect Environment.",
            "处理完成后，返回「设置 → 智能体与 ACP」点击「一键检测环境」即可验证。",
          ),
        });
      } catch (e) {
        notify({
          title: this.tr("Failed to launch AI agent", "启动 Agent 协助失败"),
          body: String(e?.message ?? e),
          level: "error",
        });
      } finally {
        this.droveProbeAgentLaunching = false;
        cx.notify();
      }
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
        const model = (this.aiStatus?.model ?? this.droveData?.config?.ai?.model ?? "").trim();
        const payload = { mode: effMode, apply: true, language: this.lang() };
        if (instruction) payload.instruction = instruction;
        if (baseUrl) payload.baseUrl = baseUrl;
        if (apiKey) payload.apiKey = apiKey;
        if (model) payload.model = model;
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload, { timeout: 180000 });
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
        const model = (this.aiStatus?.model ?? this.droveData?.config?.ai?.model ?? "").trim();
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
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload, { timeout: 180000 });
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
        const model = (this.aiStatus?.model ?? this.droveData?.config?.ai?.model ?? "").trim();
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
        const res = await this.droveCallWithJsonFile(["drove", "roles-ai-config"], payload, { timeout: 180000 });
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

  agentProbeError(agent) {
    const raw = String(agent?.error ?? "").trim();
    return raw || null;
  }

  isAcpAuthError(raw) {
    const lower = String(raw ?? "").trim().toLowerCase();
    if (!lower) return false;
    return (
      lower.includes("authentication required") ||
      lower.includes("auth_required") ||
      lower.includes("configure an api key") ||
      lower.includes("cursor_login") ||
      lower.includes("agent login") ||
      lower.includes("cursor-agent login") ||
      lower.includes("onboarding is incomplete") ||
      lower.includes("please run /login") ||
      lower.includes("not logged in") ||
      lower.includes("login required") ||
      lower.includes("please log in") ||
      lower.includes("please login") ||
      lower.includes("sign in required") ||
      lower.includes("session expired") ||
      lower.includes("unauthenticated") ||
      lower.includes("unauthorized") ||
      (lower.includes("-32000") && (lower.includes("auth") || lower.includes("login") || lower.includes("api key")))
    );
  }

  stripAcpAuthRawNoise(raw) {
    let text = String(raw ?? "").trim();
    if (!text) return "";
    if (!this.isAcpAuthError(text)) return text;
    const stderrSplit = text.split(/\r?\nstderr:\r?\n/i);
    if (stderrSplit.length > 1 && /^\s*Error handling request\b/i.test(stderrSplit[1])) {
      text = stderrSplit[0].trim();
    }
    const authMethodsIdx = text.indexOf(': {"authMethods":');
    if (authMethodsIdx >= 0) {
      const prefix = text.slice(0, authMethodsIdx).trim();
      const tail = text.slice(authMethodsIdx);
      const codeMatch = tail.match(/\(code\s+-?\d+\)\s*$/);
      text = codeMatch ? `${prefix} ${codeMatch[0]}` : prefix;
    }
    return text;
  }

  formatAgentProbeError(raw, agentId = null) {
    const cleaned = this.stripAcpAuthRawNoise(raw);
    if (!cleaned) return "";
    if (this.isAcpAuthError(cleaned) && agentId) {
      const spec = this.agentLoginSpec(agentId);
      if (spec) {
        const hint = this.tr(`Login / API key required · ${spec.hintEn}`, `需登录或配置 API Key · ${spec.hintZh}`);
        return hint.length > 160 ? `${hint.slice(0, 159).trimEnd()}…` : hint;
      }
    }
    const lines = cleaned
      .split(/\r?\n/)
      .map((l) => l.trim())
      .filter(Boolean);
    if (lines.length === 0) return "";
    const headline = lines[0];
    const stderrIdx = lines.findIndex((l) => l.toLowerCase() === "stderr:");
    const stderrLine = stderrIdx >= 0 ? lines.slice(stderrIdx + 1).find(Boolean) : null;
    let combined = stderrLine && !headline.includes(stderrLine) ? `${headline} · ${stderrLine}` : headline;
    if (combined.length > 160) combined = `${combined.slice(0, 159).trimEnd()}…`;
    return combined;
  }

  droveProbeSummary() {
    const agents = this.probedAgents();
    const installed = agents.filter((a) => Boolean(a.installed));
    const errored = installed.filter((a) => this.agentProbeError(a) !== null);
    const ready = installed.filter((a) => this.agentProbeError(a) === null);
    const missing = agents.filter((a) => !a.installed);
    return { total: agents.length, installed, ready, errored, missing };
  }

  probedModelsFromList(agentsList, agentId) {
    const canon = this.canonicalFixAgentId(agentId);
    const a = (agentsList ?? []).find((x) => x?.id === agentId || this.canonicalFixAgentId(x?.id) === canon);
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

  probedModelsFor(agentId) {
    return this.probedModelsFromList(this.probedAgents(), agentId);
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

  aiProtocolLabel(proto) {
    const clean = String(proto ?? "").trim().toLowerCase();
    if (clean === "anthropic" || clean === "claude") return "Claude";
    if (clean === "openai") return "OpenAI";
    return null;
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
      .border_color(open ? t.accent : (o.borderColor ?? t.border))
      .bg(t.raised)
      .cursor_pointer()
      .items_center()
      .justify_between()
      .gap(6)
      .when(Boolean(o.tooltip), (el) => el.tooltip(o.tooltip))
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
          .when(o.dot !== undefined || o.dotColor !== undefined, (el) =>
            el.child(div().w(5).h(5).rounded(3).flex_none().bg(o.dotColor ?? (o.dot ? t.ansi2 : t.dim))),
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
                      .when(Boolean(it.tooltip), (row) => row.tooltip(it.tooltip))
                      .when(Boolean(it.icon), (row) => row.child(this.agentIconEl(t, it.icon, 13)))
                      .when(it.dot !== undefined || it.dotColor !== undefined, (row) =>
                        row.child(div().w(6).h(6).rounded(3).flex_none().bg(it.dotColor ?? (it.dot ? t.ansi2 : t.dim))),
                      )
                      .child(div().flex_1().min_w(0).truncate().text_size(12.5).text_color(it.active ? t.foreground : t.muted).child(it.label))
                      .when(Boolean(it.meta), (row) => row.child(div().flex_none().text_size(11).text_color(it.metaColor ?? t.dim).child(it.meta))),
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

  supportedEffortsFor(agentId, modelId, probedList = null) {
    const three = ["low", "medium", "high"];
    const four = ["low", "medium", "high", "xhigh"];
    const agents = Array.isArray(probedList) ? probedList : this.probedAgents();
    const canon = this.canonicalFixAgentId(agentId);
    const probed = agents.find((a) => a?.id === agentId || this.canonicalFixAgentId(a?.id) === canon);
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

  clampEffortFor(agentId, modelId, effort, probedList = null) {
    const allowed = this.supportedEffortsFor(agentId, modelId, probedList);
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
      const curAgentErr = this.agentProbeError(curAgentInfo);
      const agentItems = probed.map((a) => {
        const err = this.agentProbeError(a);
        return {
          value: a.id,
          label: a.name && a.name !== a.id ? `${a.id} · ${a.name}` : a.id,
          icon: a.id,
          dot: Boolean(a.installed),
          dotColor: !a.installed ? t.dim : err ? t.blocked : t.ansi2,
          meta: !a.installed
            ? this.tr("missing", "未安装")
            : err
              ? this.tr("error", "异常")
              : a.models?.length
                ? `${a.models.length} ${this.tr("models", "模型")}`
                : "✓",
          metaColor: err ? t.blocked : t.dim,
          tooltip: err ? this.formatAgentProbeError(err) : undefined,
          active: target.agent === a.id,
        };
      });
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
            {
              width: 118,
              popupWidth: 280,
              icon: target.agent,
              dot: curAgentInfo ? Boolean(curAgentInfo.installed) : undefined,
              dotColor: !curAgentInfo ? undefined : !curAgentInfo.installed ? t.dim : curAgentErr ? t.blocked : t.ansi2,
              borderColor: curAgentErr ? t.blocked : undefined,
              tooltip: curAgentErr ? this.formatAgentProbeError(curAgentErr) : undefined,
            },
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
    const detectedProtocol = this.aiProtocolLabel(
      (this.aiTestResult?.ok ? this.aiTestResult.protocol : null) ?? st.protocol,
    );
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
    const remoteCount = machines.length;
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
      // 0. Global Shared Status Bar
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
              .gap(8)
              .items_center()
              .flex_wrap()
              .child(div().w(7).h(7).flex_none().rounded(4).bg(st.configured ? t.ansi2 : t.dim))
              .child(
                div()
                  .text_size(12.5)
                  .font_medium()
                  .text_color(t.foreground)
                  .child(st.configured ? this.tr("Connected", "已配置") : this.tr("Not configured", "未配置")),
              )
              .child(
                div()
                  .text_size(12)
                  .text_color(t.dim)
                  .child(
                    remoteCount > 0
                      ? this.tr(
                          `Shared across this computer and ${remoteCount} remote machine(s) — synced automatically`,
                          `所有主机共用一套配置（自动同步至本机及 ${remoteCount} 台远程主机）`,
                        )
                      : this.tr(
                          "Shared across all machines — synced automatically",
                          "所有主机共用一套配置，修改后自动同步",
                        ),
                  ),
              ),
          )
          .child(
            this.button(t, "st-ai-refresh", this.aiLoading ? this.tr("Refreshing…", "刷新中…") : this.tr("Refresh", "刷新"), (_e, cx) => {
              cx.spawn(async (cx) => { await this.loadAi(); cx.notify(); });
            }),
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
                  this.tr("Shared lightweight AI runtime (auto-detects OpenAI & Claude protocols) used by PTY status arbitration, parent note digests, and Drove orchestration.", "全局共享的轻量 AI 运行时（自动识别 OpenAI 与 Claude 协议），同时服务于终端状态仲裁、子 Agent 摘要与 Drove 编排。"),
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
                      .child(
                        h_flex()
                          .justify_between()
                          .items_center()
                          .gap(6)
                          .child(div().text_size(12).text_color(t.foreground).child(this.tr("Service URL", "远端服务地址")))
                          .child(
                            div()
                              .id("st-ai-protocol-badge")
                              .truncate()
                              .text_size(11)
                              .text_color(detectedProtocol ? t.accent : t.dim)
                              .child(
                                detectedProtocol
                                  ? this.tr(`Auto: ${detectedProtocol}`, `自动识别：${detectedProtocol}`)
                                  : this.tr("Auto: OpenAI / Claude", "自动识别 OpenAI / Claude"),
                              ),
                          ),
                      )
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
                    const protoTag = this.aiProtocolLabel(trRes.protocol);
                    return el.child(
                      div()
                        .text_size(12)
                        .text_color(trRes.ok ? t.ansi2 : t.blocked)
                        .child(
                          trRes.ok
                            ? `✓ OK (${trRes.latencyMs ?? 0}ms)${protoTag ? ` · ${protoTag}` : ""} · ${trRes.discoveredModel ?? ""}${n > 0 ? ` · ${n} ${this.tr("models", "个模型")}` : ""}`
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

  detectedAgentRows() {
    const machineKey = this.droveMachine ?? "__local__";
    const knownList = Array.isArray(this.droveFixKnownAgents?.[machineKey])
      ? this.droveFixKnownAgents[machineKey]
      : [];
    const probedList = Array.isArray(this.droveData?.agents?.agents)
      ? this.droveData.agents.agents
      : [];
    const adapterList = Array.isArray(this.adapterStatus?.adapters)
      ? this.adapterStatus.adapters
      : [];

    const catalogIndex = new Map();
    const byId = new Map();
    SUPPORTED_AGENTS_CATALOG.forEach((spec, idx) => {
      catalogIndex.set(spec.id, idx);
      byId.set(spec.id, {
        id: spec.id,
        name: spec.name,
        cli: spec.cli ?? null,
        standaloneAcp: Boolean(spec.standaloneAcp),
        acpMode: spec.acpMode ?? "none",
        acpCommand: spec.acpCommand ?? null,
        install: spec.install ?? null,
        cliInstalled: false,
        cliRun: null,
        acpSupported: spec.acpMode !== "none",
        acpInstalled: false,
        acpError: null,
        models: [],
        command: null,
        path: null,
        adapter: null,
      });
    });

    const ensure = (rawId) => {
      const id = this.canonicalFixAgentId(rawId);
      if (!id) return null;
      let rec = byId.get(id);
      if (!rec) {
        rec = {
          id,
          name: id,
          cli: id,
          standaloneAcp: false,
          acpMode: "none",
          acpCommand: null,
          install: null,
          cliInstalled: false,
          cliRun: null,
          acpSupported: false,
          acpInstalled: false,
          acpError: null,
          models: [],
          command: null,
          path: null,
          adapter: null,
        };
        byId.set(id, rec);
      }
      return rec;
    };

    for (const p of knownList) {
      const rec = ensure(p?.key);
      if (!rec) continue;
      if (p?.name) rec.name = p.name;
      if (p?.installed) rec.cliInstalled = true;
      if (Array.isArray(p?.run) && p.run.length > 0) {
        rec.cliRun = p.run;
        if (!rec.cli) rec.cli = p.run[0];
      }
    }

    for (const a of probedList) {
      const rec = ensure(a?.id);
      if (!rec) continue;
      rec.acpSupported = true;
      if (rec.acpMode === "none") rec.acpMode = "separate";
      if (a?.installed) rec.acpInstalled = true;
      const err = this.agentProbeError(a);
      if (err) rec.acpError = err;
      const models = this.probedModelsFromList(probedList, a.id);
      if (models.length > rec.models.length) rec.models = models;
      if (Array.isArray(a?.command) && a.command.length > 0) rec.command = a.command;
      if (a?.path) rec.path = a.path;
    }

    for (const ad of adapterList) {
      const rec = ensure(ad?.preset_id);
      if (!rec) continue;
      rec.acpSupported = true;
      rec.acpMode = "managed";
      rec.adapter = ad;
      if (ad?.binary_name) rec.acpCommand = ad.binary_name;
      if (ad?.companion_cli && !rec.cli) rec.cli = ad.companion_cli;
      if (!ad?.companion_cli) rec.standaloneAcp = true;
      if (ad?.companion_installed) rec.cliInstalled = true;
      // Managed adapter status is authoritative for whether the adapter itself is installed
      // (avoid treating an `npx_fallback` probe as an installed ACP adapter).
      rec.acpInstalled = Boolean(ad?.installed);
      if (ad?.path) rec.path = ad.path;
    }

    const rank = (id) => (catalogIndex.has(id) ? catalogIndex.get(id) : 999);
    const group = (r) => {
      if (r.acpReady && r.anyInstalled) return 0;
      if (r.hasError) return 1;
      if (r.adapterMissing) return 2;
      if (r.anyInstalled) return 3;
      if (r.acpSupported) return 4;
      return 5;
    };

    return [...byId.values()]
      .map((r) => {
        const adapterFeedback = this.adapterActionFeedback?.[r.id] ?? null;
        const normVer = (v) => String(v ?? "").trim().replace(/^v/i, "").toLowerCase();
        const adapterHasUpdate = Boolean(
          r.adapter?.source === "managed" &&
            r.adapter?.installed &&
            (r.adapter?.update_available === true ||
              (r.adapter?.version &&
                r.adapter?.latest_version &&
                normVer(r.adapter.version) !== normVer(r.adapter.latest_version))),
        );
        const adapterUpToDate = Boolean(
          r.adapter?.source === "managed" &&
            r.adapter?.installed &&
            r.adapter?.version &&
            r.adapter?.latest_version &&
            !adapterHasUpdate,
        );
        const adapterUpdating = Boolean(
          this.adapterBusy === `update:${r.id}` ||
            (this.adapterBusy === "update" && r.adapter?.source === "managed" && adapterHasUpdate),
        );
        const adapterInstalling = Boolean(
          r.adapter?.state === "installing" ||
            this.adapterBusy === `install:${r.id}` ||
            this.adapterBusy === `reinstall:${r.id}` ||
            adapterUpdating,
        );
        const adapterError = String(r.adapter?.error ?? "").trim() || null;
        // Built-in ACP comes bundled with the CLI binary itself.
        const effectiveAcpInstalled = r.acpMode === "builtin"
          ? Boolean(r.acpInstalled || r.cliInstalled)
          : Boolean(r.acpInstalled);
        const adapterMissing = Boolean(
          r.acpMode === "managed" &&
            (r.cliInstalled || r.adapter?.companion_installed) &&
            !effectiveAcpInstalled &&
            !adapterInstalling,
        );
        const adapterNotInstalled = Boolean(
          r.acpMode === "managed" && !effectiveAcpInstalled && !adapterInstalling,
        );
        const acpAuthRequired = Boolean(r.acpError && this.isAcpAuthError(r.acpError));
        const acpReady = Boolean(effectiveAcpInstalled && !r.acpError && !adapterError);
        const hasError = Boolean(r.acpError || adapterError);
        const needsAction = Boolean(hasError || adapterMissing);
        const anyInstalled = Boolean(r.cliInstalled || effectiveAcpInstalled || adapterInstalling);
        return {
          ...r,
          acpInstalled: effectiveAcpInstalled,
          acpAuthRequired,
          adapterInstalling,
          adapterUpdating,
          adapterFeedback,
          adapterHasUpdate,
          adapterUpToDate,
          adapterError,
          adapterMissing,
          adapterNotInstalled,
          acpReady,
          hasError,
          needsAction,
          anyInstalled,
        };
      })
      .sort((a, b) => {
        const ga = group(a);
        const gb = group(b);
        if (ga !== gb) return ga - gb;
        const ra = rank(a.id);
        const rb = rank(b.id);
        if (ra !== rb) return ra - rb;
        return a.id.localeCompare(b.id);
      });
  }

  renderProbeErrorCard(t, cx, errorAgents) {
    const focusId = this.droveProbeFixFocusId ? this.canonicalFixAgentId(this.droveProbeFixFocusId) : null;
    const focusedTarget = focusId ? errorAgents.find((a) => this.canonicalFixAgentId(a.id) === focusId) : null;
    return v_flex()
      .id("st-dr-agent-errors")
      .w_full()
      .px(12)
      .py(10)
      .gap(8)
      .rounded(8)
      .border_1()
      .border_color(t.blocked)
      .bg(t.surface)
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap(8)
          .flex_wrap()
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .flex_wrap()
              .child(
                div()
                  .text_size(12)
                  .font_semibold()
                  .text_color(t.blocked)
                  .child(
                    this.tr(
                      `Detected ${errorAgents.length} Agent / ACP issue${errorAgents.length > 1 ? "s" : ""}`,
                      `检测到 ${errorAgents.length} 项智能体 / ACP 异常`,
                    ),
                  ),
              )
              .when(Boolean(focusedTarget), (el) =>
                el.child(
                  h_flex()
                    .gap(5)
                    .items_center()
                    .px(6)
                    .py(1.5)
                    .rounded(4)
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border)
                    .child(
                      div()
                        .text_size(10.8)
                        .text_color(t.muted)
                        .child(this.tr(`Focus: ${focusedTarget.name || focusedTarget.id}`, `聚焦排查：${focusedTarget.name || focusedTarget.id}`)),
                    )
                    .child(
                      div()
                        .id("st-dr-probe-fix-clear-focus")
                        .cursor_pointer()
                        .text_size(10.8)
                        .text_color(t.accent)
                        .on_click((_e, c) => {
                          this.droveProbeFixFocusId = null;
                          c.notify();
                        })
                        .child(this.tr("All", "全部")),
                    ),
                ),
              ),
          )
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .flex_wrap()
              .child(
                (() => {
                  const fixOpen = Boolean(this.droveProbeFixOpen);
                  return h_flex()
                    .id("st-dr-probe-agent-fix-btn")
                    .px(9)
                    .py(4)
                    .gap(5)
                    .rounded(6)
                    .border_1()
                    .border_color(fixOpen ? t.accent : t.border)
                    .bg(fixOpen ? t.accent : t.raised)
                    .items_center()
                    .text_size(11.5)
                    .when(fixOpen, (b) => b.font_semibold())
                    .text_color(fixOpen ? t.accent_foreground : t.foreground)
                    .when(this.droveProbeAgentLaunching, (b) => b.opacity(0.6))
                    .when(!this.droveProbeAgentLaunching, (b) =>
                      b
                        .cursor_pointer()
                        .when(!fixOpen, (b2) => b2.hover((s) => s.bg(t.hover)))
                        .when(fixOpen, (b2) => b2.hover((s) => s.opacity(0.92)))
                        .on_click((_e, cx) => this.toggleDroveProbeFixPanel(cx)),
                    )
                    .tooltip(
                      this.tr(
                        "Choose an AI coding agent, model, and reasoning effort to explain error causes, propose a fix plan, and repair after your confirmation",
                        "选择要启动的 AI Agent、模型与思考强度，在工作区先解读错误原因并制定修复方案，待你确认后再开始修复",
                      ),
                    )
                    .when(this.droveProbeAgentLaunching, (b) =>
                      b.child(
                        svg("mods/workspace-chrome/spinner.svg")
                          .size(11)
                          .text_color(fixOpen ? t.accent_foreground : t.foreground)
                          .spin(900),
                      ),
                    )
                    .when(!this.droveProbeAgentLaunching, (b) =>
                      b.child(
                        svg(I("sparkles"))
                          .size(11.5)
                          .text_color(fixOpen ? t.accent_foreground : t.accent),
                      ),
                    )
                    .child(
                      this.droveProbeAgentLaunching
                        ? this.tr("Launching…", "启动中…")
                        : this.tr("Fix with AI", "AI 处理异常"),
                    );
                })(),
              ),
          ),
      )
      .children(
        errorAgents.map((a) => {
          const rawErr = this.agentProbeError(a) ?? "";
          const isAuth = a.issueKind === "auth" || this.isAcpAuthError(rawErr);
          const shortErr = this.formatAgentProbeError(rawErr, a.id);
          const kindLabel = a.issueKind === "adapter"
            ? this.tr("Adapter", "适配器")
            : a.issueKind === "env"
              ? this.tr("Env", "环境")
              : isAuth
                ? this.tr("Auth", "需登录")
                : "ACP";
          const badgeColor = isAuth ? t.working : t.blocked;
          return h_flex()
            .id(`st-dr-agent-err-${a.id}`)
            .w_full()
            .gap(8)
            .items_center()
            .tooltip(this.stripAcpAuthRawNoise(rawErr))
            .child(
              h_flex()
                .w(122)
                .flex_none()
                .gap(6)
                .items_center()
                .child(this.agentIconEl(t, a.id, 13))
                .child(div().w(6).h(6).rounded(3).flex_none().bg(badgeColor))
                .child(div().truncate().text_size(12).font_semibold().text_color(t.foreground).child(a.name || a.id)),
            )
            .child(
              div()
                .flex_none()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(t.background)
                .border_1()
                .border_color(alpha(badgeColor, "44") ?? t.border)
                .text_size(10)
                .text_color(badgeColor)
                .child(kindLabel),
            )
            .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.muted).child(shortErr));
        }),
      )
      .when(Boolean(this.droveProbeFixOpen), (box) => {
        const sel = this.ensureDroveFixSelection();
        const curAgent = sel.agent ?? "";
        const curOpt = sel.option;
        const curModel = sel.model;
        const curEffort = sel.effort;
        const effortDisabledReason = sel.disabledReason;
        const disableEffort = Boolean(effortDisabledReason);
        const isZcode = curAgent === "zcode";
        const agentItems = sel.options.map((o) => ({
          value: o.id,
          label: o.name && o.name !== o.id ? `${o.name} (${o.id})` : o.id,
          icon: o.id,
          dot: Boolean(o.installed),
          dotColor: !o.installed ? t.dim : o.probeError ? t.working : t.ansi2,
          meta: !o.installed
            ? this.tr("missing", "未安装")
            : o.probeError
              ? this.tr("ACP err · CLI", "ACP 异常 · CLI")
              : o.models.length > 0
                ? `${o.models.length} ${this.tr("models", "模型")}`
                : "✓",
          metaColor: o.probeError ? t.working : t.dim,
          tooltip: o.probeError ? this.formatAgentProbeError(o.probeError) : undefined,
          active: curAgent === o.id,
        }));
        const models = curOpt?.models?.length ? curOpt.models : (curAgent ? this.probedModelsFor(curAgent) : []);
        const modelItems = [
          { value: null, label: this.tr("Auto (agent default)", "自动（Agent 默认）"), active: !curModel },
          ...models.map((m) => ({
            value: m.id,
            label: m.label,
            active: curModel === m.id,
          })),
        ];
        if (curModel && !models.some((m) => m.id === curModel)) {
          modelItems.splice(1, 0, {
            value: curModel,
            label: curModel,
            meta: this.tr("custom", "自定义"),
            active: true,
          });
        }
        const efforts = curAgent ? this.supportedEffortsFor(curAgent, curModel) : ["low", "medium", "high"];
        const effortChoices = [null, ...efforts];
        const canLaunch = Boolean(curOpt && !this.droveProbeAgentLaunching);
        const footerHint = isZcode
          ? this.tr(
              "Note: ZCode ignores startup --model/--effort flags; select model inside its TUI.",
              "提示：ZCode 不支持启动参数传入模型与思考强度，进入后需在 TUI 内切换。",
            )
          : curAgent === "cursor" && !curModel
            ? this.tr(
                "Tip: Select a specific Cursor model to apply custom reasoning effort.",
                "提示：Cursor 需先指定具体模型才能生效自定义思考强度。",
              )
            : curOpt?.probeError
              ? this.tr(
                  "ACP probe errored for this agent, but its interactive CLI can still run in your terminal.",
                  "该 Agent 的 ACP 探测有异常，但在交互式终端中仍可直接启动排查。",
                )
              : this.tr(
                  "Will launch an interactive agent to explain error causes and propose a fix plan first, and wait for your confirmation before making changes.",
                  "将在目标主机工作区标签页启动交互式 Agent，先解读错误原因并制定修复方案，待你确认后再开始修复。",
                );
        return box.child(
          v_flex()
            .id("st-dr-probe-fix-panel")
            .w_full()
            .mt(2)
            .px(10)
            .py(9)
            .gap(8)
            .rounded(7)
            .border_1()
            .border_color(t.accent)
            .bg(t.background)
            .child(
              h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .gap(8)
                .child(
                  div()
                    .text_size(11.8)
                    .font_semibold()
                    .text_color(t.foreground)
                    .child(
                      this.tr(
                        "Fix with Agent · Choose Agent, Model & Reasoning Effort",
                        "用 Agent 排查处理 · 选择 Agent 类型、模型与思考强度",
                      ),
                    ),
                )
                .child(
                  div()
                    .id("st-dr-probe-fix-close")
                    .px(6)
                    .py(1)
                    .rounded(4)
                    .cursor_pointer()
                    .text_size(12)
                    .text_color(t.dim)
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => this.toggleDroveProbeFixPanel(cx))
                    .child("×"),
                ),
            )
            .child(
              h_flex()
                .w_full()
                .gap(6)
                .items_center()
                .flex_wrap()
                .child(
                  this.drovePicker(
                    t,
                    "probe-fix-agent",
                    curOpt ? (curOpt.name || curOpt.id) : this.tr("Select agent…", "选择 Agent…"),
                    agentItems,
                    (agent, cx) => this.setDroveProbeFixAgent(agent, cx),
                    {
                      width: 148,
                      popupWidth: 290,
                      icon: curAgent || undefined,
                      dot: curOpt ? Boolean(curOpt.installed) : undefined,
                      dotColor: !curOpt ? undefined : !curOpt.installed ? t.dim : curOpt.probeError ? t.working : t.ansi2,
                      tooltip: this.tr("Choose which CLI agent to launch", "选择用于排查分析与修复的 Agent 类型"),
                    },
                    cx,
                  ),
                )
                .child(
                  this.drovePicker(
                    t,
                    "probe-fix-model",
                    curModel || this.tr("Auto (default)", "自动（默认）"),
                    modelItems,
                    (model, cx) => this.setDroveProbeFixModel(model, cx),
                    {
                      flex1: true,
                      width: 150,
                      popupWidth: 360,
                      allowCustom: true,
                      tooltip: isZcode
                        ? effortDisabledReason
                        : this.tr("Choose model or type a custom model ID", "选择模型或输入自定义模型 ID"),
                    },
                    cx,
                  ),
                )
                .child(
                  h_flex()
                    .id("st-dr-probe-fix-eff-group")
                    .w(effortChoices.length >= 5 ? 196 : 164)
                    .min_w(effortChoices.length >= 5 ? 196 : 164)
                    .h(28)
                    .flex_none()
                    .p(2)
                    .gap(1)
                    .items_center()
                    .rounded(7)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.surface)
                    .when(disableEffort, (el) => el.opacity(0.35).tooltip(effortDisabledReason))
                    .when(!disableEffort, (el) =>
                      el.tooltip(this.tr("Reasoning effort level", "选择模型思考强度")),
                    )
                    .children(
                      effortChoices.map((eff) => {
                        const effKey = eff ?? "auto";
                        const active = !disableEffort && (eff === null ? !curEffort : curEffort === eff);
                        const label = eff === null ? this.tr("Auto", "默认") : this.effortLabel(eff);
                        return div()
                          .id(`st-dr-probe-fix-eff-${effKey}`)
                          .flex_1()
                          .min_w(0)
                          .h(22)
                          .px(3)
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
                              .on_click((_e, cx) => this.setDroveProbeFixEffort(eff, cx)),
                          )
                          .child(label);
                      }),
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
                  div()
                    .flex_1()
                    .min_w(0)
                    .text_size(11)
                    .line_height("15px")
                    .text_color(t.dim)
                    .child(footerHint),
                )
                .child(
                  h_flex()
                    .gap(6)
                    .items_center()
                    .child(
                      this.button(t, "st-dr-probe-fix-cancel", this.tr("Cancel", "取消"), (_e, cx) =>
                        this.toggleDroveProbeFixPanel(cx),
                      ),
                    )
                    .child(
                      h_flex()
                        .id("st-dr-probe-fix-launch")
                        .px(11)
                        .py(4)
                        .gap(5)
                        .rounded(6)
                        .border_1()
                        .border_color(t.accent)
                        .bg(t.accent)
                        .items_center()
                        .text_size(11.5)
                        .font_semibold()
                        .text_color(t.accent_foreground)
                        .when(!canLaunch, (b) => b.opacity(0.55))
                        .when(canLaunch, (b) =>
                          b
                            .cursor_pointer()
                            .hover((s) => s.opacity(0.9))
                            .on_click((_e, cx) => this.launchAgentToFixDroveProbeErrors(cx)),
                        )
                        .when(this.droveProbeAgentLaunching, (b) =>
                          b.child(
                            svg("mods/workspace-chrome/spinner.svg")
                              .size(11)
                              .text_color(t.accent_foreground)
                              .spin(900),
                          ),
                        )
                        .child(
                          this.droveProbeAgentLaunching
                            ? this.tr("Launching…", "启动中…")
                            : this.tr("Start Agent", "启动 Agent 处理"),
                        ),
                    ),
                ),
            ),
        );
      });
  }

  renderAgentAiInstallPanel(t, cx, row) {
    const sel = this.ensureAgentAiInstallSelection(row.id);
    const curAgent = sel.agent ?? "";
    const curOpt = sel.option;
    const curModel = sel.model;
    const curEffort = sel.effort;
    const effortDisabledReason = sel.disabledReason;
    const disableEffort = Boolean(effortDisabledReason);
    const isZcode = curAgent === "zcode";
    const needSeparateAcp = Boolean(row.cliInstalled && row.acpMode === "separate" && !row.acpInstalled);
    const targetName = needSeparateAcp ? `${row.name} ACP` : row.name;
    const agentItems = sel.options.map((o) => ({
      value: o.id,
      label: o.name && o.name !== o.id ? `${o.name} (${o.id})` : o.id,
      icon: o.id,
      dot: Boolean(o.installed),
      dotColor: !o.installed ? t.dim : o.probeError ? t.working : t.ansi2,
      meta: !o.installed
        ? this.tr("missing", "未安装")
        : o.probeError
          ? this.tr("ACP err · CLI", "ACP 异常 · CLI")
          : o.models.length > 0
            ? `${o.models.length} ${this.tr("models", "模型")}`
            : "✓",
      metaColor: o.probeError ? t.working : t.dim,
      tooltip: o.probeError ? this.formatAgentProbeError(o.probeError) : undefined,
      active: curAgent === o.id,
    }));
    const models = curOpt?.models?.length ? curOpt.models : (curAgent ? this.probedModelsFor(curAgent) : []);
    const modelItems = [
      { value: null, label: this.tr("Auto (agent default)", "自动（Agent 默认）"), active: !curModel },
      ...models.map((m) => ({
        value: m.id,
        label: m.label,
        active: curModel === m.id,
      })),
    ];
    if (curModel && !models.some((m) => m.id === curModel)) {
      modelItems.splice(1, 0, {
        value: curModel,
        label: curModel,
        meta: this.tr("custom", "自定义"),
        active: true,
      });
    }
    const efforts = curAgent ? this.supportedEffortsFor(curAgent, curModel) : ["low", "medium", "high"];
    const effortChoices = [null, ...efforts];
    const canLaunch = Boolean(curOpt && !this.droveProbeAgentLaunching);
    const footerHint = isZcode
      ? this.tr(
          "Note: ZCode ignores startup --model/--effort flags; select model inside its TUI.",
          "提示：ZCode 不支持启动参数传入模型与思考强度，进入后需在 TUI 内切换。",
        )
      : curAgent === "cursor" && !curModel
        ? this.tr(
            "Tip: Select a specific Cursor model to apply custom reasoning effort.",
            "提示：Cursor 需先指定具体模型才能生效自定义思考强度。",
          )
        : curOpt && !curOpt.installed
          ? this.tr(
              "This agent was not detected on the selected machine. Pick an installed agent above or install a CLI agent first.",
              "目标主机未检测到该 Agent，请在上方选择已安装的 Agent，或先使用「一键安装」安装至少一个 CLI Agent。",
            )
          : curOpt?.probeError
            ? this.tr(
                "ACP probe errored for this agent, but its interactive CLI can still run in your terminal.",
                "该 Agent 的 ACP 探测有异常，但在交互式终端中仍可直接启动协助安装。",
              )
            : needSeparateAcp
              ? this.tr(
                  `Will launch an interactive agent on the selected machine to install and configure ${row.acpCommand || `${row.name} ACP`} after your confirmation.`,
                  `将在目标主机工作区标签页启动交互式 Agent，先检查环境并在你确认后安装配置 ${row.acpCommand || `${row.name} ACP`}。`,
                )
              : this.tr(
                  `Will launch an interactive agent on the selected machine to inspect the environment and install ${row.name} after your confirmation.`,
                  `将在目标主机工作区标签页启动交互式 Agent，先检查环境并给出安装方案，待你确认后安装 ${row.name}。`,
                );

    return v_flex()
      .id("st-ag-ai-install-panel")
      .w_full()
      .px(10)
      .py(9)
      .gap(8)
      .rounded(7)
      .border_1()
      .border_color(t.accent)
      .bg(t.background)
      .child(
        h_flex()
          .w_full()
          .items_center()
          .justify_between()
          .gap(8)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg(I("sparkles")).size(12.5).flex_none().text_color(t.accent))
              .child(
                div()
                  .text_size(11.8)
                  .font_semibold()
                  .text_color(t.foreground)
                  .child(
                    this.tr(
                      `AI Install ${targetName} · Choose Agent, Model & Reasoning Effort`,
                      `AI 安装 ${targetName} · 选择 Agent 类型、模型与思考强度`,
                    ),
                  ),
              ),
          )
          .child(
            div()
              .id("st-ag-ai-install-close")
              .px(6)
              .py(1)
              .rounded(4)
              .cursor_pointer()
              .text_size(12)
              .text_color(t.dim)
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, c) => this.closeAgentAiInstall(c))
              .child("×"),
          ),
      )
      .child(
        h_flex()
          .w_full()
          .gap(6)
          .items_center()
          .flex_wrap()
          .child(
            this.drovePicker(
              t,
              "agent-install-agent",
              curOpt ? (curOpt.name || curOpt.id) : this.tr("Select agent…", "选择 Agent…"),
              agentItems,
              (agent, c) => this.setAgentAiInstallAgent(agent, c),
              {
                width: 148,
                popupWidth: 290,
                icon: curAgent || undefined,
                dot: curOpt ? Boolean(curOpt.installed) : undefined,
                dotColor: !curOpt ? undefined : !curOpt.installed ? t.dim : curOpt.probeError ? t.working : t.ansi2,
                tooltip: this.tr("Choose which CLI agent to launch for AI Install", "选择用于执行 AI 安装的 Agent 类型"),
              },
              cx,
            ),
          )
          .child(
            this.drovePicker(
              t,
              "agent-install-model",
              curModel || this.tr("Auto (default)", "自动（默认）"),
              modelItems,
              (model, c) => this.setAgentAiInstallModel(model, c),
              {
                flex1: true,
                width: 150,
                popupWidth: 360,
                allowCustom: true,
                tooltip: isZcode
                  ? effortDisabledReason
                  : this.tr("Choose model or type a custom model ID", "选择模型或输入自定义模型 ID"),
              },
              cx,
            ),
          )
          .child(
            h_flex()
              .id("st-ag-ai-install-eff-group")
              .w(effortChoices.length >= 5 ? 196 : 164)
              .min_w(effortChoices.length >= 5 ? 196 : 164)
              .h(28)
              .flex_none()
              .p(2)
              .gap(1)
              .items_center()
              .rounded(7)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .when(disableEffort, (el) => el.opacity(0.35).tooltip(effortDisabledReason))
              .when(!disableEffort, (el) =>
                el.tooltip(this.tr("Reasoning effort level", "选择模型思考强度")),
              )
              .children(
                effortChoices.map((eff) => {
                  const effKey = eff ?? "auto";
                  const active = !disableEffort && (eff === null ? !curEffort : curEffort === eff);
                  const label = eff === null ? this.tr("Auto", "默认") : this.effortLabel(eff);
                  return div()
                    .id(`st-ag-ai-install-eff-${effKey}`)
                    .flex_1()
                    .min_w(0)
                    .h(22)
                    .px(3)
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
                        .on_click((_e, c) => this.setAgentAiInstallEffort(eff, c)),
                    )
                    .child(label);
                }),
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
            div()
              .flex_1()
              .min_w(0)
              .text_size(11)
              .line_height("15px")
              .text_color(t.dim)
              .child(footerHint),
          )
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(
                this.button(t, "st-ag-ai-install-cancel", this.tr("Cancel", "取消"), (_e, c) =>
                  this.closeAgentAiInstall(c),
                ),
              )
              .child(
                h_flex()
                  .id("st-ag-ai-install-launch")
                  .px(11)
                  .py(4)
                  .gap(5)
                  .rounded(6)
                  .border_1()
                  .border_color(t.accent)
                  .bg(t.accent)
                  .items_center()
                  .text_size(11.5)
                  .font_semibold()
                  .text_color(t.accent_foreground)
                  .when(!canLaunch, (b) => b.opacity(0.55))
                  .when(canLaunch, (b) =>
                    b
                      .cursor_pointer()
                      .hover((s) => s.opacity(0.9))
                      .on_click((_e, c) => this.launchAgentToInstall(row, c)),
                  )
                  .when(this.droveProbeAgentLaunching, (b) =>
                    b.child(
                      svg("mods/workspace-chrome/spinner.svg")
                        .size(11)
                        .text_color(t.accent_foreground)
                        .spin(900),
                    ),
                  )
                  .child(
                    this.droveProbeAgentLaunching
                      ? this.tr("Launching…", "启动中…")
                      : needSeparateAcp
                        ? this.tr("Start AI Install ACP", "启动 AI 安装 ACP")
                        : this.tr("Start AI Install", "启动 AI 安装"),
                  ),
              ),
          ),
      );
  }

  agents(t, cx) {
    const machines = (this.machineList ?? []).filter((m) => !m.temporary);
    const machineChip = (id, label, value) => {
      const on = this.droveMachine === value;
      return div()
        .id(`st-ag-m-${id}`)
        .px(10)
        .py(4)
        .rounded(6)
        .border_1()
        .border_color(on ? t.accent : t.border)
        .bg(on ? t.accent : t.raised)
        .cursor_pointer()
        .text_size(12.5)
        .text_color(on ? t.accent_foreground : t.foreground)
        .on_click((_e, cx) => this.selectTargetMachine(value, cx))
        .child(label);
    };

    const rows = this.detectedAgentRows();
    const issues = this.agentIssues(rows);
    const installedRows = rows.filter((r) => r.anyInstalled);
    const missingRows = rows.filter((r) => !r.anyInstalled);
    const needsActionRows = rows.filter((r) => r.needsAction);
    const acpReadyCount = rows.filter((r) => r.acpReady).length;
    const missingAdapterRows = rows.filter((r) => r.adapterMissing);
    const anyMissingAdapterRows = rows.filter((r) => r.adapterNotInstalled);
    const managedInstalledRows = rows.filter((r) => r.adapter?.source === "managed");
    const updatableAdapterRows = rows.filter((r) => r.adapterHasUpdate);

    const activeFilter =
      this.agentFilter === "issues" && needsActionRows.length === 0
        ? "all"
        : (this.agentFilter || "all");
    const visibleRows =
      activeFilter === "installed"
        ? installedRows
        : activeFilter === "missing"
          ? missingRows
          : activeFilter === "issues"
            ? needsActionRows
            : rows;

    const detecting = Boolean(this.droveProbing || this.droveFixKnownLoading);
    const probedTime = this.formatProbeTimestamp(this.droveData?.agents?.probedAt ?? this.droveData?.agents?.probed_at);

    const autoInstall = this.adapterStatus?.auto_install !== false;
    const injectMcp = this.adapterStatus?.inject_mcp !== false;
    const currentPerms = String(this.adapterStatus?.default_permissions || "approve-reads").trim() || "approve-reads";
    const permOptions = [
      { value: "approve-reads", label: this.tr("Approve Reads (Default)", "仅自动批准只读（默认）") },
      { value: "approve-all", label: this.tr("Approve All", "全部自动批准") },
      { value: "ask", label: this.tr("Ask Every Time", "每次询问") },
      { value: "deny-all", label: this.tr("Deny All", "全部拒绝") },
    ].map((o) => ({ ...o, active: o.value === currentPerms }));
    const permLabel = permOptions.find((o) => o.active)?.label ?? currentPerms;

    const defaultAgent = String(this.person?.default_agent ?? "").trim();
    const defaultAgentItems = [
      { value: "", label: this.tr("Auto (use chosen / last agent)", "自动（按启动时选择）"), active: !defaultAgent },
      ...rows
        .filter((r) => r.acpSupported)
        .map((r) => ({
          value: r.id,
          label: r.name && r.name !== r.id ? `${r.name} (${r.id})` : r.id,
          icon: r.id,
          dot: r.anyInstalled,
          dotColor: r.acpReady ? t.ansi2 : r.hasError ? t.blocked : r.cliInstalled ? t.working : t.dim,
          meta: r.acpReady ? this.tr("ACP ready", "ACP 就绪") : r.cliInstalled ? "CLI" : this.tr("not installed", "未安装"),
          active: defaultAgent === r.id,
        })),
    ];
    if (defaultAgent && !defaultAgentItems.some((it) => it.value === defaultAgent)) {
      defaultAgentItems.splice(1, 0, { value: defaultAgent, label: defaultAgent, meta: this.tr("custom", "自定义"), active: true });
    }
    const defaultAgentLabel = defaultAgentItems.find((it) => it.active)?.label ?? defaultAgent;

    const pill = (text, color, bg = t.background, border = t.border) =>
      div()
        .px(6)
        .py(1.5)
        .rounded(4)
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(10.8)
        .text_color(color)
        .child(text);

    const actionBtn = (id, label, onClick, opts = {}) => {
      const { primary = false, danger = false, busy = false, disabled = false, icon = null, tooltip = null } = opts;
      const bg = primary ? t.accent : danger ? (alpha(t.blocked, "14") ?? t.raised) : t.raised;
      const border = primary ? t.accent : danger ? (alpha(t.blocked, "55") ?? t.blocked) : t.border;
      const fg = primary ? t.accent_foreground : danger ? t.blocked : t.foreground;
      return h_flex()
        .id(id)
        .px(8)
        .py(3.5)
        .gap(4)
        .rounded(6)
        .border_1()
        .border_color(border)
        .bg(bg)
        .items_center()
        .text_size(11.5)
        .when(primary || danger, (el) => el.font_semibold())
        .text_color(fg)
        .when(busy || disabled, (el) => el.opacity(0.6))
        .when(!busy && !disabled, (el) =>
          el
            .cursor_pointer()
            .hover((s) => (primary ? s.opacity(0.9) : s.bg(t.hover)))
            .on_click(onClick),
        )
        .when(Boolean(tooltip), (el) => el.tooltip(tooltip))
        .when(busy, (el) => el.child(svg("mods/workspace-chrome/spinner.svg").size(10).text_color(fg).spin(900)))
        .when(!busy && Boolean(icon), (el) => el.child(svg(I(icon)).size(11).text_color(primary ? fg : danger ? t.blocked : t.accent)))
        .child(label);
    };

    const filterTabs = [
      { id: "all", label: this.tr(`All (${rows.length})`, `全部 (${rows.length})`) },
      { id: "installed", label: this.tr(`Installed (${installedRows.length})`, `已安装 (${installedRows.length})`) },
      { id: "missing", label: this.tr(`Not Installed (${missingRows.length})`, `未安装 (${missingRows.length})`) },
    ];
    if (needsActionRows.length > 0) {
      filterTabs.push({
        id: "issues",
        label: this.tr(`Action Needed (${needsActionRows.length})`, `需处理 (${needsActionRows.length})`),
        warn: true,
      });
    }

    const acpStatusBadge = (r) => {
      if (!r.acpSupported) {
        return div()
          .text_size(11.5)
          .text_color(t.dim)
          .tooltip(this.tr("Interactive CLI agent only (no ACP protocol preset)", "仅支持终端交互 CLI，暂不支持 ACP 协议"))
          .child(this.tr("Not supported", "不支持"));
      }
      if (r.adapterInstalling) {
        return h_flex()
          .gap(4)
          .items_center()
          .px(6)
          .py(1.5)
          .rounded(4)
          .bg(alpha(t.accent, "18") ?? t.background)
          .border_1()
          .border_color(alpha(t.accent, "44") ?? t.border)
          .child(svg("mods/workspace-chrome/spinner.svg").size(10).text_color(t.accent).spin(900))
          .child(
            div()
              .text_size(10.8)
              .text_color(t.accent)
              .child(r.adapterUpdating ? this.tr("Updating…", "更新中…") : this.tr("Installing…", "安装中…")),
          );
      }
      if (r.adapterError) {
        return pill(
          this.tr("Install Error", "安装失败"),
          t.blocked,
          alpha(t.blocked, "18") ?? t.background,
          alpha(t.blocked, "55") ?? t.border,
        ).tooltip(r.adapterError);
      }
      if (r.acpAuthRequired) {
        return pill(
          this.tr("Login Required", "需登录"),
          t.working,
          alpha(t.working, "18") ?? t.background,
          alpha(t.working, "55") ?? t.border,
        ).tooltip(this.stripAcpAuthRawNoise(r.acpError));
      }
      if (r.acpError) {
        return pill(
          this.tr("ACP Error", "探测异常"),
          t.blocked,
          alpha(t.blocked, "18") ?? t.background,
          alpha(t.blocked, "55") ?? t.border,
        ).tooltip(r.acpError);
      }
      if (r.acpMode === "managed") {
        if (r.acpInstalled) {
          const fb = r.adapterFeedback;
          const ver = r.adapter?.version ? ` v${r.adapter.version}` : "";
          const srcHint = r.adapter?.source === "managed"
            ? this.tr("Herdr2 managed adapter", "Herdr2 隔离托管适配器")
            : r.adapter?.source === "path"
              ? "PATH"
              : r.adapter?.source === "config"
                ? this.tr("Custom config.toml", "自定义 config.toml")
                : r.adapter?.source === "npx_cache"
                  ? "npx cache"
                  : "";
          const tip = [srcHint, r.path].filter(Boolean).join(" · ");
          if (fb?.action === "failed") {
            return pill(
              this.tr("Update Failed", "更新失败"),
              t.blocked,
              alpha(t.blocked, "18") ?? t.background,
              alpha(t.blocked, "55") ?? t.border,
            ).tooltip(fb.error || this.tr("Adapter update failed", "适配器更新失败"));
          }
          if (r.adapterHasUpdate) {
            const latestVer = ` v${r.adapter.latest_version}`;
            const upTip = this.tr(
              `Installed v${r.adapter.version}, latest v${r.adapter.latest_version} available`,
              `当前 v${r.adapter.version}，可更新至 v${r.adapter.latest_version}`,
            );
            return pill(
              this.tr(`Update${latestVer}`, `可更新${latestVer}`),
              t.working,
              alpha(t.working, "16") ?? t.background,
              alpha(t.working, "50") ?? t.border,
            ).tooltip(upTip);
          }
          if (fb?.action === "updated") {
            const nextVer = fb.version ? ` v${fb.version}` : ver;
            return pill(
              this.tr(`Updated${nextVer}`, `已更新${nextVer}`),
              t.ansi2,
              alpha(t.ansi2, "16") ?? t.background,
              alpha(t.ansi2, "44") ?? t.border,
            ).when(Boolean(tip), (el) => el.tooltip(tip));
          }
          if (fb?.action === "up_to_date" || r.adapterUpToDate) {
            const curVer = fb?.version ? ` v${fb.version}` : ver;
            return pill(
              this.tr(`Up to date${curVer}`, `已是最新${curVer}`),
              t.ansi2,
              alpha(t.ansi2, "16") ?? t.background,
              alpha(t.ansi2, "44") ?? t.border,
            ).when(Boolean(tip), (el) => el.tooltip(tip));
          }
          return pill(
            this.tr(`Installed${ver}`, `已安装${ver}`),
            t.ansi2,
            alpha(t.ansi2, "16") ?? t.background,
            alpha(t.ansi2, "44") ?? t.border,
          ).when(Boolean(tip), (el) => el.tooltip(tip));
        }
        const isWarn = Boolean(r.cliInstalled);
        return pill(
          this.tr("Not Installed", "未安装"),
          isWarn ? t.working : t.dim,
          isWarn ? (alpha(t.working, "16") ?? t.background) : t.background,
          isWarn ? (alpha(t.working, "50") ?? t.border) : t.border,
        ).tooltip(
          isWarn
            ? this.tr(
                `CLI detected, but managed ACP adapter (${r.acpCommand}) is not installed. Click Install ACP.`,
                `已检测到 CLI，但托管 ACP 适配器（${r.acpCommand}）未安装，点击右侧可一键安装。`,
              )
            : this.tr(
                `Managed ACP adapter (${r.acpCommand}) can be installed with one click.`,
                `托管 ACP 适配器（${r.acpCommand}），支持一键隔离安装。`,
              ),
        );
      }
      if (r.acpMode === "builtin") {
        if (r.acpReady) {
          return pill(
            this.tr("Installed · Built-in", "已安装 · 内置"),
            t.ansi2,
            alpha(t.ansi2, "16") ?? t.background,
            alpha(t.ansi2, "44") ?? t.border,
          ).tooltip(this.tr(`Built-in ACP command: ${r.acpCommand}`, `随 CLI 内置 ACP 命令：${r.acpCommand}`));
        }
        return pill(this.tr("Not Installed", "未安装"), t.dim).tooltip(
          this.tr(
            `Built into ${r.cli || r.id} (${r.acpCommand}). Install the Agent CLI to enable ACP.`,
            `随 ${r.cli || r.id} 内置（${r.acpCommand}），安装 Agent CLI 后即可自动启用。`,
          ),
        );
      }
      // Separate non-managed ACP server (e.g. hermes-acp, zcode-acp-server, custom config.toml)
      if (r.acpReady) {
        const tip = r.path || r.acpCommand || "";
        return pill(
          this.tr("Installed", "已安装"),
          t.ansi2,
          alpha(t.ansi2, "16") ?? t.background,
          alpha(t.ansi2, "44") ?? t.border,
        ).when(Boolean(tip), (el) => el.tooltip(tip));
      }
      const sepTip = r.acpCommand ? this.tr(`Requires ACP binary: ${r.acpCommand}`, `需安装 ACP 组件：${r.acpCommand}`) : "";
      return pill(
        this.tr("Not Installed", "未安装"),
        r.cliInstalled ? t.working : t.dim,
        r.cliInstalled ? (alpha(t.working, "14") ?? t.background) : t.background,
        r.cliInstalled ? (alpha(t.working, "44") ?? t.border) : t.border,
      ).when(Boolean(sepTip), (el) => el.tooltip(sepTip));
    };

    const rowSubline = (r) => {
      if (r.acpAuthRequired && !r.adapterError) {
        return {
          text: this.formatAgentProbeError(r.acpError, r.id),
          color: t.working,
          tooltip: this.stripAcpAuthRawNoise(r.acpError),
        };
      }
      if (r.acpError || r.adapterError) {
        return {
          text: this.formatAgentProbeError(r.acpError || r.adapterError, r.id),
          color: t.blocked,
          tooltip: this.stripAcpAuthRawNoise(r.acpError || r.adapterError),
        };
      }
      if (r.adapterFeedback?.action === "failed" && r.adapterFeedback.error) {
        return {
          text: this.tr(`Update failed: ${r.adapterFeedback.error}`, `更新失败：${r.adapterFeedback.error}`),
          color: t.blocked,
          tooltip: r.adapterFeedback.error,
        };
      }
      const parts = [];
      if (r.models.length > 0) {
        parts.push(this.tr(`${r.models.length} models`, `${r.models.length} 个模型`));
      }
      if (r.acpMode === "managed" && r.adapter?.source && r.acpInstalled) {
        const srcMap = {
          managed: this.tr("Managed", "隔离托管"),
          path: "PATH",
          config: this.tr("Config", "自定义配置"),
          npx_cache: "npx cache",
        };
        parts.push(srcMap[r.adapter.source] ?? r.adapter.source);
      }
      if (r.path) {
        parts.push(r.path);
      } else if (r.acpCommand) {
        parts.push(r.acpCommand);
      } else if (r.cli) {
        parts.push(r.cli);
      }
      const modelsTooltip = r.models.length > 0 ? r.models.map((m) => m.id).join(", ") : undefined;
      return {
        text: parts.join(" · "),
        color: t.dim,
        tooltip: modelsTooltip || r.path || r.install?.display || undefined,
      };
    };

    const rowActions = (r) => {
      const isBusyThis = Boolean(
        r.adapterInstalling ||
          r.adapterUpdating ||
          this.adapterBusy === `install:${r.id}` ||
          this.adapterBusy === `reinstall:${r.id}` ||
          this.adapterBusy === `update:${r.id}`,
      );
      const btns = [];

      // 1. Surface direct sign-in terminal or AI troubleshooting when this row has any error.
      if (r.hasError) {
        if (r.acpAuthRequired && !r.adapterError) {
          const loginSpec = this.agentLoginSpec(r.id, r);
          if (loginSpec?.argv?.length) {
            btns.push(
              actionBtn(
                `st-ag-row-login-${r.id}`,
                this.tr("Sign In", "终端登录"),
                (_e, cx) => this.launchAgentLoginCli(r, cx),
                {
                  primary: true,
                  tooltip: this.tr(
                    `Open terminal to sign in (${loginSpec.display})`,
                    `打开终端运行 ${loginSpec.display} 完成登录或配置 API Key`,
                  ),
                },
              ),
            );
          }
          btns.push(
            actionBtn(
              `st-ag-row-ai-${r.id}`,
              this.tr("Fix with AI", "AI 处理"),
              (_e, cx) => this.openAgentAiFix(cx, r.id),
              {
                icon: "sparkles",
                tooltip: this.tr(`Use AI to diagnose and fix ${r.name}`, `启动 AI 排查并处理 ${r.name} 的认证/环境变量问题`),
              },
            ),
          );
          return btns;
        }
        btns.push(
          actionBtn(
            `st-ag-row-ai-${r.id}`,
            this.tr("Fix with AI", "AI 处理"),
            (_e, cx) => this.openAgentAiFix(cx, r.id),
            {
              danger: true,
              icon: "sparkles",
              tooltip: this.tr(`Use AI to diagnose and fix ${r.name}`, `启动 AI 排查并处理 ${r.name} 的异常`),
            },
          ),
        );
        if (r.acpMode === "managed") {
          btns.push(
            actionBtn(
              `st-ag-adapter-act-${r.id}`,
              isBusyThis ? this.tr("Working…", "处理中…") : this.tr("Reinstall", "重装"),
              (_e, cx) => {
                if (!this.adapterBusy && !r.adapterInstalling) this.runAdapterAction(cx, "reinstall", r.id);
              },
              { busy: isBusyThis, disabled: Boolean(this.adapterBusy) },
            ),
          );
        }
        return btns;
      }

      // 2. Managed ACP adapter rows (claude, codex, pi, glm).
      if (r.acpMode === "managed") {
        if (!r.acpInstalled) {
          if (!r.cliInstalled && !r.standaloneAcp && r.install) {
            btns.push(
              actionBtn(
                `st-ag-row-cli-${r.id}`,
                this.tr("Install CLI", "装 CLI"),
                (_e, cx) => this.installAgentCli(r, cx),
                { tooltip: r.install.display },
              ),
            );
          }
          btns.push(
            actionBtn(
              `st-ag-row-inst-${r.id}`,
              isBusyThis
                ? this.tr("Installing…", "安装中…")
                : r.cliInstalled
                  ? this.tr("Install ACP", "一键安装 ACP")
                  : this.tr("Install ACP", "装 ACP"),
              (_e, cx) => {
                if (!this.adapterBusy && !r.adapterInstalling) this.runAdapterAction(cx, "install", r.id);
              },
              {
                primary: Boolean(r.cliInstalled),
                busy: isBusyThis,
                disabled: Boolean(this.adapterBusy),
                tooltip: this.tr(`One-click install managed ACP adapter (${r.acpCommand})`, `一键隔离安装托管 ACP 适配器（${r.acpCommand}）`),
              },
            ),
          );
          return btns;
        }
        // Managed ACP is already installed.
        if (!r.cliInstalled && !r.standaloneAcp && r.install) {
          btns.push(
            actionBtn(
              `st-ag-row-cli-${r.id}`,
              this.tr("Install CLI", "安装 CLI"),
              (_e, cx) => this.installAgentCli(r, cx),
              { tooltip: r.install.display },
            ),
          );
        }
        if (r.adapter?.source === "managed") {
          const fb = r.adapterFeedback;
          // Only show a per-row Update button when an update is actually available, in progress, or failed;
          // when already up to date, stay quiet so users aren't misled into thinking an update is pending.
          if (isBusyThis || fb?.action === "failed" || r.adapterHasUpdate) {
            const updateLabel = isBusyThis
              ? this.tr("Updating…", "更新中…")
              : fb?.action === "failed"
                ? this.tr("Retry Update", "重试更新")
                : this.tr(`Update v${r.adapter.latest_version}`, `更新至 v${r.adapter.latest_version}`);
            const updateTip = fb?.action === "failed"
              ? fb.error || this.tr("Retry ACP adapter update", "重试更新 ACP 适配器")
              : this.tr(
                  `Update managed ACP adapter from v${r.adapter.version} to v${r.adapter.latest_version}`,
                  `将托管 ACP 适配器从 v${r.adapter.version} 更新至 v${r.adapter.latest_version}`,
                );
            btns.push(
              actionBtn(
                `st-ag-adapter-act-${r.id}`,
                updateLabel,
                (_e, cx) => {
                  if (!this.adapterBusy && !r.adapterInstalling) this.runAdapterAction(cx, "update", r.id);
                },
                {
                  primary: Boolean(r.adapterHasUpdate && fb?.action !== "failed"),
                  danger: fb?.action === "failed",
                  busy: isBusyThis,
                  disabled: Boolean(this.adapterBusy),
                  tooltip: updateTip,
                },
              ),
            );
          }
        }
        return btns;
      }

      // 3. Built-in ACP / separate ACP / CLI-only rows.
      if (!r.cliInstalled) {
        if (r.install) {
          btns.push(
            actionBtn(
              `st-ag-row-cli-${r.id}`,
              this.tr("Install", "一键安装"),
              (_e, cx) => this.installAgentCli(r, cx),
              { tooltip: this.tr(`Run in terminal: ${r.install.display}`, `在终端执行：${r.install.display}`) },
            ),
          );
        } else {
          const isOpen = this.agentAiInstallTargetId === r.id;
          btns.push(
            actionBtn(
              `st-ag-row-ai-inst-${r.id}`,
              this.tr("AI Install", "AI 安装"),
              (_e, cx) => this.toggleAgentAiInstall(r, cx),
              {
                primary: isOpen,
                icon: "sparkles",
                disabled: this.droveProbeAgentLaunching,
                tooltip: this.tr(
                  `Choose an AI agent, model, and reasoning effort to help install ${r.name}`,
                  `选择 AI Agent 类型、模型与思考强度，协助安装 ${r.name}`,
                ),
              },
            ),
          );
        }
        return btns;
      }

      if (r.acpMode === "separate" && !r.acpInstalled) {
        const isOpen = this.agentAiInstallTargetId === r.id;
        btns.push(
          actionBtn(
            `st-ag-row-ai-inst-${r.id}`,
            this.tr("AI Install ACP", "AI 装 ACP"),
            (_e, cx) => this.toggleAgentAiInstall(r, cx),
            {
              primary: isOpen,
              icon: "sparkles",
              disabled: this.droveProbeAgentLaunching,
              tooltip: this.tr(
                `Choose an AI agent, model, and reasoning effort to help install ${r.acpCommand}`,
                `选择 AI Agent 类型、模型与思考强度，协助安装 ${r.acpCommand}`,
              ),
            },
          ),
        );
      }
      return btns;
    };

    return v_flex()
      .id("st-agents-page")
      .w_full()
      .gap(16)
      // 0. Top Machine & Environment Detection Bar
      .child(
        v_flex()
          .id("st-ag-env-bar")
          .w_full()
          .px(14)
          .py(11)
          .gap(9)
          .rounded(10)
          .border_1()
          .border_color(t.border)
          .bg(t.surface)
          .child(
            h_flex()
              .w_full()
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
                actionBtn(
                  "st-ag-probe",
                  detecting
                    ? this.tr("Detecting Environment…", "检测机器环境中…")
                    : this.tr("Detect Environment", "一键检测环境"),
                  (_e, cx) => this.refreshAgentsDetection(cx),
                  {
                    primary: true,
                    busy: detecting,
                    icon: "refresh",
                    tooltip: this.tr(
                      "Re-scan installed CLI agents, ACP adapters, and live model catalogs on the selected machine",
                      "重新扫描目标主机的 CLI Agent、ACP 适配器状态与可用模型信息",
                    ),
                  },
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
                    pill(
                      this.tr(`Agents ${installedRows.length}/${rows.length}`, `已装 Agent ${installedRows.length}/${rows.length}`),
                      installedRows.length > 0 ? t.foreground : t.dim,
                    ),
                  )
                  .child(
                    pill(
                      this.tr(`ACP Ready ${acpReadyCount}`, `ACP 就绪 ${acpReadyCount}`),
                      acpReadyCount > 0 ? t.ansi2 : t.dim,
                      acpReadyCount > 0 ? (alpha(t.ansi2, "16") ?? t.background) : t.background,
                      acpReadyCount > 0 ? (alpha(t.ansi2, "44") ?? t.border) : t.border,
                    ),
                  )
                  .when(missingAdapterRows.length > 0, (el) =>
                    el.child(
                      pill(
                        this.tr(`${missingAdapterRows.length} ACP Missing`, `缺 ${missingAdapterRows.length} 个 ACP 适配器`),
                        t.working,
                        alpha(t.working, "16") ?? t.background,
                        alpha(t.working, "48") ?? t.border,
                      ),
                    ),
                  )
                  .when(updatableAdapterRows.length > 0, (el) =>
                    el.child(
                      pill(
                        this.tr(
                          `${updatableAdapterRows.length} ACP Update${updatableAdapterRows.length > 1 ? "s" : ""}`,
                          `${updatableAdapterRows.length} 个 ACP 可更新`,
                        ),
                        t.accent,
                        alpha(t.accent, "16") ?? t.background,
                        alpha(t.accent, "48") ?? t.border,
                      ),
                    ),
                  )
                  .when(issues.length > 0, (el) =>
                    el.child(
                      div()
                        .id("st-dr-probe-err-badge")
                        .px(7)
                        .py(1.5)
                        .rounded(4)
                        .bg(alpha(t.blocked, "14") ?? t.background)
                        .border_1()
                        .border_color(alpha(t.blocked, "55") ?? t.blocked)
                        .text_size(10.8)
                        .text_color(t.blocked)
                        .child(this.tr(`${issues.length} issue${issues.length > 1 ? "s" : ""}`, `${issues.length} 项异常`)),
                    ),
                  ),
              )
              .child(
                div()
                  .text_size(11.2)
                  .text_color(t.dim)
                  .child(
                    detecting
                      ? this.tr("Scanning CLI & ACP environment…", "正在检测本机 Agent 与 ACP 信息…")
                      : probedTime
                        ? this.tr(`Last detected ${probedTime}`, `上次检测 ${probedTime}`)
                        : this.tr("Click Detect Environment to refresh", "点击「一键检测环境」更新信息"),
                  ),
              ),
          ),
      )
      // 1. Unified Anomaly & AI Troubleshoot Card (probe errors, adapter install errors, env errors)
      .when(issues.length > 0, (el) => el.child(this.renderProbeErrorCard(t, cx, issues)))
      // 2. Unified Supported Agents & ACP Matrix
      .child(
        v_flex()
          .id("st-ag-list-card")
          .w_full()
          .gap(8)
          .child(
            h_flex()
              .w_full()
              .items_center()
              .justify_between()
              .gap(8)
              .flex_wrap()
              .child(
                v_flex()
                  .gap(2)
                  .child(this.heading(t, this.tr("Supported Agents & ACP", "支持的智能体与 ACP")))
                  .child(
                    div()
                      .text_size(12)
                      .text_color(t.dim)
                      .child(
                        this.tr(
                          "All supported agents with Agent CLI status, ACP protocol/adapter status, and one-click install.",
                          "统一查看所有支持的智能体：Agent CLI 是否安装、ACP 是否就绪，并支持一键安装与更新。",
                        ),
                      ),
                  ),
              )
              .child(
                h_flex()
                  .gap(6)
                  .items_center()
                  .flex_wrap()
                  .when(missingAdapterRows.length > 0, (el) =>
                    el.child(
                      actionBtn(
                        "st-dr-adapter-install",
                        this.adapterBusy === "install"
                          ? this.tr("Installing…", "安装中…")
                          : this.tr(`Install Missing ACP (${missingAdapterRows.length})`, `安装缺失 ACP (${missingAdapterRows.length})`),
                        (_e, cx) => this.runAdapterAction(cx, "install"),
                        {
                          primary: true,
                          busy: this.adapterBusy === "install",
                          disabled: Boolean(this.adapterBusy),
                          tooltip: this.tr(
                            `Install managed ACP adapters for detected CLIs: ${missingAdapterRows.map((r) => r.acpCommand || r.id).join(", ")}`,
                            `为已检测到的 CLI 安装缺失的托管 ACP 适配器：${missingAdapterRows.map((r) => r.acpCommand || r.id).join("、")}`,
                          ),
                        },
                      ),
                    ),
                  )
                  .when(missingAdapterRows.length === 0 && anyMissingAdapterRows.length > 0, (el) =>
                    el.child(
                      actionBtn(
                        "st-ag-adapter-install-all",
                        this.adapterBusy === "install-all"
                          ? this.tr("Downloading…", "下载中…")
                          : this.tr(`Download Other ACP (${anyMissingAdapterRows.length})`, `按需下载其余 ACP (${anyMissingAdapterRows.length})`),
                        (_e, cx) => this.runAdapterAction(cx, "install-all"),
                        {
                          busy: this.adapterBusy === "install-all",
                          disabled: Boolean(this.adapterBusy),
                          tooltip: this.tr(
                            `Optional: pre-download remaining managed ACP adapters (${anyMissingAdapterRows.map((r) => r.acpCommand || r.id).join(", ")}). Not required unless you use them.`,
                            `可选：预先下载尚未安装的其余托管 ACP 适配器（${anyMissingAdapterRows.map((r) => r.acpCommand || r.id).join("、")}），未使用的智能体无需提前下载。`,
                          ),
                        },
                      ),
                    ),
                  )
                  .when(updatableAdapterRows.length > 0, (el) =>
                    el.child(
                      actionBtn(
                        "st-dr-adapter-update",
                        this.adapterBusy === "update"
                          ? this.tr("Updating…", "更新中…")
                          : this.tr(`Update Available ACP (${updatableAdapterRows.length})`, `更新可用 ACP (${updatableAdapterRows.length})`),
                        (_e, cx) => this.runAdapterAction(cx, "update"),
                        {
                          primary: true,
                          busy: this.adapterBusy === "update",
                          disabled: Boolean(this.adapterBusy),
                          tooltip: updatableAdapterRows
                            .map((u) => `${u.name} v${u.adapter?.version ?? "?"} → v${u.adapter?.latest_version ?? "?"}`)
                            .join(" · "),
                        },
                      ),
                    ),
                  )
                  .when(updatableAdapterRows.length === 0 && managedInstalledRows.length > 0, (el) =>
                    el.child(
                      actionBtn(
                        "st-dr-adapter-check",
                        this.adapterBusy === "check"
                          ? this.tr("Checking…", "检查更新中…")
                          : this.tr("Check ACP Updates", "检查 ACP 更新"),
                        (_e, cx) => this.checkAdapterUpdates(cx),
                        {
                          icon: "refresh",
                          busy: this.adapterBusy === "check",
                          disabled: Boolean(this.adapterBusy),
                          tooltip: this.tr(
                            "Check official ACP Registry for newer managed adapter versions without re-downloading up-to-date adapters",
                            "检查官方 ACP Registry 是否有托管适配器新版本（仅拉取版本清单，不会重复下载已是最新的适配器）",
                          ),
                        },
                      ),
                    ),
                  ),
              ),
          )
          // Filter Tabs
          .child(
            h_flex()
              .id("st-ag-filter-bar")
              .gap(5)
              .items_center()
              .flex_wrap()
              .children(
                filterTabs.map((tab) => {
                  const on = activeFilter === tab.id;
                  return div()
                    .id(`st-ag-filter-${tab.id}`)
                    .px(9)
                    .py(3.5)
                    .rounded(6)
                    .border_1()
                    .border_color(on ? t.accent : tab.warn ? (alpha(t.working, "48") ?? t.border) : t.border)
                    .bg(on ? (alpha(t.accent, "18") ?? t.raised) : t.surface)
                    .cursor_pointer()
                    .text_size(11.5)
                    .when(on, (el) => el.font_semibold())
                    .text_color(on ? t.accent : tab.warn ? t.working : t.muted)
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, c) => {
                      this.agentFilter = tab.id;
                      c.notify();
                    })
                    .child(tab.label);
                }),
              ),
          )
          // Unified Table Card
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              // Table Header
              .child(
                h_flex()
                  .id("st-ag-table-header")
                  .w_full()
                  .px(14)
                  .py(7)
                  .gap(10)
                  .items_center()
                  .bg(t.background)
                  .border_b(1)
                  .border_color(t.border)
                  .child(div().flex_1().min_w(0).text_size(11).font_semibold().text_color(t.dim).child(this.tr("AGENT", "智能体")))
                  .child(div().w(86).flex_none().text_size(11).font_semibold().text_color(t.dim).child(this.tr("AGENT CLI", "Agent 状态")))
                  .child(div().w(140).flex_none().text_size(11).font_semibold().text_color(t.dim).child(this.tr("ACP STATUS", "ACP 状态")))
                  .child(div().w(166).flex_none().flex().justify_end().text_size(11).font_semibold().text_color(t.dim).child(this.tr("ACTIONS", "操作"))),
              )
              .when(visibleRows.length === 0, (el) =>
                el.child(
                  div()
                    .px(14)
                    .py(14)
                    .text_size(12)
                    .text_color(t.dim)
                    .child(this.tr("No agents match this filter.", "当前筛选条件下没有智能体。")),
                ),
              )
              .children(
                visibleRows.map((r, idx) => {
                  const sub = rowSubline(r);
                  const btns = rowActions(r);
                  const aiInstallOpen = this.agentAiInstallTargetId === r.id;
                  const rowEl = h_flex()
                    .id(`st-ag-row-${r.id}`)
                    .w_full()
                    .px(14)
                    .py(9)
                    .gap(10)
                    .items_center()
                    // Col 1: Agent identity & details
                    .child(
                      h_flex()
                        .flex_1()
                        .min_w(0)
                        .gap(10)
                        .items_center()
                        .child(this.agentIconEl(t, r.id, 17))
                        .child(
                          v_flex()
                            .flex_1()
                            .min_w(0)
                            .gap(2)
                            .child(
                              h_flex()
                                .gap(6)
                                .items_center()
                                .child(
                                  div()
                                    .truncate()
                                    .text_size(13)
                                    .font_semibold()
                                    .text_color(r.anyInstalled ? t.foreground : t.muted)
                                    .child(r.name),
                                )
                                .when(r.name.toLowerCase() !== r.id.toLowerCase(), (el) =>
                                  el.child(div().flex_none().text_size(11).text_color(t.dim).child(r.id)),
                                ),
                            )
                            .when(Boolean(sub.text), (el) =>
                              el.child(
                                div()
                                  .truncate()
                                  .text_size(11.2)
                                  .text_color(sub.color)
                                  .when(Boolean(sub.tooltip), (d) => d.tooltip(sub.tooltip))
                                  .child(sub.text),
                              ),
                            ),
                        ),
                    )
                    // Col 2: Agent CLI installed status
                    .child(
                      h_flex()
                        .w(86)
                        .flex_none()
                        .items_center()
                        .child(
                          r.standaloneAcp
                            ? pill(this.tr("ACP Only", "仅 ACP"), t.dim)
                            : r.cliInstalled
                              ? h_flex()
                                  .gap(5)
                                  .items_center()
                                  .child(div().w(6).h(6).rounded(3).bg(t.ansi2))
                                  .child(div().text_size(11.5).text_color(t.ansi2).child(this.tr("Installed", "已安装")))
                              : h_flex()
                                  .gap(5)
                                  .items_center()
                                  .child(div().w(6).h(6).rounded(3).bg(t.dim))
                                  .child(div().text_size(11.5).text_color(t.dim).child(this.tr("Missing", "未安装"))),
                        ),
                    )
                    // Col 3: ACP status
                    .child(
                      h_flex()
                        .w(140)
                        .flex_none()
                        .items_center()
                        .child(acpStatusBadge(r)),
                    )
                    // Col 4: One-click Actions / AI Fix
                    .child(
                      h_flex()
                        .w(166)
                        .flex_none()
                        .gap(6)
                        .items_center()
                        .justify_end()
                        .when(btns.length > 0, (el) => el.children(btns))
                        .when(btns.length === 0, (el) =>
                          el.child(
                            div()
                              .text_size(11.5)
                              .text_color(r.anyInstalled ? t.ansi2 : t.dim)
                              .child(r.anyInstalled ? this.tr("✓ Ready", "✓ 已就绪") : "—"),
                          ),
                        ),
                    );
                  if (!aiInstallOpen) {
                    return rowEl.when(idx > 0, (el) => el.border_t(1).border_color(t.border));
                  }
                  return v_flex()
                    .w_full()
                    .when(idx > 0, (el) => el.border_t(1).border_color(t.border))
                    .child(rowEl)
                    .child(
                      div()
                        .w_full()
                        .px(14)
                        .pb(10)
                        .child(this.renderAgentAiInstallPanel(t, cx, r)),
                    );
                }),
              ),
          ),
      )
      // 3. ACP Detection & Runtime Configuration
      .child(
        v_flex()
          .id("st-ag-config-card")
          .w_full()
          .gap(8)
          .child(
            v_flex()
              .gap(2)
              .child(this.heading(t, this.tr("ACP Configuration", "ACP 运行配置")))
              .child(
                div()
                  .text_size(12)
                  .text_color(t.dim)
                  .child(
                    this.adapterStatus?.config_path
                      ? this.tr(
                          `Saved to ${this.adapterStatus.config_path}${this.adapterStatus?.adapters_dir ? ` · Managed adapters: ${this.adapterStatus.adapters_dir}` : ""}`,
                          `配置保存至 ${this.adapterStatus.config_path}${this.adapterStatus?.adapters_dir ? ` · 托管适配器目录：${this.adapterStatus.adapters_dir}` : ""}`,
                        )
                      : this.tr(
                          "Controls managed ACP adapter auto-installation, default ACP permissions, MCP injection, and registry mirror.",
                          "配置托管 ACP 适配器自动安装、默认 ACP 权限策略、MCP 工具注入与注册表源。",
                        ),
                  ),
              ),
          )
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Auto-install Managed ACP Adapters", "自动安装托管 ACP 适配器"),
                  this.tr(
                    "Automatically download missing ACP adapters (claude, codex, pi) when their companion CLI is detected or launched.",
                    "检测到对应 CLI 或启动无头会话时，自动在隔离目录安装缺失的 ACP 适配器。",
                  ),
                  this.toggle(t, "st-ag-cfg-auto-install", autoInstall, (_e, cx) =>
                    this.saveAcpConfig({ autoInstall: !autoInstall }, cx),
                  ),
                  true,
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Default ACP Permissions", "默认 ACP 权限策略"),
                  this.tr(
                    "Permission mode used for new headless ACP sessions unless overridden per session or template.",
                    "新建无头 ACP 会话的默认权限模式（可被单会话或模板覆盖）。",
                  ),
                  this.drovePicker(
                    t,
                    "ag-cfg-perms",
                    permLabel,
                    permOptions,
                    (val, cx) => this.saveAcpConfig({ defaultPermissions: val }, cx),
                    { width: 220, popupWidth: 260, anchor: "bottom_right" },
                    cx,
                  ),
                ),
              )
              .child(
                this.droveCardRow(
                  t,
                  this.tr("Auto-inject herdr2 MCP Server", "自动注入 herdr2 MCP 工具"),
                  this.tr(
                    "Inject the built-in `h2 mcp` server into headless ACP agents so they can orchestrate child agents and terminals.",
                    "向无头 ACP 智能体自动注入内置 `h2 mcp` 服务，启用子智能体与终端编排能力。",
                  ),
                  this.toggle(t, "st-ag-cfg-inject-mcp", injectMcp, (_e, cx) =>
                    this.saveAcpConfig({ injectMcp: !injectMcp }, cx),
                  ),
                ),
              )
              .when(this.droveMachine === null, (el) =>
                el.child(
                  this.droveCardRow(
                    t,
                    this.tr("Default Agent", "默认智能体"),
                    this.tr(
                      "Preferred local ACP agent preset stored in settings.json when starting a new pane without explicit command.",
                      "本机 settings.json 中保存的默认智能体预设。",
                    ),
                    this.drovePicker(
                      t,
                      "ag-cfg-default-agent",
                      defaultAgentLabel,
                      defaultAgentItems,
                      (val, cx) => {
                        this.change((q) => {
                          if (!val) delete q.default_agent;
                          else q.default_agent = val;
                        }, cx);
                      },
                      { width: 220, popupWidth: 280, anchor: "bottom_right" },
                      cx,
                    ),
                  ),
                ),
              )
              .child(
                v_flex()
                  .w_full()
                  .px(14)
                  .py(11)
                  .gap(7)
                  .border_t(1)
                  .border_color(t.border)
                  .child(
                    v_flex()
                      .gap(2)
                      .child(div().text_size(13).text_color(t.foreground).child(this.tr("ACP Registry URL", "ACP 注册表地址（镜像源）")))
                      .child(
                        div()
                          .text_size(11.8)
                          .text_color(t.dim)
                          .child(
                            this.tr(
                              "Optional override for the official ACP Registry index JSON. Leave blank to use the default CDN.",
                              "可选覆盖官方 ACP Registry 索引地址（支持自定义镜像）；留空则使用官方默认源。",
                            ),
                          ),
                      ),
                  )
                  .child(
                    h_flex()
                      .w_full()
                      .gap(8)
                      .items_center()
                      .child(
                        div()
                          .id("st-ag-cfg-registry-url")
                          .flex_1()
                          .min_w(0)
                          .px(8)
                          .py(3)
                          .rounded(6)
                          .border_1()
                          .border_color(t.border)
                          .bg(t.background)
                          .child(Input.new(this.adapterRegistryUrlInput).h(26).text_size(12.2).text_color(t.foreground)),
                      )
                      .child(
                        this.button(
                          t,
                          "st-ag-cfg-reg-save",
                          this.adapterConfigSaving ? this.tr("Saving…", "保存中…") : this.tr("Save", "保存"),
                          (_e, cx) => this.saveAcpConfig({ registryUrl: this.adapterRegistryUrlInput?.value?.() ?? "" }, cx),
                        ),
                      )
                      .when(Boolean(this.adapterStatus?.custom_registry_url), (el) =>
                        el.child(
                          this.button(
                            t,
                            "st-ag-cfg-reg-reset",
                            this.tr("Reset", "恢复默认"),
                            (_e, cx) => this.saveAcpConfig({ registryUrl: "" }, cx),
                          ),
                        ),
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
        .on_click((_e, cx) => this.selectTargetMachine(value, cx))
        .child(label);
    };
    const magpieOn = Boolean(cfg.magpie?.enabled);
    const sharedAi = this.aiStatus ?? aiStatus;
    const aiOn = sharedAi.enabled !== false && cfg.ai?.enabled !== false;
    const aiModels = this.aiAvailableModels();
    const currentAiModel = (sharedAi.model ?? cfg.ai?.model ?? "").trim();
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

    const probeSummary = this.droveProbeSummary();
    const installedCount = probeSummary.installed.length;
    const readyCount = probeSummary.ready.length;
    const errorAgents = probeSummary.errored;
    const adapterList = Array.isArray(this.adapterStatus?.adapters) ? this.adapterStatus.adapters : [];
    const missingDetectedAdapters = adapterList.filter((a) => a.companion_installed && !a.installed && a.state !== "installing");
    const hasAgentIssues = errorAgents.length > 0 || missingDetectedAdapters.length > 0;

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
                    .child(this.tr(`${readyCount} agents ready`, `${readyCount} 个 Agent 可用`)),
                ),
              )
              .child(this.button(t, "st-dr-goto-agents", this.tr("Agents & ACP", "智能体与 ACP"), (_e, cx) => this.openAgentsSection(cx)))
              .child(this.button(t, "st-dr-web", this.tr("Open Drove Web", "打开 Drove 网页"), () => {
                if (!(typeof checking === "function" && checking())) hc(this.droveArgs(["drove", "web"]));
              })),
          ),
      )
      .when(hasAgentIssues, (el) =>
        el.child(
          h_flex()
            .id("st-dr-agents-notice")
            .w_full()
            .px(12)
            .py(8)
            .gap(10)
            .rounded(8)
            .border_1()
            .border_color(errorAgents.length > 0 ? t.blocked : t.working)
            .bg(t.surface)
            .items_center()
            .justify_between()
            .flex_wrap()
            .child(
              div()
                .text_size(12)
                .text_color(errorAgents.length > 0 ? t.blocked : t.working)
                .child(
                  errorAgents.length > 0
                    ? this.tr(
                        `Detected ${errorAgents.length} ACP agent probe error${errorAgents.length > 1 ? "s" : ""}. Inspect or fix in Settings → Agents & ACP.`,
                        `检测到 ${errorAgents.length} 个 ACP Agent 探测异常，请前往「智能体与 ACP」排查处理。`,
                      )
                    : this.tr(
                        `${missingDetectedAdapters.length} detected CLI${missingDetectedAdapters.length > 1 ? "s are" : " is"} missing managed ACP adapters.`,
                        `检测到 ${missingDetectedAdapters.length} 个 CLI 缺少托管 ACP 适配器，可在「智能体与 ACP」一键安装。`,
                      ),
                ),
            )
            .child(
              this.button(
                t,
                "st-dr-notice-goto-agents",
                this.tr("Open Agents & ACP", "前往智能体与 ACP"),
                (_e, cx) => this.openAgentsSection(cx),
              ),
            ),
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
            const aiReady = Boolean(sharedAi.configured || aiStatus.configured || this.droveUrlInput.value().trim() || cfg.ai?.baseUrl?.trim());
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
                            : this.tr("Configure AI Assistant in Settings → AI first", "请先在「设置 → AI 助手」中配置服务地址"),
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
                  (sharedAi.configured || aiStatus.configured)
                    ? this.tr(
                        `Ready · ${currentAiModel || aiModels[0] || "Auto"}`,
                        `已就绪 · ${currentAiModel || aiModels[0] || "自动模型"}`,
                      )
                    : this.tr("AI Assistant not configured", "尚未配置全局 AI 助手"),
                  (sharedAi.configured || aiStatus.configured)
                    ? this.tr(
                        `Shared across all machines & Drove (${sharedAi.baseUrl || cfg.ai?.baseUrl || aiStatus.baseUrl || "local"}). Edit endpoint, API key, cache & core switches in Settings → AI.`,
                        `所有主机与 Drove 共用一套配置（${sharedAi.baseUrl || cfg.ai?.baseUrl || aiStatus.baseUrl || "本地"}）。前往「设置 → AI 助手」管理接口地址、Key 与缓存统计。`,
                      )
                    : this.tr(
                        "Configure an OpenAI-compatible helper once in Settings → AI — shared automatically across all machines for Drove orchestration, conflict/loop checks, and PTY state arbitration.",
                        "在「设置 → AI 助手」中配置一次轻量 OpenAI 兼容模型，所有主机自动共用，同时启用 Drove 编排辅助、死循环/冲突分析与终端状态仲裁。",
                      ),
                  h_flex()
                    .gap(8)
                    .items_center()
                    .child(
                      this.toggle(t, "st-dr-ai-on", aiOn, (_e, cx) => this.saveAiPatch({ enabled: !aiOn }, cx)),
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

  skillArgs(base) {
    return this.skillMachine ? [...base, "--on", this.skillMachine] : base;
  }

  async loadSkills() {
    if (this.skillLoading || (typeof checking === "function" && checking())) return;
    this.skillLoading = true;
    this.skillError = null;
    try {
      const r = await hc([...this.skillArgs(["skill", "status"]), "--json"], { timeout: 60000 });
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
        const args = this.skillArgs(["skill", "install"]);
        args.push("--json");
        if (opts?.target) args.push("--target", opts.target);
        else if (opts?.agent) args.push("--agent", opts.agent);
        else args.push("--all");
        const r = await hc(args, { timeout: 60000 });
        let parsed = null;
        try { parsed = JSON.parse(r.stdout || r.stderr); } catch {}
        if (r.code !== 0) {
          throw new Error(parsed?.error?.message || (r.stderr || r.stdout || "Skill installation failed.").trim());
        }
        if (parsed?.status) this.skillData = parsed.status;
        else await this.loadSkills();
        const hostNote = this.skillMachine ? ` (${this.skillMachine})` : "";
        this.skillResult = {
          ok: true,
          usedAi: false,
          summary: this.tr(
            `Installed ${parsed?.installedCount ?? 0} skill file(s) across ${(parsed?.targets ?? []).length} target(s)${hostNote}.`,
            `已为${this.skillMachine ? `远程主机 ${this.skillMachine} 的 ` : ""}${(parsed?.targets ?? []).length} 个目标目录安装 ${parsed?.installedCount ?? 0} 个 Skill 文件。`,
          ),
          targets: parsed?.targets ?? [],
          tips: [],
        };
      } catch (e) {
        this.skillError = String(e?.message ?? e);
        notify({
          title: this.tr("Skill installation failed", "Skill 安装失败"),
          body: this.skillError,
          level: "error",
        });
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
        const hint = String(this.skillAiHintInput?.value?.() ?? "").replace(/\r?\n/g, " ").trim();
        const args = this.skillArgs(["skill", "ai-install", "--lang", this.lang()]);
        args.push("--json");
        if (hint) args.push("--query", hint);
        const r = await hc(args, { timeout: 120000 });
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
        notify({
          title: parsed?.aiError
            ? this.tr("Skill installation completed with warnings", "Skill 安装已完成（含提示）")
            : this.tr("AI Skill installation completed", "AI Skill 安装完成"),
          body: this.skillResult.summary,
          level: parsed?.aiError ? "warning" : "success",
        });
      } catch (e) {
        this.skillError = String(e?.message ?? e);
        notify({
          title: this.tr("AI Skill installation failed", "AI Skill 安装失败"),
          body: this.skillError,
          level: "error",
        });
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
    const rawAgents = Array.isArray(data.agents) ? data.agents : [];
    // Cross-reference Agents & ACP detection when targeting the same machine so detected agents stay aligned.
    const sameMachine = (this.skillMachine ?? null) === (this.droveMachine ?? null);
    const acpRowsById = new Map();
    if (sameMachine) {
      for (const r of this.detectedAgentRows()) {
        acpRowsById.set(r.id, r);
      }
    }
    const catalogOrder = new Map(SUPPORTED_AGENTS_CATALOG.map((c, idx) => [c.id, idx]));
    const agents = rawAgents
      .map((a) => {
        const canonicalId = a.id === "antigravity" ? "agy" : a.id;
        const acpRow = acpRowsById.get(canonicalId);
        const cliDetected = Boolean(
          a.cliDetected ||
            (Array.isArray(a.detectedBy) && a.detectedBy.some((d) => String(d).startsWith("cli:"))) ||
            acpRow?.cliInstalled ||
            acpRow?.anyInstalled,
        );
        const detected = Boolean(a.detected || cliDetected);
        return {
          ...a,
          canonicalId,
          name: acpRow?.name || a.name,
          cliDetected,
          detected,
        };
      })
      .sort((a, b) => {
        const rank = (x) => (x.cliDetected ? 0 : x.detected ? 1 : 2);
        const d = rank(a) - rank(b);
        if (d !== 0) return d;
        const oa = catalogOrder.has(a.canonicalId) ? catalogOrder.get(a.canonicalId) : 999;
        const ob = catalogOrder.has(b.canonicalId) ? catalogOrder.get(b.canonicalId) : 999;
        if (oa !== ob) return oa - ob;
        return String(a.name).localeCompare(String(b.name));
      });
    const detectedAgents = agents.filter((a) => a.detected);
    const activeSkillFilter =
      this.skillAgentFilter === "detected" && detectedAgents.length === 0
        ? "all"
        : (this.skillAgentFilter || "detected");
    const visibleAgents = activeSkillFilter === "detected" ? detectedAgents : agents;
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

    const machines = (this.machineList ?? []).filter((m) => !m.temporary);
    const machineChip = (id, label, value) => {
      const on = this.skillMachine === value;
      return div()
        .id(`st-skill-m-${id}`)
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
          this.skillMachine = value;
          this.skillData = null;
          this.skillError = null;
          this.skillResult = null;
          cx.spawn(async (cx) => {
            await this.loadSkills();
            cx.notify();
          });
        })
        .child(label);
    };

    return v_flex()
      .w_full()
      .gap(16)
      // 0. Target Machine Bar (Local / SSH Remotes)
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
              .child(div().text_size(12).text_color(t.dim).child(this.tr("Target Machine:", "目标主机：")))
              .child(machineChip("local", this.tr("This computer", "本机"), null))
              .children(machines.map((m) => machineChip(m.name, m.name, m.name))),
          )
          .child(
            div()
              .text_size(11.5)
              .text_color(t.dim)
              .child(
                this.skillMachine
                  ? this.tr(`Managing skills on ${this.skillMachine}`, `正在管理远程主机 ${this.skillMachine} 的 Skills`)
                  : this.tr("Managing skills on this computer", "正在管理本机的 Skills"),
              ),
          ),
      )
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
          .gap(8)
          .child(
            h_flex()
              .w_full()
              .items_center()
              .justify_between()
              .gap(8)
              .flex_wrap()
              .child(this.heading(t, this.tr("Coding Agents & Skill Directories", "智能体 Skill 目录列表")))
              .child(
                h_flex()
                  .id("st-skill-filter-bar")
                  .gap(5)
                  .items_center()
                  .children(
                    [
                      { id: "detected", label: this.tr(`Detected (${detectedAgents.length})`, `已检测到 (${detectedAgents.length})`) },
                      { id: "all", label: this.tr(`All (${agents.length})`, `全部 (${agents.length})`) },
                    ].map((tab) => {
                      const on = activeSkillFilter === tab.id;
                      return div()
                        .id(`st-skill-filter-${tab.id}`)
                        .px(9)
                        .py(3)
                        .rounded(6)
                        .border_1()
                        .border_color(on ? t.accent : t.border)
                        .bg(on ? (alpha(t.accent, "18") ?? t.raised) : t.surface)
                        .cursor_pointer()
                        .text_size(11.5)
                        .when(on, (el) => el.font_semibold())
                        .text_color(on ? t.accent : t.muted)
                        .hover((s) => s.bg(t.hover).text_color(t.foreground))
                        .on_click((_e, c) => {
                          this.skillAgentFilter = tab.id;
                          c.notify();
                        })
                        .child(tab.label);
                    }),
                  ),
              ),
          )
          .child(
            v_flex()
              .w_full()
              .rounded(10)
              .border_1()
              .border_color(t.border)
              .bg(t.surface)
              .overflow_hidden()
              .children(
                visibleAgents.map((a, idx) => {
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
                    .id(`st-skill-row-${a.id}`)
                    .w_full()
                    .px(14)
                    .py(10)
                    .gap(12)
                    .items_center()
                    .when(idx > 0, (el) => el.border_t(1).border_color(t.border))
                    .child(this.agentIconEl(t, a.canonicalId || a.id, 17))
                    .child(
                      v_flex()
                        .flex_1()
                        .min_w(0)
                        .gap(2)
                        .child(
                          h_flex()
                            .gap(7)
                            .items_center()
                            .flex_wrap()
                            .child(div().text_size(13).font_semibold().text_color(a.detected ? t.foreground : t.muted).child(a.name))
                            .when(a.cliDetected, (el) =>
                              el.child(
                                div()
                                  .px(6)
                                  .py(1)
                                  .rounded(4)
                                  .bg(t.ansi2 + "1c")
                                  .text_size(10.5)
                                  .text_color(t.ansi2)
                                  .child(this.tr("Detected", "已安装")),
                              ),
                            )
                            .when(!a.cliDetected && a.detected, (el) =>
                              el.child(
                                div()
                                  .px(6)
                                  .py(1)
                                  .rounded(4)
                                  .bg(t.background)
                                  .border_1()
                                  .border_color(t.border)
                                  .text_size(10.5)
                                  .text_color(t.muted)
                                  .child(this.tr("Config Dir", "仅配置目录")),
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

  saveReleasesUrl(cx) {
    const val = (this.releasesUrlInput?.value?.() ?? "").trim().replace(/\/+$/, "");
    const cur = (this.person?.releases_url ?? "").trim().replace(/\/+$/, "");
    if (val === cur) return;
    this.change((p) => {
      if (val) p.releases_url = val;
      else delete p.releases_url;
    }, cx);
  }

  triggerUpdateCheck(cx) {
    if (this.waitingUpdateCheck) return;
    const val = (this.releasesUrlInput?.value?.() ?? "").trim().replace(/\/+$/, "");
    const cur = (this.person?.releases_url ?? "").trim().replace(/\/+$/, "");
    const u = typeof updates === "function" ? updates() : {};
    this.pendingUpdateCheckSeq = Number(u?.check_seq ?? 0);
    this.waitingUpdateCheck = true;
    this.updateModal = null;
    cx?.notify?.();
    const startCheck = () => {
      run("app.check_updates");
    };
    if (val !== cur) {
      cx.spawn(async (spawnCx) => {
        const failed = await this.save((p) => {
          if (val) p.releases_url = val;
          else delete p.releases_url;
        });
        if (failed) {
          this.waitingUpdateCheck = false;
          notify({ ...failed, level: "error" });
          spawnCx.notify();
          return;
        }
        startCheck();
        spawnCx.notify();
      });
    } else {
      startCheck();
    }
  }

  cleanCoreVersion(v) {
    if (!v) return "";
    return String(v).trim().replace(/^(?:h2|herdr2|hc)\s+/i, "").split(/\s+/)[0] ?? "";
  }

  updateRemoteMachines(u) {
    const core = u?.core ?? {};
    const targetCore = this.cleanCoreVersion(core.ready || core.version || "");
    const statusList = Array.isArray(u?.machines) ? u.machines : [];
    const liveList = Array.isArray(this.machineList) ? this.machineList : [];
    const byName = new Map();
    for (const m of liveList) {
      if (m && !m.temporary && m.name) byName.set(m.name, { live: m, status: null });
    }
    for (const m of statusList) {
      if (!m || !m.name) continue;
      const prev = byName.get(m.name);
      if (prev) prev.status = m;
      else byName.set(m.name, { live: null, status: m });
    }
    const out = [];
    for (const { live, status } of byName.values()) {
      const name = status?.name ?? live?.name ?? "";
      const target = status?.target ?? live?.target ?? "";
      const version = this.cleanCoreVersion(status?.version ?? live?.version ?? "");
      const rawState = String(status?.raw_state ?? live?.state ?? status?.state ?? "unreachable");
      const why = status?.why ?? live?.why ?? null;
      const unreachable = Boolean(status?.unreachable ?? (rawState === "unreachable" || rawState === "needs_login"));
      const outdated = rawState === "needs_update"
        || Boolean(status?.ready)
        || (Boolean(targetCore) && (!version || this.olderCore(version, targetCore)));
      const needsUpdate = Boolean(status?.needs_update || outdated || unreachable);
      const state = needsUpdate ? "needs_update" : (rawState === "connected" ? "connected" : rawState);
      const ready = status?.ready || (outdated && targetCore ? targetCore : null);
      out.push({
        name,
        target,
        version: version || null,
        state,
        rawState,
        why,
        ready,
        needsUpdate,
        unreachable,
        updating: Boolean(status?.updating),
        error: status?.error ?? null,
      });
    }
    return out;
  }

  localWorkspaceSpace() {
    const all = typeof spaces === "function" ? (spaces() ?? []) : [];
    const active = all.find((s) => s.active && (s.machine ?? null) === null);
    if (active) return active;
    return all.find((s) => (s.machine ?? null) === null) ?? null;
  }

  buildAiUpdatePrompt(u, extraError = null) {
    const app = u?.app ?? {};
    const core = u?.core ?? {};
    const ui = u?.ui ?? {};
    const platform = (typeof window === "function" ? window()?.platform : null) || "unknown";
    const releasesUrl = u?.releases_url || "https://github.com/herdr2/herdr2/releases/download/ota-latest";
    const remoteMachines = this.updateRemoteMachines(u);
    const errors = [
      extraError ? `Manual update/check error: ${extraError}` : null,
      app.error ? `App update error: ${app.error}` : null,
      ui.error ? `UI bundle error: ${ui.error}` : null,
      core.error ? `Core update error: ${core.error}` : null,
      u?.last_error && u.last_error !== extraError ? `Last check error: ${u.last_error}` : null,
      ...remoteMachines
        .filter((m) => m.error || m.unreachable)
        .map((m) => `Remote ${m.name} (${m.target}) [${m.state}]: ${m.error || m.why || m.rawState}`),
    ].filter(Boolean);

    const remoteSummaryLines = remoteMachines.map((m) => {
      const ver = m.version ? fmtVer(m.version) : "unknown";
      const next = m.ready ? ` -> ${fmtVer(m.ready)}` : "";
      const note = m.unreachable ? ` (unreachable: ${m.why || m.rawState})` : "";
      return `- ${m.name} (${m.target}): herdr2-core ${ver}${next} [${m.state}]${note}`;
    });

    if (this.lang() === "zh-CN") {
      return [
        "手动更新 Herdr2 失败或需要通过本机 AI 从 GitHub 获取最新版本，请你协助完成下载与更新：",
        "",
        "当前环境信息：",
        `- 本机平台：${platform}`,
        `- 本机 Herdr2：${app.version ? fmtVer(app.version) : "未知"}${app.ready ? `（检测到新版本 ${fmtVer(app.ready)}）` : ""}`,
        `- 本机 herdr2-core：${core.version ? fmtVer(core.version) : "未知"}${core.ready ? `（检测到新版本 ${fmtVer(core.ready)}）` : ""}${core.managed ? `（包管理器托管：${core.managed}）` : ""}`,
        `- 配置更新源：${releasesUrl}`,
        "- GitHub 官方发布页：https://github.com/herdr2/herdr2/releases （OTA 滚动标签：ota-latest）",
        ...(remoteSummaryLines.length > 0 ? ["", "已配置的 SSH 远程主机：", ...remoteSummaryLines] : []),
        ...(errors.length > 0 ? ["", "失败/异常上下文：", ...errors.map((e) => `- ${e}`)] : []),
        "",
        "重要防中断规则（务必遵守）：",
        "- 你当前运行在 `herdr2-core` 的独立 PTY holder 进程内：更新 `herdr2-core` 时必须使用 `h2 update --from <file>` 或 `h2 server restart`（会移交 holder，保持当前终端和你不中断）；严禁执行 `h2 server stop`、`pkill -f herdr2`、`killall herdr2` 或 `taskkill /IM herdr2*`（会误杀 `herdr2-core` / holder 导致当前更新中途终止）。",
        "- 更新过程中不要中途重启或强杀 `herdr2` 桌面窗口；在 Windows 上优先下载压缩包并原地改名替换（如将正在运行的 `herdr2.exe` 改名为 `herdr2.exe.old` 再放入新文件，或写入 `.new` + `.new.json` 供重启切换），避免静默运行会强制关闭进程的 `.exe` 安装向导。",
        "- 请先完整做完下载校验、更新本机 `herdr2-core`、同步 SSH 远程主机、替换本机 `herdr2` 文件以及版本验证，直到全部完成后再告知我重启 Herdr2 桌面窗口（不要在中间步骤重启窗口）。",
        "",
        "请按以下步骤安全执行：",
        "1. 若上方有报错（如网络代理、TLS/证书、权限、GitHub 访问受限等），先排查原因并选择可用方式（如检查环境变量代理、`gh release`、GitHub API 或 `curl`）查询 https://github.com/herdr2/herdr2/releases 上的最新版本与 `ota-latest` 清单（`app-latest.json`、`core-latest.json`）。",
        "2. 检查本机 `herdr2` 与 `herdr2-core`（`h2`）当前的安装路径与安装方式（如 Linux `.deb` `/opt/herdr2` 或 `~/.local/share/herdr2`、`~/.local/bin/h2`；macOS `Herdr2.app` / Homebrew；Windows `%LOCALAPPDATA%\\Programs\\herdr2` 与 `%LOCALAPPDATA%\\Programs\\h2`）。",
        "3. 从 GitHub Releases 下载与本机系统/架构匹配的最新压缩包/安装包并校验 SHA-256；先用 `h2 update --from <file>`（或 `h2 server restart`）无中断更新本机 `herdr2-core`，并运行 `h2 skill sync` 同步更新本机已安装的配套 Skill，再在磁盘上安全替换/安装 `herdr2` 桌面端文件（如需管理员密码/sudo 请清晰提示或引导）。",
        ...(remoteSummaryLines.length > 0
          ? ["4. 本机 `herdr2-core` 更新完成后，对可达的 SSH 远程主机执行 `h2 machine update <主机名>` 同步更新（会同步刷新远端已安装的配套 Skill），并报告不可达主机的 `needs_update` 原因。"]
          : []),
        `${remoteSummaryLines.length > 0 ? "5" : "4"}. 最后运行 \`herdr2 --version\`、\`h2 --version\`、\`h2 server status\`${remoteSummaryLines.length > 0 ? " 与 `h2 machine ls`" : ""} 验证更新结果；全部验证通过后再提示我重启 Herdr2 桌面窗口以加载新版本界面。`,
      ].join("\n");
    }

    return [
      "Manual Herdr2 update failed or I want local AI assistance to download and install the latest release from GitHub:",
      "",
      "Current environment:",
      `- Platform: ${platform}`,
      `- Local Herdr2: ${app.version ? fmtVer(app.version) : "unknown"}${app.ready ? ` (available: ${fmtVer(app.ready)})` : ""}`,
      `- Local herdr2-core: ${core.version ? fmtVer(core.version) : "unknown"}${core.ready ? ` (available: ${fmtVer(core.ready)})` : ""}${core.managed ? ` (managed by ${core.managed})` : ""}`,
      `- Configured releases URL: ${releasesUrl}`,
      "- Official GitHub Releases: https://github.com/herdr2/herdr2/releases (rolling OTA tag: ota-latest)",
      ...(remoteSummaryLines.length > 0 ? ["", "Configured SSH remote hosts:", ...remoteSummaryLines] : []),
      ...(errors.length > 0 ? ["", "Failure / error context:", ...errors.map((e) => `- ${e}`)] : []),
      "",
      "CRITICAL — do not interrupt this session mid-update:",
      "- You are running inside a detached `herdr2-core` PTY holder: always update `herdr2-core` with `h2 update --from <file>` or `h2 server restart` (which hands over live holders without killing you). NEVER run `h2 server stop`, `pkill -f herdr2`, `killall herdr2`, or `taskkill /IM herdr2*` (which can kill `herdr2-core` / its holder and abort this session halfway).",
      "- Do NOT restart or kill the `herdr2` desktop GUI mid-update. On Windows, prefer updating from the release archive by renaming `herdr2.exe` -> `herdr2.exe.old` in place (or staging `.new` + `.new.json`) rather than running an interactive `.exe` installer that force-closes running Herdr2 processes.",
      "- Finish all download, verification, local `herdr2-core` update, SSH remote sync, local `herdr2` file replacement, and version checks BEFORE asking me to restart the Herdr2 window.",
      "",
      "Please follow these steps:",
      "1. If there are errors above (proxy, TLS, permissions, GitHub connectivity), diagnose them and fetch the latest release metadata from https://github.com/herdr2/herdr2/releases (including `ota-latest` manifests `app-latest.json` / `core-latest.json`, GitHub API, or `gh release`).",
      "2. Inspect how `herdr2` and `herdr2-core` (`h2`) are currently installed on this machine (e.g. `/opt/herdr2` `.deb` vs `~/.local/share/herdr2` & `~/.local/bin/h2`, macOS `Herdr2.app`/Homebrew, Windows `%LOCALAPPDATA%\\Programs\\herdr2` & `%LOCALAPPDATA%\\Programs\\h2`).",
      "3. Download the matching OS/architecture release from GitHub, verify SHA-256, update local `herdr2-core` safely via `h2 update --from <file>` / `h2 server restart`, run `h2 skill sync` to refresh already-installed companion skills, and update local `herdr2` files on disk without restarting the GUI mid-update (prompt clearly if elevation/sudo is needed).",
      ...(remoteSummaryLines.length > 0
        ? ["4. After local `herdr2-core` is updated, sync reachable SSH remote hosts via `h2 machine update <name>` (which also refreshes already-installed remote skills) and report any unreachable `needs_update` hosts."]
        : []),
      `${remoteSummaryLines.length > 0 ? "5" : "4"}. Verify with \`herdr2 --version\`, \`h2 --version\`, \`h2 server status\`${remoteSummaryLines.length > 0 ? ", and `h2 machine ls`" : ""}, and only after everything succeeds, let me know to restart the Herdr2 window.`,
    ].join("\n");
  }

  ensureAiUpdateSelection() {
    const options = this.aiUpdateAgentOptions();
    const localProbed = this.localProbedAgents();
    const curId = this.canonicalFixAgentId(this.aiUpdateAgent);
    let chosen = curId ? options.find((o) => o.id === curId) : null;
    if (!chosen) {
      chosen = options.find((o) => o.ready) ?? options.find((o) => o.installed) ?? options[0] ?? null;
      this.aiUpdateAgent = chosen?.id ?? null;
      this.aiUpdateModel = null;
    } else if (this.aiUpdateAgent !== chosen.id) {
      this.aiUpdateAgent = chosen.id;
    }
    const agent = chosen?.id ?? null;
    const model = String(this.aiUpdateModel ?? "").trim() || null;
    const disabledReason = agent ? this.droveFixEffortDisabledReason(agent, model) : null;
    if (agent && this.aiUpdateEffort && !disabledReason) {
      this.aiUpdateEffort = this.clampEffortFor(agent, model, this.aiUpdateEffort, localProbed);
    }
    return {
      agent,
      model,
      effort: this.aiUpdateEffort ?? null,
      disabledReason,
      option: chosen,
      options,
      localProbed,
    };
  }

  setAiUpdateAgent(agentId, cx) {
    const next = this.canonicalFixAgentId(agentId);
    if (!next) return;
    this.aiUpdateAgent = next;
    this.aiUpdateModel = null;
    if (this.aiUpdateEffort && !this.droveFixEffortDisabledReason(next, null)) {
      this.aiUpdateEffort = this.clampEffortFor(next, null, this.aiUpdateEffort, this.localProbedAgents());
    }
    cx?.notify?.();
  }

  setAiUpdateModel(modelId, cx) {
    const next = String(modelId ?? "").trim() || null;
    this.aiUpdateModel = next;
    const agent = this.canonicalFixAgentId(this.aiUpdateAgent);
    if (agent && this.aiUpdateEffort && !this.droveFixEffortDisabledReason(agent, next)) {
      this.aiUpdateEffort = this.clampEffortFor(agent, next, this.aiUpdateEffort, this.localProbedAgents());
    }
    cx?.notify?.();
  }

  setAiUpdateEffort(effort, cx) {
    const raw = String(effort ?? "").trim().toLowerCase();
    if (!raw || raw === "auto" || raw === "default") {
      this.aiUpdateEffort = null;
    } else {
      const agent = this.canonicalFixAgentId(this.aiUpdateAgent);
      this.aiUpdateEffort = agent
        ? this.clampEffortFor(agent, this.aiUpdateModel, raw, this.localProbedAgents())
        : raw;
    }
    cx?.notify?.();
  }

  loadAiUpdateCatalog(cx) {
    if ((typeof checking === "function" && checking()) || this.aiUpdateCatalogLoading) return;
    const needKnown = typeof knownAgents === "function" && !Array.isArray(this.droveFixKnownAgents?.["__local__"]);
    const hasLocalProbed =
      (Array.isArray(this.aiUpdateLocalProbed) && this.aiUpdateLocalProbed.length > 0) ||
      ((this.droveMachine ?? null) === null && Array.isArray(this.droveData?.agents?.agents) && this.droveData.agents.agents.length > 0);
    const needProbed = !hasLocalProbed && typeof hc === "function";
    if (!needKnown && !needProbed) return;
    this.aiUpdateCatalogLoading = true;
    cx?.notify?.();
    cx.spawn(async (spawnCx) => {
      try {
        await Promise.all([
          needKnown
            ? (async () => {
                try {
                  const list = await knownAgents({});
                  if (Array.isArray(list)) {
                    this.droveFixKnownAgents = { ...(this.droveFixKnownAgents ?? {}), __local__: list };
                  }
                } catch {}
              })()
            : Promise.resolve(),
          needProbed
            ? (async () => {
                try {
                  const snap = await this.droveHcJson(["drove", "probe"], { local: true, timeout: 15000 });
                  if (Array.isArray(snap?.agents) && snap.agents.length > 0) {
                    this.aiUpdateLocalProbed = snap.agents;
                  }
                } catch {}
              })()
            : Promise.resolve(),
        ]);
        this.ensureAiUpdateSelection();
      } finally {
        this.aiUpdateCatalogLoading = false;
        spawnCx?.notify?.();
      }
    });
  }

  openAiUpdatePanel(cx, extraError = null) {
    this.updateModal = null;
    this.aiUpdateOpen = true;
    if (extraError !== undefined) {
      this.aiUpdateExtraError = extraError || null;
    }
    this.ensureAiUpdateSelection();
    this.loadAiUpdateCatalog(cx);
    cx?.notify?.();
  }

  toggleAiUpdatePanel(cx, extraError = null) {
    if (this.aiUpdateOpen) {
      this.aiUpdateOpen = false;
      if (typeof this.picking === "string" && this.picking.startsWith("drove:ai-update-")) {
        this.picking = null;
        this.droveModelSubmit = null;
      }
      cx?.notify?.();
      return;
    }
    this.openAiUpdatePanel(cx, extraError);
  }

  launchAiUpdate(cx, extraError = null, override = null) {
    if (this.aiUpdateLaunching) return;
    const u = typeof updates === "function" ? updates() : {};
    const effectiveError = extraError !== null && extraError !== undefined ? extraError : this.aiUpdateExtraError;
    const selectedAgent = override?.agent !== undefined ? override.agent : this.aiUpdateAgent;
    const selectedModel = override?.model !== undefined ? override.model : this.aiUpdateModel;
    const selectedEffort = override?.effort !== undefined ? override.effort : this.aiUpdateEffort;
    this.updateModal = null;
    this.aiUpdateLaunching = true;
    cx?.notify?.();
    cx.spawn(async (spawnCx) => {
      try {
        const picked = await this.resolveFixAgentLaunch(null, [], selectedAgent);
        if (!picked) {
          notify({
            title: this.tr("No local CLI AI agent found", "未找到本机可用的 AI Agent"),
            body: this.tr(
              "Install at least one local CLI agent (e.g. claude, agy, codex, gemini, opencode) to use AI Update.",
              "请先在本机安装至少一个可用的 CLI AI Agent（如 claude、agy、codex、gemini、opencode）后再使用 AI 更新。",
            ),
            level: "error",
          });
          return;
        }
        const pickedId = this.canonicalFixAgentId(picked.id || selectedAgent || picked.name);
        let cleanModel = pickedId === "zcode" ? null : (String(selectedModel ?? "").trim() || null);
        const effortDisabled = Boolean(this.droveFixEffortDisabledReason(pickedId, cleanModel));
        let cleanEffort = !effortDisabled && selectedEffort
          ? this.clampEffortFor(pickedId, cleanModel, selectedEffort, this.localProbedAgents())
          : null;
        if (pickedId === "cursor") {
          const resolvedCursor = this.resolveCursorCliModelForLaunch(cleanModel, cleanEffort, this.localProbedAgents());
          cleanModel = resolvedCursor.model;
          cleanEffort = resolvedCursor.effort;
        }
        const prompt = this.buildAiUpdatePrompt(u, effectiveError);
        const tabName = this.tr("AI Update Herdr2", "AI 更新 Herdr2");
        const localSpace = this.targetHomeSpaceForMachine(null);
        const baseLaunchArgs = {
          folder: this.defaultHomeFolderForMachine(null),
          argv: picked.argv,
          task: prompt,
        };
        if (cleanModel) baseLaunchArgs.model = cleanModel;
        if (cleanEffort) baseLaunchArgs.effort = cleanEffort;
        if (localSpace) {
          run("tab.new", { space: localSpace.id, name: tabName, ...baseLaunchArgs });
        } else {
          run("space.new", { ...baseLaunchArgs, tab: tabName });
        }
        this.aiUpdateOpen = false;
        this.aiUpdateExtraError = null;
        this.close();
        const metaParts = [cleanModel, cleanEffort ? this.effortLabel(cleanEffort) : null].filter(Boolean);
        const metaEn = metaParts.length > 0 ? ` (${metaParts.join(" · ")})` : "";
        const metaZh = metaParts.length > 0 ? `（${metaParts.join(" · ")}）` : "";
        notify({
          title: this.tr(`Launched ${picked.name}${metaEn} for AI Update`, `已启动 ${picked.name}${metaZh} 执行 AI 更新`),
          body: this.tr(
            "Downloading and updating Herdr2 / herdr2-core from GitHub in a persistent local workspace tab.",
            "正在本机持久化终端标签页中从 GitHub 获取并更新最新版本。",
          ),
        });
      } catch (e) {
        notify({
          title: this.tr("Failed to start AI Update", "启动 AI 更新失败"),
          body: String(e?.message ?? e),
          level: "error",
        });
      } finally {
        this.aiUpdateLaunching = false;
        spawnCx.notify();
      }
    });
  }

  buildUpdateModal(u) {
    const app = u?.app ?? {};
    const ui = u?.ui ?? {};
    const core = u?.core ?? {};
    const avail = u?.available ?? null;
    const items = Array.isArray(avail?.items) ? avail.items : [];
    const fallbackItems = [];
    if (items.length === 0) {
      if (app.ready) fallbackItems.push({ kind: "app", current: app.version, next: app.ready, notes: app.notes });
      else if (ui.ready) fallbackItems.push({ kind: "ui", current: ui.version, next: ui.ready, notes: ui.ready_notes });
      if (core.ready && !core.managed) fallbackItems.push({ kind: "core", current: core.version, next: core.ready, notes: core.notes });
      for (const m of this.updateRemoteMachines(u)) {
        if (m.ready || m.rawState === "needs_update") {
          fallbackItems.push({
            kind: "remote",
            machine: m.name,
            target: m.target,
            current: m.version,
            next: m.ready ?? (core.ready || core.version || null),
            state: m.state,
            unreachable: m.unreachable,
          });
        }
      }
    }
    const list = items.length > 0 ? items : fallbackItems;
    const nextVersion = avail?.version ?? list[0]?.next ?? null;
    if (nextVersion || list.length > 0) {
      return {
        kind: "available",
        version: nextVersion ?? (core.ready || core.version || app.version || ""),
        notes: avail?.notes ?? list[0]?.notes ?? null,
        restart: Boolean(avail?.restart ?? Boolean(app.ready)),
        needsElevation: Boolean(avail?.needs_elevation ?? app.needs_elevation),
        items: list,
      };
    }
    if (u?.last_error) {
      return {
        kind: "error",
        message: String(u.last_error),
        missingUrl: !u?.releases_url,
      };
    }
    return {
      kind: "up_to_date",
      appVersion: app.version ?? "",
      coreVersion: core.version ?? "",
    };
  }

  syncUpdateCheck(cx) {
    if (!this.waitingUpdateCheck) return;
    const u = typeof updates === "function" ? updates() : null;
    if (!u) return;
    const seq = Number(u.check_seq ?? 0);
    if (!u.checking && seq > this.pendingUpdateCheckSeq) {
      this.waitingUpdateCheck = false;
      this.updateModal = this.buildUpdateModal(u);
    }
  }

  renderUpdateModal(t, cx) {
    const m = this.updateModal;
    if (!m) return null;
    const closeModal = (_e, c) => {
      this.updateModal = null;
      this.handle.focus();
      c.stop_propagation();
      c.notify();
    };
    let title = "";
    let badge = null;
    const lines = [];
    if (m.kind === "available") {
      title = this.tr("Update Available", "发现新版本");
      badge = m.version ? `v${String(m.version).replace(/^v/i, "")}` : null;
      for (const item of m.items ?? []) {
        if (item.kind === "remote") {
          const from = item.current ? `${fmtVer(item.current)} → ` : "";
          const to = item.next ? fmtVer(item.next) : "needs_update";
          const suffix = item.unreachable ? this.tr(" (unreachable · needs_update)", "（不可达 · needs_update）") : "";
          lines.push(`${this.tr("Remote", "远程主机")} ${item.machine} (herdr2-core)：${from}${to}${suffix}`);
          continue;
        }
        const label = item.kind === "app"
          ? "Herdr2"
          : item.kind === "ui"
            ? this.tr("UI Bundle", "UI 热更新包")
            : "herdr2-core";
        const from = item.current ? `v${String(item.current).replace(/^v/i, "")} → ` : "";
        const to = `v${String(item.next).replace(/^v/i, "")}`;
        lines.push(`${label}：${from}${to}`);
      }
      if (m.notes) lines.push(String(m.notes));
      if (m.needsElevation) {
        lines.push(
          this.tr(
            "Installed in a system directory — your administrator password will be requested to apply the update.",
            "当前安装在系统目录中，更新时将弹出系统管理员密码验证框以完成替换。",
          ),
        );
      }
      lines.push(
        m.restart
          ? this.tr(
              "Downloading runs in the background; local Herdr2 / herdr2-core, already-installed skills, and all SSH remote hosts will sync automatically while terminals stay alive.",
              "点击更新后将在后台自动更新本机（herdr2 与 herdr2-core）、同步已安装的 Skill 与所有 SSH 远程主机的 herdr2-core，就绪后自动重启切换，现有终端会话不会中断。",
            )
          : this.tr(
              "Applies live in the background and syncs already-installed skills and all SSH remote hosts without restarting Herdr2 or interrupting terminals.",
              "点击更新后将在后台自动免重启生效，并同步更新已安装的 Skill 与所有 SSH 远程主机的 herdr2-core，不会中断现有终端会话。",
            ),
      );
    } else if (m.kind === "error") {
      title = this.tr("Update Check Failed", "检查更新失败");
      lines.push(
        m.missingUrl
          ? this.tr("Please enter a valid update URL first.", "请先填写有效的更新地址。")
          : m.message,
      );
    } else {
      title = this.tr("Already Up to Date", "已是最新版本");
      const cur = [
        m.appVersion ? `Herdr2 ${fmtVer(m.appVersion)}` : null,
        m.coreVersion ? `Core ${fmtVer(m.coreVersion)}` : null,
      ].filter(Boolean).join(" · ");
      lines.push(
        cur
          ? this.tr(`You are running the latest version (${cur}).`, `当前已是最新版本（${cur}）。`)
          : this.tr("You are already running the latest version.", "当前已是最新版本。"),
      );
    }

    const actions = h_flex()
      .w_full()
      .justify_end()
      .gap(8)
      .pt(6);
    if (m.kind === "available") {
      actions
        .child(this.button(t, "st-update-modal-cancel", this.tr("Later", "暂不更新"), closeModal))
        .child(
          this.button(
            t,
            "st-update-modal-ai",
            this.tr("✨ AI Update", "✨ AI 更新"),
            (_e, c) => {
              c.stop_propagation();
              this.openAiUpdatePanel(c, null);
            },
          ),
        )
        .child(
          div()
            .id("st-update-modal-confirm")
            .px(14)
            .py(6)
            .rounded(7)
            .cursor_pointer()
            .bg(t.accent)
            .text_color(t.accent_foreground)
            .text_size(13)
            .font_semibold()
            .hover((s) => s.opacity(0.9))
            .on_click((_e, c) => {
              this.updateModal = null;
              this.handle.focus();
              run("app.update");
              c.stop_propagation();
              c.notify();
            })
            .child(this.tr("Update Now", "立即更新")),
        );
    } else if (m.kind === "error") {
      actions
        .child(this.button(t, "st-update-modal-ok", this.tr("Close", "关闭"), closeModal))
        .child(
          div()
            .id("st-update-modal-ai")
            .px(14)
            .py(6)
            .rounded(7)
            .cursor_pointer()
            .bg(t.accent)
            .text_color(t.accent_foreground)
            .text_size(13)
            .font_semibold()
            .hover((s) => s.opacity(0.9))
            .on_click((_e, c) => {
              c.stop_propagation();
              this.openAiUpdatePanel(c, m.message);
            })
            .child(this.tr("✨ AI Update from GitHub", "✨ AI 从 GitHub 更新")),
        );
    } else {
      actions.child(
        div()
          .id("st-update-modal-ok")
          .px(14)
          .py(6)
          .rounded(7)
          .cursor_pointer()
          .bg(t.accent)
          .text_color(t.accent_foreground)
          .text_size(13)
          .font_semibold()
          .hover((s) => s.opacity(0.9))
          .on_click(closeModal)
          .child(this.tr("OK", "知道了")),
      );
    }

    return div()
      .id("st-update-modal-backdrop")
      .absolute()
      .inset_0()
      .flex()
      .items_center()
      .justify_center()
      .bg("#00000073")
      .on_click(closeModal)
      .child(
        v_flex()
          .id("st-update-modal")
          .w(420)
          .p(20)
          .gap(12)
          .rounded(12)
          .bg(t.surface)
          .border_1()
          .border_color(t.border)
          .shadow_lg()
          .on_click((_e, c) => c.stop_propagation())
          .child(
            h_flex()
              .items_center()
              .gap(8)
              .child(div().text_size(15).font_semibold().text_color(t.foreground).child(title))
              .when(Boolean(badge), (el) =>
                el.child(
                  div()
                    .px(7)
                    .py(2)
                    .rounded(5)
                    .bg(t.raised)
                    .border_1()
                    .border_color(t.border)
                    .text_size(12)
                    .font_semibold()
                    .text_color(t.accent)
                    .child(badge),
                ),
              ),
          )
          .child(
            v_flex()
              .gap(6)
              .children(
                lines.map((line) =>
                  div().text_size(13).line_height("19px").text_color(t.muted).child(line),
                ),
              ),
          )
          .child(actions),
      );
  }

  updates(t, cx) {
    const u = updates();
    const c = u.core ?? {};
    const remoteMachines = this.updateRemoteMachines(u);
    const anyRemoteUpdating = remoteMachines.some((m) => m.updating);
    const busyChecking = Boolean(this.waitingUpdateCheck || u.checking);
    const busyUpdating = Boolean(u.updating || u.app?.updating || u.ui?.applying || c.updating || anyRemoteUpdating);
    const verSummary = [
      u.app?.version ? `Herdr2 ${fmtVer(u.app.version)}` : null,
      c.version ? `Core ${fmtVer(c.version)}` : null,
    ].filter(Boolean).join(" · ");
    const at = (secs) => {
      const d = new Date(secs * 1000);
      return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    };
    const subLine = u.checked
      ? `${verSummary} · ${this.tr(`Last checked ${at(u.checked)}`, `上次检查 ${at(u.checked)}`)}`
      : verSummary;

    const checkBtn = busyChecking
      ? h_flex()
          .id("st-check-updates")
          .px(14)
          .py(6)
          .gap(6)
          .items_center()
          .rounded(7)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .text_size(13)
          .text_color(t.dim)
          .child(svg("mods/workspace-chrome/spinner.svg").size(13).flex_none().spin(900))
          .child(this.tr("Checking…", "检查中…"))
      : busyUpdating
        ? h_flex()
            .id("st-check-updates")
            .px(14)
            .py(6)
            .gap(6)
            .items_center()
            .rounded(7)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_size(13)
            .text_color(t.accent)
            .child(svg("mods/workspace-chrome/spinner.svg").size(13).flex_none().spin(900))
            .child(this.tr("Updating…", "更新中…"))
        : h_flex()
            .id("st-check-updates")
            .px(14)
            .py(6)
            .gap(6)
            .items_center()
            .rounded(7)
            .cursor_pointer()
            .bg(t.accent)
            .text_color(t.accent_foreground)
            .text_size(13)
            .font_semibold()
            .hover((s) => s.opacity(0.9))
            .on_click((_e, c) => this.triggerUpdateCheck(c))
            .child(this.tr("Check for Updates", "检查更新"));

    const updateErr = u.app?.error || u.ui?.error || (!c.managed ? c.error : null) || u.last_error;
    const aiPanelOpen = Boolean(this.aiUpdateOpen);

    const aiUpdateBtn = h_flex()
      .id("st-ai-update")
      .px(12)
      .py(6)
      .gap(5)
      .items_center()
      .rounded(7)
      .border_1()
      .border_color(aiPanelOpen ? t.accent : t.border)
      .bg(aiPanelOpen ? t.accent : t.raised)
      .text_size(13)
      .when(aiPanelOpen, (el) => el.font_semibold())
      .text_color(aiPanelOpen ? t.accent_foreground : t.foreground)
      .tooltip(
        this.tr(
          "Choose a local AI agent, model, and reasoning effort to download and install the latest release from GitHub",
          "选择本机 AI Agent 类型、模型与思考强度，从 GitHub 获取并更新到最新版本",
        ),
      )
      .when(this.aiUpdateLaunching, (el) => el.opacity(0.6))
      .when(!this.aiUpdateLaunching, (el) =>
        el
          .cursor_pointer()
          .when(!aiPanelOpen, (el2) => el2.hover((s) => s.bg(t.hover)))
          .when(aiPanelOpen, (el2) => el2.hover((s) => s.opacity(0.92)))
          .on_click((_e, c) => this.toggleAiUpdatePanel(c, updateErr)),
      )
      .child(
        this.aiUpdateLaunching
          ? svg("mods/workspace-chrome/spinner.svg")
              .size(13)
              .flex_none()
              .text_color(aiPanelOpen ? t.accent_foreground : t.foreground)
              .spin(900)
          : svg(I("sparkles")).size(13).flex_none().text_color(aiPanelOpen ? t.accent_foreground : t.accent),
      )
      .child(this.aiUpdateLaunching ? this.tr("Launching AI…", "启动 AI 中…") : this.tr("AI Update", "AI 更新"));

    const renderAiUpdatePanel = () => {
      const sel = this.ensureAiUpdateSelection();
      const curAgent = sel.agent ?? "";
      const curOpt = sel.option;
      const curModel = sel.model;
      const curEffort = sel.effort;
      const effortDisabledReason = sel.disabledReason;
      const disableEffort = Boolean(effortDisabledReason);
      const isZcode = curAgent === "zcode";
      const agentItems = sel.options.map((o) => ({
        value: o.id,
        label: o.name && o.name !== o.id ? `${o.name} (${o.id})` : o.id,
        icon: o.id,
        dot: Boolean(o.installed),
        dotColor: !o.installed ? t.dim : o.probeError ? t.working : t.ansi2,
        meta: !o.installed
          ? this.tr("missing", "未安装")
          : o.probeError
            ? this.tr("ACP err · CLI", "ACP 异常 · CLI")
            : o.models.length > 0
              ? `${o.models.length} ${this.tr("models", "模型")}`
              : "✓",
        metaColor: o.probeError ? t.working : t.dim,
        tooltip: o.probeError ? this.formatAgentProbeError(o.probeError) : undefined,
        active: curAgent === o.id,
      }));
      const models = curOpt?.models?.length
        ? curOpt.models
        : (curAgent ? this.probedModelsFromList(sel.localProbed, curAgent) : []);
      const modelItems = [
        { value: null, label: this.tr("Auto (agent default)", "自动（Agent 默认）"), active: !curModel },
        ...models.map((m) => ({
          value: m.id,
          label: m.label,
          active: curModel === m.id,
        })),
      ];
      if (curModel && !models.some((m) => m.id === curModel)) {
        modelItems.splice(1, 0, {
          value: curModel,
          label: curModel,
          meta: this.tr("custom", "自定义"),
          active: true,
        });
      }
      const efforts = curAgent ? this.supportedEffortsFor(curAgent, curModel, sel.localProbed) : ["low", "medium", "high"];
      const effortChoices = [null, ...efforts];
      const canLaunch = Boolean(curOpt && !this.aiUpdateLaunching);
      const footerHint = isZcode
        ? this.tr(
            "Note: ZCode ignores startup --model/--effort flags; select model inside its TUI.",
            "提示：ZCode 不支持启动参数传入模型与思考强度，进入后需在 TUI 内切换。",
          )
        : curAgent === "cursor" && !curModel
          ? this.tr(
              "Tip: Select a specific Cursor model to apply custom reasoning effort.",
              "提示：Cursor 需先指定具体模型才能生效自定义思考强度。",
            )
          : this.tr(
              "Runs in a persistent local PTY tab (survives GUI / h2 server restart); updates files & remotes first and only asks to restart Herdr2 at the end.",
              "在本机持久化 PTY 标签页中运行（不因桌面端或 h2 server restart 中断）；完成全部更新与校验后才提示重启窗口。",
            );

      return v_flex()
        .id("st-ai-update-panel")
        .w_full()
        .px(12)
        .py(10)
        .gap(8)
        .rounded(8)
        .border_1()
        .border_color(t.accent)
        .bg(t.background)
        .child(
          h_flex()
            .w_full()
            .items_center()
            .justify_between()
            .gap(8)
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .child(svg(I("sparkles")).size(13).flex_none().text_color(t.accent))
                .child(
                  div()
                    .text_size(12.5)
                    .font_semibold()
                    .text_color(t.foreground)
                    .child(
                      this.tr(
                        "AI Update · Choose Agent, Model & Reasoning Effort",
                        "AI 更新 · 选择 Agent 类型、模型与思考强度",
                      ),
                    ),
                ),
            )
            .child(
              div()
                .id("st-ai-update-close")
                .px(6)
                .py(1)
                .rounded(4)
                .cursor_pointer()
                .text_size(12)
                .text_color(t.dim)
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_click((_e, c) => this.toggleAiUpdatePanel(c))
                .child("×"),
            ),
        )
        .child(
          h_flex()
            .w_full()
            .gap(6)
            .items_center()
            .flex_wrap()
            .child(
              this.drovePicker(
                t,
                "ai-update-agent",
                curOpt ? (curOpt.name || curOpt.id) : this.tr("Select agent…", "选择 Agent…"),
                agentItems,
                (agent, c) => this.setAiUpdateAgent(agent, c),
                {
                  width: 148,
                  popupWidth: 290,
                  icon: curAgent || undefined,
                  dot: curOpt ? Boolean(curOpt.installed) : undefined,
                  dotColor: !curOpt ? undefined : !curOpt.installed ? t.dim : curOpt.probeError ? t.working : t.ansi2,
                  tooltip: this.tr("Choose which local CLI agent to launch for AI Update", "选择用于执行 AI 更新的本机 Agent 类型"),
                },
                cx,
              ),
            )
            .child(
              this.drovePicker(
                t,
                "ai-update-model",
                curModel || this.tr("Auto (default)", "自动（默认）"),
                modelItems,
                (model, c) => this.setAiUpdateModel(model, c),
                {
                  flex1: true,
                  width: 150,
                  popupWidth: 360,
                  allowCustom: true,
                  tooltip: isZcode
                    ? effortDisabledReason
                    : this.tr("Choose model or type a custom model ID", "选择模型或输入自定义模型 ID"),
                },
                cx,
              ),
            )
            .child(
              h_flex()
                .id("st-ai-update-eff-group")
                .w(effortChoices.length >= 5 ? 196 : 164)
                .min_w(effortChoices.length >= 5 ? 196 : 164)
                .h(28)
                .flex_none()
                .p(2)
                .gap(1)
                .items_center()
                .rounded(7)
                .border_1()
                .border_color(t.border)
                .bg(t.surface)
                .when(disableEffort, (el) => el.opacity(0.35).tooltip(effortDisabledReason))
                .when(!disableEffort, (el) => el.tooltip(this.tr("Reasoning effort level", "选择模型思考强度")))
                .children(
                  effortChoices.map((eff) => {
                    const effKey = eff ?? "auto";
                    const active = !disableEffort && (eff === null ? !curEffort : curEffort === eff);
                    const label = eff === null ? this.tr("Auto", "默认") : this.effortLabel(eff);
                    return div()
                      .id(`st-ai-update-eff-${effKey}`)
                      .flex_1()
                      .min_w(0)
                      .h(22)
                      .px(3)
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
                          .on_click((_e, c) => this.setAiUpdateEffort(eff, c)),
                      )
                      .child(label);
                  }),
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
              div()
                .flex_1()
                .min_w(0)
                .text_size(11.5)
                .line_height("16px")
                .text_color(t.dim)
                .child(footerHint),
            )
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .child(
                  this.button(t, "st-ai-update-cancel", this.tr("Cancel", "取消"), (_e, c) =>
                    this.toggleAiUpdatePanel(c),
                  ),
                )
                .child(
                  h_flex()
                    .id("st-ai-update-launch")
                    .px(12)
                    .py(5)
                    .gap(5)
                    .rounded(6)
                    .border_1()
                    .border_color(t.accent)
                    .bg(t.accent)
                    .items_center()
                    .text_size(12)
                    .font_semibold()
                    .text_color(t.accent_foreground)
                    .when(!canLaunch, (b) => b.opacity(0.55))
                    .when(canLaunch, (b) =>
                      b
                        .cursor_pointer()
                        .hover((s) => s.opacity(0.9))
                        .on_click((_e, c) => this.launchAiUpdate(c, this.aiUpdateExtraError ?? updateErr)),
                    )
                    .when(this.aiUpdateLaunching, (b) =>
                      b.child(
                        svg("mods/workspace-chrome/spinner.svg")
                          .size(11)
                          .text_color(t.accent_foreground)
                          .spin(900),
                      ),
                    )
                    .child(
                      this.aiUpdateLaunching
                        ? this.tr("Launching…", "启动中…")
                        : this.tr("Start AI Update", "启动 AI 更新"),
                    ),
                ),
            ),
        );
    };

    const headerActions = h_flex()
      .gap(8)
      .items_center()
      .child(aiUpdateBtn)
      .child(checkBtn);

    const statusBadge = (id, label, color) =>
      div()
        .id(id)
        .px(7)
        .py(2)
        .rounded(5)
        .bg(alpha(color, "1f") ?? t.raised)
        .border_1()
        .border_color(alpha(color, "55") ?? t.border)
        .text_size(11.5)
        .font_semibold()
        .text_color(color)
        .child(label);

    const localNeedsUpdate = Boolean(u.app?.ready || u.ui?.ready || (c.ready && !c.managed));
    const localUpdating = Boolean(u.app?.updating || u.ui?.applying || c.updating);
    const localDetailParts = [
      u.app?.version
        ? `Herdr2 ${fmtVer(u.app.version)}${u.app.ready ? ` → ${fmtVer(u.app.ready)}` : ""}`
        : null,
      c.version
        ? `herdr2-core ${fmtVer(c.version)}${c.ready && !c.managed ? ` → ${fmtVer(c.ready)}` : ""}${c.managed ? ` (${c.managed})` : ""}`
        : `herdr2-core ${this.tr("unknown", "版本未知")}`,
    ].filter(Boolean).join(" · ");

    const localHostRow = h_flex()
      .id("st-update-host-local")
      .w_full()
      .px(12)
      .py(10)
      .gap(10)
      .items_center()
      .when(remoteMachines.length > 0, (el) => el.border_b(1).border_color(t.border))
      .child(
        localUpdating
          ? svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)
          : div().w(7).h(7).flex_none().rounded(4).bg(localNeedsUpdate ? t.working : t.ansi2),
      )
      .child(
        v_flex()
          .flex_1()
          .min_w(0)
          .gap(2)
          .child(div().text_size(13.5).font_semibold().text_color(t.foreground).child(this.tr("This computer", "本机")))
          .child(div().text_size(12).text_color(t.dim).child(localDetailParts)),
      )
      .child(
        div()
          .flex_none()
          .child(
            localUpdating
              ? statusBadge("st-update-status-local", this.tr("Updating…", "更新中…"), t.accent)
              : localNeedsUpdate
                ? statusBadge("st-update-status-local", "needs_update", t.working)
                : statusBadge("st-update-status-local", this.tr("Up to date", "最新"), t.ansi2),
          ),
      );

    const renderRemoteRow = (m, idx) => {
      const isLast = idx === remoteMachines.length - 1;
      const verText = m.version
        ? `herdr2-core ${fmtVer(m.version)}${m.ready ? ` → ${fmtVer(m.ready)}` : ""}`
        : `herdr2-core ${this.tr("version unknown", "版本未知")}`;
      const detailParts = [m.target || null, verText];
      if (m.unreachable) {
        detailParts.push(
          m.rawState === "needs_login"
            ? this.tr("SSH login required", "需要 SSH 登录认证")
            : this.tr("unreachable", "不可达"),
        );
      }
      const dotColor = m.needsUpdate ? t.blocked : m.state === "connected" ? t.ansi2 : t.working;
      const badgeEl = m.updating
        ? statusBadge(`st-update-status-${m.name}`, this.tr("Updating…", "更新中…"), t.accent)
        : m.needsUpdate
          ? statusBadge(`st-update-status-${m.name}`, "needs_update", t.blocked)
          : m.state === "connected"
            ? statusBadge(`st-update-status-${m.name}`, this.tr("Up to date", "最新"), t.ansi2)
            : statusBadge(`st-update-status-${m.name}`, m.state, t.working);
      const errDetail = m.error || (m.unreachable && m.why ? m.why : null);
      return v_flex()
        .id(`st-update-host-${m.name}`)
        .w_full()
        .px(12)
        .py(10)
        .gap(4)
        .when(!isLast, (el) => el.border_b(1).border_color(t.border))
        .child(
          h_flex()
            .w_full()
            .gap(10)
            .items_center()
            .child(
              m.updating
                ? svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)
                : div().w(7).h(7).flex_none().rounded(4).bg(dotColor),
            )
            .child(
              v_flex()
                .flex_1()
                .min_w(0)
                .gap(2)
                .child(div().text_size(13.5).font_semibold().text_color(t.foreground).child(m.name))
                .child(div().text_size(12).text_color(t.dim).child(detailParts.filter(Boolean).join(" · "))),
            )
            .child(div().flex_none().child(badgeEl)),
        )
        .when(Boolean(errDetail), (el) =>
          el.child(
            div()
              .pl(17)
              .text_size(11.5)
              .line_height("16px")
              .text_color(t.blocked)
              .child(String(errDetail)),
          ),
        );
    };

    const hostsSection = v_flex()
      .id("st-update-hosts")
      .w_full()
      .gap(8)
      .child(
        div()
          .text_size(13.5)
          .font_semibold()
          .text_color(t.foreground)
          .child(this.tr("Hosts & Versions", "本机与远程主机版本")),
      )
      .child(
        v_flex()
          .w_full()
          .rounded(8)
          .border_1()
          .border_color(t.border)
          .bg(t.background)
          .child(localHostRow)
          .children(remoteMachines.map(renderRemoteRow)),
      )
      .when(remoteMachines.length === 0, (el) =>
        el.child(
          div()
            .text_size(12)
            .text_color(t.dim)
            .child(
              this.tr(
                "No SSH remote hosts configured. Any added SSH hosts will appear here and sync automatically on update.",
                "暂未配置 SSH 远程主机。添加 SSH 主机后将在此展示版本，并在点击更新时自动同步更新。",
              ),
            ),
        ),
      );

    return v_flex()
      .w_full()
      .gap(16)
      .child(this.heading(t, this.tr("Updates", "更新")))
      .child(
        this.row(
          t,
          this.tr("Version", "当前版本"),
          subLine || this.tr("Ready to check for updates.", "点击右侧按钮检查更新。"),
          headerActions,
        ),
      )
      .when(Boolean(updateErr), (el) =>
        el.child(
          h_flex()
            .id("st-update-error")
            .w_full()
            .px(12)
            .py(10)
            .gap(12)
            .items_center()
            .rounded(7)
            .bg(alpha(t.blocked ?? "#ef4444", "1a") ?? t.raised)
            .border_1()
            .border_color(alpha(t.blocked ?? "#ef4444", "55") ?? t.border)
            .child(
              div()
                .flex_1()
                .min_w(0)
                .text_size(12.5)
                .line_height("18px")
                .text_color(t.blocked ?? "#ef4444")
                .child(String(updateErr)),
            )
            .child(
              h_flex()
                .id("st-ai-update-retry")
                .flex_none()
                .px(10)
                .py(5)
                .gap(5)
                .items_center()
                .rounded(6)
                .bg(t.accent)
                .text_color(t.accent_foreground)
                .text_size(12)
                .font_semibold()
                .cursor_pointer()
                .hover((s) => s.opacity(0.9))
                .on_click((_e, c) => this.openAiUpdatePanel(c, updateErr))
                .child(svg(I("sparkles")).size(12).flex_none())
                .child(this.tr("AI Update via GitHub", "用 AI 从 GitHub 更新")),
            ),
        ),
      )
      .when(aiPanelOpen, (el) => el.child(renderAiUpdatePanel()))
      .child(hostsSection)
      .child(
        v_flex()
          .gap(8)
          .pt(4)
          .child(div().text_size(13.5).font_semibold().text_color(t.foreground).child(this.tr("Update URL", "更新地址")))
          .child(
            div()
              .id("st-releases-url")
              .w_full()
              .px(10)
              .py(4)
              .rounded(7)
              .border_1()
              .border_color(t.border)
              .bg(t.background)
              .child(Input.new(this.releasesUrlInput).h(26).text_size(13).text_color(t.foreground)),
          )
          .child(
            div()
              .text_size(12.5)
              .line_height("18px")
              .text_color(t.dim)
              .child(
                this.tr(
                  "Leave blank to use the built-in GitHub OTA feed. Herdr2 checks automatically every hour and shows an Update button in the top-right corner to sync this computer and all SSH remote hosts together.",
                  "留空则使用内置 GitHub 热更源。Herdr2 每小时自动检查一次更新，发现新版本或远程主机需同步时会在右上角显示更新按钮，一键同步更新本机与所有 SSH 远程主机。",
                ),
              ),
          ),
      );
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

  syncExternalKeysChange(cx) {
    const evt = shared("keys.reassigned");
    const seq = Number(evt?.seq ?? 0);
    if (seq <= this.lastKeysReassignedSeq) return;
    this.lastKeysReassignedSeq = seq;
    const isMac = this.isMacPlatform();
    const list = typeof keys === "function" ? keys() : [];
    this.keysDoc = parseKeysDoc(null, list, isMac);
    this.keysRaw = null;
    if (evt?.from && evt?.to) {
      const fromLabel = formatChord(String(evt.from), isMac);
      const toLabel = formatChord(String(evt.to), isMac);
      this.keysNotice = this.tr(
        `${fromLabel} is occupied by another app as a global hotkey — switched "${findActionLabel(String(evt.action ?? "prompt.polish"), false)}" to ${toLabel}.`,
        `检测到 ${fromLabel} 已被其他软件占用为全局热键，已自动将「${findActionLabel(String(evt.action ?? "prompt.polish"), true)}」切换为 ${toLabel}。`,
      );
      this.keysError = null;
    } else if (evt?.from) {
      const fromLabel = formatChord(String(evt.from), isMac);
      this.keysError = this.tr(
        `${fromLabel} is occupied by another app as a global hotkey. Click the shortcut badge below to assign a free key combination.`,
        `检测到 ${fromLabel} 已被其他软件占用为全局热键，请点击下方快捷键徽章重新绑定未占用的组合键。`,
      );
    }
    if (!(typeof checking === "function" && checking())) {
      cx.spawn(async (reloadCx) => {
        await this.loadKeys();
        reloadCx.notify();
      });
    }
  }

  render(cx) {
    this.syncRecordedShortcut(cx);
    this.syncExternalKeysChange(cx);
    this.syncUpdateCheck(cx);
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
              if (s.id === "machines" || s.id === "updates") cx.spawn(async (cx) => { await this.loadMachines(); cx.notify(); });
              if (s.id === "ai") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await this.loadAi();
                cx.notify();
              });
              if (s.id === "agents") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await this.loadDrove();
                this.loadDroveFixKnownAgents(cx);
                this.maybeAutoProbeDrove(cx);
                cx.notify();
              });
              if (s.id === "drove") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await Promise.all([this.loadDrove(), this.aiStatus ? Promise.resolve() : this.loadAi()]);
                this.maybeAutoProbeDrove(cx);
                cx.notify();
              });
              if (s.id === "skills") cx.spawn(async (cx) => {
                if (this.machineList === null) await this.loadMachines();
                await Promise.all([this.loadSkills(), this.loadAi(), this.droveData ? Promise.resolve() : this.loadDrove()]);
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
      .relative()
      .track_focus(this.handle)
      .when(Boolean(this.picking || this.updateModal), (el) => el.on_action("overlay.close", (_e, cx) => {
        if (this.updateModal) this.updateModal = null;
        else this.picking = null;
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
          if (this.updateModal) this.updateModal = null;
          else if (this.picking) this.picking = null;
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
                      : this.section === "agents"
                        ? this.agents(t, cx)
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
      )
      .when(Boolean(this.updateModal), (el) => el.child(this.renderUpdateModal(t, cx)));
  }
}
