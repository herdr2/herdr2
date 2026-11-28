// Flash Note (闪念): floating idea capture window + bottom-left saved ideas list + recycle bin.
// Props:
//   - { list: true }               -> bottom-left sidebar checklist
//   - { trashMenu: true }          -> recycle bin popup
//   - { mode: "compose", noteId? } -> draggable/resizable floating composer window

import { View, div, svg, image } from "gpui-kit";
import { h_flex, v_flex, Popup, InputState, Input, TextareaState, Textarea } from "gpui-base";
import {
  agents,
  terminals,
  panes,
  spaces,
  focused,
  knownAgents,
  run,
  h2,
  notify,
  openOverlay,
  closeOverlay,
  nextCommand,
  theme,
  settings,
  props,
  layout,
  shared,
  setShared,
  checking,
  keys,
  window as appWindow,
} from "herdr";

function shortcutFor(action) {
  if (typeof keys !== "function" || !action) return "";
  const list = keys();
  if (!Array.isArray(list)) return "";
  const k = list.find((b) => b.section === "global" && b.action === action);
  if (!k?.keys?.trim()) return "";
  const mac = typeof appWindow === "function" && appWindow()?.platform === "macos";
  const names = mac ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" } : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: mac ? "cmd" : "ctrl", super: "cmd", win: "cmd" };
  return k.keys.trim().split(/\s+/).map((stroke) => {
    const parts = stroke.split("-");
    let key = parts.pop();
    if (key === "") { key = "-"; parts.pop(); }
    const held = new Set(parts.map((m) => alias[m.toLowerCase()] ?? m.toLowerCase()));
    if (/^[A-Z]$/.test(key)) held.add("shift");
    const label = key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + key.slice(1).toLowerCase();
    return [...Object.keys(names).filter((m) => held.has(m)).map((m) => names[m]), label].join(mac ? "" : "+");
  }).join(" ");
}

function withShortcut(text, action) {
  const sc = shortcutFor(action);
  return sc ? `${text} (${sc})` : text;
}
import {
  FLASH_NOTES_KEY,
  FLASH_NOTES_TRASH_KEY,
  KNOWN_AGENTS_KEY,
  AGENT_CATALOG_KEY,
  ACP_PRESETS,
  ACP_AGENT_KEY,
  withAlpha,
  normalizeNotes,
  reorderNotes,
  normalizeTrash,
  makeEntryId,
  upsertNote,
  renameNote,
  markEntriesSent,
  moveNoteToTrash,
  restoreNoteFromTrash,
  purgeTrashedNote,
  emptyNotesTrash,
  notePreview,
  noteDisplayTitle,
  noteEntryStats,
  formatEntriesForSend,
  formatNoteTime,
  formatTrashAge,
  resolveSpaceFolder,
  resolveExistingAgents,
  normalizeAgentCatalog,
  catalogForPreset,
  resolveEffectiveModel,
  buildSentToLabel,
} from "./model.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

const AMBER = "#f59e0b";
const EFFORT_LABELS = () => ({
  "": tr("Default", "默认"),
  off: tr("Off", "关闭"),
  low: tr("Low", "低"),
  medium: tr("Med", "中"),
  high: tr("High", "高"),
  xhigh: tr("XHigh", "超高"),
  max: tr("Max", "最高"),
});
const FALLBACK_PRESETS = [
  { key: "claude", name: "Claude Code", icon: "icons/claude.svg", tint: false, installed: true, run: ["claude", "--dangerously-skip-permissions"] },
  { key: "codex", name: "Codex", icon: "icons/codex.svg", tint: false, installed: true, run: ["codex", "--no-daemon", "--dangerously-bypass-approvals-and-sandbox"] },
  { key: "antigravity", name: "Antigravity", icon: "icons/antigravity.svg", tint: false, installed: true, run: ["agy", "--dangerously-skip-permissions"] },
  { key: "opencode", name: "OpenCode", icon: "icons/opencode.svg", tint: true, installed: true, run: ["opencode", "--auto"] },
];

function findNode(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = findNode(child, id);
    if (found) return found;
  }
  return null;
}

const isSidebarNarrow = () => (findNode(layout(), "sidebar")?.size?.px ?? Infinity) < 160;

const stop = (_e, cx) => cx.stop_propagation();

export function openFlashNoteWindow(noteId = null) {
  openOverlay({
    mod: "flash-note",
    kind: "window",
    key: "flash-note",
    resizable: true,
    width: 500,
    height: 560,
    min_width: 400,
    min_height: 440,
    props: noteId ? { mode: "compose", noteId } : { mode: "compose" },
  });
}

export function openFlashNoteTrashMenu() {
  openOverlay({
    mod: "flash-note",
    kind: "popup",
    width: 360,
    height: 340,
    props: { trashMenu: true },
  });
}

export default class FlashNote extends View {
  init(_props, cx) {
    this.cx = cx;
    this.hoveredId = null;

    if (props()?.trashMenu) {
      this.handle = cx.focus_handle?.();
      this.handle?.focus?.();
      return;
    }

    if (props()?.list) {
      this.renamingId = null;
      this.renameText = "";
      this.noteDrag = null;
      this.noteSettling = null;
      this.noteSettleTask = null;
      this.noteHeights = {};
      this.noteSnapFromOrder = null;
      this.noteSnapUntil = 0;
      this.listFocus = cx.focus_handle?.() ?? null;
      this.renameInput = InputState.new({
        placeholder: tr("Name (empty: auto preview)", "名称（留空使用正文预览）"),
      });
      this.renameInput.on("change", () => {
        this.renameText = this.renameInput.value();
      });
      this.renameInput.on("submit", (_e, cx) => this.commitInlineRename(cx));
      this.renameInput.on("blur", (_e, cx) => this.commitInlineRename(cx));
      return;
    }

    const p = props() ?? {};
    const notes = normalizeNotes(shared(FLASH_NOTES_KEY));
    const existingNote = p.noteId ? notes.find((n) => n.id === p.noteId) ?? null : null;

    this.editingNoteId = existingNote?.id ?? null;
    this.lastPropNoteId = p.noteId ?? null;
    this.titleFocused = false;
    this.inputFocused = false;
    this.busy = false;
    this.error = null;

    const liveAgents = resolveExistingAgents(terminals(), agents(), panes(), focused());
    this.targetMode = liveAgents.length > 0 ? "existing" : "new";
    this.selectedExisting = liveAgents[0]?.address ?? null;
    this.steerMode = false;

    const activeSp = spaces().find((s) => s.active) ?? spaces()[0] ?? null;
    this.selectedSpaceId = activeSp?.id ?? null;
    this.newRuntimeMode = "acp";

    const where = activeSp?.machine ?? "";
    const cachedKnown = shared(KNOWN_AGENTS_KEY)?.[where];
    this.knownList = Array.isArray(cachedKnown) && cachedKnown.length > 0 ? cachedKnown : [];
    this.selectedPresetKey = null;
    this.agentCatalog = shared(AGENT_CATALOG_KEY) ?? {};
    this.selectedModel = "";
    this.selectedEffort = "";

    this.existingPopupOpen = false;
    this.presetPopupOpen = false;
    this.modelPopupOpen = false;
    this.spacePopupOpen = false;

    this.customModelInput = InputState.new({
      value: "",
      placeholder: tr("Custom model ID…", "自定义模型 ID…"),
    });
    this.customModelInput.on("submit", (_e, cx) => {
      const val = String(this.customModelInput?.value?.() ?? "").trim();
      if (val) {
        this.selectModel(val);
        this.modelPopupOpen = false;
        cx.notify();
      }
    });

    this.titleInput = InputState.new({
      value: existingNote?.title ?? "",
      placeholder: tr("Group or idea name (optional — defaults to first idea)", "闪念组或条目名称（可选，留空自动使用首条预览）"),
    });
    this.titleInput.on("focus", (_e, cx) => {
      this.titleFocused = true;
      this.closeAllPopups();
      cx.notify();
    });
    this.titleInput.on("blur", (_e, cx) => {
      this.titleFocused = false;
      cx.notify();
    });
    this.titleInput.on("change", (_e, cx) => {
      this.error = null;
      cx.notify();
    });
    this.titleInput.on("submit", () => {
      this.input?.focus?.();
    });

    this.entryStates = [];
    this.activeEntryIndex = 0;
    this.sendScope = "all";
    this.polishing = false;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.lastPolishedEntryId = null;
    this.loadNoteIntoComposer(existingNote, false);

    this.handle = cx.focus_handle();
    if (!checking()) {
      this.input?.focus?.();
      if (typeof nextCommand === "function") {
        cx.spawn(async (cmdCx) => {
          for (;;) {
            const cmd = await nextCommand();
            if (cmd === "polish") await this.polishIdea(cmdCx);
            else if (cmd === "polish_undo") this.revertPolish(cmdCx);
          }
        });
      }
      this.refreshMachineAgentsAndCatalog(this.activeSpace()?.machine ?? null, cx);
    }
  }

  get input() {
    return this.entryStates?.[this.activeEntryIndex]?.input ?? this.entryStates?.[0]?.input ?? null;
  }

  activeEntry() {
    return this.entryStates?.[this.activeEntryIndex] ?? this.entryStates?.[0] ?? null;
  }

  createEntryState({ id = null, text = "", sentTo = null, sentAt = null, selected = true } = {}, idx = 0) {
    const entryId = id || makeEntryId(idx, Date.now());
    const ta = TextareaState.new({
      value: String(text ?? ""),
      placeholder: idx === 0
        ? tr("Jot down a sudden idea, reminder, or prompt…", "记下此刻的灵感、待办或想交给 Agent 处理的想法…")
        : tr(`Idea #${idx + 1}…`, `第 ${idx + 1} 条闪念…`),
    });
    ta.set_auto_grow(3, 8);
    const entry = {
      id: entryId,
      input: ta,
      selected: Boolean(selected),
      focused: false,
      sentTo: sentTo || null,
      sentAt: sentAt || null,
      polishUndoText: null,
      polishSummary: null,
    };
    ta.on("focus", (_e, cx) => {
      const curIdx = this.entryStates.indexOf(entry);
      if (curIdx >= 0) this.activeEntryIndex = curIdx;
      for (const e of this.entryStates) e.focused = e === entry;
      this.inputFocused = true;
      this.polishUndoText = entry.polishUndoText;
      this.polishSummary = entry.polishSummary;
      this.closeAllPopups();
      cx.notify();
    });
    ta.on("blur", (_e, cx) => {
      entry.focused = false;
      this.inputFocused = this.entryStates.some((e) => e.focused);
      cx.notify();
    });
    ta.on("change", (_e, cx) => {
      this.error = null;
      cx.notify();
    });
    ta.on("submit", (e, cx) => {
      if (e?.shift) return;
      if (e?.secondary || e?.modifiers?.control || e?.modifiers?.platform) {
        this.sendIdea(cx);
      }
    });
    return entry;
  }

  releaseEntryStates() {
    for (const entry of this.entryStates ?? []) {
      try { entry.input?.release?.(); } catch {}
    }
    this.entryStates = [];
  }

  loadNoteIntoComposer(note, shouldFocus = false) {
    this.releaseEntryStates();
    this.editingNoteId = note?.id ?? null;
    this.titleInput?.set_value?.(note?.title ?? "");
    const rawEntries = Array.isArray(note?.entries) && note.entries.length > 0
      ? note.entries
      : (note?.text ? [{ id: `${note.id}-e0`, text: note.text, sentTo: note.sentTo ?? null, sentAt: note.sentAt ?? null }] : []);
    if (rawEntries.length > 0) {
      const hasPartialSent = rawEntries.some((e) => Boolean(e.sentTo)) && rawEntries.some((e) => !e.sentTo);
      this.entryStates = rawEntries.map((e, idx) =>
        this.createEntryState({
          id: e.id,
          text: e.text,
          sentTo: e.sentTo ?? null,
          sentAt: e.sentAt ?? null,
          selected: hasPartialSent ? !e.sentTo : true,
        }, idx),
      );
      this.sendScope = hasPartialSent ? "selected" : "all";
    } else {
      this.entryStates = [this.createEntryState({ selected: true }, 0)];
      this.sendScope = "all";
    }
    this.activeEntryIndex = 0;
    this.inputFocused = false;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.lastPolishedEntryId = null;
    this.polishing = false;
    this.error = null;
    if (shouldFocus && !checking()) {
      this.entryStates[0]?.input?.focus?.();
    }
  }

  addEntry(cx, initialText = "") {
    if (this.busy) return null;
    const idx = this.entryStates.length;
    const entry = this.createEntryState({ text: initialText, selected: true }, idx);
    this.entryStates.push(entry);
    this.activeEntryIndex = idx;
    this.error = null;
    if (!checking()) entry.input.focus();
    cx?.notify?.();
    return entry;
  }

  removeEntry(idx, cx) {
    if (this.busy || this.entryStates.length <= 1) return;
    if (idx < 0 || idx >= this.entryStates.length) return;
    const [removed] = this.entryStates.splice(idx, 1);
    try { removed?.input?.release?.(); } catch {}
    if (this.activeEntryIndex >= this.entryStates.length) {
      this.activeEntryIndex = Math.max(0, this.entryStates.length - 1);
    }
    const active = this.activeEntry();
    this.polishUndoText = active?.polishUndoText ?? null;
    this.polishSummary = active?.polishSummary ?? null;
    this.error = null;
    cx?.notify?.();
  }

  toggleEntrySelected(idx, cx) {
    const entry = this.entryStates[idx];
    if (!entry) return;
    entry.selected = !entry.selected;
    const nonEmpty = this.collectNonEmptyEntries();
    const selCount = nonEmpty.filter((e) => e.selected).length;
    if (nonEmpty.length > 1) {
      this.sendScope = selCount < nonEmpty.length ? "selected" : "all";
    }
    this.error = null;
    cx?.notify?.();
  }

  selectEntries(mode, cx) {
    for (const entry of this.entryStates) {
      if (mode === "all") entry.selected = true;
      else if (mode === "none") entry.selected = false;
      else if (mode === "unsent") entry.selected = !entry.sentTo;
    }
    this.sendScope = mode === "all" ? "all" : "selected";
    this.error = null;
    cx?.notify?.();
  }

  collectNonEmptyEntries() {
    const out = [];
    this.entryStates.forEach((entry, index) => {
      const text = String(entry.input?.value?.() ?? "").trim();
      if (text) {
        out.push({
          index,
          id: entry.id,
          text,
          selected: Boolean(entry.selected),
          sentTo: entry.sentTo || null,
          sentAt: entry.sentAt || null,
          state: entry,
        });
      }
    });
    return out;
  }

  collectTargetEntries(overrideEntryIds = null) {
    const nonEmpty = this.collectNonEmptyEntries();
    if (Array.isArray(overrideEntryIds)) {
      const idSet = new Set(overrideEntryIds);
      return nonEmpty.filter((e) => idSet.has(e.id));
    }
    if (nonEmpty.length <= 1) return nonEmpty;
    if (this.sendScope === "selected") {
      return nonEmpty.filter((e) => e.selected);
    }
    return nonEmpty;
  }

  close() {
    this.releaseEntryStates();
    try { this.titleInput?.release?.(); } catch {}
    try { this.customModelInput?.release?.(); } catch {}
    try { this.renameInput?.release?.(); } catch {}
  }

  refreshMachineAgentsAndCatalog(machine, cx) {
    if (checking() || typeof cx?.spawn !== "function") return;
    const where = machine ?? "";
    cx.spawn(async (taskCx) => {
      try {
        const list = await knownAgents({ on: machine || undefined });
        if (Array.isArray(list) && list.length > 0) {
          this.knownList = list;
          const prevCache = shared(KNOWN_AGENTS_KEY);
          setShared(KNOWN_AGENTS_KEY, { ...(prevCache && typeof prevCache === "object" ? prevCache : {}), [where]: list });
        }
      } catch {}
      taskCx.notify();
    });
    cx.spawn(async (taskCx) => {
      try {
        const args = ["drove", "settings", "--json", ...(machine ? ["--on", machine] : [])];
        const res = await h2(args, { timeout: 6000 });
        if (res?.code === 0 && res.stdout) {
          const parsed = JSON.parse(res.stdout);
          const catalog = normalizeAgentCatalog(parsed?.agents?.agents);
          if (Object.keys(catalog).length > 0) {
            this.agentCatalog = catalog;
            setShared(AGENT_CATALOG_KEY, catalog);
            const curPreset = this.chosenPreset();
            if (curPreset && this.selectedEffort) {
              const cat = catalogForPreset(this.agentCatalog, curPreset.key, this.selectedModel);
              if (!cat.efforts.includes(this.selectedEffort)) this.selectedEffort = "";
            }
          }
        }
      } catch {}
      taskCx.notify();
    });
  }

  closeAllPopups() {
    this.existingPopupOpen = false;
    this.presetPopupOpen = false;
    this.modelPopupOpen = false;
    this.spacePopupOpen = false;
  }

  selectPreset(nextKey) {
    if (this.selectedPresetKey !== nextKey) {
      this.selectedPresetKey = nextKey;
      this.selectedModel = "";
      this.selectedEffort = "";
      this.modelPopupOpen = false;
      this.customModelInput?.set_value?.("");
    }
  }

  selectModel(nextModel) {
    this.selectedModel = String(nextModel ?? "").trim();
    const preset = this.chosenPreset();
    const cat = catalogForPreset(this.agentCatalog, preset?.key, this.selectedModel);
    if (this.selectedEffort && !cat.efforts.includes(this.selectedEffort)) {
      this.selectedEffort = "";
    }
  }

  revertPolish(cx, entryIndex = null) {
    let entry = null;
    if (entryIndex !== null) {
      entry = this.entryStates?.[entryIndex] ?? null;
    } else {
      const active = this.activeEntry();
      if (active?.polishUndoText != null) {
        entry = active;
      } else if (this.lastPolishedEntryId) {
        entry = this.entryStates?.find((e) => e.id === this.lastPolishedEntryId && e.polishUndoText != null) ?? null;
      }
      if (!entry) {
        for (let i = (this.entryStates?.length ?? 0) - 1; i >= 0; i--) {
          if (this.entryStates[i]?.polishUndoText != null) {
            entry = this.entryStates[i];
            break;
          }
        }
      }
    }
    const orig = entry ? entry.polishUndoText : (entryIndex === null ? this.polishUndoText : null);
    if (orig == null) return;
    if (entry) {
      const idx = this.entryStates.indexOf(entry);
      if (idx >= 0) this.activeEntryIndex = idx;
      entry.polishUndoText = null;
      entry.polishSummary = null;
      entry.input?.set_value?.(orig);
      entry.input?.focus?.();
      if (this.lastPolishedEntryId === entry.id) this.lastPolishedEntryId = null;
    } else {
      this.input?.set_value?.(orig);
      this.input?.focus?.();
    }
    const active = this.activeEntry();
    this.polishUndoText = active?.polishUndoText ?? null;
    this.polishSummary = active?.polishSummary ?? null;
    cx.notify();
  }

  startPolishIdea(cx, entryIndex = null) {
    if (this.polishing || this.busy || checking()) return;
    if (typeof cx?.spawn === "function") {
      cx.spawn(async (taskCx) => {
        await this.polishIdea(taskCx, entryIndex);
      });
    } else {
      this.polishIdea(cx, entryIndex);
    }
  }

  async polishIdea(cx, entryIndex = null) {
    let entry = entryIndex !== null ? this.entryStates?.[entryIndex] : this.activeEntry();
    if (!entry || !String(entry.input?.value?.() ?? "").trim()) {
      entry = this.entryStates?.find((e) => String(e.input?.value?.() ?? "").trim()) ?? entry;
    }
    const raw = String(entry?.input?.value?.() ?? "").trim();
    if (!raw || this.polishing || this.busy || checking()) return;
    const candidates = resolveExistingAgents(terminals(), agents(), panes(), focused());
    const targetKind = this.targetMode === "existing"
      ? (candidates.find((c) => c.address === this.selectedExisting)?.kind === "acp" ? "acp" : "cli")
      : (this.newRuntimeMode === "cli" ? "cli" : "acp");
    this.polishing = true;
    this.error = null;
    try { cx?.notify?.(); } catch {}
    try {
      const args = [
        "ai",
        "polish-prompt",
        "--json",
        "--kind",
        targetKind,
        "--lang",
        isZh() ? "zh-CN" : "en",
      ];
      const noteTitle = String(this.titleInput?.value?.() ?? "").trim();
      const ctxParts = [];
      if (noteTitle) ctxParts.push(`note=${noteTitle}`);
      if (this.targetMode === "existing" && this.selectedExisting) {
        args.push("--target", this.selectedExisting);
        if (this.selectedExisting.includes("/")) {
          args.push("--on", this.selectedExisting.split("/")[0]);
        }
      } else {
        const space = this.activeSpace();
        const folder = resolveSpaceFolder(space, panes());
        if (space?.name) ctxParts.push(`space=${space.name}`);
        if (folder) ctxParts.push(`cwd=${folder}`);
        const spaceMachine = space?.machine ?? null;
        if (spaceMachine) args.push("--on", spaceMachine);
      }
      if (ctxParts.length > 0) {
        args.push("--context", ctxParts.join(" · "));
      }
      args.push("--", raw);
      const res = await h2(args, { timeout: 22000 });
      if (res?.code === 0 && res.stdout) {
        const parsed = JSON.parse(res.stdout);
        const polished = String(parsed?.polished ?? "").trim();
        if (parsed?.usedAi && polished) {
          const summary = parsed?.summary ? String(parsed.summary).trim() : null;
          this.polishing = false;
          if (entry) {
            const idx = this.entryStates.indexOf(entry);
            if (idx >= 0) this.activeEntryIndex = idx;
            entry.polishUndoText = raw;
            entry.polishSummary = summary;
            this.lastPolishedEntryId = entry.id;
            entry.input?.set_value?.(polished);
            entry.input?.focus?.();
          }
          this.polishUndoText = raw;
          this.polishSummary = summary;
        } else {
          const reason = String(parsed?.fallbackReason ?? "");
          this.error = reason === "ai_not_configured"
            ? tr("Core AI is not configured. Enable it in Settings → AI Assistant first.", "Core AI 尚未配置，请先在「设置 → AI 助手」中启用后再使用润色功能。")
            : tr("AI polish returned no changes. Please retry.", "AI 润色暂未返回有效结果，请重试。");
        }
      } else {
        this.error = String(res?.stderr || res?.stdout || tr("AI polish failed", "AI 润色失败")).trim();
      }
    } catch (e) {
      this.error = tr(`AI polish failed: ${e?.message ?? e}`, `AI 润色失败：${e?.message ?? e}`);
    } finally {
      this.polishing = false;
      try { cx?.notify?.(); } catch {}
    }
  }

  savedNotes() {
    return normalizeNotes(shared(FLASH_NOTES_KEY));
  }

  trashedNotes() {
    return normalizeTrash(shared(FLASH_NOTES_TRASH_KEY));
  }

  startInlineRename(note, cx) {
    if (!note?.id || !this.renameInput) return;
    this.renamingId = note.id;
    this.renameText = note.title ?? "";
    this.renameInput.set_value(this.renameText);
    if (this.renameText && typeof this.renameInput.set_selected_range === "function") {
      this.renameInput.set_selected_range({ start: 0, end: this.renameText.length });
    }
    this.renameInput.focus();
    cx?.notify?.();
  }

  commitInlineRename(cx) {
    if (!this.renamingId) return;
    const id = this.renamingId;
    const nextTitle = String(this.renameInput?.value?.() ?? this.renameText ?? "").trim();
    this.renamingId = null;
    this.renameText = "";
    const { list } = renameNote(shared(FLASH_NOTES_KEY), id, nextTitle);
    setShared(FLASH_NOTES_KEY, list);
    cx?.notify?.();
  }

  cancelInlineRename(cx) {
    if (!this.renamingId) return;
    this.renamingId = null;
    this.renameText = "";
    this.listFocus?.focus?.();
    cx?.notify?.();
  }

  trashNote(id, cx) {
    if (!id) return;
    if (this.renamingId === id) {
      this.renamingId = null;
      this.renameText = "";
    }
    if (this.hoveredId === id) this.hoveredId = null;
    const { list, trash, note } = moveNoteToTrash(shared(FLASH_NOTES_KEY), shared(FLASH_NOTES_TRASH_KEY), id, Date.now());
    if (!note) return;
    setShared(FLASH_NOTES_KEY, list);
    setShared(FLASH_NOTES_TRASH_KEY, trash);
    notify({
      title: tr("Moved to recycle bin", "已移至闪念回收站"),
      body: noteDisplayTitle(note, 56),
      level: "info",
    });
    cx?.notify?.();
  }

  activeSpace() {
    const list = spaces();
    if (this.selectedSpaceId) {
      const found = list.find((s) => s.id === this.selectedSpaceId);
      if (found) return found;
    }
    return list.find((s) => s.active) ?? list[0] ?? null;
  }

  availablePresets() {
    const raw = this.knownList.length > 0 ? this.knownList : FALLBACK_PRESETS;
    const isAcp = this.newRuntimeMode === "acp";
    const filtered = raw.filter((a) => {
      if (!a?.key) return false;
      if (isAcp && !ACP_PRESETS.has(a.key)) return false;
      return true;
    });
    const installed = filtered.filter((a) => a.installed);
    const others = filtered.filter((a) => !a.installed);
    const ordered = installed.length > 0 ? [...installed, ...others] : filtered;
    return ordered.length > 0 ? ordered : FALLBACK_PRESETS;
  }

  chosenPreset() {
    const list = this.availablePresets();
    if (this.selectedPresetKey) {
      const exact = list.find((a) => a.key === this.selectedPresetKey);
      if (exact) return exact;
    }
    return list.find((a) => a.installed) ?? list[0] ?? FALLBACK_PRESETS[0];
  }

  chosenExisting(candidates) {
    if (this.selectedExisting) {
      const exact = candidates.find((c) => c.address === this.selectedExisting);
      if (exact) return exact;
    }
    return candidates[0] ?? null;
  }

  saveIdea(cx) {
    const nonEmpty = this.collectNonEmptyEntries();
    const title = String(this.titleInput?.value?.() ?? "").trim();
    if (nonEmpty.length === 0 || this.busy) return;
    const { list, note } = upsertNote(shared(FLASH_NOTES_KEY), {
      id: this.editingNoteId,
      title,
      entries: nonEmpty.map((e) => ({ id: e.id, text: e.text, sentTo: e.sentTo, sentAt: e.sentAt })),
      now: Date.now(),
    });
    setShared(FLASH_NOTES_KEY, list);
    this.editingNoteId = note?.id ?? this.editingNoteId;
    notify({
      title: nonEmpty.length > 1
        ? tr(`Saved flash note group (${nonEmpty.length})`, `已保存闪念组（${nonEmpty.length} 条）`)
        : tr("Idea saved", "闪念已保存"),
      body: noteDisplayTitle(note ?? { title, text: nonEmpty[0].text }, 64),
      level: "info",
    });
    cx?.notify?.();
  }

  deleteEditingNote(cx) {
    if (!this.editingNoteId || this.busy) return;
    const { list, trash, note } = moveNoteToTrash(
      shared(FLASH_NOTES_KEY),
      shared(FLASH_NOTES_TRASH_KEY),
      this.editingNoteId,
      Date.now(),
    );
    setShared(FLASH_NOTES_KEY, list);
    setShared(FLASH_NOTES_TRASH_KEY, trash);
    if (note) {
      notify({
        title: tr("Moved to recycle bin", "已移至闪念回收站"),
        body: noteDisplayTitle(note, 56),
        level: "info",
      });
    }
    this.loadNoteIntoComposer(null, true);
    cx?.notify?.();
  }

  finalizeSentEntries(targetEntries, sentLabel, now, cx) {
    const sentIdSet = new Set(targetEntries.map((e) => e.id));
    for (const entry of this.entryStates) {
      if (sentIdSet.has(entry.id)) {
        entry.sentTo = sentLabel;
        entry.sentAt = now;
      }
    }
    const allNonEmpty = this.collectNonEmptyEntries();
    const remainingUnsent = allNonEmpty.filter((e) => !e.sentTo);
    const isPartialGroupSend = allNonEmpty.length > 1 && remainingUnsent.length > 0;
    const title = String(this.titleInput?.value?.() ?? "").trim();

    if (this.editingNoteId || isPartialGroupSend) {
      const upserted = upsertNote(shared(FLASH_NOTES_KEY), {
        id: this.editingNoteId,
        title,
        entries: allNonEmpty.map((e) => ({ id: e.id, text: e.text, sentTo: e.sentTo, sentAt: e.sentAt })),
        now,
      });
      setShared(FLASH_NOTES_KEY, upserted.list);
      if (upserted.note?.id) this.editingNoteId = upserted.note.id;
    }

    if (isPartialGroupSend) {
      // Keep the group window open with remaining unsent entries pre-selected for easy dispatch.
      for (const entry of this.entryStates) {
        entry.selected = !entry.sentTo;
      }
      this.sendScope = "selected";
    }

    // Keep the flash note window open after sending; only the top-right close button closes it.
    cx?.notify?.();
  }

  sendSingleEntry(idx, cx) {
    const entry = this.entryStates?.[idx];
    if (!entry) return;
    const text = String(entry.input?.value?.() ?? "").trim();
    if (!text) return;
    this.sendIdea(cx, [entry.id]);
  }

  sendIdea(cx, overrideEntryIds = null) {
    const title = String(this.titleInput?.value?.() ?? "").trim();
    const targetEntries = this.collectTargetEntries(overrideEntryIds);
    const text = formatEntriesForSend(targetEntries, title);
    if (!text || this.busy) {
      if (!this.busy && this.collectNonEmptyEntries().length > 0 && targetEntries.length === 0) {
        this.error = tr("Select at least one flash note to send, or switch to Send Whole Group.", "请至少勾选一条要发送的闪念，或切换为「整组发送」。");
        cx?.notify?.();
      }
      return;
    }
    this.error = null;
    const sentCount = targetEntries.length;

    if (this.targetMode === "new") {
      const preset = this.chosenPreset();
      if (!preset) {
        this.error = tr("No available agent preset selected.", "未选择可用的 Agent 预设。");
        cx?.notify?.();
        return;
      }
      const space = this.activeSpace();
      const folder = resolveSpaceFolder(space, panes());
      const acpMode = this.newRuntimeMode === "acp" && ACP_PRESETS.has(preset.key);
      const effectiveModel = resolveEffectiveModel(
        this.agentCatalog,
        preset.key,
        this.selectedModel,
        this.selectedEffort,
      );
      const effectiveEffort = String(this.selectedEffort ?? "").trim();
      if (acpMode) {
        const agentKey = ACP_AGENT_KEY[preset.key] ?? preset.key;
        const args = { acp: true, agent: agentKey, task: text };
        if (effectiveModel) args.model = effectiveModel;
        if (effectiveEffort) args.effort = effectiveEffort;
        if (space?.id) args.space = space.id;
        if (folder) args.folder = folder;
        run("tab.new", args);
      } else {
        const runArgv =
          preset.key === "cursor" && Array.isArray(preset.run) && preset.run[0] === "agent"
            ? ["cursor-agent", ...preset.run.slice(1)]
            : preset.run;
        const args = { name: preset.name, argv: runArgv, task: text };
        if (effectiveModel) args.model = effectiveModel;
        if (effectiveEffort) args.effort = effectiveEffort;
        if (space?.id) args.space = space.id;
        if (folder) args.folder = folder;
        run("tab.new", args);
      }
      const sentLabel = buildSentToLabel(preset.name, acpMode, this.selectedModel, effectiveEffort);
      notify({
        title: sentCount > 1
          ? tr(`Started ${preset.name} with ${sentCount} ideas`, `已新建 ${preset.name} 并发送 ${sentCount} 条闪念`)
          : tr(`Started ${preset.name} with idea`, `已新建 ${preset.name} 并发送闪念`),
        body: noteDisplayTitle({ title: sentCount > 1 ? title : "", text: targetEntries[0].text }, 64),
        level: "info",
      });
      this.finalizeSentEntries(targetEntries, sentLabel, Date.now(), cx);
      return;
    }

    const candidates = resolveExistingAgents(terminals(), agents(), panes(), focused());
    const target = this.chosenExisting(candidates);
    if (!target) {
      this.error = tr("No live agent available. Switch to New Agent or save for later.", "当前没有运行中的 Agent，可切换为「发给新开 Agent」或先保存。");
      cx?.notify?.();
      return;
    }
    const steer = Boolean(this.steerMode && target.kind === "acp" && target.status === "working");
    this.busy = true;
    cx?.notify?.();

    const spawner = cx?.spawn ? cx : this.cx;
    const executeSend = async (taskCx) => {
      try {
        const args = ["send", "--json", ...(steer ? ["--steer"] : []), target.address, text];
        const res = await h2(args, { timeout: 20000 });
        if (res.code !== 0) {
          let msg = (res.stderr || res.stdout || tr("Failed to send idea", "发送失败")).trim();
          try {
            const parsed = JSON.parse(res.stderr || res.stdout || "{}");
            if (parsed?.error?.message) msg = parsed.error.message;
          } catch {}
          this.error = msg;
          return;
        }
        run("agent.focus", { terminal: target.address });
        notify({
          title: sentCount > 1
            ? tr(`Sent ${sentCount} ideas to ${target.label}`, `已将 ${sentCount} 条闪念发送给 ${target.label}`)
            : tr(`Sent idea to ${target.label}`, `已将闪念发送给 ${target.label}`),
          body: noteDisplayTitle({ title: sentCount > 1 ? title : "", text: targetEntries[0].text }, 64),
          level: "info",
          action: { name: "agent.focus", args: { terminal: target.address } },
        });
        this.finalizeSentEntries(targetEntries, target.label, Date.now(), taskCx ?? this.cx);
      } catch (e) {
        this.error = e?.message ?? String(e);
      } finally {
        this.busy = false;
        try {
          (taskCx ?? this.cx)?.notify?.();
        } catch {}
      }
    };
    if (spawner?.spawn) spawner.spawn(executeSend);
    else executeSend(this.cx);
  }

  // ---- Mode A: Bottom-left saved flash notes list ---------------------------

  noteZoom() {
    return Math.max(0.25, Number(settings?.()?.zoom ?? 1) || 1);
  }

  noteHeight(id) {
    return this.noteHeights?.[id] ?? 44;
  }

  noteSlotTop(notes, index) {
    const GAP = 2;
    let y = 0;
    for (let j = 0; j < index && j < notes.length; j++) y += this.noteHeight(notes[j].id) + GAP;
    return y;
  }

  noteSlotOffset(notes, from, to) {
    const GAP = 2;
    let off = 0;
    if (to > from) {
      for (let j = from + 1; j <= to && j < notes.length; j++) off += this.noteHeight(notes[j].id) + GAP;
    } else if (to < from) {
      for (let j = to; j < from && j < notes.length; j++) off -= this.noteHeight(notes[j].id) + GAP;
    }
    return off;
  }

  computeNoteDragTo(notes, from, startTop, dragHeight, dy) {
    let to = from;
    const center = startTop + dy + dragHeight * 0.5;
    if (dy > 0) {
      for (let j = from + 1; j < notes.length; j++) {
        const top = this.noteSlotTop(notes, j);
        const h = this.noteHeight(notes[j].id);
        if (center > top + h * 0.5) to = j;
        else break;
      }
    } else if (dy < 0) {
      for (let j = from - 1; j >= 0; j--) {
        const top = this.noteSlotTop(notes, j);
        const h = this.noteHeight(notes[j].id);
        if (center < top + h * 0.5) to = j;
        else break;
      }
    }
    return to;
  }

  clampNoteDragDy(notes, from, dragHeight, dy) {
    const GAP = 2;
    let minDy = 0;
    for (let j = 0; j < from; j++) minDy -= this.noteHeight(notes[j].id) + GAP;
    let maxDy = 0;
    for (let j = from + 1; j < notes.length; j++) maxDy += this.noteHeight(notes[j].id) + GAP;
    const extra = Math.max(14, Math.round(dragHeight * 0.4));
    return Math.max(minDy - extra, Math.min(maxDy + extra, dy));
  }

  flushNoteSettle() {
    this.noteSettleTask?.cancel?.();
    this.noteSettleTask = null;
    const s = this.noteSettling;
    if (!s) return;
    this.noteSettling = null;
    if (s.to !== s.from) {
      this.noteSnapFromOrder = s.orderSig;
      this.noteSnapUntil = Date.now() + 240;
      const reordered = reorderNotes(shared(FLASH_NOTES_KEY), s.id, s.to);
      setShared(FLASH_NOTES_KEY, reordered);
    }
  }

  handleNoteDragMove(e, cx) {
    if (!this.noteDrag) return;
    if (e && "pressed_button" in e && e.pressed_button !== undefined && e.pressed_button !== "left") {
      this.finishNoteDrag(cx);
      return;
    }
    const rawY = e?.position?.y;
    if (typeof rawY !== "number") return;
    const rawDy = rawY / this.noteZoom() - this.noteDrag.startY;
    if (!this.noteDrag.moving && Math.abs(rawDy) < 4) return;
    if (!this.noteDrag.moving) {
      this.noteDrag.moving = true;
    }
    const list = this.noteDrag.list;
    this.noteDrag.dy = this.clampNoteDragDy(list, this.noteDrag.from, this.noteDrag.dragHeight, rawDy);
    this.noteDrag.to = this.computeNoteDragTo(list, this.noteDrag.from, this.noteDrag.startTop, this.noteDrag.dragHeight, this.noteDrag.dy);
    cx?.notify?.();
  }

  finishNoteDrag(cx) {
    if (!this.noteDrag) return;
    const d = this.noteDrag;
    this.noteDrag = null;
    if (!d.moving) {
      openFlashNoteWindow(d.id);
      cx?.notify?.();
      return;
    }
    const destDy = this.noteSlotOffset(d.list, d.from, d.to);
    const orderSig = d.list.map((n) => n.id).join(",");
    this.noteSettling = {
      id: d.id,
      from: d.from,
      to: d.to,
      startTop: d.startTop,
      destDy,
      dragHeight: d.dragHeight,
      orderSig,
      note: d.list[d.from],
    };
    if (typeof cx?.spawn === "function") {
      this.noteSettleTask = cx.spawn(async (acx) => {
        await acx.sleep(170);
        this.noteSettleTask = null;
        this.flushNoteSettle();
        acx.notify();
      });
    } else {
      this.flushNoteSettle();
    }
    cx?.notify?.();
  }

  renderSidebarList(t) {
    const collapsed = isSidebarNarrow();
    const notes = this.savedNotes();
    const trashed = this.trashedNotes();
    const unsentCount = notes.filter((n) => !n.sentTo).length;
    const accent = t.accent ?? "#3b82f6";

    if (collapsed) {
      return v_flex()
        .id("fn-rail")
        .size_full()
        .bg(t.chrome)
        .border_t(1)
        .border_color(t.border)
        .items_center()
        .justify_center()
        .gap(4)
        .py(6)
        .child(
          v_flex()
            .id("fn-rail-open")
            .w(32)
            .py(5)
            .gap(2)
            .items_center()
            .justify_center()
            .rounded(6)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .tooltip(withShortcut(notes.length > 0 ? tr(`Flash Notes (${notes.length})`, `闪念清单 (${notes.length})`) : tr("Flash Notes", "闪念清单"), "mod.open:flash-note"))
            .on_click(() => openFlashNoteWindow())
            .child(svg("mods/peek/lightbulb.svg").size(14).text_color(unsentCount > 0 ? AMBER : t.muted))
            .when(notes.length > 0, (el) =>
              el.child(div().text_size(9.5).font_semibold().text_color(unsentCount > 0 ? AMBER : t.dim).child(String(notes.length))),
            ),
        )
        .when(trashed.length > 0, (el) =>
          el.child(
            div()
              .id("fn-recycle-rail")
              .p(4)
              .rounded(5)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(tr(`Recycle bin (${trashed.length})`, `闪念回收站 (${trashed.length})`))
              .on_click(() => openFlashNoteTrashMenu())
              .child(svg("mods/workspace-chrome/archive.svg").size(13)),
          ),
        );
    }

    const noteOrderSig = notes.map((n) => n.id).join(",");
    if (this.noteDrag && !notes.some((n) => n.id === this.noteDrag.id)) {
      this.noteDrag = null;
    }
    const snapNotes = this.noteSnapFromOrder !== null && (noteOrderSig !== this.noteSnapFromOrder || Date.now() > this.noteSnapUntil);
    if (snapNotes) this.noteSnapFromOrder = null;
    const noteMotion = this.noteDrag?.moving ? this.noteDrag : this.noteSettling;

    const renderNoteRow = (note, idx, opts = {}) => {
      const { shift = 0, isHiddenSource = false, isFloating = false } = opts;
      const hovered = !isFloating && !this.noteDrag?.moving && this.hoveredId === note.id;
      const renaming = !isFloating && this.renamingId === note.id;
      const timeStr = formatNoteTime(note.updatedAt || note.createdAt);
      const stats = noteEntryStats(note);
      const sent = stats.allSent;
      const hasCustomTitle = Boolean(note.title);
      const displayTitle = noteDisplayTitle(note, 64);
      const bodySnippet = hasCustomTitle
        ? notePreview(note.entries?.[0]?.text ?? note.text, 56)
        : (stats.isGroup && note.entries?.[1]?.text ? notePreview(note.entries[1].text, 52) : "");
      const showBodySnippet = Boolean(bodySnippet && bodySnippet !== displayTitle);
      return h_flex()
        .relative()
        .id(isFloating ? `fn-item-float-${note.id}` : `fn-item-${note.id}`)
        .w_full()
        .min_h(36)
        .px(8)
        .py(5)
        .gap(7)
        .flex_none()
        .items_start()
        .rounded(7)
        .when(isFloating, (e2) => e2.bg(t.raised ?? t.selected).border_1().border_color(AMBER).shadow_lg().cursor_grabbing())
        .when(!isFloating && !renaming && !this.noteDrag?.moving, (e2) => e2.cursor_pointer().hover((s) => s.bg(t.hover)))
        .when(!isFloating && renaming, (e2) => e2.bg(t.raised))
        .when(!isFloating, (e2) =>
          e2
            .top(shift)
            .spring("top", { response: snapNotes || isHiddenSource ? 0 : 165, damping: 0.86, epsilon: 0.25 })
            .when(isHiddenSource, (x) => x.opacity(0))
            .on_size_change((size) => {
              const z = this.noteZoom();
              const mh = size && typeof size.height === "number" ? size.height / z : 0;
              if (mh > 0 && Math.abs((this.noteHeights?.[note.id] ?? 0) - mh) > 0.5) {
                this.noteHeights ??= {};
                this.noteHeights[note.id] = mh;
              }
            }),
        )
        .when(!isFloating && !isHiddenSource, (e2) =>
          e2
            .on_hover((on, cx) => {
              if (this.noteDrag?.moving) return;
              if (on) this.hoveredId = note.id;
              else if (this.hoveredId === note.id) this.hoveredId = null;
              cx.notify();
            })
            .when(!renaming, (e3) =>
              e3
                .on_mouse_down("left", (e) => {
                  this.flushNoteSettle();
                  if (notes.length > 1 && e && typeof e.position?.y === "number") {
                    const z = this.noteZoom();
                    const dragH = this.noteHeight(note.id);
                    this.noteDrag = {
                      id: note.id,
                      list: notes,
                      from: idx,
                      to: idx,
                      startY: e.position.y / z,
                      startTop: this.noteSlotTop(notes, idx),
                      dy: 0,
                      moving: false,
                      dragHeight: dragH,
                    };
                  } else {
                    openFlashNoteWindow(note.id);
                  }
                })
                .on_mouse_move((e, cx) => this.handleNoteDragMove(e, cx))
                .on_mouse_up("left", (_e, cx) => {
                  if (this.noteDrag) {
                    cx.stop_propagation();
                    this.finishNoteDrag(cx);
                  }
                }),
            ),
        )
        .child(
          div()
            .mt(5)
            .size(6)
            .flex_none()
            .rounded(3)
            .bg(sent ? (t.done ?? t.dim) : AMBER),
        )
        .child(
          v_flex()
            .flex_1()
            .min_w(0)
            .gap(2)
            .when(renaming, (e2) =>
              e2.child(
                h_flex()
                  .id(`fn-rename-wrap-${note.id}`)
                  .w_full()
                  .gap(4)
                  .items_center()
                  .on_mouse_down("left", stop)
                  .on_click(stop)
                  .on_key_down((e, cx) => {
                    if (e?.key === "escape") {
                      cx.stop_propagation();
                      this.cancelInlineRename(cx);
                    }
                  })
                  .child(
                    Input.new(this.renameInput)
                      .id(`fn-rename-input-${note.id}`)
                      .flex_1()
                      .min_w(0)
                      .h(22)
                      .px(6)
                      .rounded(4)
                      .border_1()
                      .border_color(accent)
                      .bg(t.background)
                      .text_size(11.5)
                      .text_color(t.foreground),
                  )
                  .child(
                    div()
                      .id(`fn-rename-ok-${note.id}`)
                      .p(3)
                      .flex_none()
                      .rounded(4)
                      .text_color(accent)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.hover))
                      .tooltip(tr("Save name", "保存名称"))
                      .on_mouse_down("left", (_e, cx) => {
                        cx.stop_propagation();
                        this.commitInlineRename(cx);
                      })
                      .child(svg("mods/workspace-chrome/check.svg").size(11)),
                  )
                  .child(
                    div()
                      .id(`fn-rename-cancel-${note.id}`)
                      .p(3)
                      .flex_none()
                      .rounded(4)
                      .text_color(t.dim)
                      .cursor_pointer()
                      .hover((s) => s.bg(t.hover).text_color(t.foreground))
                      .tooltip(tr("Cancel", "取消"))
                      .on_mouse_down("left", (_e, cx) => {
                        cx.stop_propagation();
                        this.cancelInlineRename(cx);
                      })
                      .child(svg("mods/workspace-chrome/x.svg").size(11)),
                  ),
              ),
            )
            .when(!renaming, (e2) =>
              e2
                .child(
                  h_flex()
                    .w_full()
                    .min_w(0)
                    .gap(5)
                    .items_center()
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .truncate()
                        .text_size(12)
                        .when(hasCustomTitle || stats.isGroup, (x) => x.font_medium())
                        .text_color(sent ? t.muted : t.foreground)
                        .child(displayTitle),
                    )
                    .when(stats.isGroup, (e3) =>
                      e3.child(
                        div()
                          .id(isFloating ? `fn-group-badge-float-${note.id}` : `fn-group-badge-${note.id}`)
                          .flex_none()
                          .px(5)
                          .py(0.5)
                          .rounded(999)
                          .bg(sent ? t.raised : withAlpha(AMBER, "20"))
                          .text_size(9.5)
                          .font_semibold()
                          .text_color(sent ? t.dim : AMBER)
                          .child(tr(`${stats.total} ideas`, `${stats.total} 条`)),
                      ),
                    ),
                )
                .when(showBodySnippet, (e3) =>
                  e3.child(
                    div()
                      .w_full()
                      .min_w(0)
                      .truncate()
                      .text_size(10.5)
                      .text_color(t.dim)
                      .child(bodySnippet),
                  ),
                ),
            )
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .text_size(10)
                .text_color(t.dim)
                .when(Boolean(timeStr), (e2) => e2.child(div().flex_none().child(timeStr)))
                .when(stats.partialSent, (e2) =>
                  e2.child(
                    div()
                      .flex_none()
                      .px(4)
                      .py(0.5)
                      .rounded(4)
                      .bg(withAlpha(accent, "1c") ?? t.raised)
                      .text_color(accent)
                      .child(tr(`${stats.sentCount}/${stats.total} sent`, `已发 ${stats.sentCount}/${stats.total}`)),
                  ),
                )
                .when(sent && Boolean(note.sentTo), (e2) =>
                  e2.child(
                    div()
                      .min_w(0)
                      .truncate()
                      .px(4)
                      .py(0.5)
                      .rounded(4)
                      .bg(t.raised)
                      .text_color(t.muted)
                      .child(tr(`Sent → ${note.sentTo}`, `已发 → ${note.sentTo}`)),
                  ),
                ),
            ),
        )
        .when(hovered && !renaming, (e2) =>
          e2.child(
            h_flex()
              .mt(1)
              .gap(2)
              .flex_none()
              .items_center()
              .child(
                div()
                  .id(`fn-rename-${note.id}`)
                  .p(3)
                  .flex_none()
                  .rounded(4)
                  .text_color(t.dim)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.selected).text_color(t.foreground))
                  .tooltip(tr("Rename idea", "修改名称"))
                  .on_mouse_down("left", stop)
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.startInlineRename(note, cx);
                  })
                  .child(svg("mods/workspace-chrome/square-pen.svg").size(11)),
              )
              .child(
                div()
                  .id(`fn-del-${note.id}`)
                  .p(3)
                  .flex_none()
                  .rounded(4)
                  .text_color(t.dim)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.selected).text_color(t.blocked ?? t.foreground))
                  .tooltip(tr("Move to recycle bin", "移至回收站"))
                  .on_mouse_down("left", stop)
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.trashNote(note.id, cx);
                  })
                  .child(svg("mods/pane/trash.svg").size(11)),
              ),
          ),
        );
    };

    const floatingNote =
      noteMotion && (notes[noteMotion.from] || noteMotion.note)
        ? div()
            .id(`fn-float-${noteMotion.id}`)
            .absolute()
            .left(0)
            .right(0)
            .top(noteMotion.startTop + (this.noteDrag?.moving ? this.noteDrag.dy : noteMotion.destDy))
            .spring("top", { response: this.noteDrag?.moving ? 0 : 160, damping: 0.88, epsilon: 0.25 })
            .child(renderNoteRow(notes[noteMotion.from] ?? noteMotion.note, noteMotion.from, { isFloating: true }))
        : null;

    return v_flex()
      .relative()
      .id("fn-sidebar")
      .when(Boolean(this.listFocus), (el) => el.track_focus(this.listFocus))
      .on_mouse_move((e, cx) => this.handleNoteDragMove(e, cx))
      .on_mouse_up("left", (_e, cx) => {
        if (this.noteDrag) this.finishNoteDrag(cx);
      })
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .border_t(1)
      .border_color(t.border)
      .text_size(12.5)
      .child(
        h_flex()
          .px(16)
          .h(38)
          .pt(10)
          .pb(4)
          .flex_none()
          .items_center()
          .justify_between()
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg("mods/peek/lightbulb.svg").size(13).flex_none().text_color(unsentCount > 0 ? AMBER : t.dim))
              .child(div().text_size(11.5).font_medium().text_color(t.dim).child(tr("Flash Notes", "闪念清单")))
              .when(notes.length > 0, (el) =>
                el.child(
                  div()
                    .px(5)
                    .py(0.5)
                    .rounded(999)
                    .bg(unsentCount > 0 ? withAlpha(AMBER, "20") : t.raised)
                    .text_size(10)
                    .font_semibold()
                    .text_color(unsentCount > 0 ? AMBER : t.dim)
                    .child(String(notes.length)),
                ),
              ),
          )
          .child(
            h_flex()
              .gap(4)
              .items_center()
              .child(
                h_flex()
                  .id("fn-recycle")
                  .h(22)
                  .px(trashed.length > 0 ? 5 : 4)
                  .gap(3)
                  .items_center()
                  .justify_center()
                  .rounded(5)
                  .text_size(10.5)
                  .text_color(trashed.length > 0 ? t.muted : t.dim)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .tooltip(trashed.length > 0 ? tr(`Recycle bin (${trashed.length})`, `闪念回收站 (${trashed.length})`) : tr("Recycle bin", "闪念回收站"))
                  .on_click(() => openFlashNoteTrashMenu())
                  .child(svg("mods/workspace-chrome/archive.svg").size(13))
                  .when(trashed.length > 0, (el) =>
                    el.child(div().font_semibold().text_color(t.foreground).child(String(trashed.length))),
                  ),
              )
              .child(
                div()
                  .id("fn-list-new")
                  .size(22)
                  .flex()
                  .items_center()
                  .justify_center()
                  .rounded(5)
                  .text_color(t.dim)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .tooltip(withShortcut(tr("Capture a flash note", "新建闪念"), "mod.open:flash-note"))
                  .on_click(() => openFlashNoteWindow())
                  .child(svg("mods/workspace-chrome/plus.svg").size(13)),
              ),
          ),
      )
      .when(notes.length === 0, (el) =>
        el.child(
          v_flex()
            .px(16)
            .pt(2)
            .pb(8)
            .gap(8)
            .child(
              div()
                .text_size(11.5)
                .text_color(t.dim)
                .whitespace_normal()
                .child(tr("Click + to jot down thoughts or dispatch to an agent.", "点击 + 随时记录想法或派发给 Agent。")),
            )
            .when(trashed.length > 0, (el2) =>
              el2.child(
                h_flex()
                  .id("fn-recycle-empty-btn")
                  .px(8)
                  .h(26)
                  .gap(6)
                  .items_center()
                  .rounded(6)
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .text_size(11.5)
                  .text_color(t.foreground)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover))
                  .on_click(() => openFlashNoteTrashMenu())
                  .child(svg("mods/workspace-chrome/archive.svg").size(13).text_color(t.muted))
                  .child(tr(`Recycle bin (${trashed.length} recoverable)`, `打开回收站（${trashed.length} 条可恢复）`)),
              ),
            ),
        ),
      )
      .when(notes.length > 0, (el) =>
        el.child(
          v_flex()
            .id("fn-list")
            .flex_1()
            .min_h(0)
            .px(8)
            .pb(6)
            .overflow_y_scrollbar()
            .mode("always")
            .child(
              v_flex()
                .id("fn-list-track")
                .relative()
                .w_full()
                .flex_none()
                .gap(2)
                .children(
                  notes.map((note, idx) => {
                    const isHiddenSource = Boolean(noteMotion && noteMotion.from === idx);
                    let shift = 0;
                    if (noteMotion && idx !== noteMotion.from) {
                      if (noteMotion.from < noteMotion.to && idx > noteMotion.from && idx <= noteMotion.to) {
                        shift = -(noteMotion.dragHeight + 2);
                      } else if (noteMotion.from > noteMotion.to && idx >= noteMotion.to && idx < noteMotion.from) {
                        shift = noteMotion.dragHeight + 2;
                      }
                    }
                    return renderNoteRow(note, idx, { shift, isHiddenSource });
                  }),
                )
                .when(Boolean(floatingNote), (x) => x.child(floatingNote)),
            ),
        ),
      )
      .when(Boolean(this.noteDrag?.moving), (el) =>
        el.child(
          div()
            .id("fn-drag-shield")
            .absolute()
            .inset(0)
            .bg("#00000000")
            .cursor_grabbing()
            .on_mouse_move((e, cx) => this.handleNoteDragMove(e, cx))
            .on_mouse_up("left", (_e, cx) => {
              cx.stop_propagation();
              this.finishNoteDrag(cx);
            }),
        ),
      );
  }

  // ---- Mode B: Recycle bin popup --------------------------------------------

  renderTrashMenu(t) {
    const list = this.trashedNotes();
    const now = Date.now();
    const overlay = props()?.overlay;
    const accent = t.accent ?? "#3b82f6";
    const menuH = list.length === 0 ? 132 : Math.min(340, 52 + list.length * 56);

    return v_flex()
      .id("fn-trash-menu")
      .when(Boolean(this.handle), (el) => el.track_focus(this.handle))
      .w_full()
      .h(menuH)
      .min_h(0)
      .p(6)
      .gap(6)
      .bg(t.popover ?? t.background)
      .text_size(12.5)
      .on_key_down((e) => {
        if (e?.key === "escape" && overlay) closeOverlay(overlay);
      })
      .child(
        h_flex()
          .px(6)
          .pt(2)
          .pb(4)
          .flex_none()
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg("mods/workspace-chrome/archive.svg").size(14).flex_none().text_color(t.muted))
              .child(div().font_semibold().text_color(t.foreground).child(tr(`Flash Notes Bin (${list.length})`, `闪念回收站 (${list.length})`))),
          )
          .when(list.length > 0, (el) =>
            el.child(
              div()
                .id("fn-trash-empty")
                .px(7)
                .py(3)
                .rounded(5)
                .text_size(11)
                .text_color(t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.blocked ?? t.foreground))
                .on_mouse_down("left", stop)
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  setShared(FLASH_NOTES_TRASH_KEY, emptyNotesTrash());
                  notify({
                    title: tr("Recycle bin emptied", "已清空闪念回收站"),
                    body: "",
                    level: "info",
                  });
                  cx?.notify?.();
                })
                .child(tr("Empty", "清空")),
            ),
          ),
      )
      .when(list.length === 0, (el) =>
        el.child(
          v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px(16)
            .child(
              div()
                .text_size(12)
                .text_color(t.dim)
                .whitespace_normal()
                .child(tr("Recycle bin is empty. Deleted flash notes can be restored here.", "回收站为空，删除的闪念可在这里恢复。")),
            ),
        ),
      )
      .when(list.length > 0, (el) =>
        el.child(
          v_flex()
            .id("fn-trash-list")
            .flex_1()
            .min_h(0)
            .gap(4)
            .overflow_y_scrollbar()
            .children(
              list.map((it) => {
                const displayTitle = noteDisplayTitle(it, 44);
                const stats = noteEntryStats(it);
                const bodySnippet = it.title ? notePreview(it.entries?.[0]?.text ?? it.text, 36) : "";
                const age = it.trashedAt ? formatTrashAge(it.trashedAt, now) : "";
                const meta = [
                  stats.isGroup ? tr(`${stats.total} ideas`, `${stats.total} 条闪念`) : "",
                  bodySnippet && bodySnippet !== displayTitle ? bodySnippet : "",
                  it.sentTo ? tr(`Sent → ${it.sentTo}`, `已发 → ${it.sentTo}`) : "",
                  age ? tr(`Deleted ${age} ago`, `${age} 前删除`) : "",
                ].filter(Boolean).join(" · ");

                return h_flex()
                  .id(`fn-trash-row-${it.id}`)
                  .min_h(48)
                  .px(8)
                  .py(5)
                  .gap(8)
                  .flex_none()
                  .rounded(7)
                  .items_center()
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .child(
                    div()
                      .size(6)
                      .flex_none()
                      .rounded(3)
                      .bg(it.sentTo ? (t.done ?? t.dim) : AMBER),
                  )
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(2)
                      .child(div().truncate().text_size(12).font_medium().text_color(t.foreground).child(displayTitle))
                      .when(Boolean(meta), (e2) =>
                        e2.child(div().truncate().text_size(10.5).text_color(t.dim).child(meta)),
                      ),
                  )
                  .child(
                    h_flex()
                      .gap(4)
                      .flex_none()
                      .items_center()
                      .child(
                        div()
                          .id(`fn-trash-restore-${it.id}`)
                          .px(8)
                          .h(22)
                          .flex()
                          .items_center()
                          .rounded(5)
                          .bg(t.selected ?? t.hover)
                          .border_1()
                          .border_color(accent)
                          .text_size(11)
                          .font_medium()
                          .text_color(t.foreground)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover))
                          .on_mouse_down("left", stop)
                          .on_click((_e, cx) => {
                            cx.stop_propagation();
                            const res = restoreNoteFromTrash(
                              shared(FLASH_NOTES_KEY),
                              shared(FLASH_NOTES_TRASH_KEY),
                              it.id,
                              Date.now(),
                            );
                            if (res.note) {
                              setShared(FLASH_NOTES_KEY, res.list);
                              setShared(FLASH_NOTES_TRASH_KEY, res.trash);
                              notify({
                                title: tr("Flash note restored", "已恢复闪念"),
                                body: noteDisplayTitle(res.note, 56),
                                level: "info",
                              });
                            }
                            if (res.trash.length === 0 && overlay) closeOverlay(overlay);
                            else cx?.notify?.();
                          })
                          .child(tr("Restore", "恢复")),
                      )
                      .child(
                        div()
                          .id(`fn-trash-purge-${it.id}`)
                          .size(22)
                          .flex()
                          .items_center()
                          .justify_center()
                          .rounded(5)
                          .text_color(t.dim)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.blocked ?? "#ef4444"))
                          .tooltip(tr("Delete permanently", "彻底删除"))
                          .on_mouse_down("left", stop)
                          .on_click((_e, cx) => {
                            cx.stop_propagation();
                            const nextTrash = purgeTrashedNote(shared(FLASH_NOTES_TRASH_KEY), it.id);
                            setShared(FLASH_NOTES_TRASH_KEY, nextTrash);
                            cx?.notify?.();
                          })
                          .child(svg("mods/workspace-chrome/x.svg").size(12)),
                      ),
                  );
              }),
            ),
        ),
      );
  }

  // ---- Mode C: Floating Flash Note composer window --------------------------

  renderComposeWindow(t, cx) {
    const p = props() ?? {};
    const requestedNoteId = p.noteId ?? null;
    if (requestedNoteId !== this.lastPropNoteId) {
      this.lastPropNoteId = requestedNoteId;
      const found = requestedNoteId ? this.savedNotes().find((n) => n.id === requestedNoteId) ?? null : null;
      this.loadNoteIntoComposer(found, false);
    }

    const accent = t.accent ?? "#3b82f6";
    const candidates = resolveExistingAgents(terminals(), agents(), panes(), focused());
    const chosenExisting = this.chosenExisting(candidates);
    const presets = this.availablePresets();
    const chosenPreset = this.chosenPreset();
    const spaceList = spaces();
    const chosenSpace = this.activeSpace();
    const nonEmptyEntries = this.collectNonEmptyEntries();
    const nonEmptyCount = nonEmptyEntries.length;
    const selectedNonEmpty = nonEmptyEntries.filter((e) => e.selected);
    const selectedCount = selectedNonEmpty.length;
    const unsentCount = nonEmptyEntries.filter((e) => !e.sentTo).length;
    const hasPartialSent = unsentCount > 0 && unsentCount < nonEmptyCount;
    const isMultiEntry = this.entryStates.length > 1;
    const hasMultiNonEmpty = nonEmptyCount > 1;
    const targetEntries = this.collectTargetEntries();
    const hasText = nonEmptyCount > 0;
    const targetReady = this.targetMode === "new" ? Boolean(chosenPreset) : Boolean(chosenExisting);
    const isWorkingAcp = Boolean(this.targetMode === "existing" && chosenExisting?.kind === "acp" && chosenExisting?.status === "working");
    const canSend = targetEntries.length > 0 && !this.busy && targetReady;
    const statusColor = (st) => (st === "working" ? (t.working ?? "#3b82f6") : st === "blocked" ? (t.blocked ?? "#ef4444") : st === "done" ? (t.done ?? "#22c55e") : t.dim);

    const modeTab = (id, mode, label) => {
      const active = this.targetMode === mode;
      return h_flex()
        .id(id)
        .flex_1()
        .h(28)
        .px(10)
        .items_center()
        .justify_center()
        .rounded(6)
        .text_size(11.5)
        .font_medium()
        .bg(active ? t.selected : t.background)
        .border_1()
        .border_color(active ? accent : t.border)
        .text_color(active ? t.foreground : t.muted)
        .cursor_pointer()
        .hover((s) => s.bg(active ? t.selected : t.hover).text_color(t.foreground))
        .on_mouse_down("left", stop)
        .on_click((_e, cx) => {
          this.targetMode = mode;
          this.error = null;
          this.closeAllPopups();
          cx.notify();
        })
        .child(label);
    };

    const existingTrigger = h_flex()
      .id("fn-existing-btn")
      .w_full()
      .h(30)
      .px(9)
      .gap(7)
      .items_center()
      .rounded(7)
      .bg(t.background)
      .border_1()
      .border_color(this.existingPopupOpen ? accent : t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", stop)
      .on_click((_e, cx) => {
        const next = !this.existingPopupOpen;
        this.closeAllPopups();
        this.existingPopupOpen = next;
        cx.notify();
      })
      .when(Boolean(chosenExisting), (e) => e.child(div().size(6).flex_none().rounded(3).bg(statusColor(chosenExisting.status))))
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(12)
          .text_color(chosenExisting ? t.foreground : t.dim)
          .child(chosenExisting ? chosenExisting.label : tr("No live agents running", "暂无运行中的 Agent")),
      )
      .when(Boolean(chosenExisting), (e) =>
        e.child(
          div()
            .flex_none()
            .px(4)
            .py(1)
            .rounded(4)
            .bg(t.raised)
            .text_size(9.5)
            .text_color(t.dim)
            .child(chosenExisting.kind === "acp" ? "ACP" : "CLI"),
        ),
      )
      .child(svg("mods/workspace-chrome/chevron-down.svg").size(11).flex_none().text_color(t.dim));

    const presetTrigger = h_flex()
      .id("fn-preset-btn")
      .flex_1()
      .min_w(0)
      .h(30)
      .px(9)
      .gap(7)
      .items_center()
      .rounded(7)
      .bg(t.background)
      .border_1()
      .border_color(this.presetPopupOpen ? accent : t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", stop)
      .on_click((_e, cx) => {
        const next = !this.presetPopupOpen;
        this.closeAllPopups();
        this.presetPopupOpen = next;
        cx.notify();
      })
      .when(Boolean(chosenPreset?.icon), (e) =>
        e.child(
          chosenPreset.tint
            ? svg(chosenPreset.icon).size(13).flex_none().text_color(t.foreground)
            : image(chosenPreset.icon).size(13).flex_none(),
        ),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(12)
          .text_color(t.foreground)
          .child(chosenPreset?.name || chosenPreset?.key || "Agent"),
      )
      .child(svg("mods/workspace-chrome/chevron-down.svg").size(11).flex_none().text_color(t.dim));

    const presetCat = catalogForPreset(this.agentCatalog, chosenPreset?.key, this.selectedModel);
    const effLabels = EFFORT_LABELS();
    const modelDisplay = this.selectedModel
      ? this.selectedModel
      : presetCat.defaultModel
        ? `${tr("Default", "默认")} (${presetCat.defaultModel})`
        : tr("Default (agent choice)", "默认（跟随 Agent 配置）");

    const modelTrigger = h_flex()
      .id("fn-model-btn")
      .flex_1()
      .min_w(0)
      .h(28)
      .px(9)
      .gap(6)
      .items_center()
      .rounded(7)
      .bg(this.selectedModel ? withAlpha(accent, "16") : t.background)
      .border_1()
      .border_color(this.modelPopupOpen || this.selectedModel ? accent : t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", stop)
      .on_click((_e, cx) => {
        const next = !this.modelPopupOpen;
        this.closeAllPopups();
        this.modelPopupOpen = next;
        cx.notify();
      })
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(11.5)
          .text_color(this.selectedModel ? t.foreground : t.muted)
          .child(modelDisplay),
      )
      .child(svg("mods/workspace-chrome/chevron-down.svg").size(11).flex_none().text_color(t.dim));

    const effChip = (id, val, label) => {
      const active = (this.selectedEffort || "") === val;
      return h_flex()
        .id(id)
        .h(24)
        .px(8)
        .flex_none()
        .items_center()
        .rounded(6)
        .border_1()
        .border_color(active ? accent : t.border)
        .bg(active ? (withAlpha(accent, "1f") ?? t.selected) : t.background)
        .text_size(11)
        .when(active, (x) => x.font_semibold())
        .text_color(active ? t.foreground : t.muted)
        .cursor_pointer()
        .hover((s) => s.bg(active ? (withAlpha(accent, "29") ?? t.selected) : t.hover).text_color(t.foreground))
        .on_mouse_down("left", stop)
        .on_click((_e, cx) => {
          this.selectedEffort = val;
          this.closeAllPopups();
          cx.notify();
        })
        .child(label);
    };

    const spaceTrigger = h_flex()
      .id("fn-space-btn")
      .w_full()
      .h(28)
      .px(9)
      .gap(6)
      .items_center()
      .rounded(7)
      .bg(t.background)
      .border_1()
      .border_color(this.spacePopupOpen ? accent : t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", stop)
      .on_click((_e, cx) => {
        const next = !this.spacePopupOpen;
        this.closeAllPopups();
        this.spacePopupOpen = next;
        cx.notify();
      })
      .child(svg("mods/workspace-chrome/folder.svg").size(12).flex_none().text_color(t.dim))
      .child(
        div()
          .flex_1()
          .min_w(0)
          .truncate()
          .text_size(11.5)
          .text_color(chosenSpace ? t.foreground : t.dim)
          .child(chosenSpace ? `${chosenSpace.name || tr("Space", "工作区")}${chosenSpace.folder ? ` · ${chosenSpace.folder}` : ""}` : tr("Default workspace", "默认工作区")),
      )
      .child(svg("mods/workspace-chrome/chevron-down.svg").size(11).flex_none().text_color(t.dim));

    const rtChip = (id, rt, label) => {
      const active = this.newRuntimeMode === rt;
      return h_flex()
        .id(id)
        .h(30)
        .px(9)
        .flex_none()
        .items_center()
        .rounded(7)
        .border_1()
        .border_color(active ? accent : t.border)
        .bg(active ? t.selected : t.background)
        .text_size(11)
        .font_medium()
        .text_color(active ? t.foreground : t.muted)
        .cursor_pointer()
        .hover((s) => s.bg(active ? t.selected : t.hover))
        .on_mouse_down("left", stop)
        .on_click((_e, cx) => {
          const prevKey = this.chosenPreset()?.key ?? null;
          this.newRuntimeMode = rt;
          const nextKey = this.chosenPreset()?.key ?? null;
          if (prevKey !== nextKey) {
            this.selectedModel = "";
            this.selectedEffort = "";
            this.customModelInput?.set_value?.("");
          }
          this.closeAllPopups();
          cx.notify();
        })
        .child(label);
    };

    return v_flex()
      .id("flash-note-window")
      .size_full()
      .min_h(0)
      .rounded(12)
      .overflow_hidden()
      .bg(t.popover ?? t.background)
      .border_1()
      .border_color(t.border)
      .shadow_2xl()
      .text_color(t.foreground)
      .text_size(12.5)
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          if (this.existingPopupOpen || this.presetPopupOpen || this.modelPopupOpen || this.spacePopupOpen) {
            this.closeAllPopups();
            cx.stop_propagation();
            cx.notify();
          }
        }
      })
      // Draggable header
      .child(
        h_flex()
          .id("fn-header")
          .h(42)
          .px(14)
          .gap(8)
          .flex_none()
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .bg(t.surface ?? t.chrome)
          .child(
            div()
              .size(22)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .rounded(6)
              .bg(withAlpha(AMBER, "22"))
              .child(svg("mods/peek/lightbulb.svg").size(13).text_color(AMBER)),
          )
          .child(
            div()
              .font_semibold()
              .text_size(13)
              .text_color(t.foreground)
              .child(
                isMultiEntry
                  ? (this.editingNoteId ? tr("Edit Flash Group", "编辑闪念组") : tr("Flash Note Group", "闪念组"))
                  : (this.editingNoteId ? tr("Edit Flash Note", "编辑闪念") : tr("Flash Note", "闪念")),
              ),
          )
          .when(isMultiEntry, (el) =>
            el.child(
              div()
                .px(5)
                .py(0.5)
                .rounded(999)
                .bg(withAlpha(AMBER, "22"))
                .text_size(10)
                .font_semibold()
                .text_color(AMBER)
                .child(tr(`${this.entryStates.length} ideas`, `${this.entryStates.length} 条`)),
            ),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11)
              .text_color(t.dim)
              .child(tr("Record multiple ideas · Send selected or whole group", "可记录多条闪念 · 支持单条/多选或整组发送")),
          )
          .child(
            div()
              .id("fn-close")
              .size(24)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .rounded(6)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_mouse_down("left", stop)
              .on_click(() => {
                if (props()?.overlay) closeOverlay(props().overlay);
              })
              .child(svg("mods/workspace-chrome/x.svg").size(13)),
          ),
      )
      // Body: optional title + editor + target selector
      .child(
        v_flex()
          .flex_1()
          .min_h(0)
          .p(12)
          .gap(8)
          .child(
            h_flex()
              .id("fn-title-wrap")
              .h(32)
              .w_full()
              .px(10)
              .gap(6)
              .flex_none()
              .items_center()
              .rounded(8)
              .bg(t.background)
              .border_1()
              .border_color(this.titleFocused ? accent : t.border)
              .cursor_text()
              .on_mouse_down("left", stop)
              .on_click(() => this.titleInput?.focus?.())
              .child(svg("mods/workspace-chrome/square-pen.svg").size(12).flex_none().text_color(t.dim))
              .child(
                Input.new(this.titleInput)
                  .id("fn-title-input")
                  .flex_1()
                  .min_w(0)
                  .h(24)
                  .text_size(12.5)
                  .text_color(t.foreground),
              ),
          )
          .child(
            h_flex()
              .id("fn-group-bar")
              .w_full()
              .px(2)
              .gap(6)
              .flex_none()
              .items_center()
              .justify_between()
              .on_mouse_down("left", stop)
              .child(
                h_flex()
                  .gap(5)
                  .items_center()
                  .min_w(0)
                  .child(
                    div()
                      .text_size(11)
                      .font_medium()
                      .text_color(t.muted)
                      .child(
                        isMultiEntry
                          ? tr(`Ideas (${selectedCount}/${nonEmptyCount} selected)`, `闪念条目（已选 ${selectedCount}/${nonEmptyCount}）`)
                          : tr("Idea", "闪念内容"),
                      ),
                  )
                  .when(isMultiEntry, (e2) =>
                    e2
                      .child(
                        div()
                          .id("fn-select-all")
                          .px(5)
                          .py(1)
                          .rounded(4)
                          .text_size(10.5)
                          .text_color(selectedCount === nonEmptyCount && nonEmptyCount > 0 ? accent : t.dim)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.foreground))
                          .on_click((_e, cx) => this.selectEntries("all", cx))
                          .child(tr("All", "全选")),
                      )
                      .when(hasPartialSent, (e3) =>
                        e3.child(
                          div()
                            .id("fn-select-unsent")
                            .px(5)
                            .py(1)
                            .rounded(4)
                            .text_size(10.5)
                            .text_color(t.dim)
                            .cursor_pointer()
                            .hover((s) => s.bg(t.hover).text_color(t.foreground))
                            .on_click((_e, cx) => this.selectEntries("unsent", cx))
                            .child(tr("Unsent", "仅未发")),
                        ),
                      )
                      .child(
                        div()
                          .id("fn-select-none")
                          .px(5)
                          .py(1)
                          .rounded(4)
                          .text_size(10.5)
                          .text_color(t.dim)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.foreground))
                          .on_click((_e, cx) => this.selectEntries("none", cx))
                          .child(tr("Clear", "取消勾选")),
                      ),
                  ),
              )
              .child(
                h_flex()
                  .id("fn-add-entry")
                  .h(22)
                  .px(7)
                  .gap(4)
                  .flex_none()
                  .items_center()
                  .rounded(5)
                  .bg(withAlpha(accent, "18") ?? t.raised)
                  .border_1()
                  .border_color(withAlpha(accent, "44") ?? t.border)
                  .text_size(11)
                  .font_medium()
                  .text_color(accent)
                  .cursor_pointer()
                  .hover((s) => s.bg(withAlpha(accent, "28") ?? t.hover))
                  .tooltip(tr("Add another flash note input to this group", "在当前闪念组中新增一个输入区域"))
                  .on_click((_e, cx) => this.addEntry(cx))
                  .child(svg("mods/workspace-chrome/plus.svg").size(11))
                  .child(tr("Add Idea", "添加闪念")),
              ),
          )
          .when(this.polishUndoText != null && !isMultiEntry, (el) =>
            el.child(
              h_flex()
                .id("fn-polish-banner")
                .w_full()
                .px(9)
                .py(5)
                .gap(6)
                .flex_none()
                .items_center()
                .rounded(6)
                .bg(withAlpha("#a855f7", "16"))
                .border_1()
                .border_color(withAlpha("#a855f7", "44"))
                .child(svg("mods/peek/sparkles.svg").size(11.5).flex_none().text_color("#a855f7"))
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .truncate()
                    .text_size(11)
                    .text_color(t.foreground)
                    .child(this.polishSummary ? tr(`Polished · ${this.polishSummary}`, `已润色 · ${this.polishSummary}`) : tr("Polished by AI", "已由 AI 润色")),
                )
                .child(
                  div()
                    .id("fn-polish-undo")
                    .flex_none()
                    .px(6)
                    .py(1)
                    .rounded(4)
                    .bg(t.background)
                    .border_1()
                    .border_color(t.border)
                    .text_size(10.5)
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .tooltip(withShortcut(tr("Undo last polish and restore original input", "撤销本次润色并恢复润色前的内容"), "prompt.polish_undo"))
                    .on_mouse_down("left", stop)
                    .on_click((_e, cx) => {
                      cx.stop_propagation();
                      this.revertPolish(cx);
                    })
                    .child(tr("↩ Undo", "↩ 反悔恢复")),
                ),
            ),
          )
          .child(
            v_flex()
              .id("fn-entries-list")
              .flex_1()
              .min_h(100)
              .w_full()
              .gap(8)
              .overflow_y_scrollbar()
              .on_mouse_down("left", stop)
              .children(
                this.entryStates.map((entry, idx) => {
                  const isActive = idx === this.activeEntryIndex;
                  const val = String(entry.input?.value?.() ?? "").trim();
                  const entryHasText = val.length > 0;
                  const canQuickSend = entryHasText && !this.busy && targetReady;
                  const checked = Boolean(entry.selected);
                  const borderCol = entry.focused
                    ? accent
                    : (isMultiEntry && checked && this.sendScope === "selected" ? withAlpha(accent, "66") : t.border);

                  return v_flex()
                    .id(isActive ? "fn-input-wrap" : `fn-entry-card-${idx}`)
                    .w_full()
                    .when(!isMultiEntry, (el) => el.flex_1().min_h(90))
                    .when(isMultiEntry, (el) => el.flex_none())
                    .rounded(8)
                    .overflow_hidden()
                    .bg(t.background)
                    .border_1()
                    .border_color(borderCol)
                    .when(isMultiEntry, (card) =>
                      card.child(
                        h_flex()
                          .id(`fn-entry-hdr-${idx}`)
                          .h(26)
                          .px(8)
                          .gap(6)
                          .flex_none()
                          .items_center()
                          .justify_between()
                          .bg(isActive ? (withAlpha(accent, "10") ?? t.surface) : (t.surface ?? t.raised))
                          .border_b(1)
                          .border_color(t.border)
                          .child(
                            h_flex()
                              .gap(6)
                              .items_center()
                              .min_w(0)
                              .flex_1()
                              .child(
                                div()
                                  .id(`fn-entry-check-${idx}`)
                                  .size(14)
                                  .flex_none()
                                  .flex()
                                  .items_center()
                                  .justify_center()
                                  .rounded(3.5)
                                  .border_1()
                                  .border_color(checked ? accent : t.border)
                                  .bg(checked ? accent : t.background)
                                  .cursor_pointer()
                                  .tooltip(checked ? tr("Selected for sending", "已勾选，发送时包含此条") : tr("Click to include when sending selected", "勾选以便选择发送此条"))
                                  .on_mouse_down("left", stop)
                                  .on_click((_e, cx) => {
                                    cx.stop_propagation();
                                    this.toggleEntrySelected(idx, cx);
                                  })
                                  .when(checked, (ck) =>
                                    ck.child(svg("mods/workspace-chrome/check.svg").size(9.5).text_color(t.solid_foreground ?? "#ffffff")),
                                  ),
                              )
                              .child(
                                div()
                                  .flex_none()
                                  .text_size(10.5)
                                  .font_semibold()
                                  .text_color(isActive ? accent : t.muted)
                                  .child(`#${idx + 1}`),
                              )
                              .when(Boolean(entry.sentTo), (s) =>
                                s.child(
                                  div()
                                    .min_w(0)
                                    .truncate()
                                    .px(5)
                                    .py(0.5)
                                    .rounded(4)
                                    .bg(t.raised)
                                    .text_size(9.5)
                                    .text_color(t.done ?? t.muted)
                                    .child(tr(`Sent → ${entry.sentTo}`, `已发 → ${entry.sentTo}`)),
                                ),
                              ),
                          )
                          .child(
                            h_flex()
                              .gap(3)
                              .flex_none()
                              .items_center()
                              .when(entryHasText, (e2) =>
                                e2.child(
                                  h_flex()
                                    .id(`fn-entry-send-${idx}`)
                                    .h(18)
                                    .px(5)
                                    .gap(3)
                                    .items_center()
                                    .rounded(4)
                                    .bg(canQuickSend ? withAlpha(accent, "1a") : t.raised)
                                    .text_size(10)
                                    .font_medium()
                                    .text_color(canQuickSend ? accent : t.dim)
                                    .when(canQuickSend, (b) =>
                                      b
                                        .cursor_pointer()
                                        .hover((st) => st.bg(withAlpha(accent, "2c")))
                                        .on_mouse_down("left", stop)
                                        .on_click((_e, cx) => {
                                          cx.stop_propagation();
                                          this.sendSingleEntry(idx, cx);
                                        }),
                                    )
                                    .tooltip(tr("Send only this flash note to the selected agent", "仅将此条闪念发给当前目标 Agent"))
                                    .child(svg("mods/peek/arrow-up.svg").size(9.5))
                                    .child(tr("Send", "发此条")),
                                ),
                              )
                              .child(
                                div()
                                  .id(`fn-entry-del-${idx}`)
                                  .size(18)
                                  .flex()
                                  .items_center()
                                  .justify_center()
                                  .rounded(4)
                                  .text_color(t.dim)
                                  .cursor_pointer()
                                  .hover((st) => st.bg(t.hover).text_color(t.blocked ?? t.foreground))
                                  .tooltip(tr("Remove this input area", "删除此闪念输入区"))
                                  .on_mouse_down("left", stop)
                                  .on_click((_e, cx) => {
                                    cx.stop_propagation();
                                    this.removeEntry(idx, cx);
                                  })
                                  .child(svg("mods/workspace-chrome/x.svg").size(10.5)),
                              ),
                          ),
                      ),
                    )
                    .when(isMultiEntry && entry.polishUndoText != null, (card) =>
                      card.child(
                        h_flex()
                          .id(`fn-polish-banner-${idx}`)
                          .w_full()
                          .px(8)
                          .py(3)
                          .gap(6)
                          .flex_none()
                          .items_center()
                          .bg(withAlpha("#a855f7", "14"))
                          .border_b(1)
                          .border_color(withAlpha("#a855f7", "33"))
                          .child(svg("mods/peek/sparkles.svg").size(10.5).flex_none().text_color("#a855f7"))
                          .child(
                            div()
                              .flex_1()
                              .min_w(0)
                              .truncate()
                              .text_size(10.5)
                              .text_color(t.foreground)
                              .child(entry.polishSummary ? tr(`Polished · ${entry.polishSummary}`, `已润色 · ${entry.polishSummary}`) : tr("Polished by AI", "已由 AI 润色")),
                          )
                          .child(
                            div()
                              .id(`fn-polish-undo-${idx}`)
                              .flex_none()
                              .px(5)
                              .py(0.5)
                              .rounded(4)
                              .bg(t.background)
                              .border_1()
                              .border_color(t.border)
                              .text_size(10)
                              .text_color(t.muted)
                              .cursor_pointer()
                              .hover((s) => s.bg(t.hover).text_color(t.foreground))
                              .tooltip(withShortcut(tr("Undo last polish and restore original input", "撤销本次润色并恢复润色前的内容"), "prompt.polish_undo"))
                              .on_mouse_down("left", stop)
                              .on_click((_e, cx) => {
                                cx.stop_propagation();
                                this.revertPolish(cx, idx);
                              })
                              .child(tr("↩ Undo", "↩ 反悔恢复")),
                          ),
                      ),
                    )
                    .child(
                      div()
                        .id(`fn-entry-input-${idx}`)
                        .w_full()
                        .when(!isMultiEntry, (el) => el.flex_1().min_h(84))
                        .when(isMultiEntry, (el) => el.min_h(62))
                        .px(10)
                        .py(7)
                        .cursor_text()
                        .on_mouse_down("left", stop)
                        .on_click(() => {
                          this.activeEntryIndex = idx;
                          entry.input?.focus?.();
                        })
                        .child(
                          Textarea.new(entry.input)
                            .when(!isMultiEntry, (el) => el.size_full())
                            .when(isMultiEntry, (el) => el.w_full().min_h(48))
                            .text_size(12.5)
                            .line_height("19px")
                            .text_color(t.foreground),
                        ),
                    );
                }),
              )
              .when(isMultiEntry, (listEl) =>
                listEl.child(
                  h_flex()
                    .id("fn-add-entry-bottom")
                    .w_full()
                    .h(28)
                    .px(10)
                    .gap(5)
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(7)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.surface ?? t.raised)
                    .text_size(11)
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => this.addEntry(cx))
                    .child(svg("mods/workspace-chrome/plus.svg").size(11))
                    .child(tr("Add another flash note", "再添加一条闪念")),
                ),
              ),
          )
          // Dispatch target selector card
          .child(
            v_flex()
              .id("fn-target-card")
              .flex_none()
              .p(8)
              .gap(8)
              .rounded(8)
              .bg(t.surface ?? t.raised)
              .border_1()
              .border_color(t.border)
              .on_mouse_down("left", stop)
              .when(hasMultiNonEmpty, (el) =>
                el.child(
                  h_flex()
                    .id("fn-send-scope")
                    .w_full()
                    .gap(6)
                    .items_center()
                    .child(
                      div()
                        .flex_none()
                        .text_size(11)
                        .text_color(t.dim)
                        .child(tr("Scope", "发送范围")),
                    )
                    .child(
                      h_flex()
                        .id("fn-scope-all")
                        .flex_1()
                        .h(24)
                        .px(8)
                        .items_center()
                        .justify_center()
                        .rounded(5)
                        .border_1()
                        .border_color(this.sendScope === "all" ? accent : t.border)
                        .bg(this.sendScope === "all" ? (withAlpha(accent, "1f") ?? t.selected) : t.background)
                        .text_size(11)
                        .when(this.sendScope === "all", (x) => x.font_semibold())
                        .text_color(this.sendScope === "all" ? t.foreground : t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover).text_color(t.foreground))
                        .on_click((_e, cx) => {
                          this.sendScope = "all";
                          this.error = null;
                          cx.notify();
                        })
                        .child(tr(`Whole Group (${nonEmptyCount})`, `整组发送 (${nonEmptyCount})`)),
                    )
                    .child(
                      h_flex()
                        .id("fn-scope-selected")
                        .flex_1()
                        .h(24)
                        .px(8)
                        .items_center()
                        .justify_center()
                        .rounded(5)
                        .border_1()
                        .border_color(this.sendScope === "selected" ? accent : t.border)
                        .bg(this.sendScope === "selected" ? (withAlpha(accent, "1f") ?? t.selected) : t.background)
                        .text_size(11)
                        .when(this.sendScope === "selected", (x) => x.font_semibold())
                        .text_color(this.sendScope === "selected" ? t.foreground : t.muted)
                        .cursor_pointer()
                        .hover((s) => s.bg(t.hover).text_color(t.foreground))
                        .on_click((_e, cx) => {
                          this.sendScope = "selected";
                          this.error = null;
                          cx.notify();
                        })
                        .child(tr(`Selected (${selectedCount}/${nonEmptyCount})`, `发送已选 (${selectedCount}/${nonEmptyCount})`)),
                    ),
                ),
              )
              .child(
                h_flex()
                  .w_full()
                  .gap(6)
                  .child(modeTab("fn-mode-existing", "existing", tr("Send to Existing Agent", "发给已有 Agent")))
                  .child(modeTab("fn-mode-new", "new", tr("Send to New Agent", "发给新开 Agent"))),
              )
              .when(this.targetMode === "existing", (el) =>
                el.child(
                  h_flex()
                    .w_full()
                    .gap(6)
                    .items_center()
                    .child(
                      div()
                        .flex_1()
                        .min_w(0)
                        .child(
                          Popup.new("fn-existing-popup", existingTrigger)
                            .anchor("top_left")
                            .when(this.existingPopupOpen, (pop) =>
                              pop.content(
                                v_flex()
                                  .w(340)
                                  .max_h(210)
                                  .p(4)
                                  .gap(2)
                                  .rounded(8)
                                  .bg(t.popover ?? t.background)
                                  .border_1()
                                  .border_color(t.border)
                                  .shadow_lg()
                                  .overflow_y_scroll()
                                  .on_mouse_down("left", stop)
                                  .children(
                                    candidates.length > 0
                                      ? candidates.map((c) => {
                                          const active = chosenExisting?.address === c.address;
                                          return h_flex()
                                            .id(`fn-ex-${c.address}`)
                                            .h(28)
                                            .px(8)
                                            .gap(6)
                                            .items_center()
                                            .rounded(6)
                                            .cursor_pointer()
                                            .when(active, (x) => x.bg(t.selected))
                                            .hover((s) => s.bg(t.hover))
                                            .on_click((_e, cx) => {
                                              this.selectedExisting = c.address;
                                              this.existingPopupOpen = false;
                                              cx.notify();
                                            })
                                            .child(div().size(6).flex_none().rounded(3).bg(statusColor(c.status)))
                                            .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(t.foreground).child(c.label))
                                            .when(Boolean(c.spaceName), (x) =>
                                              x.child(div().max_w(80).truncate().text_size(10).text_color(t.dim).child(c.spaceName)),
                                            )
                                            .when(c.isSideChat, (x) =>
                                              x.child(
                                                div()
                                                  .flex_none()
                                                  .px(4)
                                                  .py(0.5)
                                                  .rounded(4)
                                                  .bg(withAlpha("#14b8a6", "22"))
                                                  .text_size(9.5)
                                                  .text_color("#14b8a6")
                                                  .child(tr("Side", "旁路")),
                                              ),
                                            )
                                            .child(
                                              div()
                                                .flex_none()
                                                .px(4)
                                                .py(0.5)
                                                .rounded(4)
                                                .bg(t.raised)
                                                .text_size(9.5)
                                                .text_color(t.dim)
                                                .child(c.kind === "acp" ? "ACP" : "CLI"),
                                            );
                                        })
                                      : [div().p(8).text_size(11.5).text_color(t.dim).child(tr("No live agents running", "暂无运行中的 Agent"))],
                                  ),
                              ),
                            ),
                        ),
                    )
                    .when(isWorkingAcp, (e2) =>
                      e2.child(
                        div()
                          .id("fn-steer-toggle")
                          .h(30)
                          .px(8)
                          .flex()
                          .flex_none()
                          .items_center()
                          .rounded(7)
                          .border_1()
                          .border_color(this.steerMode ? (t.blocked ?? AMBER) : t.border)
                          .bg(this.steerMode ? withAlpha(t.blocked ?? AMBER, "20") : t.background)
                          .text_size(11)
                          .text_color(this.steerMode ? (t.blocked ?? AMBER) : t.muted)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover))
                          .tooltip(tr("Queue after current turn or steer immediately", "当前 Agent 运行中：可排队等待或抢占立即发送"))
                          .on_click((_e, cx) => {
                            this.steerMode = !this.steerMode;
                            cx.notify();
                          })
                          .child(this.steerMode ? tr("Steer", "抢占") : tr("Queue", "排队")),
                      ),
                    ),
                ),
              )
              .when(this.targetMode === "new", (el) =>
                el
                  .child(
                    h_flex()
                      .w_full()
                      .gap(6)
                      .items_center()
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .child(
                            Popup.new("fn-preset-popup", presetTrigger)
                              .anchor("top_left")
                              .when(this.presetPopupOpen, (pop) =>
                                pop.content(
                                  v_flex()
                                    .w(240)
                                    .max_h(210)
                                    .p(4)
                                    .gap(2)
                                    .rounded(8)
                                    .bg(t.popover ?? t.background)
                                    .border_1()
                                    .border_color(t.border)
                                    .shadow_lg()
                                    .overflow_y_scroll()
                                    .on_mouse_down("left", stop)
                                    .children(
                                      presets.map((a) => {
                                        const active = chosenPreset?.key === a.key;
                                        return h_flex()
                                          .id(`fn-pr-${a.key}`)
                                          .h(28)
                                          .px(8)
                                          .gap(7)
                                          .items_center()
                                          .rounded(6)
                                          .cursor_pointer()
                                          .when(active, (x) => x.bg(t.selected))
                                          .hover((s) => s.bg(t.hover))
                                          .on_click((_e, cx) => {
                                            this.selectPreset(a.key);
                                            this.presetPopupOpen = false;
                                            cx.notify();
                                          })
                                          .when(Boolean(a.icon), (x) =>
                                            x.child(a.tint ? svg(a.icon).size(13).flex_none().text_color(t.foreground) : image(a.icon).size(13).flex_none()),
                                          )
                                          .child(div().flex_1().min_w(0).truncate().text_size(12).text_color(a.installed ? t.foreground : t.dim).child(a.name || a.key))
                                          .when(!a.installed, (x) => x.child(div().text_size(10).text_color(t.dim).child(tr("not installed", "未安装"))));
                                      }),
                                    ),
                                ),
                              ),
                          ),
                      )
                      .child(rtChip("fn-new-rt-acp", "acp", "ACP"))
                      .child(rtChip("fn-new-rt-cli", "cli", tr("CLI", "终端"))),
                  )
                  .child(
                    h_flex()
                      .w_full()
                      .gap(8)
                      .items_center()
                      .child(div().w(52).flex_none().text_size(11).text_color(t.dim).child(tr("Model", "模型类型")))
                      .child(
                        div()
                          .flex_1()
                          .min_w(0)
                          .child(
                            Popup.new("fn-model-popup", modelTrigger)
                              .anchor("top_left")
                              .when(this.modelPopupOpen, (pop) =>
                                pop.content(
                                  v_flex()
                                    .id("fn-model-dropdown")
                                    .w(320)
                                    .p(5)
                                    .gap(4)
                                    .rounded(8)
                                    .bg(t.popover ?? t.background)
                                    .border_1()
                                    .border_color(t.border)
                                    .shadow_lg()
                                    .on_mouse_down("left", stop)
                                    .on_scroll_wheel(stop)
                                    .child(
                                      v_flex()
                                        .max_h(190)
                                        .gap(2)
                                        .overflow_y_scroll()
                                        .child(
                                          h_flex()
                                            .id("fn-model-opt-default")
                                            .h(26)
                                            .px(8)
                                            .gap(6)
                                            .flex_none()
                                            .items_center()
                                            .justify_between()
                                            .rounded(6)
                                            .cursor_pointer()
                                            .when(!this.selectedModel, (x) => x.bg(withAlpha(accent, "1f") ?? t.selected))
                                            .hover((s) => s.bg(t.hover))
                                            .on_click((_e, cx) => {
                                              this.selectModel("");
                                              this.modelPopupOpen = false;
                                              cx.notify();
                                            })
                                            .child(
                                              div()
                                                .flex_1()
                                                .min_w(0)
                                                .truncate()
                                                .text_size(11.5)
                                                .text_color(!this.selectedModel ? t.foreground : t.muted)
                                                .child(
                                                  presetCat.defaultModel
                                                    ? `${tr("Default", "默认")} (${presetCat.defaultModel})`
                                                    : tr("Default (agent choice)", "默认（跟随 Agent 配置）"),
                                                ),
                                            )
                                            .when(!this.selectedModel, (ck) =>
                                              ck.child(svg("mods/workspace-chrome/check.svg").size(11).flex_none().text_color(accent)),
                                            ),
                                        )
                                        .children(
                                          presetCat.models.map((m, idx) => {
                                            const picked = this.selectedModel === m;
                                            return h_flex()
                                              .id(`fn-model-opt-${idx}`)
                                              .h(26)
                                              .px(8)
                                              .gap(6)
                                              .flex_none()
                                              .items_center()
                                              .justify_between()
                                              .rounded(6)
                                              .cursor_pointer()
                                              .when(picked, (x) => x.bg(withAlpha(accent, "1f") ?? t.selected))
                                              .hover((s) => s.bg(t.hover))
                                              .on_click((_e, cx) => {
                                                this.selectModel(m);
                                                this.modelPopupOpen = false;
                                                cx.notify();
                                              })
                                              .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(picked ? t.foreground : t.muted).child(m))
                                              .when(picked, (ck) =>
                                                ck.child(svg("mods/workspace-chrome/check.svg").size(11).flex_none().text_color(accent)),
                                              );
                                          }),
                                        ),
                                    )
                                    .child(
                                      h_flex()
                                        .w_full()
                                        .pt(4)
                                        .gap(5)
                                        .items_center()
                                        .border_t(1)
                                        .border_color(t.border)
                                        .child(
                                          Input.new(this.customModelInput)
                                            .id("fn-model-custom-input")
                                            .flex_1()
                                            .min_w(0)
                                            .h(24)
                                            .px(7)
                                            .rounded(5)
                                            .border_1()
                                            .border_color(t.input)
                                            .bg(t.raised)
                                            .text_size(11)
                                            .text_color(t.foreground),
                                        )
                                        .child(
                                          div()
                                            .id("fn-model-custom-ok")
                                            .h(24)
                                            .px(8)
                                            .flex_none()
                                            .flex()
                                            .items_center()
                                            .rounded(5)
                                            .bg(t.raised)
                                            .border_1()
                                            .border_color(t.border)
                                            .text_size(10.5)
                                            .text_color(t.foreground)
                                            .cursor_pointer()
                                            .hover((s) => s.bg(t.hover))
                                            .on_click((_e, cx) => {
                                              const val = String(this.customModelInput?.value?.() ?? "").trim();
                                              if (val) {
                                                this.selectModel(val);
                                                this.modelPopupOpen = false;
                                                cx.notify();
                                              }
                                            })
                                            .child(tr("Use", "使用")),
                                        ),
                                    ),
                                ),
                              ),
                          ),
                      ),
                  )
                  .child(
                    h_flex()
                      .w_full()
                      .gap(8)
                      .items_center()
                      .child(div().w(52).flex_none().text_size(11).text_color(t.dim).child(tr("Effort", "思考强度")))
                      .child(
                        h_flex()
                          .flex_1()
                          .min_w(0)
                          .gap(4)
                          .flex_wrap()
                          .child(effChip("fn-eff-default", "", effLabels[""]))
                          .children(
                            presetCat.efforts.map((eff) =>
                              effChip(`fn-eff-${eff}`, eff, effLabels[eff] ?? eff),
                            ),
                          ),
                      ),
                  )
                  .when(spaceList.length > 0, (e2) =>
                    e2.child(
                      Popup.new("fn-space-popup", spaceTrigger)
                        .anchor("top_left")
                        .when(this.spacePopupOpen, (pop) =>
                          pop.content(
                            v_flex()
                              .w(340)
                              .max_h(190)
                              .p(4)
                              .gap(2)
                              .rounded(8)
                              .bg(t.popover ?? t.background)
                              .border_1()
                              .border_color(t.border)
                              .shadow_lg()
                              .overflow_y_scroll()
                              .on_mouse_down("left", stop)
                              .children(
                                spaceList.map((sp) => {
                                  const active = chosenSpace?.id === sp.id;
                                  return h_flex()
                                    .id(`fn-sp-${sp.id}`)
                                    .h(28)
                                    .px(8)
                                    .gap(6)
                                    .items_center()
                                    .rounded(6)
                                    .cursor_pointer()
                                    .when(active, (x) => x.bg(t.selected))
                                    .hover((s) => s.bg(t.hover))
                                    .on_click((_e, cx) => {
                                      const prevMachine = this.activeSpace()?.machine ?? null;
                                      this.selectedSpaceId = sp.id;
                                      this.spacePopupOpen = false;
                                      const nextMachine = sp.machine ?? null;
                                      if (prevMachine !== nextMachine) {
                                        const cached = shared(KNOWN_AGENTS_KEY)?.[nextMachine ?? ""];
                                        if (Array.isArray(cached) && cached.length > 0) this.knownList = cached;
                                        this.refreshMachineAgentsAndCatalog(nextMachine, cx);
                                      }
                                      cx.notify();
                                    })
                                    .child(svg("mods/workspace-chrome/folder.svg").size(12).flex_none().text_color(t.dim))
                                    .child(div().font_medium().text_size(11.5).text_color(t.foreground).child(sp.name || sp.id))
                                    .when(Boolean(sp.folder), (x) => x.child(div().flex_1().min_w(0).truncate().text_size(10.5).text_color(t.dim).child(sp.folder)));
                                }),
                              ),
                          ),
                        ),
                    ),
                  ),
              ),
          )
          .when(Boolean(this.error), (el) =>
            el.child(
              div()
                .id("fn-error")
                .px(9)
                .py(5)
                .rounded(6)
                .bg(withAlpha(t.blocked ?? "#ef4444", "18"))
                .border_1()
                .border_color(withAlpha(t.blocked ?? "#ef4444", "55"))
                .text_size(11.5)
                .text_color(t.blocked ?? "#ef4444")
                .whitespace_normal()
                .child(this.error),
            ),
          ),
      )
      // Footer actions
      .child(
        h_flex()
          .id("fn-footer")
          .h(46)
          .px(12)
          .gap(8)
          .flex_none()
          .items_center()
          .border_t(1)
          .border_color(t.border)
          .bg(t.surface ?? t.chrome)
          .on_mouse_down("left", stop)
          .when(Boolean(this.editingNoteId), (el) =>
            el.child(
              h_flex()
                .id("fn-delete")
                .h(28)
                .px(9)
                .gap(5)
                .items_center()
                .rounded(6)
                .text_size(11.5)
                .text_color(t.ansi1 ?? t.blocked ?? "#ef4444")
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => this.deleteEditingNote(cx))
                .child(svg("mods/pane/trash.svg").size(12))
                .child(tr("Move to bin", "移至回收站")),
            ),
          )
          .child(div().flex_1())
          .child((() => {
            const canPolish = Boolean(hasText && !this.polishing && !this.busy);
            const purple = "#a855f7";
            return h_flex()
              .id("fn-polish")
              .h(28)
              .px(10)
              .gap(4)
              .items_center()
              .rounded(6)
              .border_1()
              .border_color(canPolish || this.polishing ? withAlpha(purple, "66") : t.border)
              .bg(canPolish || this.polishing ? withAlpha(purple, "18") : t.raised)
              .text_size(11.5)
              .font_medium()
              .text_color(canPolish || this.polishing ? purple : t.dim)
              .when(canPolish, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(purple, "2c"))).on_click((_e, cx) => this.startPolishIdea(cx)))
              .tooltip(withShortcut(tr("Polish prompt with Core AI for clarity", "使用 Core AI 润色提示词，让表达更清晰"), "prompt.polish"))
              .child(
                this.polishing
                  ? svg("mods/workspace-chrome/spinner.svg").size(11.5).spin(900).text_color(purple)
                  : svg("mods/peek/sparkles.svg").size(11.5).text_color(canPolish ? purple : t.dim),
              )
              .child(this.polishing ? tr("Polishing…", "润色中…") : tr("Polish", "AI 润色"));
          })())
          .child(
            h_flex()
              .id("fn-save")
              .h(28)
              .px(11)
              .gap(5)
              .items_center()
              .rounded(6)
              .border_1()
              .border_color(hasText ? t.border : withAlpha(t.border, "66"))
              .bg(t.background)
              .text_size(12)
              .font_medium()
              .text_color(hasText ? t.foreground : t.dim)
              .when(hasText && !this.busy, (e) => e.cursor_pointer().hover((s) => s.bg(t.hover)).on_click((_e, cx) => this.saveIdea(cx)))
              .child(
                hasMultiNonEmpty
                  ? (this.editingNoteId ? tr(`Update Group (${nonEmptyCount})`, `保存闪念组 (${nonEmptyCount})`) : tr(`Save Group (${nonEmptyCount})`, `保存闪念组 (${nonEmptyCount})`))
                  : (this.editingNoteId ? tr("Update", "保存修改") : tr("Save", "保存到清单")),
              ),
          )
          .child(
            h_flex()
              .id("fn-send")
              .h(28)
              .px(12)
              .gap(5)
              .items_center()
              .rounded(6)
              .bg(canSend ? (t.solid ?? accent) : t.raised)
              .text_size(12)
              .font_medium()
              .text_color(canSend ? (t.solid_foreground ?? "#ffffff") : t.dim)
              .when(canSend, (e) => e.cursor_pointer().on_click((_e, cx) => this.sendIdea(cx)))
              .child(svg("mods/peek/arrow-up.svg").size(12))
              .child(
                this.busy
                  ? tr("Sending…", "发送中…")
                  : hasMultiNonEmpty
                    ? (this.sendScope === "all"
                        ? (this.targetMode === "new"
                            ? tr(`Start & Send Group (${nonEmptyCount})`, `新建并整组发送 (${nonEmptyCount})`)
                            : tr(`Send Group (${nonEmptyCount})`, `整组发送 (${nonEmptyCount})`))
                        : (this.targetMode === "new"
                            ? tr(`Start & Send (${targetEntries.length})`, `新建并发送已选 (${targetEntries.length})`)
                            : tr(`Send Selected (${targetEntries.length})`, `发送已选 (${targetEntries.length})`)))
                    : (this.targetMode === "new"
                        ? tr("Start & Send", "新建并发送")
                        : tr("Send", "发送给 Agent")),
              ),
          ),
      );
  }

  render(cx) {
    const t = theme();
    if (props()?.trashMenu) {
      return this.renderTrashMenu(t);
    }
    if (props()?.list) {
      return this.renderSidebarList(t);
    }
    return this.renderComposeWindow(t, cx);
  }
}
