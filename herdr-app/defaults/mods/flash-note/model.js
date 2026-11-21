// Pure data model & target resolution helpers for Flash Notes (闪念).

export const FLASH_NOTES_KEY = "flash_notes.items";
export const FLASH_NOTES_TRASH_KEY = "flash_notes.trash";
export const KNOWN_AGENTS_KEY = "new-menu.agents";
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
export const ACP_AGENT_KEY = { antigravity: "agy", "dsh-tui": "dsh" };

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
    .sort((a, b) => (b.updatedAt || b.createdAt) - (a.updatedAt || a.createdAt));
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
    const next = [updated, ...list.slice(0, idx), ...list.slice(idx + 1)];
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
  return { list: [created, ...list], note: created };
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
  return [updated, ...list.slice(0, idx), ...list.slice(idx + 1)];
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
  const nextList = [restored, ...list.filter((n) => n.id !== restored.id)];
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
