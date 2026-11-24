// Pure catalog, formatting, and state helpers for the first-run Herdr2 welcome wizard.

export const PAGES = 4;

export const DEEPSEEK_PRESET = {
  id: "deepseek",
  nameEn: "DeepSeek Official (Recommended)",
  nameZh: "DeepSeek 官方 API（推荐）",
  baseUrl: "https://api.deepseek.com/v1",
  model: "deepseek-chat",
  models: ["deepseek-chat", "deepseek-reasoner"],
  portalUrl: "https://platform.deepseek.com/api_keys",
};

export const AI_PAGE = 3;

export function isAiGateSatisfied(aiStatus, aiTestResult, bypassed = false) {
  if (bypassed) return true;
  if (aiTestResult?.ok === true && Boolean(aiStatus?.configured)) return true;
  return false;
}

export function canAccessPage(targetPage, currentPage, aiReady) {
  const clamped = clampPage(targetPage);
  if (aiReady) return clamped;
  if (clamped > AI_PAGE) return AI_PAGE;
  return clamped;
}

export const LANGUAGES = [
  ["en", "English"],
  ["zh-CN", "简体中文"],
];

export const APPEARANCES = [
  [null, "System", "跟随系统"],
  ["light", "Light", "浅色"],
  ["dark", "Dark", "深色"],
];

export const PANELS = [
  {
    id: "files",
    icon: "files",
    en: "Files",
    zh: "文件",
    aboutEn: "Browse the workspace folder tree with syntax-highlighted file preview.",
    aboutZh: "树形浏览当前项目目录，支持语法高亮只读预览。",
  },
  {
    id: "changes",
    icon: "git-compare",
    en: "Changes",
    zh: "变更",
    aboutEn: "Git branch, recent commits, line-by-line diffs, and a commit box.",
    aboutZh: "查看 Git 分支、最近提交、逐行 diff 并直接提交。",
  },
  {
    id: "checkpoints",
    icon: "clock",
    en: "Checkpoints",
    zh: "快照",
    aboutEn: "Turn-by-turn session and workspace snapshots to diff or restore.",
    aboutZh: "按对话轮次保存会话与工作区快照，支持对比 diff 与一键回滚。",
  },
];

export const SHORTCUT_HIGHLIGHTS = [
  { action: "mod.open:new-menu", en: "New Terminal / Agent", zh: "新建终端 / 智能体", fallbackLinux: "ctrl-alt-n", fallbackMac: "cmd-shift-n" },
  { action: "mod.open:picker", en: "Quick switcher", zh: "快速跳转", fallbackLinux: "ctrl-shift-k", fallbackMac: "cmd-p" },
  { action: "terminal.view:toggle", en: "Chat / terminal view", zh: "切换对话 / 终端", fallbackLinux: "ctrl-shift-m", fallbackMac: "cmd-shift-m" },
  { action: "agent.handoff", en: "Context Handoff", zh: "上下文接力", fallbackLinux: "ctrl-shift-h", fallbackMac: "cmd-shift-h" },
  { action: "prompt.polish", en: "AI Polish prompt", zh: "AI 润色提示词", fallbackLinux: "ctrl-alt-p", fallbackMac: "cmd-alt-p" },
  { action: "mod.open:flash-note", en: "Quick Flash Note", zh: "快速记录闪念", fallbackLinux: "ctrl-alt-f", fallbackMac: "cmd-alt-f" },
];

export function resolveLang(settingsObj, fallback = "en") {
  const raw = settingsObj?.language ?? fallback ?? "en";
  return String(raw).toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

export function clampPage(page) {
  const n = Number(page);
  if (!Number.isFinite(n)) return 1;
  return Math.max(1, Math.min(PAGES, Math.trunc(n)));
}

export function resolveEnabledPanels(saved, hasCheckpoints, defaultIds = PANELS.map((p) => p.id)) {
  if (!Array.isArray(saved)) return defaultIds;
  if (!hasCheckpoints && !saved.includes("checkpoints")) {
    return [...saved, "checkpoints"];
  }
  return saved;
}

export function formatKeyChord(rawKeys, isMac = false) {
  const raw = String(rawKeys ?? "").trim();
  if (!raw) return "";
  const names = isMac
    ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" }
    : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: isMac ? "cmd" : "ctrl", super: "cmd", win: "cmd", meta: "cmd", option: "alt" };
  const specials = {
    enter: isMac ? "↩" : "Enter",
    escape: "Esc",
    tab: "Tab",
    space: "Space",
    backspace: isMac ? "⌫" : "Backspace",
    up: "↑",
    down: "↓",
    left: "←",
    right: "→",
    pageup: "PgUp",
    pagedown: "PgDn",
  };
  return raw
    .split(/\s+/)
    .map((stroke) => {
      const parts = stroke.split("-");
      let key = parts.pop();
      if (key === "") {
        key = "-";
        parts.pop();
      }
      const held = new Set(parts.map((m) => alias[m.toLowerCase()] ?? m.toLowerCase()));
      if (/^[A-Z]$/.test(key)) held.add("shift");
      const low = key.toLowerCase();
      const label = specials[low] ?? (key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + low.slice(1));
      const mods = Object.keys(names)
        .filter((m) => held.has(m))
        .map((m) => names[m]);
      return [...mods, label].join(isMac ? "" : "+");
    })
    .join(" ");
}

export function boundShortcut(keyList, action, isMac = false, fallback = "") {
  const list = Array.isArray(keyList) ? keyList : [];
  const match = list.find((b) => b?.section === "global" && b?.action === action && b?.keys?.trim());
  const raw = match?.keys ?? fallback;
  return raw ? formatKeyChord(raw, isMac) : "";
}

export function normalizeMachines(rawStdout) {
  if (!rawStdout) return [];
  try {
    const parsed = typeof rawStdout === "string" ? JSON.parse(rawStdout) : rawStdout;
    const list = Array.isArray(parsed?.machines) ? parsed.machines : [];
    return list.filter((m) => m && !m.temporary);
  } catch {
    return [];
  }
}

export function summarizeAiStatus(rawStdout) {
  if (!rawStdout) return null;
  try {
    const parsed = typeof rawStdout === "string" ? JSON.parse(rawStdout) : rawStdout;
    if (!parsed || typeof parsed !== "object") return null;
    return {
      configured: Boolean(parsed.configured),
      enabled: parsed.enabled !== false,
      model: typeof parsed.model === "string" ? parsed.model.trim() : "",
      baseUrl: typeof parsed.baseUrl === "string" ? parsed.baseUrl.trim() : "",
    };
  } catch {
    return null;
  }
}

export function agentCustomizePrompt(lang) {
  return lang === "zh-CN"
    ? "阅读 ~/.config/herdr2-app/AGENTS.md，告诉我你可以在 Herdr2 里定制哪些布局或面板。"
    : "Read ~/.config/herdr2-app/AGENTS.md, then tell me what you can customize in Herdr2.";
}

export function fmtVersion(v) {
  const raw = String(v ?? "").trim();
  if (!raw) return "";
  return `v${raw.replace(/^v/i, "")}`;
}

export function findLayoutNode(node, id) {
  if (!node) return null;
  if (node.id === id) return node;
  for (const c of node.children ?? []) {
    const f = findLayoutNode(c, id);
    if (f) return f;
  }
  return null;
}
