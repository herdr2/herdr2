// Pure data model & target resolution helpers for Flash Notes (闪念).

export const FLASH_NOTES_KEY = "flash_notes.items";
export const FLASH_NOTES_TRASH_KEY = "flash_notes.trash";
export const KNOWN_AGENTS_KEY = "new-menu.agents";
export const AGENT_CATALOG_KEY = "handoff.agentCatalog";
export const EFFORT_ORDER = ["off", "disabled", "minimal", "low", "medium", "enabled", "on", "high", "xhigh", "max", "ultra"];
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
    rawModels: ["GLM-5.4", "GLM-5.3", "GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"],
    defaultModel: "GLM-5.3",
    efforts: ["low", "high", "max"],
    defaultEffort: "max",
    modelEfforts: {
      "GLM-5.4": ["low", "high", "max"],
      "GLM-5.3": ["low", "high", "max"],
      "GLM-5.3-Flash": ["low", "high", "max"],
      "GLM-5.2": ["disabled", "high", "max"],
      "GLM-5-Turbo": ["disabled", "enabled"],
    },
  },
  zcode: {
    rawModels: ["GLM-5.4", "GLM-5.3", "GLM-5.3-Flash", "GLM-5.2", "GLM-5-Turbo"],
    defaultModel: "GLM-5.3",
    efforts: ["low", "high", "max"],
    defaultEffort: "max",
    modelEfforts: {
      "GLM-5.4": ["low", "high", "max"],
      "GLM-5.3": ["low", "high", "max"],
      "GLM-5.3-Flash": ["low", "high", "max"],
      "GLM-5.2": ["disabled", "high", "max"],
      "GLM-5-Turbo": ["disabled", "enabled"],
    },
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
  const lookupModelEfforts = (name) => {
    if (!name) return null;
    if (Array.isArray(modelEfforts[name])) return modelEfforts[name];
    const bare = name.includes("\\") ? name.slice(name.lastIndexOf("\\") + 1) : name;
    if (Array.isArray(modelEfforts[bare])) return modelEfforts[bare];
    const lowerBare = bare.toLowerCase();
    for (const [k, v] of Object.entries(modelEfforts)) {
      if (k.toLowerCase() === lowerBare && Array.isArray(v)) return v;
    }
    return null;
  };
  const matchedPerModel = lookupModelEfforts(selectedBase);
  const perModelEffs = matchedPerModel
    ? matchedPerModel.map((e) => String(e).toLowerCase()).filter((e) => e && e !== "default")
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

export function makeNoteId(now = Date.now()) {
  const rand = Math.random().toString(36).slice(2, 7);
  return `fn-${now}-${rand}`;
}

export function makeEntryId(index = 0, now = Date.now()) {
  const rand = Math.random().toString(36).slice(2, 6);
  return `fe-${now}-${index}-${rand}`;
}

export function normalizeEntries(rawEntries, fallbackText = "", fallbackId = "fn", fallbackSentTo = null, fallbackSentAt = null) {
  const list = [];
  if (Array.isArray(rawEntries) && rawEntries.length > 0) {
    rawEntries.forEach((entry, idx) => {
      if (!entry || typeof entry !== "object") return;
      const text = typeof entry.text === "string" ? entry.text.trim() : "";
      if (!text) return;
      const id = typeof entry.id === "string" && entry.id.trim() ? entry.id.trim() : `${fallbackId}-e${idx}`;
      const sentTo = typeof entry.sentTo === "string" && entry.sentTo.trim() ? entry.sentTo.trim() : null;
      const sentAt = Number(entry.sentAt) || null;
      list.push({ id, text, sentTo, sentAt });
    });
  }
  if (list.length === 0) {
    const cleanFallback = String(fallbackText ?? "").trim();
    if (cleanFallback) {
      list.push({
        id: `${fallbackId}-e0`,
        text: cleanFallback,
        sentTo: typeof fallbackSentTo === "string" && fallbackSentTo.trim() ? fallbackSentTo.trim() : null,
        sentAt: Number(fallbackSentAt) || null,
      });
    }
  }
  return list;
}

function deriveGroupSent(entries, explicitSentTo = null, explicitSentAt = null) {
  if (entries.length === 0) {
    return { sentTo: explicitSentTo || null, sentAt: explicitSentAt || null };
  }
  const sentEntries = entries.filter((e) => Boolean(e.sentTo));
  if (sentEntries.length === entries.length) {
    const latest = sentEntries.reduce((best, cur) => ((cur.sentAt || 0) >= (best.sentAt || 0) ? cur : best), sentEntries[0]);
    return {
      sentTo: latest.sentTo || explicitSentTo || null,
      sentAt: latest.sentAt || explicitSentAt || null,
    };
  }
  return { sentTo: null, sentAt: null };
}

function joinEntriesText(entries) {
  if (entries.length === 0) return "";
  if (entries.length === 1) return entries[0].text;
  return entries.map((e) => e.text).join("\n\n");
}

export function normalizeNotes(raw) {
  if (!Array.isArray(raw)) return [];
  const out = [];
  for (const item of raw) {
    if (!item || typeof item !== "object" || typeof item.id !== "string" || !item.id.trim()) continue;
    const rawSentTo = typeof item.sentTo === "string" && item.sentTo.trim() ? item.sentTo.trim() : null;
    const rawSentAt = Number(item.sentAt) || null;
    const entries = normalizeEntries(item.entries, item.text, item.id, rawSentTo, rawSentAt);
    if (entries.length === 0) continue;
    const derivedSent = deriveGroupSent(entries, rawSentTo, rawSentAt);
    const text = joinEntriesText(entries);
    out.push({
      id: item.id,
      title: typeof item.title === "string" && item.title.trim() ? item.title.trim() : null,
      text,
      entries,
      createdAt: Number(item.createdAt) || Number(item.updatedAt) || 0,
      updatedAt: Number(item.updatedAt) || Number(item.createdAt) || 0,
      sentTo: derivedSent.sentTo,
      sentAt: derivedSent.sentAt,
      ...(typeof item.order === "number" && Number.isFinite(item.order) ? { order: item.order } : {}),
    });
  }
  const hasOrder = out.some((n) => typeof n.order === "number");
  if (hasOrder) {
    return out.sort((a, b) => {
      const oa = typeof a.order === "number" ? a.order : Number.POSITIVE_INFINITY;
      const ob = typeof b.order === "number" ? b.order : Number.POSITIVE_INFINITY;
      if (oa !== ob) return oa - ob;
      return (a.updatedAt || a.createdAt) - (b.updatedAt || b.createdAt) || (a.createdAt - b.createdAt);
    });
  }
  return out.sort((a, b) => (a.updatedAt || a.createdAt) - (b.updatedAt || b.createdAt) || (a.createdAt - b.createdAt));
}

export function reorderNotes(rawList, fromId, toIndex) {
  const list = normalizeNotes(rawList);
  const from = list.findIndex((n) => n.id === fromId);
  if (from < 0 || list.length < 2) return list;
  const clamped = Math.max(0, Math.min(list.length - 1, Number(toIndex) || 0));
  if (from === clamped) return list;
  const next = [...list];
  const [moved] = next.splice(from, 1);
  next.splice(clamped, 0, moved);
  return next.map((n, idx) => ({ ...n, order: idx }));
}

export function normalizeTrash(raw) {
  if (!Array.isArray(raw)) return [];
  const out = [];
  for (const item of raw) {
    if (!item || typeof item !== "object" || typeof item.id !== "string" || !item.id.trim()) continue;
    const rawSentTo = typeof item.sentTo === "string" && item.sentTo.trim() ? item.sentTo.trim() : null;
    const rawSentAt = Number(item.sentAt) || null;
    const entries = normalizeEntries(item.entries, item.text, item.id, rawSentTo, rawSentAt);
    if (entries.length === 0) continue;
    const derivedSent = deriveGroupSent(entries, rawSentTo, rawSentAt);
    const text = joinEntriesText(entries);
    out.push({
      id: item.id,
      title: typeof item.title === "string" && item.title.trim() ? item.title.trim() : null,
      text,
      entries,
      createdAt: Number(item.createdAt) || Number(item.updatedAt) || 0,
      updatedAt: Number(item.updatedAt) || Number(item.createdAt) || 0,
      sentTo: derivedSent.sentTo,
      sentAt: derivedSent.sentAt,
      trashedAt: Number(item.trashedAt) || Number(item.updatedAt) || Number(item.createdAt) || 0,
    });
  }
  return out.sort((a, b) => (b.trashedAt || b.updatedAt || b.createdAt) - (a.trashedAt || a.updatedAt || a.createdAt));
}

export function upsertNote(rawList, { id = null, title = undefined, text = "", entries = undefined, sentTo = undefined, sentAt = undefined, now = Date.now() }) {
  const hasTitleArg = title !== undefined;
  const cleanTitle = hasTitleArg ? (String(title ?? "").trim() || null) : undefined;
  const list = normalizeNotes(rawList);
  const idx = id ? list.findIndex((n) => n.id === id) : -1;
  const prev = idx >= 0 ? list[idx] : null;
  const noteId = prev?.id || id || makeNoteId(now);

  let nextEntries;
  if (Array.isArray(entries)) {
    nextEntries = entries
      .map((e, i) => {
        const clean = String(e?.text ?? "").trim();
        if (!clean) return null;
        return {
          id: typeof e?.id === "string" && e.id.trim() ? e.id.trim() : makeEntryId(i, now),
          text: clean,
          sentTo: typeof e?.sentTo === "string" && e.sentTo.trim() ? e.sentTo.trim() : null,
          sentAt: Number(e?.sentAt) || null,
        };
      })
      .filter(Boolean);
    if (sentTo !== undefined && nextEntries.length === 1 && !nextEntries[0].sentTo) {
      nextEntries[0] = { ...nextEntries[0], sentTo: sentTo || null, sentAt: sentAt ?? now };
    }
  } else {
    const cleanText = String(text ?? "").trim();
    if (!cleanText) return { list, note: null };
    const existingFirst = prev?.entries?.[0];
    nextEntries = [
      {
        id: existingFirst?.id || `${noteId}-e0`,
        text: cleanText,
        sentTo: sentTo !== undefined ? (sentTo || null) : (existingFirst?.sentTo ?? prev?.sentTo ?? null),
        sentAt: sentAt !== undefined ? (sentAt || null) : (existingFirst?.sentAt ?? prev?.sentAt ?? null),
      },
    ];
  }

  if (nextEntries.length === 0) return { list, note: null };
  const derivedSent = deriveGroupSent(nextEntries, sentTo ?? null, sentAt ?? null);
  const joinedText = joinEntriesText(nextEntries);

  if (idx >= 0) {
    const updated = {
      ...prev,
      ...(hasTitleArg ? { title: cleanTitle } : {}),
      text: joinedText,
      entries: nextEntries,
      updatedAt: now,
      sentTo: derivedSent.sentTo,
      sentAt: derivedSent.sentAt,
    };
    const next = normalizeNotes([...list.slice(0, idx), updated, ...list.slice(idx + 1)]);
    return { list: next, note: updated };
  }

  const maxOrder = list.reduce((m, n) => (typeof n.order === "number" && Number.isFinite(n.order) ? Math.max(m, n.order) : m), -1);
  const created = {
    id: noteId,
    title: cleanTitle ?? null,
    text: joinedText,
    entries: nextEntries,
    createdAt: now,
    updatedAt: now,
    sentTo: derivedSent.sentTo,
    sentAt: derivedSent.sentAt,
    ...(maxOrder >= 0 ? { order: maxOrder + 1 } : {}),
  };
  return { list: normalizeNotes([...list, created]), note: created };
}

export function markEntriesSent(rawList, noteId, entryIds, sentTo, now = Date.now()) {
  const list = normalizeNotes(rawList);
  const idx = list.findIndex((n) => n.id === noteId);
  if (idx < 0) return { list, note: null };
  const prev = list[idx];
  const idSet = Array.isArray(entryIds) ? new Set(entryIds) : null;
  const nextEntries = prev.entries.map((entry) => {
    if (!idSet || idSet.has(entry.id)) {
      return { ...entry, sentTo: sentTo || null, sentAt: now };
    }
    return entry;
  });
  const derivedSent = deriveGroupSent(nextEntries, sentTo || null, now);
  const updated = {
    ...prev,
    entries: nextEntries,
    text: joinEntriesText(nextEntries),
    sentTo: derivedSent.sentTo,
    sentAt: derivedSent.sentAt,
    updatedAt: now,
  };
  const next = normalizeNotes([...list.slice(0, idx), updated, ...list.slice(idx + 1)]);
  return { list: next, note: updated };
}

export function noteEntryStats(note) {
  const entries = Array.isArray(note?.entries) && note.entries.length > 0
    ? note.entries
    : (note?.text ? [{ id: "e0", text: note.text, sentTo: note.sentTo ?? null, sentAt: note.sentAt ?? null }] : []);
  const total = entries.length;
  const sentCount = entries.filter((e) => Boolean(e.sentTo)).length;
  const unsentCount = total - sentCount;
  return {
    total,
    sentCount,
    unsentCount,
    allSent: total > 0 && sentCount === total,
    partialSent: sentCount > 0 && sentCount < total,
    isGroup: total > 1,
  };
}

export function formatEntriesForSend(entries, groupTitle = "") {
  const clean = (Array.isArray(entries) ? entries : [])
    .map((e) => (typeof e === "string" ? e.trim() : String(e?.text ?? "").trim()))
    .filter(Boolean);
  if (clean.length === 0) return "";
  if (clean.length === 1) return clean[0];
  const title = String(groupTitle ?? "").trim();
  const numbered = clean.map((text, idx) => `${idx + 1}. ${text}`).join("\n\n");
  return title ? `[${title}]\n\n${numbered}` : numbered;
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
  const firstEntryText = Array.isArray(note?.entries) && note.entries[0]?.text ? note.entries[0].text : (note?.text ?? "");
  return notePreview(firstEntryText, maxLen);
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
