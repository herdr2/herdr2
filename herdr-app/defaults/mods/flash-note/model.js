// Pure data model & target resolution helpers for Flash Notes (闪念).

export const FLASH_NOTES_KEY = "flash_notes.items";
export const FLASH_NOTES_TRASH_KEY = "flash_notes.trash";
export const KNOWN_AGENTS_KEY = "new-menu.agents";
export const AGENT_CATALOG_KEY = "handoff.agentCatalog";
export const EFFORT_ORDER = ["off", "low", "medium", "high", "xhigh", "max"];
export const ACP_PRESETS = new Set([
  "claude",
  "codex",
  "antigravity",
  "agy",
  "gemini",
  "opencode",
  "copilot",
  "cursor",
  "kimi",
  "qwen",
  "pi",
  "hermes",
  "glm",
  "zcode",
  "dsh-tui",
  "dsh",
]);
export const ACP_AGENT_KEY = { antigravity: "agy", "dsh-tui": "dsh", "cursor-agent": "cursor" };

const CURATED_AGENT_CATALOG = {
  claude: {
    rawModels: ["opus", "sonnet", "haiku", "opusplan", "claude-opus-4-7", "claude-sonnet-4-6", "claude-haiku-4-5"],
    defaultModel: "",
    efforts: ["low", "medium", "high", "xhigh", "max"],
    defaultEffort: "",
    modelEfforts: {},
  },
  codex: {
    rawModels: ["gpt-5.4", "gpt-5.3-codex", "gpt-5.2-codex", "o3"],
    defaultModel: "",
    efforts: ["low", "medium", "high", "xhigh"],
    defaultEffort: "",
    modelEfforts: {},
  },
  agy: {
    rawModels: [
      "gemini-4-neon-high",
      "gemini-4-neon-xhigh",
      "gemini-4-neon-medium",
      "gemini-4-neon-low",
      "gemini-4-carbon-high",
      "gemini-4-carbon-medium",
      "gemini-4-carbon-low",
      "gemini-4-argon-high",
      "gemini-4-argon-medium",
      "gemini-4-argon-low",
      "gemini-4-hydrogen-xhigh",
      "gemini-4-hydrogen-high",
      "gemini-4-hydrogen-medium",
      "gemini-4-hydrogen-low",
      "gemini-pro-orchestrator",
      "gemini-pro-orchestrator-neon",
      "gemini-pro-orchestrator-argon",
      "gemini-3.8-flash-high",
      "gemini-3.8-flash-medium",
      "gemini-3.8-flash-low",
      "claude-opus-5-5-high",
      "claude-opus-5-5-medium",
      "claude-opus-5-5-low",
      "claude-sonnet-5-5-high",
      "claude-sonnet-5-5-medium",
      "claude-sonnet-5-5-low",
      "gpt-oss-120b-medium",
    ],
    defaultModel: "",
    efforts: ["low", "medium", "high", "xhigh"],
    defaultEffort: "",
    modelEfforts: {
      "gemini-4-neon": ["low", "medium", "high", "xhigh"],
      "gemini-4-carbon": ["low", "medium", "high"],
      "gemini-4-argon": ["low", "medium", "high"],
      "gemini-4-hydrogen": ["low", "medium", "high", "xhigh"],
      "gemini-3.8-flash": ["low", "medium", "high"],
      "claude-opus-5-5": ["low", "medium", "high"],
      "claude-sonnet-5-5": ["low", "medium", "high"],
    },
  },
  gemini: {
    rawModels: ["gemini-2.5-pro", "gemini-2.5-flash"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
  cursor: {
    rawModels: ["auto", "claude-4.6-sonnet-thinking", "gpt-5.4", "gemini-2.5-pro"],
    defaultModel: "",
    efforts: ["low", "medium", "high", "xhigh"],
    defaultEffort: "",
    modelEfforts: {},
  },
  kimi: {
    rawModels: ["kimi-k2.5", "kimi-for-coding"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
  qwen: {
    rawModels: ["qwen3-coder-plus", "qwen-max"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
  dsh: {
    rawModels: ["deepseek-chat", "deepseek-reasoner"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
  glm: {
    rawModels: ["glm-5", "glm-4.7"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
  zcode: {
    rawModels: ["glm-5", "glm-4.7"],
    defaultModel: "",
    efforts: ["low", "medium", "high"],
    defaultEffort: "",
    modelEfforts: {},
  },
};

export function stripEffortSuffix(modelId) {
  return String(modelId ?? "").replace(/-(extra-high|xhigh|high|medium|med|low|max)$/i, "");
}

export function normalizeAgentCatalog(rawAgents) {
  const map = {};
  for (const a of Array.isArray(rawAgents) ? rawAgents : []) {
    const rawId = String(a?.id ?? "").trim().toLowerCase();
    const id = ACP_AGENT_KEY[rawId] ?? rawId;
    if (!id) continue;
    const rawModels = Array.isArray(a.models) ? a.models.map((m) => String(m).trim()).filter(Boolean) : [];
    const modelEfforts = a.modelEfforts && typeof a.modelEfforts === "object" ? a.modelEfforts : {};
    const efforts = (Array.isArray(a.efforts) ? a.efforts : [])
      .map((e) => String(e).trim().toLowerCase())
      .filter((e) => e && e !== "default");
    map[id] = {
      rawModels,
      defaultModel: typeof a.defaultModel === "string" ? a.defaultModel.trim() : "",
      efforts,
      defaultEffort: typeof a.defaultEffort === "string" ? a.defaultEffort.trim().toLowerCase() : "",
      modelEfforts,
    };
  }
  return map;
}

export function catalogForPreset(agentCatalog, presetKey, selectedModel = "") {
  const rawKey = String(presetKey ?? "").trim().toLowerCase();
  const key = ACP_AGENT_KEY[rawKey] ?? rawKey;
  const live = agentCatalog && typeof agentCatalog === "object" ? agentCatalog[key] : null;
  const fallback = CURATED_AGENT_CATALOG[key] ?? null;
  const rawModels = Array.isArray(live?.rawModels) && live.rawModels.length > 0
    ? live.rawModels
    : (fallback?.rawModels ?? []);
  const modelEfforts = {
    ...(fallback?.modelEfforts ?? {}),
    ...(live?.modelEfforts && typeof live.modelEfforts === "object" ? live.modelEfforts : {}),
  };

  const models = [];
  const seen = new Set();
  for (const raw of rawModels) {
    if (!raw || raw.toLowerCase() === "default") continue;
    if (key === "agy") {
      const base = stripEffortSuffix(raw);
      const candidate = Object.prototype.hasOwnProperty.call(modelEfforts, base) ? base : raw;
      if (!seen.has(candidate)) {
        seen.add(candidate);
        models.push(candidate);
      }
    } else if (!seen.has(raw)) {
      seen.add(raw);
      models.push(raw);
    }
  }

  const rawDefault = (live?.defaultModel || fallback?.defaultModel || "").trim();
  const defaultModel = rawDefault && rawDefault.toLowerCase() !== "default"
    ? (key === "agy" && Object.prototype.hasOwnProperty.call(modelEfforts, stripEffortSuffix(rawDefault))
        ? stripEffortSuffix(rawDefault)
        : rawDefault)
    : "";

  const cleanSelected = String(selectedModel ?? "").trim();
  const selectedBase = cleanSelected
    ? (key === "agy" ? stripEffortSuffix(cleanSelected) : cleanSelected)
    : defaultModel;
  const perModelEffs = selectedBase && Array.isArray(modelEfforts[selectedBase])
    ? modelEfforts[selectedBase].map((e) => String(e).toLowerCase()).filter((e) => e && e !== "default")
    : [];
  const baseEffs = Array.isArray(live?.efforts) && live.efforts.length > 0
    ? live.efforts
    : (fallback?.efforts ?? ["low", "medium", "high"]);
  const rawEffs = perModelEffs.length > 0 ? perModelEffs : baseEffs;
  const efforts = [...new Set(rawEffs)].sort((a, b) => {
    const ia = EFFORT_ORDER.indexOf(a);
    const ib = EFFORT_ORDER.indexOf(b);
    return (ia === -1 ? 99 : ia) - (ib === -1 ? 99 : ib);
  });

  return {
    key,
    models,
    rawModels,
    defaultModel,
    efforts,
    defaultEffort: live?.defaultEffort || fallback?.defaultEffort || "",
    modelEfforts,
  };
}

export function resolveEffectiveModel(agentCatalog, presetKey, selectedModel, selectedEffort) {
  const cat = catalogForPreset(agentCatalog, presetKey, selectedModel);
  const model = String(selectedModel ?? "").trim();
  const eff = String(selectedEffort ?? "").trim().toLowerCase();
  if (!model) {
    if (cat.key === "agy" && eff && cat.defaultModel) {
      const combined = `${cat.defaultModel}-${eff}`;
      if (cat.rawModels.includes(combined) || Object.prototype.hasOwnProperty.call(cat.modelEfforts, cat.defaultModel)) {
        return combined;
      }
    }
    return "";
  }
  if (cat.key === "agy") {
    if (eff) {
      const combined = `${model}-${eff}`;
      if (cat.rawModels.includes(combined) || Object.prototype.hasOwnProperty.call(cat.modelEfforts, model)) {
        return combined;
      }
    }
    if (!cat.rawModels.includes(model)) {
      const pref = cat.defaultEffort ? `${model}-${cat.defaultEffort}` : "";
      if (pref && cat.rawModels.includes(pref)) return pref;
      const highMatch = `${model}-high`;
      if (cat.rawModels.includes(highMatch)) return highMatch;
      const firstMatch = cat.rawModels.find((m) => m.startsWith(`${model}-`));
      if (firstMatch) return firstMatch;
    }
  }
  return model;
}

export function buildSentToLabel(presetName, isAcp, selectedModel, selectedEffort) {
  const details = [isAcp ? "ACP" : "CLI"];
  const m = String(selectedModel ?? "").trim();
  const e = String(selectedEffort ?? "").trim();
  if (m) details.push(m);
  if (e) details.push(e);
  return `${presetName} (${details.join(" · ")})`;
}

export function withAlpha(color, alphaHex) {
  if (typeof color !== "string") return color;
  if (/^#[0-9a-fA-F]{6}$/.test(color)) return `${color}${alphaHex}`;
  if (/^#[0-9a-fA-F]{8}$/.test(color)) return `${color.slice(0, 7)}${alphaHex}`;
  return color;
}

export function normalizeNotes(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((item) => item && typeof item === "object" && typeof item.id === "string" && item.id.trim() && typeof item.text === "string" && item.text.trim())
    .map((item) => ({
      id: item.id,
      title: typeof item.title === "string" && item.title.trim() ? item.title.trim() : null,
      text: item.text,
      createdAt: Number(item.createdAt) || Number(item.updatedAt) || 0,
      updatedAt: Number(item.updatedAt) || Number(item.createdAt) || 0,
      sentTo: typeof item.sentTo === "string" && item.sentTo.trim() ? item.sentTo.trim() : null,
      sentAt: Number(item.sentAt) || null,
    }))
    .sort((a, b) => (a.updatedAt || a.createdAt) - (b.updatedAt || b.createdAt) || (a.createdAt - b.createdAt));
}

export function normalizeTrash(raw) {
  if (!Array.isArray(raw)) return [];
  return raw
    .filter((item) => item && typeof item === "object" && typeof item.id === "string" && item.id.trim() && typeof item.text === "string" && item.text.trim())
    .map((item) => ({
      id: item.id,
      title: typeof item.title === "string" && item.title.trim() ? item.title.trim() : null,
      text: item.text,
      createdAt: Number(item.createdAt) || Number(item.updatedAt) || 0,
      updatedAt: Number(item.updatedAt) || Number(item.createdAt) || 0,
      sentTo: typeof item.sentTo === "string" && item.sentTo.trim() ? item.sentTo.trim() : null,
      sentAt: Number(item.sentAt) || null,
      trashedAt: Number(item.trashedAt) || Number(item.updatedAt) || Number(item.createdAt) || 0,
    }))
    .sort((a, b) => (b.trashedAt || b.updatedAt || b.createdAt) - (a.trashedAt || a.updatedAt || a.createdAt));
}

export function makeNoteId(now = Date.now()) {
  const rand = Math.random().toString(36).slice(2, 7);
  return `fn-${now}-${rand}`;
}

export function upsertNote(rawList, { id = null, title = undefined, text, sentTo = undefined, sentAt = undefined, now = Date.now() }) {
  const cleanText = String(text ?? "").trim();
  const hasTitleArg = title !== undefined;
  const cleanTitle = hasTitleArg ? (String(title ?? "").trim() || null) : undefined;
  const list = normalizeNotes(rawList);
  if (!cleanText) return { list, note: null };
  const idx = id ? list.findIndex((n) => n.id === id) : -1;
  if (idx >= 0) {
    const prev = list[idx];
    const updated = {
      ...prev,
      ...(hasTitleArg ? { title: cleanTitle } : {}),
      text: cleanText,
      updatedAt: now,
      ...(sentTo !== undefined ? { sentTo } : {}),
      ...(sentAt !== undefined ? { sentAt } : {}),
    };
    const next = normalizeNotes([...list.slice(0, idx), updated, ...list.slice(idx + 1)]);
    return { list: next, note: updated };
  }
  const created = {
    id: id || makeNoteId(now),
    title: cleanTitle ?? null,
    text: cleanText,
    createdAt: now,
    updatedAt: now,
    sentTo: sentTo ?? null,
    sentAt: sentAt ?? null,
  };
  return { list: normalizeNotes([...list, created]), note: created };
}

export function renameNote(rawList, id, title) {
  const list = normalizeNotes(rawList);
  const idx = list.findIndex((n) => n.id === id);
  if (idx < 0) return { list, note: null };
  const cleanTitle = String(title ?? "").trim() || null;
  const updated = { ...list[idx], title: cleanTitle };
  const next = [...list.slice(0, idx), updated, ...list.slice(idx + 1)];
  return { list: next, note: updated };
}

export function markNoteSent(rawList, id, sentTo, now = Date.now()) {
  const list = normalizeNotes(rawList);
  const idx = list.findIndex((n) => n.id === id);
  if (idx < 0) return list;
  const updated = { ...list[idx], sentTo: sentTo || null, sentAt: now, updatedAt: now };
  return normalizeNotes([...list.slice(0, idx), updated, ...list.slice(idx + 1)]);
}

export function removeNote(rawList, id) {
  return normalizeNotes(rawList).filter((n) => n.id !== id);
}

export function moveNoteToTrash(rawList, rawTrash, id, now = Date.now()) {
  const list = normalizeNotes(rawList);
  const trash = normalizeTrash(rawTrash);
  const idx = list.findIndex((n) => n.id === id);
  if (idx < 0) return { list, trash, note: null };
  const target = list[idx];
  const nextList = [...list.slice(0, idx), ...list.slice(idx + 1)];
  const trashed = { ...target, trashedAt: now };
  const nextTrash = [trashed, ...trash.filter((t) => t.id !== target.id)];
  return { list: nextList, trash: nextTrash, note: trashed };
}

export function restoreNoteFromTrash(rawList, rawTrash, id, now = Date.now()) {
  const list = normalizeNotes(rawList);
  const trash = normalizeTrash(rawTrash);
  const idx = trash.findIndex((t) => t.id === id);
  if (idx < 0) return { list, trash, note: null };
  const { trashedAt: _trashedAt, ...rest } = trash[idx];
  const restored = { ...rest, updatedAt: now };
  const nextTrash = [...trash.slice(0, idx), ...trash.slice(idx + 1)];
  const nextList = normalizeNotes([...list.filter((n) => n.id !== restored.id), restored]);
  return { list: nextList, trash: nextTrash, note: restored };
}

export function purgeTrashedNote(rawTrash, id) {
  return normalizeTrash(rawTrash).filter((t) => t.id !== id);
}

export function emptyNotesTrash() {
  return [];
}

export function notePreview(text, maxLen = 76) {
  const single = String(text ?? "").replace(/[\r\n\t]+/g, " ").replace(/\s{2,}/g, " ").trim();
  if (single.length <= maxLen) return single;
  return `${single.slice(0, Math.max(1, maxLen - 1))}…`;
}

export function noteDisplayTitle(note, maxLen = 64) {
  if (note && typeof note.title === "string" && note.title.trim()) {
    return notePreview(note.title, maxLen);
  }
  return notePreview(note?.text ?? "", maxLen);
}

export function formatTrashAge(tsMs, nowMs = Date.now()) {
  if (!Number.isFinite(tsMs) || tsMs <= 0) return "";
  const m = Math.floor(Math.max(0, nowMs - tsMs) / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

export function formatNoteTime(tsMs, nowMs = Date.now()) {
  if (!Number.isFinite(tsMs) || tsMs <= 0) return "";
  try {
    const d = new Date(tsMs);
    const now = new Date(nowMs);
    if (Number.isNaN(d.getTime())) return "";
    const sameDay =
      d.getFullYear() === now.getFullYear() &&
      d.getMonth() === now.getMonth() &&
      d.getDate() === now.getDate();
    if (sameDay) {
      return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    }
    return `${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
  } catch {
    return "";
  }
}

export function shortAgentId(addr) {
  const s = String(addr ?? "").trim();
  if (!s) return "";
  const slash = s.lastIndexOf("/");
  return slash >= 0 ? s.slice(slash + 1) : s;
}

export function resolveSpaceFolder(space, allPanes = []) {
  if (!space) return null;
  const tabIds = new Set((space.tabs ?? []).map((t) => t.id));
  const list = Array.isArray(allPanes) ? allPanes : [];
  return list.find((p) => tabIds.has(p.tab) && p.cwd)?.cwd || space.folder || null;
}

export function resolveExistingAgents(allTerminals = [], allAgents = [], allPanes = [], foc = null) {
  const termList = Array.isArray(allTerminals) ? allTerminals : [];
  const agentList = Array.isArray(allAgents) ? allAgents : [];
  const paneList = Array.isArray(allPanes) ? allPanes : [];

  const agentByAddr = new Map();
  for (const a of agentList) {
    if (a?.address) agentByAddr.set(a.address, a);
  }
  const paneByAddr = new Map();
  for (const p of paneList) {
    if (p?.terminal) paneByAddr.set(p.terminal, p);
  }

  const focusedAddr =
    paneList.find((p) => p?.focused && p?.terminal)?.terminal ||
    agentList.find((a) => a?.focused && a?.address)?.address ||
    foc?.shown ||
    foc?.terminal ||
    foc?.agent ||
    null;

  const out = [];
  const seen = new Set();

  const pushCandidate = (rec, aInfo) => {
    const addr = rec?.address || aInfo?.address;
    if (!addr || seen.has(addr)) return;
    if (rec?.labels?.["herdr.trashed"]) return;
    const state = rec?.state ?? "live";
    if (state !== "live" && state !== "running") return;
    const isAcp = Boolean(rec?.acp || aInfo?.acp || /(^|\/)a[0-9a-z]+$/i.test(addr));
    const isCliAgent = !isAcp && Boolean(aInfo || rec?.program?.agent || rec?.status);
    if (!isAcp && !isCliAgent) return;
    seen.add(addr);

    const pInfo = paneByAddr.get(addr);
    const customName = rec?.labels?.["herdr.name"] || aInfo?.custom_name || pInfo?.custom_name || "";
    const progName = rec?.program?.name || aInfo?.program?.name || rec?.agent || aInfo?.app || "";
    const rawTitle = rec?.name || aInfo?.title || rec?.title || "";
    const baseName = customName || (rawTitle && rawTitle !== addr ? rawTitle : "") || progName || addr;
    const shortId = shortAgentId(addr);
    const label = shortId && baseName !== shortId && !baseName.includes(shortId) ? `${baseName} (${shortId})` : baseName;
    const status = aInfo?.status || (typeof rec?.status === "string" ? rec.status : rec?.status?.state) || pInfo?.status || "idle";
    const isSideChat = String(rec?.lineage?.kind || "").toLowerCase().replace("_", "-") === "side-chat";
    const spaceName = aInfo?.space_name || "";

    out.push({
      address: addr,
      label,
      shortId,
      kind: isAcp ? "acp" : "cli",
      status,
      spaceName,
      isSideChat,
      isFocused: addr === focusedAddr,
    });
  };

  for (const t of termList) {
    pushCandidate(t, agentByAddr.get(t.address));
  }
  for (const a of agentList) {
    if (!seen.has(a.address)) pushCandidate(null, a);
  }

  out.sort((a, b) => {
    if (a.isFocused !== b.isFocused) return a.isFocused ? -1 : 1;
    if (a.isSideChat !== b.isSideChat) return a.isSideChat ? 1 : -1;
    if (a.kind !== b.kind) return a.kind === "acp" ? -1 : 1;
    return a.label.localeCompare(b.label);
  });

  return out;
}
