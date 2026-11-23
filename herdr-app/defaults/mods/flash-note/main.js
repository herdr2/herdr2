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
} from "herdr";
import {
  FLASH_NOTES_KEY,
  FLASH_NOTES_TRASH_KEY,
  KNOWN_AGENTS_KEY,
  AGENT_CATALOG_KEY,
  ACP_PRESETS,
  ACP_AGENT_KEY,
  withAlpha,
  normalizeNotes,
  normalizeTrash,
  upsertNote,
  renameNote,
  moveNoteToTrash,
  restoreNoteFromTrash,
  purgeTrashedNote,
  emptyNotesTrash,
  notePreview,
  noteDisplayTitle,
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
  { key: "gemini", name: "Gemini", icon: "icons/gemini.svg", tint: true, installed: true, run: ["gemini", "--yolo"] },
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
    width: 468,
    height: 488,
    min_width: 380,
    min_height: 420,
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
      placeholder: tr("Name (optional — defaults to idea preview)", "名称（可选，留空自动使用正文预览）"),
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

    this.input = TextareaState.new({
      value: existingNote?.text ?? "",
      placeholder: tr("Jot down a sudden idea, reminder, or prompt…", "记下此刻的灵感、待办或想交给 Agent 处理的想法…"),
    });
    this.polishing = false;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.input.set_auto_grow(4, 10);
    this.input.on("focus", (_e, cx) => {
      this.inputFocused = true;
      this.closeAllPopups();
      cx.notify();
    });
    this.input.on("blur", (_e, cx) => {
      this.inputFocused = false;
      cx.notify();
    });
    this.input.on("change", (_e, cx) => {
      this.error = null;
      cx.notify();
    });
    this.input.on("submit", (e, cx) => {
      if (e?.shift) return;
      if (e?.secondary || e?.modifiers?.control || e?.modifiers?.platform) {
        this.sendIdea(cx);
      }
    });

    this.handle = cx.focus_handle();
    if (!checking()) {
      this.input.focus();
      if (typeof nextCommand === "function") {
        cx.spawn(async (cmdCx) => {
          for (;;) {
            const cmd = await nextCommand();
            if (cmd === "polish") await this.polishIdea(cmdCx);
          }
        });
      }
      this.refreshMachineAgentsAndCatalog(this.activeSpace()?.machine ?? null, cx);
    }
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

  revertPolish(cx) {
    if (this.polishUndoText == null) return;
    const orig = this.polishUndoText;
    this.polishUndoText = null;
    this.polishSummary = null;
    this.input?.set_value?.(orig);
    this.input?.focus?.();
    cx.notify();
  }

  async polishIdea(cx) {
    const raw = String(this.input?.value?.() ?? "").trim();
    if (!raw || this.polishing || this.busy || checking()) return;
    const candidates = resolveExistingAgents(terminals(), agents(), panes(), focused());
    const targetKind = this.targetMode === "existing"
      ? (candidates.find((c) => c.address === this.selectedExisting)?.kind === "acp" ? "acp" : "cli")
      : (this.newRuntimeMode === "cli" ? "cli" : "acp");
    this.polishing = true;
    this.error = null;
    cx.notify();
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
      if (this.targetMode === "existing" && this.selectedExisting) {
        args.push("--target", this.selectedExisting);
      }
      args.push("--", raw);
      const res = await h2(args, { timeout: 22000 });
      if (res?.code === 0 && res.stdout) {
        const parsed = JSON.parse(res.stdout);
        const polished = String(parsed?.polished ?? "").trim();
        if (parsed?.usedAi && polished) {
          this.polishUndoText = raw;
          this.polishSummary = parsed?.summary ? String(parsed.summary).trim() : null;
          this.input?.set_value?.(polished);
          this.input?.focus?.();
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
      cx.notify();
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
    const text = String(this.input?.value?.() ?? "").trim();
    const title = String(this.titleInput?.value?.() ?? "").trim();
    if (!text || this.busy) return;
    const { list, note } = upsertNote(shared(FLASH_NOTES_KEY), {
      id: this.editingNoteId,
      title,
      text,
      now: Date.now(),
    });
    setShared(FLASH_NOTES_KEY, list);
    this.editingNoteId = note?.id ?? this.editingNoteId;
    notify({
      title: tr("Idea saved", "闪念已保存"),
      body: noteDisplayTitle(note ?? { title, text }, 64),
      level: "info",
    });
    const ov = props()?.overlay;
    if (ov) closeOverlay(ov);
    else cx?.notify?.();
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
    const ov = props()?.overlay;
    if (ov) closeOverlay(ov);
    else cx?.notify?.();
  }

  sendIdea(cx) {
    const text = String(this.input?.value?.() ?? "").trim();
    const title = String(this.titleInput?.value?.() ?? "").trim();
    if (!text || this.busy) return;
    this.error = null;

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
      if (this.editingNoteId) {
        const updated = upsertNote(shared(FLASH_NOTES_KEY), {
          id: this.editingNoteId,
          title,
          text,
          sentTo: buildSentToLabel(preset.name, acpMode, this.selectedModel, effectiveEffort),
          sentAt: Date.now(),
          now: Date.now(),
        });
        setShared(FLASH_NOTES_KEY, updated.list);
      }
      notify({
        title: tr(`Started ${preset.name} with idea`, `已新建 ${preset.name} 并发送闪念`),
        body: noteDisplayTitle({ title, text }, 64),
        level: "info",
      });
      const ov = props()?.overlay;
      if (ov) closeOverlay(ov);
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
        if (this.editingNoteId) {
          const updated = upsertNote(shared(FLASH_NOTES_KEY), {
            id: this.editingNoteId,
            title,
            text,
            sentTo: target.label,
            sentAt: Date.now(),
            now: Date.now(),
          });
          setShared(FLASH_NOTES_KEY, updated.list);
        }
        run("agent.focus", { terminal: target.address });
        notify({
          title: tr(`Sent idea to ${target.label}`, `已将闪念发送给 ${target.label}`),
          body: noteDisplayTitle({ title, text }, 64),
          level: "info",
          action: { name: "agent.focus", args: { terminal: target.address } },
        });
        const ov = props()?.overlay;
        if (ov) closeOverlay(ov);
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
            .tooltip(notes.length > 0 ? tr(`Flash Notes (${notes.length})`, `闪念清单 (${notes.length})`) : tr("Flash Notes", "闪念清单"))
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

    return v_flex()
      .id("fn-sidebar")
      .when(Boolean(this.listFocus), (el) => el.track_focus(this.listFocus))
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
                  .tooltip(tr("Capture a flash note", "新建闪念"))
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
            .gap(2)
            .overflow_y_scrollbar()
            .mode("always")
            .children(
              notes.map((note) => {
                const hovered = this.hoveredId === note.id;
                const renaming = this.renamingId === note.id;
                const timeStr = formatNoteTime(note.updatedAt || note.createdAt);
                const sent = Boolean(note.sentTo);
                const hasCustomTitle = Boolean(note.title);
                const displayTitle = noteDisplayTitle(note, 64);
                const bodySnippet = hasCustomTitle ? notePreview(note.text, 56) : "";
                const showBodySnippet = Boolean(bodySnippet && bodySnippet !== displayTitle);
                return h_flex()
                  .id(`fn-item-${note.id}`)
                  .w_full()
                  .min_h(36)
                  .px(8)
                  .py(5)
                  .gap(7)
                  .flex_none()
                  .items_start()
                  .rounded(7)
                  .when(!renaming, (e2) => e2.cursor_pointer().hover((s) => s.bg(t.hover)))
                  .when(renaming, (e2) => e2.bg(t.raised))
                  .on_hover((on, cx) => {
                    if (on) this.hoveredId = note.id;
                    else if (this.hoveredId === note.id) this.hoveredId = null;
                    cx.notify();
                  })
                  .when(!renaming, (e2) => e2.on_click(() => openFlashNoteWindow(note.id)))
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
                            div()
                              .w_full()
                              .min_w(0)
                              .truncate()
                              .text_size(12)
                              .when(hasCustomTitle, (x) => x.font_medium())
                              .text_color(sent ? t.muted : t.foreground)
                              .child(displayTitle),
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
                          .when(sent, (e2) =>
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
              }),
            ),
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
                const bodySnippet = it.title ? notePreview(it.text, 36) : "";
                const age = it.trashedAt ? formatTrashAge(it.trashedAt, now) : "";
                const meta = [
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
      this.editingNoteId = found?.id ?? null;
      this.titleInput?.set_value?.(found?.title ?? "");
      this.input?.set_value?.(found?.text ?? "");
      this.polishUndoText = null;
      this.polishSummary = null;
      this.polishing = false;
      this.error = null;
    }

    const accent = t.accent ?? "#3b82f6";
    const candidates = resolveExistingAgents(terminals(), agents(), panes(), focused());
    const chosenExisting = this.chosenExisting(candidates);
    const presets = this.availablePresets();
    const chosenPreset = this.chosenPreset();
    const spaceList = spaces();
    const chosenSpace = this.activeSpace();
    const text = String(this.input?.value?.() ?? "").trim();
    const hasText = text.length > 0;
    const isWorkingAcp = Boolean(this.targetMode === "existing" && chosenExisting?.kind === "acp" && chosenExisting?.status === "working");
    const canSend = hasText && !this.busy && (this.targetMode === "new" ? Boolean(chosenPreset) : Boolean(chosenExisting));
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
          } else if (props()?.overlay) {
            closeOverlay(props().overlay);
            cx.stop_propagation();
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
              .child(this.editingNoteId ? tr("Edit Flash Note", "编辑闪念") : tr("Flash Note", "闪念")),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11)
              .text_color(t.dim)
              .child(tr("Drag to move · Save or dispatch to an agent", "可拖动 · 保存到清单或发送给 Agent")),
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
          .when(this.polishUndoText != null, (el) =>
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
                    .on_click((_e, cx) => this.revertPolish(cx))
                    .child(tr("↩ Undo", "↩ 还原")),
                ),
            ),
          )
          .child(
            div()
              .id("fn-input-wrap")
              .flex_1()
              .min_h(90)
              .w_full()
              .px(10)
              .py(8)
              .rounded(8)
              .bg(t.background)
              .border_1()
              .border_color(this.inputFocused ? accent : t.border)
              .cursor_text()
              .on_mouse_down("left", stop)
              .on_click(() => this.input?.focus?.())
              .child(
                Textarea.new(this.input)
                  .size_full()
                  .text_size(13)
                  .line_height("20px")
                  .text_color(t.foreground),
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
              .when(canPolish, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(purple, "2c"))).on_click((_e, cx) => this.polishIdea(cx)))
              .tooltip(tr("Polish prompt with Core AI for clarity", "使用 Core AI 润色提示词，让表达更清晰"))
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
              .child(this.editingNoteId ? tr("Update", "保存修改") : tr("Save", "保存到清单")),
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
                  : this.targetMode === "new"
                    ? tr("Start & Send", "新建并发送")
                    : tr("Send", "发送给 Agent"),
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
