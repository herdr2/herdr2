// A side-by-side collapsible file browser for the focused pane's folder, with Typora-style live Markdown editing, image rendering, and IDE-themed code viewing.
import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, h_resizable, resizable_panel, Editor, EditorState, Input, InputState, TextView } from "gpui-base";
import { spaces, panes, focused, exec, readTextFile, readImageDataUrl, writeTextFile, watchFolder, theme, settings, window, layout, run, props, shared, setShared, checking, notify } from "herdr";
import { detectFileKind, CODE_THEMES, MD_THEMES, resolveCodeTheme, resolveMdTheme, resolveRelativeFileLink, resolveRelativeAssetPath } from "./themes.js";
import {
  parseMarkdownBlocks,
  serializeMarkdownBlocks,
  prepareBlockMarkdown,
  extractMarkdownImageUrls,
  toggleMarkdownTask,
  estimateEditorRows,
  detectHeadingLevel,
  parseMarkdownTable,
  serializeMarkdownTable,
  parseCodeFence,
  serializeCodeFence,
  resolveFenceLanguage,
} from "./markdown_blocks.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const stopMouse = (e, cx) => { e?.stopPropagation?.(); cx?.stop_propagation?.(); };

function location() {
  const space = spaces().find(s => s.active), f = focused();
  const pane = f?.pane ? panes().find(p => p.pane === f.pane) : null;
  const on = f?.pane === null ? (f.terminal?.includes("/") ? f.terminal.split("/")[0] : null) : space?.machine ?? null;
  return { cwd: pane?.cwd || f?.cwd || space?.folder, on, space: space?.id };
}
const key = () => JSON.stringify(location());
const PER_FOLDER = 500, TOTAL = 5000, MAX_PREVIEW_CHARS = 400000;
const MAX_FILE_CACHE = 24, MAX_CACHE_TEXT_CHARS = 200000, MAX_CACHE_IMAGE_CHARS = 2000000;
const INITIAL_MD_BLOCKS = 32, MD_PUMP_STEP = 48;
const INLINE_MD_RE = /[*_`~\[\]!<]|https?:\/\//i;

function readSetting(k, fallback) {
  try {
    const val = shared(k);
    return typeof val === "string" && val ? val : fallback;
  } catch {
    return fallback;
  }
}

function readBool(k, fallback) {
  try {
    const val = shared(k);
    return typeof val === "boolean" ? val : fallback;
  } catch {
    return fallback;
  }
}

function findLayoutNode(node, id) {
  if (!node || typeof node !== "object") return null;
  if (node.id === id) return node;
  if (Array.isArray(node.children)) {
    for (const child of node.children) {
      const found = findLayoutNode(child, id);
      if (found) return found;
    }
  }
  return null;
}

function editorSignature(language, readonly, softWrap, fontFamily, codeTheme) {
  return JSON.stringify([language, readonly, softWrap, fontFamily ?? null, codeTheme.id, codeTheme.colors.background, codeTheme.colors.foreground, codeTheme.syntax.keyword]);
}

function blockEditorColors(mdTheme) {
  const bg = mdTheme.background;
  return {
    background: bg,
    foreground: mdTheme.textView.foreground,
    gutter: bg,
    line_number: mdTheme.textView.muted,
    active_line: bg,
    selection: mdTheme.textView.selection,
    caret: mdTheme.textView.link ?? mdTheme.textView.foreground,
    border: bg,
  };
}

function codeBlockEditorColors(mdTheme) {
  const bg = mdTheme.textView.code_background ?? mdTheme.surface ?? mdTheme.background;
  return {
    background: bg,
    foreground: mdTheme.textView.foreground,
    gutter: bg,
    line_number: mdTheme.textView.muted,
    active_line: bg,
    selection: mdTheme.textView.selection,
    caret: mdTheme.textView.link ?? mdTheme.textView.foreground,
    border: mdTheme.border,
  };
}

function blockTypography(block, sansFont, monoFont) {
  const level = detectHeadingLevel(block?.raw) || block?.headingLevel || 0;
  if (level === 1) return { fontFamily: sansFont, fontSize: 23, lineHeight: 30, wrapCols: 34, bold: true };
  if (level === 2) return { fontFamily: sansFont, fontSize: 17.5, lineHeight: 24, wrapCols: 42, bold: true };
  if (level === 3) return { fontFamily: sansFont, fontSize: 15, lineHeight: 21, wrapCols: 48, bold: true };
  if (level === 4) return { fontFamily: sansFont, fontSize: 13.5, lineHeight: 20, wrapCols: 52, bold: true };
  if (level >= 5) return { fontFamily: sansFont, fontSize: 13, lineHeight: 19, wrapCols: 56, bold: true };
  if (block?.kind === "frontmatter" || block?.kind === "html") {
    return { fontFamily: monoFont, fontSize: 12.5, lineHeight: 20, wrapCols: 58, bold: false };
  }
  return { fontFamily: sansFont, fontSize: 13.5, lineHeight: 21, wrapCols: 54, bold: false };
}

async function directory(path, on) {
  const windowsDirectory = async () => {
    // Windows PowerShell otherwise uses the console code page; exec decodes UTF-8.
    // Links keep directory attributes even when dangling; check their target.
    // Other reparse points (OneDrive's folders) have no LinkType and are folders.
    // PowerShell reads curly single quotes as quotes too, so they're doubled like '.
    const r = await exec("powershell", ["-NoProfile", "-Command", "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); & { param($p) @(Get-ChildItem -LiteralPath $p -Force -ErrorAction Stop | Where-Object Name -ne '.git' | Select-Object -First 501 | ForEach-Object { @{ name=$_.Name; dir=($_.PSIsContainer -and (-not ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or -not $_.LinkType -or ($_.Target -and [IO.Directory]::Exists(([IO.Path]::Combine([IO.Path]::GetDirectoryName($_.FullName), $_.Target[0])))))) } }) | ConvertTo-Json -Compress } '" + path.replace(/['‘’‚‛]/g, "$&$&") + "'"], { on });
    if (r.code !== 0) throw new Error(r.stderr || "Cannot list this folder");
    const result = JSON.parse(r.stdout || "[]");
    return Array.isArray(result) ? result : [result];
  };
  if (!on && window()?.platform === "windows") return windowsDirectory();
  const script = 'n=0; for p in "$1"/* "$1"/.[!.]* "$1"/..?*; do [ -e "$p" ] || [ -L "$p" ] || continue; name=${p##*/}; [ "$name" = .git ] && continue; if [ -d "$p" ]; then kind=d; else kind=f; fi; printf "%s\\0%s\\0" "$kind" "$name"; n=$((n+1)); [ "$n" -gt 500 ] && break; done; test -d "$1"';
  let r;
  try { r = await exec("sh", ["-c", script, "files", path], { on }); }
  catch (e) {
    if (on && String(e).includes("cannot start sh: program not found")) return windowsDirectory();
    throw e;
  }
  if (r.code !== 0) throw new Error(r.stderr || "Cannot list this folder");
  const fields = r.stdout.split("\0"), out = [];
  for (let i = 0; i + 1 < fields.length; i += 2) out.push({ name: fields[i + 1], dir: fields[i] === "d" });
  return out;
}

export default class Files extends View {
  init(_props, cx) {
    let initialProps = null;
    try { initialProps = props?.(); } catch {}
    this.saved = new Map();
    this.entries = new Map(); this.folders = new Set(); this.pending = new Set(); this.marks = {};
    this.diskText = ""; this.liveText = ""; this.dirty = false; this.truncated = false;
    this.doc = parseMarkdownBlocks("");
    this.editingBlock = null;
    this.editingMode = null;
    this.editingCodeFence = null;
    this.blockEditor = null;
    this.blockEditorRows = 2;
    this.blockEditorCols = 54;
    this.blockPress = null;
    this.codeBlockPress = null;
    this.committedOut = null;
    this.editingCell = null;
    this.cellInput = null;
    this.hoveredTable = null;
    this.editingCodeLang = null;
    this.codeLangInput = null;
    this.autoSaveTask = null;
    this.loadingTimer = null;
    this.mdPumpTask = null;
    this.mdRenderLimit = INITIAL_MD_BLOCKS;
    this.fileCache = new Map();
    this.selectedFile = null;
    this.showSeq = 0;
    this.suppressCodeChange = false;
    this.savedUntil = 0;
    this.sansFont = null;
    try { this.sansFont = cx.theme()?.typography?.sans ?? null; } catch {}
    this.lastLinkClickAt = 0;
    this.lastTaskClickAt = 0;
    this.imageDataUrl = null;
    this.remoteImages = new Map();
    this.remoteAttempted = new Set();
    this.remoteImageRev = 0;
    this.themeMenuOpen = false; this.copiedUntil = 0; this.saving = false;
    this.handle = cx.focus_handle();
    if (!checking()) {
      try {
        cx.bind_keys([
          { keystroke: "tab", action: "files.tableNext", context: "FilesMdTable && Input" },
          { keystroke: "shift-tab", action: "files.tablePrev", context: "FilesMdTable && Input" },
          { keystroke: "enter", action: "files.tableEnter", context: "FilesMdTable && Input" },
          { keystroke: "escape", action: "files.tableEscape", context: "FilesMdTable && Input" },
        ]);
      } catch {}
    }
    this.refresh(cx);
    if (typeof initialProps?.file === "string" && initialProps.file) {
      this.show(initialProps.file, cx);
    }
    cx.timer.every(10000, cx => this.refresh(cx));
    if (!checking()) cx.timer.every(1500, cx => this.pollOpenFile(cx));
  }

  treeOpen() {
    return readBool("files.treeOpen", true);
  }

  setTreeOpen(open, cx) {
    setShared("files.treeOpen", Boolean(open));
    if (open && this.file && !checking()) this.ensurePanelRoom();
    cx.notify();
  }

  ensurePanelRoom() {
    try {
      const p = props?.();
      if (p?.tab) return;
      const panel = findLayoutNode(layout(), "panel");
      if (panel && !panel.hidden && (panel.size?.px ?? 0) < 520) {
        run("layout.resize", { id: "panel", px: 640 });
      }
    } catch {}
  }

  mdMode() {
    const mode = readSetting("files.mdMode", "preview");
    return mode === "code" ? "code" : "preview";
  }

  setMdMode(mode, cx) {
    const next = mode === "code" ? "code" : "preview";
    if (next === "code") {
      this.commitActiveInlineEdit(cx, false);
    }
    setShared("files.mdMode", next);
    this.themeMenuOpen = false;
    if (next === "preview") {
      this.code?.release();
      this.code = null;
      this.editorSig = null;
      this.doc = parseMarkdownBlocks(this.liveText);
      if (this.on) this.prefetchRemoteImages(cx);
    } else if (this.file) {
      this.rebuildEditor();
    }
    cx.notify();
  }

  codeThemeId() {
    return readSetting("files.codeTheme", "auto");
  }

  mdThemeId() {
    return readSetting("files.mdTheme", "auto");
  }

  softWrap() {
    return readBool("files.wordWrap", false);
  }

  watch(root, cx) {
    if (this.on || this.watched === root) return;
    this.watched = root;
    const context = this.context;
    cx.spawn(async cx => {
      try {
        while (this.context === context) {
          await watchFolder(root);
          if (this.current(context)) {
            this.refresh(cx);
            this.pollOpenFile(cx);
          }
        }
      } catch { if (this.context === context) this.watched = null; }
    });
  }

  current(context) {
    return this.context === context && key() === context.key;
  }

  async loadFolder(path, context) {
    const { on, cwd: root } = context;
    const entries = await directory(path ? `${root}/${path}` : root, on);
    if (!this.current(context) || (path && !this.folders.has(path))) return;
    const used = [...this.entries.entries()].filter(([p]) => p !== path).reduce((n, [, e]) => n + e.length, 0);
    this.entries.set(path, entries.slice(0, Math.max(0, Math.min(PER_FOLDER, TOTAL - used))).sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name)));
    if (entries.length > PER_FOLDER || used + entries.length > TOTAL) this.limited = true;
  }

  refresh(cx) {
    const { cwd, on } = location(), next = key();
    if (next !== this.context?.key) {
      if (this.context) this.saved.set(this.context.key, { folders: this.folders, file: this.file });
      const saved = this.saved.get(next);
      this.context = { key: next, cwd, on }; this.root = cwd; this.on = on; this.entries = new Map(); this.folders = new Set(saved?.folders);
      this.fileCache.clear();
      this.pending = new Set(); this.marks = {}; this.show(null); this.watched = null; this.error = null; this.limited = false;
      this.loading = false;
      if (saved?.file) this.show(saved.file, cx);
    }
    if (!cwd || this.loading) return;
    const context = this.context;
    this.loading = true;
    this.limited = false;
    cx.spawn(async cx => {
      try {
        for (const path of ["", ...this.folders]) {
          if (!this.current(context)) return;
          await this.loadFolder(path, context);
        }
        const top = await exec("git", ["rev-parse", "--show-toplevel"], { cwd, on }).catch(() => null);
        const marks = {};
        if (top?.code === 0) {
          // Writes nothing under .git (index lock, fsmonitor cookie): the folder watch would refresh again.
          const status = await exec("git", ["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"], { cwd, on }).catch(() => null);
          if (status?.code === 0) for (const entry of status.stdout.split("\0").filter(Boolean).slice(0, TOTAL)) {
            const xy = entry.slice(0, 2);
            marks[`${top.stdout.trim()}/${entry.slice(3)}`] = xy === "??" || xy.includes("A") ? "A" : xy.includes("U") ? "U" : xy.includes("D") ? "D" : "M";
          }
        }
        if (!this.current(context)) return;
        this.marks = marks; this.error = null; this.watch(cwd, cx);
      } catch (e) { if (this.current(context)) this.error = String(e); }
      finally { if (this.context === context) this.loading = false; cx.notify(); }
    });
  }

  expandParents(path, cx) {
    if (!path || !path.includes("/") || !this.context) return;
    const parts = path.split("/");
    parts.pop();
    let prefix = "";
    const context = this.context;
    for (const part of parts) {
      prefix = prefix ? `${prefix}/${part}` : part;
      this.folders.add(prefix);
      if (!this.entries.has(prefix) && !this.pending.has(prefix) && cx) {
        const dirPath = prefix;
        this.pending.add(dirPath);
        cx.spawn(async cx => {
          try { await this.loadFolder(dirPath, context); }
          catch {}
          finally { if (this.context === context) this.pending.delete(dirPath); cx.notify(); }
        });
      }
    }
  }

  toggle(path, cx) {
    if (this.folders.has(path)) {
      for (const folder of [...this.folders]) if (folder === path || folder.startsWith(path + "/")) {
        this.folders.delete(folder);
        this.entries.delete(folder);
      }
    } else {
      this.folders.add(path);
      if (!this.entries.has(path) && !this.pending.has(path)) {
        const context = this.context;
        this.pending.add(path);
        cx.spawn(async cx => {
          try { await this.loadFolder(path, context); }
          catch (e) { if (this.current(context)) this.error = String(e); }
          finally { if (this.context === context) this.pending.delete(path); cx.notify(); }
        });
      }
    }
    cx.notify();
  }

  activeEditorStyle(kind, appTheme) {
    if (kind.isMarkdown) {
      const md = resolveMdTheme(this.mdThemeId(), appTheme);
      return resolveCodeTheme(md.editorThemeId, appTheme);
    }
    return resolveCodeTheme(this.codeThemeId(), appTheme);
  }

  cancelAutoSave() {
    if (this.autoSaveTask) {
      try { this.autoSaveTask.cancel(); } catch {}
      this.autoSaveTask = null;
    }
  }

  scheduleAutoSave(cx, delayMs = 900) {
    this.cancelAutoSave();
    if (!this.dirty || checking() || !this.file || this.truncated || detectFileKind(this.file).isImage) return;
    const targetFile = this.file;
    this.autoSaveTask = cx.timer.after(delayMs, cx => {
      this.autoSaveTask = null;
      if (this.file === targetFile && this.dirty && !this.saving && !this.truncated) {
        this.saveFile(cx, { keepEditor: true });
      }
    });
  }

  markDirty(cx) {
    this.dirty = this.liveText !== this.diskText;
    if (this.dirty) {
      this.scheduleAutoSave(cx);
    } else {
      this.cancelAutoSave();
    }
  }

  releaseInlineEditors() {
    this.blockEditor?.release();
    this.blockEditor = null;
    this.editingBlock = null;
    this.editingMode = null;
    this.editingCodeFence = null;
    this.cellInput?.release();
    this.cellInput = null;
    this.editingCell = null;
    this.codeLangInput?.release();
    this.codeLangInput = null;
    this.editingCodeLang = null;
    this.blockPress = null;
    this.codeBlockPress = null;
    this.committedOut = null;
  }

  flushActiveInlineToDoc() {
    if (this.editingBlock !== null && this.blockEditor && this.doc?.blocks?.[this.editingBlock]) {
      const val = this.blockEditor.value();
      this.doc.blocks[this.editingBlock].raw =
        this.editingMode === "code" && this.editingCodeFence
          ? serializeCodeFence(this.editingCodeFence, val)
          : val;
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.dirty = this.liveText !== this.diskText;
    } else if (this.editingCell && this.cellInput && this.doc?.blocks?.[this.editingCell.blockIndex]) {
      const { blockIndex, row, col, table } = this.editingCell;
      const val = this.cellInput.value();
      if (row < 0) table.headers[col] = val;
      else if (table.rows[row]) table.rows[row][col] = val;
      this.doc.blocks[blockIndex].raw = serializeMarkdownTable(table);
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.dirty = this.liveText !== this.diskText;
    } else if (this.editingCodeLang && this.codeLangInput && this.doc?.blocks?.[this.editingCodeLang.blockIndex]) {
      const { blockIndex, fence } = this.editingCodeLang;
      this.doc.blocks[blockIndex].raw = serializeCodeFence(fence, fence.body, this.codeLangInput.value());
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.dirty = this.liveText !== this.diskText;
    }
  }

  commitActiveInlineEdit(cx, notifyView = true) {
    if (this.editingCell || this.cellInput) {
      this.commitTableCellEdit(cx, notifyView);
      return;
    }
    if (this.editingCodeLang || this.codeLangInput) {
      this.commitCodeLangEdit(cx, notifyView);
      return;
    }
    if (this.editingBlock !== null || this.blockEditor) {
      this.commitBlockEdit(cx, notifyView);
    }
  }

  cancelLoadingTimer() {
    if (this.loadingTimer) {
      try { this.loadingTimer.cancel(); } catch {}
      this.loadingTimer = null;
    }
  }

  cancelMdPump() {
    if (this.mdPumpTask) {
      try { this.mdPumpTask.cancel(); } catch {}
      this.mdPumpTask = null;
    }
  }

  pumpMarkdownBlocks(cx) {
    this.cancelMdPump();
    if (!cx || checking()) return;
    const targetFile = this.file;
    const total = this.doc?.blocks?.length ?? 0;
    if ((this.mdRenderLimit ?? 0) >= total) return;
    this.mdPumpTask = cx.timer.after(12, cx => {
      this.mdPumpTask = null;
      if (this.file !== targetFile) return;
      const len = this.doc?.blocks?.length ?? 0;
      if ((this.mdRenderLimit ?? 0) >= len) return;
      this.mdRenderLimit = Math.min(len, (this.mdRenderLimit ?? INITIAL_MD_BLOCKS) + MD_PUMP_STEP);
      cx.notify();
      if (this.mdRenderLimit < len) {
        this.pumpMarkdownBlocks(cx);
      }
    });
  }

  applyMarkdownDoc(text, cx, existingDoc = null) {
    this.doc = existingDoc ?? parseMarkdownBlocks(text);
    const total = this.doc.blocks.length;
    if (checking() || total <= INITIAL_MD_BLOCKS) {
      this.cancelMdPump();
      this.mdRenderLimit = total;
      return;
    }
    this.mdRenderLimit = INITIAL_MD_BLOCKS;
    if (cx) this.pumpMarkdownBlocks(cx);
  }

  cacheGet(path) {
    const entry = this.fileCache.get(path);
    if (!entry) return null;
    this.fileCache.delete(path);
    this.fileCache.set(path, entry);
    return entry;
  }

  cachePut(path, entry) {
    if (!path || !entry) return;
    this.fileCache.delete(path);
    while (this.fileCache.size >= MAX_FILE_CACHE) {
      const oldest = this.fileCache.keys().next().value;
      if (oldest === undefined) break;
      this.fileCache.delete(oldest);
    }
    this.fileCache.set(path, entry);
  }

  cachePutText(path, text, truncated, doc = null, remoteImages = null) {
    if (!path || typeof text !== "string" || text.length > MAX_CACHE_TEXT_CHARS) return;
    this.cachePut(path, {
      kind: "text",
      diskText: text,
      truncated: Boolean(truncated),
      doc,
      remoteImages: remoteImages && remoteImages.size > 0 ? new Map(remoteImages) : null,
    });
  }

  cachePutImage(path, dataUrl) {
    if (!path || typeof dataUrl !== "string" || dataUrl.length > MAX_CACHE_IMAGE_CHARS) return;
    this.cachePut(path, { kind: "image", imageDataUrl: dataUrl });
  }

  getBlockTable(block) {
    if (block._tableRaw === block.raw) return block._table;
    block._tableRaw = block.raw;
    block._table = parseMarkdownTable(block.raw);
    return block._table;
  }

  getBlockFence(block) {
    if (block._fenceRaw === block.raw) return block._fence;
    block._fenceRaw = block.raw;
    block._fence = parseCodeFence(block.raw);
    return block._fence;
  }

  getBlockRendered(block, defs) {
    if (
      block._renderedRaw === block.raw &&
      block._renderedDefs === defs &&
      block._renderedRev === this.remoteImageRev &&
      block._renderedFile === this.file
    ) {
      return block._rendered;
    }
    block._renderedRaw = block.raw;
    block._renderedDefs = defs;
    block._renderedRev = this.remoteImageRev;
    block._renderedFile = this.file;
    block._rendered = prepareBlockMarkdown(block.raw, defs, this.remoteImages, this.file);
    return block._rendered;
  }

  close(_cx) {
    this.cancelAutoSave();
    this.cancelLoadingTimer();
    this.cancelMdPump();
    this.code?.release();
    this.code = null;
    this.releaseInlineEditors();
  }

  rebuildEditor() {
    if (!this.file || this.fileError || this.loadingFile) {
      this.code?.release();
      this.code = null;
      this.editorSig = null;
      return;
    }
    const kind = detectFileKind(this.file);
    if (kind.isImage || (kind.isMarkdown && this.mdMode() === "preview")) {
      this.code?.release();
      this.code = null;
      this.editorSig = null;
      return;
    }

    const readonly = Boolean(this.truncated);
    const softWrap = kind.isMarkdown ? true : this.softWrap();
    const fontFamily = settings()?.font_family;
    const codeTheme = this.activeEditorStyle(kind, theme());
    const sig = editorSignature(kind.language, readonly, softWrap, fontFamily, codeTheme);
    if (this.code && this.editorSig === sig) {
      if (this.code.value() !== this.liveText) {
        this.suppressCodeChange = true;
        try { this.code.set_value(this.liveText); }
        finally { this.suppressCodeChange = false; }
      }
      try { this.code.set_selected_range({ start: 0, end: 0 }); } catch {}
      return;
    }

    this.code?.release();
    this.code = null;
    this.editorSig = null;
    const state = EditorState.new({
      language: kind.language,
      readonly,
      folding: false,
      soft_wrap: softWrap,
      value: this.liveText,
      font_family: fontFamily,
      font_size: 12.5,
      colors: codeTheme.colors,
      syntax: codeTheme.syntax,
    });
    if (!readonly) {
      state.on("change", (_event, cx) => {
        if (this.suppressCodeChange || this.code !== state || !this.file) return;
        this.liveText = state.value();
        this.markDirty(cx);
        if (detectFileKind(this.file).isMarkdown) this.applyMarkdownDoc(this.liveText, cx);
        cx.notify();
      });
    }
    this.code = state;
    this.editorSig = sig;
  }

  setBlockCursor(state, text, placement = "end") {
    const pos = placement === "start" ? 0 : String(text ?? "").length;
    try { state.set_selected_range({ start: pos, end: pos }); } catch {}
    state.focus();
  }

  startBlockEdit(index, cx, placement = "end") {
    if (!this.file || this.fileError || this.loadingFile || this.truncated) return;
    if (this.editingBlock === index && this.editingMode === "block" && this.blockEditor) {
      this.blockEditor.focus();
      return;
    }
    this.commitActiveInlineEdit(cx, false);
    const block = this.doc?.blocks?.[index];
    if (!block) return;

    try { this.sansFont = cx.theme()?.typography?.sans ?? this.sansFont; } catch {}
    const mdTheme = resolveMdTheme(this.mdThemeId(), theme());
    const typo = blockTypography(block, this.sansFont, settings()?.font_family);
    const state = EditorState.new({
      language: "md",
      readonly: false,
      line_numbers: false,
      folding: false,
      soft_wrap: true,
      value: block.raw,
      font_family: typo.fontFamily ?? settings()?.font_family,
      font_size: typo.fontSize,
      colors: blockEditorColors(mdTheme),
      syntax: mdTheme.textView.syntax,
    });
    this.editingBlock = index;
    this.editingMode = "block";
    this.editingCodeFence = null;
    this.blockEditor = state;
    this.blockEditorCols = typo.wrapCols;
    this.blockEditorRows = estimateEditorRows(block.raw, typo.wrapCols);
    if (index >= (this.mdRenderLimit ?? 0)) this.mdRenderLimit = index + 16;
    state.on("change", (_event, cx) => {
      if (this.blockEditor !== state || this.editingBlock !== index || this.editingMode !== "block") return;
      const target = this.doc?.blocks?.[index];
      if (!target) return;
      const value = state.value();
      target.raw = value;
      target.headingLevel = detectHeadingLevel(value);
      this.blockEditorRows = estimateEditorRows(value, typo.wrapCols);
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.markDirty(cx);
      cx.notify();
    });
    this.setBlockCursor(state, block.raw, placement);
    cx.notify();
  }

  startCodeBlockEdit(index, cx, placement = "end") {
    if (!this.file || this.fileError || this.loadingFile || this.truncated) return;
    if (this.editingBlock === index && this.editingMode === "code" && this.blockEditor) {
      this.blockEditor.focus();
      return;
    }
    const block = this.doc?.blocks?.[index];
    if (!block) return;
    const fence = parseCodeFence(block.raw);
    if (!fence) {
      this.startBlockEdit(index, cx, placement);
      return;
    }
    this.commitActiveInlineEdit(cx, false);
    const freshBlock = this.doc?.blocks?.[index] ?? block;
    const freshFence = parseCodeFence(freshBlock.raw) ?? fence;
    const mdTheme = resolveMdTheme(this.mdThemeId(), theme());
    const codeTheme = this.activeEditorStyle(detectFileKind(this.file), theme());
    const state = EditorState.new({
      language: resolveFenceLanguage(freshFence.lang),
      readonly: false,
      line_numbers: false,
      folding: false,
      soft_wrap: true,
      value: freshFence.body,
      font_family: settings()?.font_family,
      font_size: 12.5,
      colors: codeBlockEditorColors(mdTheme),
      syntax: codeTheme.syntax,
    });
    this.editingBlock = index;
    this.editingMode = "code";
    this.editingCodeFence = freshFence;
    this.blockEditor = state;
    this.blockEditorCols = 64;
    this.blockEditorRows = estimateEditorRows(freshFence.body, 64);
    if (index >= (this.mdRenderLimit ?? 0)) this.mdRenderLimit = index + 16;
    state.on("change", (_event, cx) => {
      if (this.blockEditor !== state || this.editingBlock !== index || this.editingMode !== "code") return;
      const target = this.doc?.blocks?.[index];
      if (!target) return;
      const nextBody = state.value();
      target.raw = serializeCodeFence(freshFence, nextBody);
      this.blockEditorRows = estimateEditorRows(nextBody, 64);
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.markDirty(cx);
      cx.notify();
    });
    this.setBlockCursor(state, freshFence.body, placement);
    cx.notify();
  }

  startCodeLangEdit(index, cx) {
    if (!this.file || this.fileError || this.loadingFile || this.truncated) return;
    if (this.editingCodeLang?.blockIndex === index && this.codeLangInput) {
      this.codeLangInput.focus();
      return;
    }
    const block = this.doc?.blocks?.[index];
    if (!block) return;
    const fence = parseCodeFence(block.raw);
    if (!fence) return;
    this.commitActiveInlineEdit(cx, false);
    const input = InputState.new({ value: fence.lang, placeholder: "plain" });
    this.editingCodeLang = { blockIndex: index, fence };
    this.codeLangInput = input;
    input.on("change", (_event, cx) => {
      if (this.codeLangInput !== input || this.editingCodeLang?.blockIndex !== index) return;
      const target = this.doc?.blocks?.[index];
      if (!target) return;
      const nextLang = input.value();
      target.raw = serializeCodeFence(fence, fence.body, nextLang);
      target.lang = nextLang.trim();
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.markDirty(cx);
      cx.notify();
    });
    input.on("submit", (_event, cx) => this.commitCodeLangEdit(cx));
    try { input.set_selected_range({ start: 0, end: fence.lang.length }); } catch {}
    input.focus();
    cx.notify();
  }

  commitCodeLangEdit(cx, notifyView = true) {
    if (!this.editingCodeLang && !this.codeLangInput) return;
    const { blockIndex, fence } = this.editingCodeLang ?? {};
    if (this.codeLangInput && blockIndex !== undefined && this.doc?.blocks?.[blockIndex]) {
      const nextLang = this.codeLangInput.value();
      this.doc.blocks[blockIndex].raw = serializeCodeFence(fence, fence.body, nextLang);
    }
    this.codeLangInput?.release();
    this.codeLangInput = null;
    this.editingCodeLang = null;
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.doc = parseMarkdownBlocks(this.liveText);
    if (notifyView) cx.notify();
  }

  startNewBlockEdit(cx) {
    if (!this.file || this.fileError || this.loadingFile || this.truncated) return;
    this.commitActiveInlineEdit(cx, false);
    const blocks = this.doc?.blocks ?? [];
    if (blocks.length === 0) {
      this.doc = { leading: this.doc?.leading ?? "", blocks: [{ raw: "", gapAfter: "", kind: "paragraph", lang: "", headingLevel: 0, tasks: [] }], defs: "" };
    } else {
      const last = blocks[blocks.length - 1];
      if (!last.gapAfter) last.gapAfter = "\n\n";
      else if (last.gapAfter === "\n") last.gapAfter = "\n\n";
      else if (last.gapAfter === "\r\n") last.gapAfter = "\r\n\r\n";
      blocks.push({ raw: "", gapAfter: "\n", kind: "paragraph", lang: "", headingLevel: 0, tasks: [] });
    }
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.startBlockEdit(this.doc.blocks.length - 1, cx);
  }

  commitBlockEdit(cx, notifyView = true) {
    if (this.editingBlock === null && !this.blockEditor) return;
    const editedIdx = this.editingBlock;
    const wasCode = this.editingMode === "code";
    const fence = this.editingCodeFence;
    const prevCount = this.doc?.blocks?.length ?? 0;
    if (this.blockEditor && editedIdx !== null && this.doc?.blocks?.[editedIdx]) {
      const val = this.blockEditor.value();
      this.doc.blocks[editedIdx].raw = wasCode && fence ? serializeCodeFence(fence, val) : val;
    }
    this.blockEditor?.release();
    this.blockEditor = null;
    this.editingBlock = null;
    this.editingMode = null;
    this.editingCodeFence = null;
    if (!wasCode && editedIdx !== null && this.doc?.blocks?.[editedIdx]?.raw === "" && this.doc.blocks.length > 1) {
      this.doc.blocks.splice(editedIdx, 1);
      if (editedIdx === this.doc.blocks.length && this.doc.blocks.length > 0 && !this.doc.blocks[this.doc.blocks.length - 1].gapAfter) {
        this.doc.blocks[this.doc.blocks.length - 1].gapAfter = "\n";
      }
    }
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.doc = parseMarkdownBlocks(this.liveText);
    const nextCount = this.doc?.blocks?.length ?? 0;
    const delta = nextCount - prevCount;
    if (this.blockPress && editedIdx !== null && this.blockPress.index > editedIdx) {
      this.blockPress.index += delta;
    }
    if (this.codeBlockPress && editedIdx !== null && this.codeBlockPress.index > editedIdx) {
      this.codeBlockPress.index += delta;
    }
    this.committedOut = editedIdx !== null ? { editedIdx, delta } : null;
    if (this.on) this.prefetchRemoteImages(cx);
    if (notifyView) cx.notify();
  }

  startTableCellEdit(blockIndex, row, col, cx) {
    if (!this.file || this.fileError || this.loadingFile || this.truncated) return;
    if (
      this.editingCell?.blockIndex === blockIndex &&
      this.editingCell.row === row &&
      this.editingCell.col === col &&
      this.cellInput
    ) {
      this.cellInput.focus();
      return;
    }
    this.commitActiveInlineEdit(cx, false);
    const block = this.doc?.blocks?.[blockIndex];
    if (!block) return;
    const table = parseMarkdownTable(block.raw);
    if (!table) {
      this.startBlockEdit(blockIndex, cx);
      return;
    }
    const maxRow = table.rows.length - 1;
    const clampedRow = Math.max(-1, Math.min(maxRow, row));
    const clampedCol = Math.max(0, Math.min(table.headers.length - 1, col));
    const val = clampedRow < 0 ? (table.headers[clampedCol] ?? "") : (table.rows[clampedRow]?.[clampedCol] ?? "");
    const input = InputState.new({ value: val });
    this.editingCell = { blockIndex, row: clampedRow, col: clampedCol, table };
    this.cellInput = input;
    if (blockIndex >= (this.mdRenderLimit ?? 0)) this.mdRenderLimit = blockIndex + 16;
    input.on("change", (_event, cx) => {
      if (this.cellInput !== input || this.editingCell?.blockIndex !== blockIndex) return;
      const target = this.doc?.blocks?.[blockIndex];
      if (!target) return;
      const nextVal = input.value();
      if (clampedRow < 0) table.headers[clampedCol] = nextVal;
      else if (table.rows[clampedRow]) table.rows[clampedRow][clampedCol] = nextVal;
      target.raw = serializeMarkdownTable(table);
      this.liveText = serializeMarkdownBlocks(this.doc);
      this.markDirty(cx);
      cx.notify();
    });
    try { input.set_selected_range({ start: val.length, end: val.length }); } catch {}
    input.focus();
    cx.notify();
  }

  commitTableCellEdit(cx, notifyView = true) {
    if (!this.editingCell && !this.cellInput) return;
    const { blockIndex, row, col, table } = this.editingCell ?? {};
    if (this.cellInput && blockIndex !== undefined && table && this.doc?.blocks?.[blockIndex]) {
      const val = this.cellInput.value();
      if (row < 0) table.headers[col] = val;
      else if (table.rows[row]) table.rows[row][col] = val;
      this.doc.blocks[blockIndex].raw = serializeMarkdownTable(table);
    }
    this.cellInput?.release();
    this.cellInput = null;
    this.editingCell = null;
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.doc = parseMarkdownBlocks(this.liveText);
    if (this.on) this.prefetchRemoteImages(cx);
    if (notifyView) cx.notify();
  }

  stepTableCell(delta, cx) {
    if (!this.editingCell) return;
    const { blockIndex, row, col, table } = this.editingCell;
    const colCount = Math.max(1, table.headers.length);
    const totalRows = table.rows.length;
    const linear = (row + 1) * colCount + col + delta;
    if (linear < 0) {
      this.focusAdjacentBlock(blockIndex, -1, cx);
      return;
    }
    if (linear >= (totalRows + 1) * colCount) {
      if (delta > 0) {
        this.commitTableCellEdit(cx, false);
        this.mutateTable(blockIndex, t => {
          t.rows.push(Array(t.headers.length).fill(""));
          return { row: t.rows.length - 1, col: 0 };
        }, cx);
      } else {
        this.focusAdjacentBlock(blockIndex, 1, cx);
      }
      return;
    }
    const nextRow = Math.floor(linear / colCount) - 1;
    const nextCol = linear % colCount;
    this.startTableCellEdit(blockIndex, nextRow, nextCol, cx);
  }

  stepTableRow(dir, cx) {
    if (!this.editingCell) return;
    const { blockIndex, row, col, table } = this.editingCell;
    if (dir < 0 && row <= -1) {
      this.focusAdjacentBlock(blockIndex, -1, cx);
      return;
    }
    if (dir > 0 && row >= table.rows.length - 1) {
      this.focusAdjacentBlock(blockIndex, 1, cx);
      return;
    }
    this.startTableCellEdit(blockIndex, row + dir, col, cx);
  }

  mutateTable(blockIndex, mutateFn, cx) {
    if (!this.file || this.truncated) return;
    this.commitActiveInlineEdit(cx, false);
    const block = this.doc?.blocks?.[blockIndex];
    if (!block) return;
    const table = parseMarkdownTable(block.raw);
    if (!table) return;
    const nextFocus = mutateFn(table);
    block.raw = serializeMarkdownTable(table);
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.doc = parseMarkdownBlocks(this.liveText);
    if (nextFocus) {
      this.startTableCellEdit(blockIndex, nextFocus.row, nextFocus.col, cx);
    } else {
      cx.notify();
    }
  }

  focusAdjacentBlock(fromIndex, dir, cx) {
    const blocks = this.doc?.blocks ?? [];
    if (dir < 0 && fromIndex <= 0) return;
    if (dir > 0 && fromIndex >= blocks.length - 1) return;
    this.commitActiveInlineEdit(cx, false);
    const commitInfo = this.committedOut;
    this.committedOut = null;
    const nextBlocks = this.doc?.blocks ?? [];
    let targetIndex = fromIndex + dir;
    if (commitInfo && dir > 0 && fromIndex >= commitInfo.editedIdx) {
      targetIndex += commitInfo.delta;
    }
    if (targetIndex < 0 || targetIndex >= nextBlocks.length) return;
    const target = nextBlocks[targetIndex];
    if (target.kind === "table") {
      const table = parseMarkdownTable(target.raw);
      if (table) {
        const row = dir > 0 ? -1 : table.rows.length - 1;
        const col = dir > 0 ? 0 : Math.max(0, table.headers.length - 1);
        this.startTableCellEdit(targetIndex, row, col, cx);
        return;
      }
    }
    if (target.kind === "code" && parseCodeFence(target.raw)) {
      this.startCodeBlockEdit(targetIndex, cx, dir > 0 ? "start" : "end");
      return;
    }
    this.startBlockEdit(targetIndex, cx, dir > 0 ? "start" : "end");
  }

  toggleTask(blockIndex, lineIndex, cx) {
    if (!this.file || this.truncated) return;
    this.lastTaskClickAt = Date.now();
    this.commitActiveInlineEdit(cx, false);
    const block = this.doc?.blocks?.[blockIndex];
    if (!block) return;
    block.raw = toggleMarkdownTask(block.raw, lineIndex);
    this.liveText = serializeMarkdownBlocks(this.doc);
    this.markDirty(cx);
    this.doc = parseMarkdownBlocks(this.liveText);
    if (this.mdRenderLimit != null && blockIndex >= this.mdRenderLimit) {
      this.mdRenderLimit = Math.min(this.doc.blocks.length, blockIndex + 1);
    }
    cx.notify();
  }

  prefetchRemoteImages(cx) {
    if (!this.on || !this.file || !this.root) return;
    const kind = detectFileKind(this.file);
    if (!kind.isMarkdown) return;
    const urls = extractMarkdownImageUrls(this.liveText);
    const context = this.context, path = this.file, root = this.root, on = this.on;
    for (const rawUrl of urls) {
      const rel = resolveRelativeAssetPath(path, rawUrl);
      if (!rel || this.remoteImages.has(rel) || this.remoteAttempted.has(rel)) continue;
      this.remoteAttempted.add(rel);
      cx.spawn(async cx => {
        try {
          const dataUrl = await readImageDataUrl(`${root}/${rel}`, { on });
          if (dataUrl && this.file === path && this.current(context)) {
            this.remoteImages.set(rel, dataUrl);
            this.remoteImageRev++;
            const cached = this.fileCache.get(path);
            if (cached && cached.kind === "text") {
              cached.remoteImages = new Map(this.remoteImages);
            }
            cx.notify();
          }
        } catch {}
      });
    }
  }

  isEditingInline() {
    return this.editingBlock !== null || this.editingCell !== null || this.editingCodeLang !== null;
  }

  resetFileViewState(path, { loading = false, keepCode = false } = {}) {
    this.releaseInlineEditors();
    if (!keepCode) {
      this.code?.release();
      this.code = null;
      this.editorSig = null;
    }
    this.file = path;
    this.fileError = null;
    this.diskText = "";
    this.liveText = "";
    this.dirty = false;
    this.truncated = false;
    this.savedUntil = 0;
    this.doc = parseMarkdownBlocks("");
    this.mdRenderLimit = 0;
    this.imageDataUrl = null;
    this.remoteImages = new Map();
    this.remoteAttempted = new Set();
    this.loadingFile = loading;
  }

  applyLoadedText(path, kind, bounded, truncated, cx, { rebuild = true } = {}) {
    this.diskText = bounded;
    this.liveText = bounded;
    this.truncated = Boolean(truncated);
    this.fileError = null;
    if (kind.isMarkdown) {
      this.applyMarkdownDoc(bounded, cx);
      if (this.on) this.prefetchRemoteImages(cx);
    } else {
      this.cancelMdPump();
      this.doc = parseMarkdownBlocks("");
      this.mdRenderLimit = 0;
    }
    this.cachePutText(
      path,
      bounded,
      this.truncated,
      kind.isMarkdown ? this.doc : null,
      kind.isMarkdown ? this.remoteImages : null
    );
    if (rebuild) this.rebuildEditor();
  }

  applyCachedEntry(path, kind, entry, cx) {
    this.releaseInlineEditors();
    this.file = path;
    this.fileError = null;
    this.loadingFile = false;
    this.dirty = false;
    this.savedUntil = 0;
    if (entry.kind === "image") {
      this.code?.release();
      this.code = null;
      this.editorSig = null;
      this.diskText = "";
      this.liveText = "";
      this.truncated = false;
      this.doc = parseMarkdownBlocks("");
      this.mdRenderLimit = 0;
      this.imageDataUrl = entry.imageDataUrl;
      this.remoteImages = new Map();
      this.remoteAttempted = new Set();
    } else {
      this.imageDataUrl = null;
      this.truncated = entry.truncated;
      this.diskText = entry.diskText;
      this.liveText = entry.diskText;
      this.remoteImages = entry.remoteImages ? new Map(entry.remoteImages) : new Map();
      this.remoteAttempted = new Set(this.remoteImages.keys());
      if (this.remoteImages.size > 0) this.remoteImageRev++;
      if (kind.isMarkdown) {
        this.applyMarkdownDoc(entry.diskText, cx, entry.doc);
        if (this.on) this.prefetchRemoteImages(cx);
      } else {
        this.doc = parseMarkdownBlocks("");
        this.mdRenderLimit = 0;
      }
      this.rebuildEditor();
    }
    cx.notify();
  }

  revalidateCachedFile(path, kind, seq, cx) {
    if (checking() || !this.root) return;
    const context = this.context, root = this.root, on = this.on;
    cx.spawn(async cx => {
      try {
        if (kind.isImage) {
          const dataUrl = await readImageDataUrl(`${root}/${path}`, { on });
          if (this.showSeq !== seq || this.file !== path || !this.current(context)) return;
          if (dataUrl === null) {
            this.fileCache.delete(path);
            this.fileError = tr("This file is no longer available", "文件已不存在");
            this.imageDataUrl = null;
            cx.notify();
            return;
          }
          if (dataUrl !== this.imageDataUrl) {
            this.imageDataUrl = dataUrl;
            this.cachePutImage(path, dataUrl);
            cx.notify();
          }
          return;
        }
        const text = await readTextFile(`${root}/${path}`, { on });
        if (this.showSeq !== seq || this.file !== path || !this.current(context) || this.dirty || this.isEditingInline()) return;
        if (text === null) {
          this.fileCache.delete(path);
          this.fileError = tr("This file is no longer available", "文件已不存在");
          this.code?.release(); this.code = null; this.editorSig = null;
          cx.notify();
          return;
        }
        if (text.includes("\0")) {
          this.fileCache.delete(path);
          this.fileError = tr("Binary file · no text preview", "二进制文件 · 无法文本预览");
          this.code?.release(); this.code = null; this.editorSig = null;
          cx.notify();
          return;
        }
        const truncated = text.length > MAX_PREVIEW_CHARS;
        const bounded = truncated ? text.slice(0, MAX_PREVIEW_CHARS) + "\n… (file truncated)" : text;
        if (bounded === this.diskText && !this.fileError) return;
        this.applyLoadedText(path, kind, bounded, truncated, cx);
        cx.notify();
      } catch {}
    });
  }

  pollOpenFile(cx) {
    const path = this.file;
    if (!path || this.loadingFile || this.reloadingFile || this.saving || this.dirty || this.isEditingInline()) return;
    const context = this.context, root = this.root, on = this.on;
    const kind = detectFileKind(path);
    this.reloadingFile = true;
    cx.spawn(async cx => {
      try {
        if (kind.isImage) {
          const dataUrl = await readImageDataUrl(`${root}/${path}`, { on });
          if (this.file !== path || !this.current(context)) return;
          if (dataUrl === null) {
            this.fileCache.delete(path);
            this.fileError = tr("This file is no longer available", "文件已不存在");
            this.imageDataUrl = null;
            cx.notify();
            return;
          }
          if (dataUrl !== this.imageDataUrl || this.fileError) {
            this.imageDataUrl = dataUrl;
            this.fileError = null;
            this.cachePutImage(path, dataUrl);
            cx.notify();
          }
          return;
        }
        const text = await readTextFile(`${root}/${path}`, { on });
        if (this.file !== path || !this.current(context) || this.dirty || this.isEditingInline()) return;
        if (text === null) {
          this.fileCache.delete(path);
          this.fileError = tr("This file is no longer available", "文件已不存在");
          this.code?.release(); this.code = null; this.editorSig = null;
          cx.notify();
          return;
        }
        if (text.includes("\0")) return;
        const truncated = text.length > MAX_PREVIEW_CHARS;
        const bounded = truncated ? text.slice(0, MAX_PREVIEW_CHARS) + "\n… (file truncated)" : text;
        if (bounded === this.diskText && !this.fileError) return;
        this.applyLoadedText(path, kind, bounded, truncated, cx, { rebuild: false });
        if (this.code) {
          this.suppressCodeChange = true;
          try { this.code.set_value(bounded); }
          catch { this.rebuildEditor(); }
          finally { this.suppressCodeChange = false; }
        } else {
          this.rebuildEditor();
        }
        cx.notify();
      } catch {}
      finally { this.reloadingFile = false; }
    });
  }

  show(path, cx) {
    if (!path) {
      this.showSeq++;
      this.cancelAutoSave();
      this.cancelLoadingTimer();
      this.cancelMdPump();
      this.selectedFile = null;
      this.themeMenuOpen = false;
      this.resetFileViewState(null);
      return;
    }
    if (path === this.file && !this.fileError && !this.loadingFile) {
      this.themeMenuOpen = false;
      if (cx) this.expandParents(path, cx);
      return;
    }
    this.cancelAutoSave();
    this.flushActiveInlineToDoc();
    if (
      cx &&
      !checking() &&
      this.file &&
      this.file !== path &&
      this.dirty &&
      !this.saving &&
      !this.truncated &&
      this.root &&
      !detectFileKind(this.file).isImage
    ) {
      const prevPath = this.file;
      const prevLive = this.liveText;
      const prevExpected = this.diskText;
      const prevRoot = this.root;
      const prevOn = this.on;
      this.cachePutText(
        prevPath,
        prevLive,
        false,
        detectFileKind(prevPath).isMarkdown ? parseMarkdownBlocks(prevLive) : null,
        this.remoteImages
      );
      cx.spawn(async () => {
        try {
          await writeTextFile(`${prevRoot}/${prevPath}`, prevLive, prevExpected, { on: prevOn });
        } catch (e) {
          this.fileCache.delete(prevPath);
          notify({ title: tr("Could not save file", "保存文件失败"), body: String(e), level: "error" });
        }
      });
    }
    if (!cx) {
      this.selectedFile = path;
      this.themeMenuOpen = false;
      this.resetFileViewState(path, { loading: true });
      return;
    }
    const seq = ++this.showSeq;
    this.cancelLoadingTimer();
    this.cancelMdPump();
    this.selectedFile = path;
    this.themeMenuOpen = false;
    this.expandParents(path, cx);
    const wasShowingFile = Boolean(this.file);
    if (!wasShowingFile && this.treeOpen() && !checking()) this.ensurePanelRoom();

    const kind = detectFileKind(path);
    const cached = this.cacheGet(path);
    if (cached && ((cached.kind === "image") === kind.isImage)) {
      this.applyCachedEntry(path, kind, cached, cx);
      this.revalidateCachedFile(path, kind, seq, cx);
      return;
    }

    if (!wasShowingFile) {
      this.resetFileViewState(path, { loading: true });
      cx.notify();
    } else if (!checking()) {
      const waitContext = this.context;
      this.loadingTimer = cx.timer.after(80, cx => {
        this.loadingTimer = null;
        if (this.showSeq !== seq || !this.current(waitContext) || this.file === path) return;
        this.resetFileViewState(path, { loading: true });
        cx.notify();
      });
    }

    const context = this.context, root = this.root, on = this.on;
    cx.spawn(async cx => {
      try {
        if (kind.isImage) {
          const dataUrl = await readImageDataUrl(`${root}/${path}`, { on });
          if (this.showSeq !== seq || !this.current(context)) return;
          this.cancelLoadingTimer();
          this.resetFileViewState(path);
          if (dataUrl === null) {
            this.fileCache.delete(path);
            this.fileError = tr("This file is no longer available", "文件已不存在");
            return;
          }
          this.imageDataUrl = dataUrl;
          this.cachePutImage(path, dataUrl);
          return;
        }
        const text = await readTextFile(`${root}/${path}`, { on });
        if (this.showSeq !== seq || !this.current(context)) return;
        this.cancelLoadingTimer();
        if (text === null) {
          this.resetFileViewState(path);
          this.fileCache.delete(path);
          this.fileError = tr("This file is no longer available", "文件已不存在");
          return;
        }
        if (text.includes("\0")) {
          this.resetFileViewState(path);
          this.fileCache.delete(path);
          this.fileError = tr("Binary file · no text preview", "二进制文件 · 无法文本预览");
          return;
        }
        this.resetFileViewState(path, { keepCode: true });
        const truncated = text.length > MAX_PREVIEW_CHARS;
        const bounded = truncated ? text.slice(0, MAX_PREVIEW_CHARS) + "\n… (file truncated)" : text;
        this.applyLoadedText(path, kind, bounded, truncated, cx);
      } catch {
        if (this.showSeq === seq && this.current(context)) {
          this.cancelLoadingTimer();
          this.resetFileViewState(path);
          this.fileError = kind.isImage
            ? tr("Cannot preview this image (too large, unsupported format, or unreadable)", "无法预览此图片（过大、格式不支持或不可读）")
            : tr("Cannot preview this file (binary, too large, or unreadable)", "无法预览此文件（二进制、过大或不可读）");
        }
      } finally {
        if (this.showSeq === seq && this.current(context)) cx.notify();
      }
    });
  }

  saveFile(cx, { keepEditor = false } = {}) {
    this.cancelAutoSave();
    if (keepEditor) {
      this.flushActiveInlineToDoc();
    } else {
      this.commitActiveInlineEdit(cx, false);
    }
    const path = this.file;
    if (!path || !this.dirty || this.saving || this.truncated || detectFileKind(path).isImage) return;
    const nextText = this.liveText, expected = this.diskText, root = this.root, on = this.on;
    this.saving = true;
    cx.notify();
    cx.spawn(async cx => {
      try {
        await writeTextFile(`${root}/${path}`, nextText, expected, { on });
        if (this.file === path) {
          this.diskText = nextText;
          this.dirty = this.liveText !== nextText;
          if (!this.dirty) {
            const isMd = detectFileKind(path).isMarkdown;
            this.cachePutText(path, nextText, this.truncated, isMd ? this.doc : null, isMd ? this.remoteImages : null);
            this.savedUntil = Date.now() + 1800;
            cx.timer.after(1850, cx => {
              if (Date.now() >= this.savedUntil) cx.notify();
            });
          } else {
            this.scheduleAutoSave(cx);
          }
          this.refresh(cx);
        } else {
          const isMd = detectFileKind(path).isMarkdown;
          this.cachePutText(path, nextText, false, isMd ? parseMarkdownBlocks(nextText) : null, null);
        }
      } catch (e) {
        notify({ title: tr("Could not save file", "保存文件失败"), body: String(e), level: "error" });
      } finally {
        this.saving = false;
        cx.notify();
      }
    });
  }

  openLink(url, cx) {
    this.lastLinkClickAt = Date.now();
    const rel = resolveRelativeFileLink(this.file, url);
    if (rel) {
      this.show(rel, cx);
      cx.notify();
      return;
    }
    if (/^https?:\/\//i.test(url)) {
      try { cx.open_url(url); } catch {}
    }
  }

  rows(prefix, depth, out, t) {
    if (out.length >= TOTAL || depth > 40) return out;
    for (const entry of this.entries.get(prefix) ?? []) {
      if (out.length >= TOTAL) break;
      const path = prefix ? `${prefix}/${entry.name}` : entry.name;
      const open = this.folders.has(path);
      const selected = !entry.dir && (this.selectedFile ?? this.file) === path;
      const mark = this.marks[`${this.root}/${path}`];
      out.push(h_flex().id(`file-${path}`).h(26).flex_none().pl(6 + depth * 15).pr(6).gap(6).items_center().rounded(6).cursor_pointer()
        .bg(selected ? (t.selected ?? t.raised) : "#00000000")
        .hover(s => s.bg(selected ? (t.selected ?? t.raised) : t.hover))
        .on_click((_e, cx) => entry.dir ? this.toggle(path, cx) : this.show(path, cx))
        .child(entry.dir ? svg(`mods/files/chevron-${open ? "down" : "right"}.svg`).size(12).flex_none().text_color(t.dim) : div().w(12).flex_none())
        .child(svg(`mods/files/${entry.dir ? "folder" : "file"}.svg`).size(13).flex_none().text_color(selected ? t.foreground : t.muted))
        .child(div().flex_1().min_w(0).truncate().text_size(12.5).text_color(selected ? t.foreground : t.muted).child(entry.name))
        .when(Boolean(mark), el => el.child(div().text_size(10.5).text_color(mark === "A" ? t.ansi2 : mark === "D" || mark === "U" ? t.ansi1 : t.blocked).child(mark))));
      if (entry.dir && open) this.rows(path, depth + 1, out, t);
    }
    return out;
  }

  pillButton(id, label, active, t, onClick) {
    return div()
      .id(id)
      .px(7)
      .py(2)
      .rounded(5)
      .cursor_pointer()
      .text_size(11)
      .font_medium()
      .bg(active ? (t.selected ?? t.raised) : "#00000000")
      .border_1()
      .border_color(active ? (t.accent ?? t.border) : "#00000000")
      .text_color(active ? t.foreground : t.muted)
      .hover(s => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", stopMouse)
      .on_click(onClick)
      .child(label);
  }

  iconButton(id, iconPath, active, title, t, onClick) {
    return div()
      .id(id)
      .w(24)
      .h(24)
      .flex()
      .items_center()
      .justify_center()
      .rounded(5)
      .cursor_pointer()
      .bg(active ? (t.selected ?? t.raised) : "#00000000")
      .text_color(active ? t.foreground : t.muted)
      .hover(s => s.bg(t.hover).text_color(t.foreground))
      .tooltip(title)
      .on_mouse_down("left", stopMouse)
      .on_click(onClick)
      .child(svg(iconPath).size(14));
  }

  renderThemePicker(t, kind, mdMode) {
    const useMd = kind.isMarkdown && mdMode !== "code";
    const list = useMd ? MD_THEMES : CODE_THEMES;
    const currentId = useMd ? this.mdThemeId() : this.codeThemeId();
    const resolved = useMd
      ? list.map(item => resolveMdTheme(item.id, t))
      : list.map(item => resolveCodeTheme(item.id, t));

    return v_flex()
      .id("files-theme-menu")
      .flex_none()
      .px(10)
      .py(8)
      .gap(6)
      .bg(t.surface ?? t.background)
      .border_b(1)
      .border_color(t.border)
      .child(div().text_size(11).font_medium().text_color(t.dim)
        .child(useMd ? tr("Markdown Theme", "Markdown 主题") : tr("Code Highlight Theme", "代码高亮主题")))
      .child(h_flex().flex_wrap().gap(6).children(resolved.map(item => {
        const active = item.id === currentId;
        const label = tr(item.name, item.zhName);
        return h_flex()
          .id(`files-theme-${item.id}`)
          .px(8)
          .py(4)
          .gap(6)
          .items_center()
          .rounded(6)
          .cursor_pointer()
          .bg(active ? (t.selected ?? t.raised) : t.background)
          .border_1()
          .border_color(active ? (t.accent ?? t.foreground) : t.border)
          .hover(s => s.bg(t.hover))
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => {
            setShared(useMd ? "files.mdTheme" : "files.codeTheme", item.id);
            this.themeMenuOpen = false;
            if (!useMd || mdMode === "code") this.rebuildEditor();
            if (useMd && this.editingBlock !== null) {
              const idx = this.editingBlock;
              const wasCode = this.editingMode === "code";
              this.commitActiveInlineEdit(cx, false);
              if (wasCode) this.startCodeBlockEdit(idx, cx);
              else this.startBlockEdit(idx, cx);
              return;
            }
            cx.notify();
          })
          .child(h_flex().size(12).rounded(999).overflow_hidden().border_1().border_color(t.border)
            .child(div().w(6).h(12).bg(item.bgSwatch))
            .child(div().w(6).h(12).bg(item.swatch)))
          .child(div().text_size(11.5).text_color(active ? t.foreground : t.muted).child(label));
      })));
  }

  renderToolbar(t, kind, mdMode, treeOpen) {
    const lines = this.liveText ? this.liveText.split("\n").length : 0;
    const lineText = kind.isImage ? kind.label : tr(`${lines} lines`, `${lines} 行`);
    const useMdTheme = kind.isMarkdown && mdMode !== "code";
    const activeTheme = useMdTheme ? resolveMdTheme(this.mdThemeId(), t) : resolveCodeTheme(this.codeThemeId(), t);
    const themeLabel = tr(activeTheme.name, activeTheme.zhName);
    const copied = Date.now() < this.copiedUntil;
    const recentlySaved = !this.dirty && !this.saving && Date.now() < this.savedUntil;
    const mono = settings()?.font_family;
    const mark = this.file ? this.marks[`${this.root}/${this.file}`] : null;
    const fileName = this.file ? this.file.split("/").pop() : "";

    return h_flex()
      .id("files-toolbar")
      .h(36)
      .flex_none()
      .px(8)
      .gap(6)
      .items_center()
      .justify_between()
      .bg(t.raised ?? t.surface)
      .border_b(1)
      .border_color(t.border)
      .child(
        h_flex().gap(6).items_center().min_w(0).flex_1()
          .child(div().min_w(0).truncate().font_semibold().text_size(12.5).text_color(t.foreground).child(fileName))
          .when(Boolean(mark), el => el.child(div().text_size(10.5).font_semibold().text_color(mark === "A" ? t.ansi2 : mark === "D" || mark === "U" ? t.ansi1 : t.blocked).child(mark)))
          .child(div().px(5).py(1).rounded(4).bg(t.background).border_1().border_color(t.border)
            .when(Boolean(mono), el => el.font_family(mono))
            .text_size(10).text_color(t.muted).child(kind.label))
          .when(!this.loadingFile && !this.fileError && !kind.isImage, el =>
            el.child(div().text_size(11).text_color(t.dim).flex_none().child(lineText)))
          .when(this.saving || this.dirty || recentlySaved, el =>
            el.child(
              div()
                .id("files-doc-status")
                .text_size(10.5)
                .font_medium()
                .text_color(this.dirty || this.saving ? (t.accent ?? t.working) : t.dim)
                .flex_none()
                .child(
                  this.saving
                    ? tr("Saving…", "保存中…")
                    : this.dirty
                      ? tr("Unsaved", "未保存")
                      : tr("Saved", "已保存")
                )
            ))
      )
      .child(
        h_flex().gap(4).items_center().flex_none()
          .when(kind.isMarkdown, el => el
            .child(this.pillButton("files-md-preview", tr("Preview", "预览"), mdMode === "preview", t, (_e, cx) => this.setMdMode("preview", cx)))
            .child(this.pillButton("files-md-code", tr("Code", "源码"), mdMode === "code", t, (_e, cx) => this.setMdMode("code", cx)))
          )
          .when(!kind.isImage && this.dirty && !this.truncated, el =>
            el.child(this.pillButton("files-save", this.saving ? tr("Saving…", "保存中…") : tr("Save", "保存"), true, t, (_e, cx) => this.saveFile(cx)))
          )
          .when(!kind.isImage, el => el.child(
            h_flex()
              .id("files-theme-btn")
              .px(7)
              .py(2)
              .gap(5)
              .items_center()
              .rounded(5)
              .cursor_pointer()
              .bg(this.themeMenuOpen ? (t.selected ?? t.background) : t.background)
              .border_1()
              .border_color(this.themeMenuOpen ? (t.accent ?? t.border) : t.border)
              .hover(s => s.bg(t.hover))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => { this.themeMenuOpen = !this.themeMenuOpen; cx.notify(); })
              .child(div().size(8).rounded(999).bg(activeTheme.swatch))
              .child(div().text_size(11).text_color(t.foreground).child(themeLabel))
              .child(div().text_size(10).text_color(t.dim).child("▾"))
          ))
          .when(!kind.isImage && !this.loadingFile && !this.fileError, el =>
            el.child(this.pillButton("files-copy", copied ? tr("Copied", "已复制") : tr("Copy", "复制"), copied, t, (_e, cx) => {
              try { cx.write_to_clipboard(this.liveText); } catch {}
              this.copiedUntil = Date.now() + 1800;
              cx.notify();
              cx.spawn(async cx => {
                await new Promise(r => setTimeout(r, 1850));
                if (Date.now() >= this.copiedUntil) cx.notify();
              });
            }))
          )
          .child(this.iconButton(
            "files-toggle-tree",
            "mods/files/panel-right.svg",
            treeOpen,
            treeOpen ? tr("Collapse file tree", "收起文件目录") : tr("Expand file tree", "展开文件目录"),
            t,
            (_e, cx) => this.setTreeOpen(!treeOpen, cx)
          ))
      );
  }

  tableCellElement(blockIndex, row, col, rawText, isHeader, t, mdTheme, textViewOptions, blockHover, editable) {
    const rowKey = row < 0 ? "h" : String(row);
    const editingThisCell =
      this.editingCell?.blockIndex === blockIndex &&
      this.editingCell.row === row &&
      this.editingCell.col === col &&
      this.cellInput;
    const displayText = String(rawText ?? "");

    return div()
      .id(`files-md-cell-${blockIndex}-${rowKey}-${col}`)
      .flex_1()
      .min_w(0)
      .min_h(28)
      .px(10)
      .py(4)
      .when(col > 0, el => el.border_l(1).border_color(mdTheme.border))
      .when(editingThisCell, el =>
        el
          .bg(mdTheme.background)
          .border_1()
          .border_color(mdTheme.textView.link ?? t.accent ?? mdTheme.border)
          .child(
            div()
              .id("files-md-table-cell-editor")
              .w_full()
              .child(
                Input.new(this.cellInput)
                  .w_full()
                  .h(20)
                  .text_size(12.5)
                  .when(isHeader, s => s.font_semibold())
                  .text_color(mdTheme.textView.foreground)
              )
          )
      )
      .when(!editingThisCell, el =>
        el
          .when(editable, cell =>
            cell
              .cursor_text()
              .hover(s => s.bg(blockHover))
              .on_mouse_down("left", stopMouse)
              .on_click((_e, cx) => {
                cx.stop_propagation();
                if (Date.now() - this.lastLinkClickAt < 200) return;
                this.startTableCellEdit(blockIndex, row, col, cx);
              })
          )
          .child(
            displayText
              ? (
                INLINE_MD_RE.test(displayText)
                  ? TextView.markdown(
                      `files-md-cell-tv-${this.file}-${blockIndex}-${rowKey}-${col}`,
                      isHeader ? `**${displayText}**` : displayText,
                      textViewOptions
                    )
                      .w_full()
                      .min_w(0)
                      .text_color(mdTheme.textView.foreground)
                      .selectable(false)
                      .scrollable(false)
                      .on_link_click((url, cx) => this.openLink(url, cx))
                  : div()
                      .w_full()
                      .min_h(20)
                      .text_size(12.5)
                      .when(isHeader, s => s.font_semibold())
                      .text_color(mdTheme.textView.foreground)
                      .child(displayText)
              )
              : div().w_full().min_h(20)
          )
      );
  }

  tableActionButton(id, label, t, mdTheme, blockHover, onClick) {
    return div()
      .id(id)
      .px(6)
      .py(1)
      .rounded(4)
      .cursor_pointer()
      .text_size(10.5)
      .text_color(mdTheme.textView.muted)
      .hover(s => s.bg(blockHover).text_color(mdTheme.textView.foreground))
      .on_mouse_down("left", stopMouse)
      .on_click((_e, cx) => {
        cx.stop_propagation();
        onClick(cx);
      })
      .child(label);
  }

  renderTableBlock(i, block, table, t, mdTheme, textViewOptions, blockHover, editable) {
    const isActive = this.editingCell?.blockIndex === i;
    const showControls = editable && (this.hoveredTable === i || isActive);
    const colCount = Math.max(1, table.headers.length);

    return v_flex()
      .id(`files-md-table-${i}`)
      .w_full()
      .min_w(0)
      .my(3)
      .gap(3)
      .key_context("FilesMdTable")
      .on_hover((hovered, cx) => {
        const next = hovered ? i : (this.hoveredTable === i ? null : this.hoveredTable);
        if (next === this.hoveredTable) return;
        this.hoveredTable = next;
        cx.notify();
      })
      .on_mouse_down_out((_e, cx) => {
        if (this.editingCell?.blockIndex === i) {
          this.commitTableCellEdit(cx);
        }
      })
      .on_action("files.tableNext", (_e, cx) => this.stepTableCell(1, cx))
      .on_action("files.tablePrev", (_e, cx) => this.stepTableCell(-1, cx))
      .on_action("files.tableEnter", (_e, cx) => this.stepTableRow(1, cx))
      .on_action("files.tableEscape", (_e, cx) => this.commitTableCellEdit(cx))
      .on_key_down((e, cx) => {
        if (e.key === "up" && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
          cx.stop_propagation();
          this.stepTableRow(-1, cx);
        } else if (e.key === "down" && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
          cx.stop_propagation();
          this.stepTableRow(1, cx);
        } else if (e.key === "escape") {
          cx.stop_propagation();
          this.commitTableCellEdit(cx);
        } else if (e.keystroke === "cmd-s" || e.keystroke === "ctrl-s") {
          cx.stop_propagation();
          this.saveFile(cx);
        }
      })
      .child(
        v_flex()
          .w_full()
          .rounded(6)
          .overflow_hidden()
          .border_1()
          .border_color(mdTheme.border)
          .bg(mdTheme.background)
          .child(
            h_flex()
              .w_full()
              .items_stretch()
              .bg(mdTheme.textView.code_background ?? mdTheme.surface ?? mdTheme.background)
              .when(table.rows.length > 0, el => el.border_b(1).border_color(mdTheme.border))
              .children(table.headers.map((cell, c) =>
                this.tableCellElement(i, -1, c, cell, true, t, mdTheme, textViewOptions, blockHover, editable)
              ))
          )
          .children(table.rows.map((rowCells, r) =>
            h_flex()
              .w_full()
              .items_stretch()
              .bg(r % 2 === 1 ? (mdTheme.surface ?? mdTheme.background) : mdTheme.background)
              .when(r < table.rows.length - 1, el => el.border_b(1).border_color(mdTheme.border))
              .children(Array.from({ length: colCount }, (_, c) =>
                this.tableCellElement(i, r, c, rowCells?.[c] ?? "", false, t, mdTheme, textViewOptions, blockHover, editable)
              ))
          ))
      )
      .when(showControls, el =>
        el.child(
          h_flex()
            .gap(4)
            .px(2)
            .items_center()
            .child(this.tableActionButton(`files-table-add-row-${i}`, tr("+ Row", "+ 行"), t, mdTheme, blockHover, cx =>
              this.mutateTable(i, tbl => {
                tbl.rows.push(Array(colCount).fill(""));
                return { row: tbl.rows.length - 1, col: 0 };
              }, cx)
            ))
            .child(this.tableActionButton(`files-table-add-col-${i}`, tr("+ Col", "+ 列"), t, mdTheme, blockHover, cx =>
              this.mutateTable(i, tbl => {
                tbl.headers.push("");
                tbl.alignments.push(null);
                for (const r of tbl.rows) r.push("");
                return { row: -1, col: tbl.headers.length - 1 };
              }, cx)
            ))
            .when(table.rows.length > 0, b =>
              b.child(this.tableActionButton(`files-table-del-row-${i}`, tr("- Row", "- 行"), t, mdTheme, blockHover, cx =>
                this.mutateTable(i, tbl => {
                  tbl.rows.pop();
                  return null;
                }, cx)
              ))
            )
            .when(colCount > 1, b =>
              b.child(this.tableActionButton(`files-table-del-col-${i}`, tr("- Col", "- 列"), t, mdTheme, blockHover, cx =>
                this.mutateTable(i, tbl => {
                  tbl.headers.pop();
                  tbl.alignments.pop();
                  for (const r of tbl.rows) r.pop();
                  return null;
                }, cx)
              ))
            )
            .child(this.tableActionButton(`files-table-raw-${i}`, tr("Raw", "源码"), t, mdTheme, blockHover, cx =>
              this.startBlockEdit(i, cx)
            ))
        )
      );
  }

  renderCodeFenceBlock(i, block, fence, t, mdTheme, textViewOptions, blockHover, editable) {
    const isEditingBody = this.editingBlock === i && this.editingMode === "code" && this.blockEditor;
    const isEditingLang = this.editingCodeLang?.blockIndex === i && this.codeLangInput;
    const codeBg = mdTheme.textView.code_background ?? mdTheme.surface ?? mdTheme.background;
    const mono = settings()?.font_family;
    const editorHeight = Math.max(40, Math.min(540, this.blockEditorRows * 19 + 12));

    return v_flex()
      .id(`files-md-code-block-${i}`)
      .w_full()
      .min_w(0)
      .my(3)
      .rounded(6)
      .overflow_hidden()
      .bg(codeBg)
      .border_1()
      .border_color(isEditingBody ? (mdTheme.textView.link ?? t.accent ?? mdTheme.border) : mdTheme.border)
      .when(isEditingBody, el =>
        el
          .on_mouse_down("left", stopMouse)
          .on_mouse_down_out((_e, cx) => this.commitBlockEdit(cx))
          .on_key_down((e, cx) => {
            if (e.key === "escape" || e.keystroke === "cmd-enter" || e.keystroke === "ctrl-enter") {
              cx.stop_propagation();
              this.commitBlockEdit(cx);
            } else if (e.keystroke === "cmd-s" || e.keystroke === "ctrl-s") {
              cx.stop_propagation();
              this.saveFile(cx);
            } else if ((e.key === "up" || e.key === "left") && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
              cx.stop_propagation();
              this.focusAdjacentBlock(i, -1, cx);
            } else if ((e.key === "down" || e.key === "right") && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
              cx.stop_propagation();
              this.focusAdjacentBlock(i, 1, cx);
            }
          })
      )
      .child(
        h_flex()
          .h(26)
          .px(10)
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(mdTheme.border)
          .child(
            isEditingLang
              ? div()
                  .id("files-md-code-lang-input")
                  .w(110)
                  .on_mouse_down("left", stopMouse)
                  .on_mouse_down_out((_e, cx) => this.commitCodeLangEdit(cx))
                  .on_key_down((e, cx) => {
                    if (e.key === "escape") {
                      cx.stop_propagation();
                      this.commitCodeLangEdit(cx);
                    } else if (e.keystroke === "cmd-s" || e.keystroke === "ctrl-s") {
                      cx.stop_propagation();
                      this.saveFile(cx);
                    }
                  })
                  .child(
                    Input.new(this.codeLangInput)
                      .w_full()
                      .h(20)
                      .when(Boolean(mono), s => s.font_family(mono))
                      .text_size(11)
                      .text_color(mdTheme.textView.foreground)
                  )
              : div()
                  .id(`files-md-code-lang-${i}`)
                  .px(5)
                  .py(1)
                  .rounded(4)
                  .when(Boolean(mono), s => s.font_family(mono))
                  .text_size(11)
                  .text_color(mdTheme.textView.muted)
                  .when(editable, s =>
                    s
                      .cursor_pointer()
                      .hover(h => h.bg(blockHover).text_color(mdTheme.textView.foreground))
                      .on_mouse_down("left", stopMouse)
                      .on_click((_e, cx) => {
                        cx.stop_propagation();
                        this.startCodeLangEdit(i, cx);
                      })
                  )
                  .child(fence.lang || tr("plain", "文本"))
          )
          .child(
            this.tableActionButton(`files-md-code-copy-${i}`, tr("Copy", "复制"), t, mdTheme, blockHover, cx => {
              try { cx.write_to_clipboard(fence.body); } catch {}
              notify({ title: tr("Copied code block", "已复制代码块"), level: "info" });
            })
          )
      )
      .child(
        isEditingBody
          ? div()
              .id("files-md-code-editor")
              .w_full()
              .h(editorHeight)
              .px(6)
              .py(4)
              .child(Editor.new(this.blockEditor).size_full().bg(codeBg))
          : div()
              .id(`files-md-code-body-${i}`)
              .w_full()
              .min_w(0)
              .px(10)
              .py(6)
              .when(editable, el =>
                el
                  .cursor_text()
                  .hover(s => s.bg(blockHover))
                  .on_mouse_down("left", e => {
                    this.codeBlockPress = { index: i, x: e.position?.x ?? 0, y: e.position?.y ?? 0 };
                  })
                  .on_mouse_up("left", (e, cx) => {
                    const press = this.codeBlockPress;
                    this.codeBlockPress = null;
                    const commitOut = this.committedOut;
                    this.committedOut = null;
                    if (!press || press.index !== i) return;
                    const dx = (e.position?.x ?? press.x) - press.x;
                    const dy = (e.position?.y ?? press.y) - press.y;
                    if (dx * dx + dy * dy > 64) return;
                    const targetIndex = commitOut && i > commitOut.editedIdx ? i + commitOut.delta : i;
                    if (this.doc?.blocks?.[targetIndex]) {
                      this.startCodeBlockEdit(targetIndex, cx, "end");
                    }
                  })
              )
              .child(
                fence.body
                  ? TextView.markdown(`files-md-code-tv-${this.file}-${i}`, block.raw, textViewOptions)
                      .w_full()
                      .min_w(0)
                      .text_color(mdTheme.textView.foreground)
                      .selectable(true)
                      .scrollable(false)
                  : div()
                      .when(Boolean(mono), s => s.font_family(mono))
                      .text_size(12)
                      .text_color(mdTheme.textView.muted)
                      .child(tr("Empty code block · click to write", "空代码块 · 点击编写代码"))
              )
      );
  }

  renderMarkdownPreview(t, mdTheme) {
    const baseDir = this.file.includes("/")
      ? `${this.root}/${this.file.slice(0, this.file.lastIndexOf("/"))}`
      : this.root;
    const textViewOptions = {
      ...mdTheme.textView,
      base_dir: this.on ? undefined : baseDir,
      root_dir: this.on ? undefined : this.root,
      allow_data_images: true,
    };
    const editable = !this.truncated;
    const blocks = this.doc?.blocks ?? [];
    const blockHover = mdTheme.light ? "#00000008" : "#ffffff0a";
    const editorBg = mdTheme.background;
    const monoFont = settings()?.font_family;

    const limit = Math.min(blocks.length, this.mdRenderLimit ?? blocks.length);
    const items = [];
    for (let i = 0; i < limit; i++) {
      const block = blocks[i];
      if (this.editingBlock === i && this.editingMode === "block" && this.blockEditor) {
        const typo = blockTypography(block, this.sansFont, monoFont);
        const editorHeight = Math.max(typo.lineHeight + 8, Math.min(540, this.blockEditorRows * typo.lineHeight + 8));
        items.push(
          v_flex()
            .id("files-md-block-editor")
            .w_full()
            .min_w(0)
            .px(8)
            .py(2)
            .bg(editorBg)
            .border_l(2)
            .border_color(block.kind === "blockquote" ? mdTheme.border : (mdTheme.textView.link ?? t.accent ?? mdTheme.border))
            .on_mouse_down("left", stopMouse)
            .on_mouse_down_out((_e, cx) => {
              this.commitBlockEdit(cx);
            })
            .on_key_down((e, cx) => {
              if (e.key === "escape" || e.keystroke === "cmd-enter" || e.keystroke === "ctrl-enter") {
                cx.stop_propagation();
                this.commitBlockEdit(cx);
              } else if (e.keystroke === "cmd-s" || e.keystroke === "ctrl-s") {
                cx.stop_propagation();
                this.saveFile(cx);
              } else if ((e.key === "up" || e.key === "left") && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
                cx.stop_propagation();
                this.focusAdjacentBlock(i, -1, cx);
              } else if ((e.key === "down" || e.key === "right") && !e.modifiers?.shift && !e.modifiers?.control && !e.modifiers?.alt && !e.modifiers?.platform) {
                cx.stop_propagation();
                this.focusAdjacentBlock(i, 1, cx);
              }
            })
            .child(
              div()
                .w_full()
                .h(editorHeight)
                .child(
                  Editor.new(this.blockEditor)
                    .size_full()
                    .bg(editorBg)
                    .when(typo.bold, el => el.font_semibold())
                )
            )
        );
        continue;
      }

      if (block.kind === "table") {
        const table = this.getBlockTable(block);
        if (table) {
          items.push(this.renderTableBlock(i, block, table, t, mdTheme, textViewOptions, blockHover, editable));
          continue;
        }
      }

      if (block.kind === "code") {
        const fence = this.getBlockFence(block);
        if (fence) {
          items.push(this.renderCodeFenceBlock(i, block, fence, t, mdTheme, textViewOptions, blockHover, editable));
          continue;
        }
      }

      const rendered = this.getBlockRendered(block, this.doc.defs);
      const blockEl = v_flex()
        .id(`files-md-block-${i}`)
        .w_full()
        .min_w(0)
        .rounded(6)
        .px(8)
        .py(2)
        .when(editable, el => el
          .cursor_text()
          .hover(s => s.bg(blockHover))
          .on_mouse_down("left", (e) => {
            this.blockPress = { index: i, x: e.position?.x ?? 0, y: e.position?.y ?? 0 };
          })
          .on_mouse_up("left", (e, cx) => {
            const press = this.blockPress;
            this.blockPress = null;
            const commitOut = this.committedOut;
            this.committedOut = null;
            if (!press || press.index !== i) return;
            if (Date.now() - this.lastLinkClickAt < 200 || Date.now() - this.lastTaskClickAt < 200) return;
            const dx = (e.position?.x ?? press.x) - press.x;
            const dy = (e.position?.y ?? press.y) - press.y;
            if (dx * dx + dy * dy > 64) return;
            const targetIndex = commitOut && i > commitOut.editedIdx ? i + commitOut.delta : i;
            if (this.doc?.blocks?.[targetIndex]) {
              this.startBlockEdit(targetIndex, cx);
            }
          })
        )
        .child(
          TextView.markdown(`files-md-${this.file}-${i}`, rendered, textViewOptions)
            .w_full()
            .min_w(0)
            .text_color(mdTheme.textView.foreground)
            .selectable(true)
            .scrollable(false)
            .on_link_click((url, cx) => this.openLink(url, cx))
        )
        .when(editable && block.tasks.length > 0, el =>
          el.child(
            h_flex()
              .flex_wrap()
              .gap(4)
              .pt(3)
              .pb(2)
              .children(block.tasks.map(task =>
                h_flex()
                  .id(`files-task-${i}-${task.lineIndex}`)
                  .px(6)
                  .py(1)
                  .gap(4)
                  .items_center()
                  .rounded(4)
                  .cursor_pointer()
                  .bg(mdTheme.surface ?? t.raised)
                  .border_1()
                  .border_color(mdTheme.border)
                  .hover(s => s.bg(blockHover))
                  .on_mouse_down("left", stopMouse)
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    this.toggleTask(i, task.lineIndex, cx);
                  })
                  .child(div().text_size(11).text_color(task.checked ? (mdTheme.textView.link ?? t.accent) : mdTheme.textView.muted).child(task.checked ? "☑" : "☐"))
                  .child(div().max_w(220).truncate().text_size(11).text_color(mdTheme.textView.muted).child(task.label || tr("Task", "任务")))
              ))
          )
        );
      items.push(blockEl);
    }

    return v_flex()
      .id("files-md-view")
      .flex_1()
      .min_h(0)
      .w_full()
      .overflow_y_scrollbar()
      .bg(mdTheme.background)
      .text_color(mdTheme.textView.foreground)
      .px(14)
      .py(12)
      .gap(4)
      .when(items.length === 0, el => el.child(
        div()
          .id("files-md-empty")
          .px(10)
          .py(12)
          .rounded(6)
          .text_size(12.5)
          .text_color(mdTheme.textView.muted)
          .when(editable, e => e
            .cursor_text()
            .hover(s => s.bg(blockHover))
            .on_mouse_down("left", stopMouse)
            .on_click((_e, cx) => this.startNewBlockEdit(cx))
          )
          .child(editable ? tr("Empty Markdown file · click to write", "空 Markdown 文件 · 点击开始编写") : tr("Empty file", "空文件"))
      ))
      .children(items)
      .when(limit < blocks.length, el => el.child(
        div()
          .px(8)
          .py(6)
          .text_size(11.5)
          .text_color(mdTheme.textView.muted)
          .child(tr(`Rendering ${limit}/${blocks.length} blocks…`, `正在渲染 ${limit}/${blocks.length} 段…`))
      ))
      .when(limit === blocks.length && editable && items.length > 0 && !this.isEditingInline(), el => el.child(
        div()
          .id("files-md-tail")
          .w_full()
          .min_h(72)
          .flex_1()
          .rounded(6)
          .cursor_text()
          .on_mouse_down("left", stopMouse)
          .on_click((_e, cx) => this.startNewBlockEdit(cx))
      ));
  }

  renderFileBody(t, kind, mdMode) {
    if (this.loadingFile || this.fileError) {
      return div().flex_1().min_h(0).p(14).text_color(t.dim)
        .child(this.fileError || tr("Loading file…", "正在加载文件…"));
    }
    const mdTheme = resolveMdTheme(this.mdThemeId(), t);
    const codeTheme = this.activeEditorStyle(kind, t);

    if (kind.isImage) {
      return v_flex()
        .id("files-image-view")
        .flex_1()
        .min_h(0)
        .overflow_y_scrollbar()
        .p(16)
        .bg(t.surface ?? t.background)
        .child(
          this.imageDataUrl
            ? TextView.markdown(`files-img-${this.file}`, `![${this.file}](${this.imageDataUrl})`, { ...mdTheme.textView, allow_data_images: true })
                .w_full()
                .min_w(0)
                .scrollable(false)
            : div().text_color(t.dim).child(tr("Loading image…", "正在加载图片…"))
        );
    }

    if (kind.isMarkdown && mdMode === "preview") {
      return this.renderMarkdownPreview(t, mdTheme);
    }

    return div()
      .id("files-code-view")
      .flex_1()
      .min_h(0)
      .overflow_hidden()
      .bg(codeTheme.colors.background)
      .child(this.code ? Editor.new(this.code).size_full().bg(codeTheme.colors.background) : div().p(12).text_color(t.dim).child(tr("Preparing editor…", "正在准备编辑器…")));
  }

  render(cx) {
    if (key() !== this.context?.key) this.refresh(cx);
    const t = theme();
    const kind = this.file ? detectFileKind(this.file) : null;
    const mdMode = this.mdMode();
    if (this.file && !this.fileError && !this.loadingFile && !kind.isImage && (!kind.isMarkdown || mdMode === "code")) {
      const codeTheme = this.activeEditorStyle(kind, t);
      const readonly = Boolean(this.truncated);
      const softWrap = kind.isMarkdown ? true : this.softWrap();
      const sig = editorSignature(kind.language, readonly, softWrap, settings()?.font_family, codeTheme);
      if (this.editorSig !== sig && !this.pendingRebuild) {
        this.pendingRebuild = true;
        cx.spawn(async cx => {
          this.pendingRebuild = false;
          this.rebuildEditor();
          cx.notify();
        });
      }
    }

    const treeOpen = this.treeOpen();
    const treeVisible = !this.file || treeOpen;
    const rootName = this.root?.split(/[\\/]/).filter(Boolean).pop() ?? tr("Files", "文件");

    const treeSidebar = treeVisible ? v_flex()
      .size_full()
      .min_h(0)
      .bg(t.surface ?? t.background)
      .border_l(1)
      .border_color(t.border)
      .child(
        h_flex()
          .h(36)
          .flex_none()
          .px(10)
          .gap(6)
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex().gap(6).items_center().min_w(0).flex_1()
              .child(svg("mods/files/folder.svg").size(13).flex_none().text_color(t.muted))
              .child(div().min_w(0).truncate().font_semibold().text_size(12).text_color(t.foreground).child(rootName))
          )
          .when(Boolean(this.file), el => el.child(
            this.iconButton("files-collapse-tree", "mods/files/panel-right.svg", false, tr("Collapse file tree", "收起文件目录"), t, (_e, cx) => this.setTreeOpen(false, cx))
          ))
      )
      .child(
        v_flex()
          .id("files-tree")
          .flex_1()
          .min_h(0)
          .overflow_y_scrollbar()
          .p(6)
          .children(this.rows("", 0, [], t))
          .when(Boolean(this.error) || !this.entries.get("")?.length, el => el.child(div().p(8).text_size(12).text_color(t.dim).child(this.error || (this.loading ? tr("Loading files…", "正在加载文件…") : this.root ? tr("No files", "暂无文件") : tr("No folder", "无工作目录")))))
          .when(this.limited, el => el.child(div().p(8).text_size(11).text_color(t.dim).child(tr("File list limited to 500 entries per folder and 5,000 total", "文件列表限制为每目录最多 500 项，总计 5,000 项"))))
      ) : null;

    const detailPane = this.file
      ? v_flex()
          .size_full()
          .min_w(0)
          .min_h(0)
          .child(this.renderToolbar(t, kind, mdMode, treeOpen))
          .when(this.themeMenuOpen && !kind.isImage, panel => panel.child(this.renderThemePicker(t, kind, mdMode)))
          .child(this.renderFileBody(t, kind, mdMode))
      : v_flex()
          .size_full()
          .min_w(0)
          .min_h(0)
          .items_center()
          .justify_center()
          .p(20)
          .text_size(12.5)
          .text_color(t.dim)
          .child(tr("Select a file from the directory on the right to view or edit", "从右侧目录选择文件以预览或编辑"));

    return v_flex()
      .id("files")
      .size_full()
      .min_h(0)
      .rounded(10)
      .overflow_hidden()
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .text_color(t.foreground)
      .text_size(13)
      .track_focus(this.handle)
      .on_key_down((e, cx) => {
        if ((e.keystroke === "cmd-s" || e.keystroke === "ctrl-s") && this.file && !detectFileKind(this.file).isImage && !this.truncated) {
          cx.stop_propagation();
          this.saveFile(cx);
        }
      })
      .child(
        h_resizable("files-split")
          .flex_1()
          .min_h(0)
          .w_full()
          .child(
            resizable_panel()
              .child(detailPane)
          )
          .child(
            resizable_panel()
              .size(210)
              .size_range(140, 380)
              .visible(treeVisible)
              .when(Boolean(treeSidebar), p => p.child(treeSidebar))
          )
      );
  }
}
