// Pure catalog, formatting, and state helpers for the first-run Herdr2 welcome wizard.

export const PAGES = 5;

export const DEEPSEEK_PRESET = {
  id: "deepseek",
  nameEn: "DeepSeek Official",
  nameZh: "DeepSeek 官方",
  baseUrl: "https://api.deepseek.com/v1",
  portalUrl: "https://platform.deepseek.com/api_keys",
};

export const AI_PROVIDERS = [
  { id: "deepseek", en: "DeepSeek Official", zh: "DeepSeek 官方" },
  { id: "custom", en: "Custom (OpenAI / Claude)", zh: "自定义（OpenAI / Claude）" },
];

export const AI_PAGE = 3;
export const AGENTS_PAGE = 4;

export const ACP_PERMISSIONS = [
  { id: "approve-reads", en: "Approve Reads", zh: "批准只读" },
  { id: "approve-all", en: "Approve All", zh: "全部批准" },
  { id: "ask", en: "Ask", zh: "每次询问" },
  { id: "deny-all", en: "Deny All", zh: "全部拒绝" },
];

export function normalizeUrl(url) {
  return String(url ?? "").trim().replace(/\/+$/, "");
}

export function normalizeFetchedModels(rawStdout) {
  if (!rawStdout) return { ok: false, models: [], error: null, latencyMs: 0 };
  try {
    const parsed = typeof rawStdout === "string" ? JSON.parse(rawStdout) : rawStdout;
    if (!parsed || typeof parsed !== "object") {
      return { ok: false, models: [], error: null, latencyMs: 0 };
    }
    const rawList = Array.isArray(parsed.models)
      ? parsed.models
      : Array.isArray(parsed.availableModels)
        ? parsed.availableModels
        : [];
    const seen = new Set();
    const models = [];
    for (const item of rawList) {
      const id = String(item ?? "").trim();
      if (id && !seen.has(id)) {
        seen.add(id);
        models.push(id);
      }
    }
    const err =
      typeof parsed.error === "string"
        ? parsed.error
        : typeof parsed.error?.message === "string"
          ? parsed.error.message
          : typeof parsed.message === "string"
            ? parsed.message
            : null;
    return {
      ok: Boolean(parsed.ok) && models.length > 0,
      models,
      error: err,
      latencyMs: Number(parsed.latencyMs ?? parsed.latency_ms ?? 0) || 0,
    };
  } catch {
    return { ok: false, models: [], error: null, latencyMs: 0 };
  }
}

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
    const availableModels = Array.isArray(parsed.availableModels)
      ? parsed.availableModels.map((m) => String(m ?? "").trim()).filter(Boolean)
      : [];
    const protocol = typeof parsed.protocol === "string" && parsed.protocol.trim() ? parsed.protocol.trim() : null;
    return {
      configured: Boolean(parsed.configured),
      enabled: parsed.enabled !== false,
      model: typeof parsed.model === "string" ? parsed.model.trim() : "",
      baseUrl: typeof parsed.baseUrl === "string" ? parsed.baseUrl.trim() : "",
      keyMasked: typeof parsed.keyMasked === "string" && parsed.keyMasked.trim() ? parsed.keyMasked.trim() : null,
      availableModels,
      ...(protocol ? { protocol } : {}),
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

export function canonicalAgentId(raw) {
  const id = String(raw ?? "").trim().toLowerCase();
  if (!id) return "";
  if (id === "antigravity") return "agy";
  if (id === "cursor-agent") return "cursor";
  if (id === "dsh-tui") return "dsh";
  return id;
}

export function formatShortProbeError(raw) {
  const text = String(raw ?? "").trim();
  if (!text) return "";
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  if (lines.length === 0) return "";
  const headline = lines[0];
  const stderrIdx = lines.findIndex((l) => l.toLowerCase() === "stderr:");
  const stderrLine = stderrIdx >= 0 ? lines.slice(stderrIdx + 1).find(Boolean) : null;
  let combined = stderrLine && !headline.includes(stderrLine) ? `${headline} · ${stderrLine}` : headline;
  if (combined.length > 120) combined = `${combined.slice(0, 119).trimEnd()}…`;
  return combined;
}

export function normalizeAdapterStatus(rawStdout) {
  if (!rawStdout) return null;
  try {
    const parsed = typeof rawStdout === "string" ? JSON.parse(rawStdout) : rawStdout;
    if (!parsed || typeof parsed !== "object") return null;
    const adapters = Array.isArray(parsed.adapters) ? parsed.adapters : [];
    return {
      adaptersDir: String(parsed.adapters_dir ?? ""),
      autoInstall: parsed.auto_install !== false,
      registryUrl: String(parsed.registry_url ?? ""),
      customRegistryUrl: typeof parsed.custom_registry_url === "string" && parsed.custom_registry_url.trim()
        ? parsed.custom_registry_url.trim()
        : null,
      defaultPermissions: String(parsed.default_permissions || "approve-reads").trim() || "approve-reads",
      injectMcp: parsed.inject_mcp !== false,
      configPath: String(parsed.config_path ?? ""),
      adapters,
    };
  } catch {
    return null;
  }
}

export function normalizeAgentProbe(rawStdout) {
  if (!rawStdout) return null;
  try {
    const parsed = typeof rawStdout === "string" ? JSON.parse(rawStdout) : rawStdout;
    if (!parsed || typeof parsed !== "object") return null;
    const agents = Array.isArray(parsed.agents) ? parsed.agents : [];
    return {
      fullProbe: Boolean(parsed.fullProbe ?? parsed.full_probe),
      agents,
    };
  } catch {
    return null;
  }
}

const AGENT_DISPLAY_NAMES = {
  claude: "Claude Code",
  codex: "Codex",
  agy: "Antigravity",
  cursor: "Cursor",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  pi: "Pi",
  kimi: "Kimi",
  copilot: "GitHub Copilot",
  hermes: "Hermes",
  zcode: "ZCode",
  trae: "TRAE CLI",
  qwen: "Qwen Code",
  qoder: "Qoder",
  glm: "GLM",
  dsh: "DSH",
  kiro: "Kiro",
  amp: "Amp",
  grok: "Grok",
};

const AGENT_PREF_ORDER = [
  "claude", "codex", "agy", "cursor", "gemini", "opencode", "pi",
  "kimi", "copilot", "hermes", "zcode", "trae", "qwen", "glm", "dsh", "qoder", "kiro", "amp", "grok",
];

export function summarizeDetectedAgents(knownList, probeSnap, adapterStatus) {
  const byId = new Map();
  const ensure = (rawId) => {
    const id = canonicalAgentId(rawId);
    if (!id) return null;
    let rec = byId.get(id);
    if (!rec) {
      rec = {
        id,
        name: AGENT_DISPLAY_NAMES[id] ?? id,
        cliInstalled: false,
        acpSupported: false,
        acpInstalled: false,
        acpError: null,
        modelsCount: 0,
        adapter: null,
      };
      byId.set(id, rec);
    }
    return rec;
  };

  for (const p of Array.isArray(knownList) ? knownList : []) {
    const rec = ensure(p?.key);
    if (!rec) continue;
    if (p?.name) rec.name = p.name;
    if (p?.installed) rec.cliInstalled = true;
  }

  for (const a of Array.isArray(probeSnap?.agents) ? probeSnap.agents : []) {
    const rec = ensure(a?.id);
    if (!rec) continue;
    rec.acpSupported = true;
    if (a?.installed) rec.acpInstalled = true;
    const err = String(a?.error ?? "").trim();
    if (err) rec.acpError = err;
    const count = Array.isArray(a?.models) ? a.models.length : 0;
    if (count > rec.modelsCount) rec.modelsCount = count;
  }

  const adapters = Array.isArray(adapterStatus?.adapters) ? adapterStatus.adapters : [];
  for (const ad of adapters) {
    const rec = ensure(ad?.preset_id);
    if (!rec) continue;
    rec.acpSupported = true;
    rec.adapter = ad;
    if (ad?.companion_installed) rec.cliInstalled = true;
    if (ad?.installed) rec.acpInstalled = true;
  }

  const rank = (id) => {
    const i = AGENT_PREF_ORDER.indexOf(id);
    return i === -1 ? 99 : i;
  };
  const group = (r) => {
    if (r.acpReady) return 0;
    if (r.acpError) return 1;
    if (r.adapterMissing) return 2;
    if (r.cliInstalled || r.acpInstalled || r.adapterInstalling) return 3;
    if (r.adapter) return 4;
    return 5;
  };

  const rows = [...byId.values()]
    .map((r) => {
      const adapterInstalling = r.adapter?.state === "installing";
      const adapterMissing = Boolean(r.adapter && r.adapter.companion_installed && !r.adapter.installed && !adapterInstalling);
      const acpReady = Boolean(r.acpInstalled && !r.acpError);
      const anyInstalled = Boolean(r.cliInstalled || r.acpInstalled || adapterInstalling);
      return { ...r, adapterInstalling, adapterMissing, acpReady, anyInstalled };
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

  const missingAdapters = adapters.filter((a) => a.companion_installed && !a.installed && a.state !== "installing");
  const installingAdapters = adapters.filter((a) => a.state === "installing");
  const managedAdapters = adapters.filter((a) => a.source === "managed");

  return {
    rows,
    installedRows: rows.filter((r) => r.anyInstalled),
    cliCount: rows.filter((r) => r.cliInstalled).length,
    acpReadyCount: rows.filter((r) => r.acpReady).length,
    acpErrorCount: rows.filter((r) => r.acpInstalled && Boolean(r.acpError)).length,
    missingAdapters,
    installingAdapters,
    managedAdapters,
  };
}
