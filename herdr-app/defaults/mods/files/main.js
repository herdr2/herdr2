// A lazy file browser for the focused pane's folder, with live Markdown rendering and IDE-themed code viewing.
import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, Editor, EditorState, TextView } from "gpui-base";
import { spaces, panes, focused, exec, readTextFile, writeTextFile, watchFolder, theme, settings, window, shared, setShared, checking, notify } from "herdr";
import { detectFileKind, CODE_THEMES, MD_THEMES, resolveCodeTheme, resolveMdTheme, prepareMarkdown, resolveRelativeFileLink } from "./themes.js";

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

function editorSignature(file, language, readonly, softWrap, fontFamily, codeTheme) {
  return JSON.stringify([file, language, readonly, softWrap, fontFamily ?? null, codeTheme.id, codeTheme.colors.background, codeTheme.colors.foreground, codeTheme.syntax.keyword]);
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
    this.saved = new Map();
    this.entries = new Map(); this.folders = new Set(); this.pending = new Set(); this.marks = {};
    this.diskText = ""; this.liveText = ""; this.dirty = false; this.truncated = false;
    this.themeMenuOpen = false; this.copiedUntil = 0; this.saving = false;
    this.refresh(cx);
    cx.timer.every(10000, cx => this.refresh(cx));
    if (!checking()) cx.timer.every(1500, cx => this.pollOpenFile(cx));
  }

  mdMode() {
    const mode = readSetting("files.mdMode", "preview");
    return mode === "split" || mode === "code" ? mode : "preview";
  }

  setMdMode(mode, cx) {
    setShared("files.mdMode", mode);
    this.themeMenuOpen = false;
    if (this.file && mode !== "preview") this.rebuildEditor();
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
    if (kind.isMarkdown && this.mdMode() === "split") {
      const md = resolveMdTheme(this.mdThemeId(), appTheme);
      return resolveCodeTheme(md.editorThemeId, appTheme);
    }
    return resolveCodeTheme(this.codeThemeId(), appTheme);
  }

  rebuildEditor() {
    this.code?.release();
    this.code = null;
    this.editorSig = null;
    if (!this.file || this.fileError || this.loadingFile) return;
    const kind = detectFileKind(this.file);
    const mdMode = this.mdMode();
    if (kind.isMarkdown && mdMode === "preview") return;

    const readonly = !(kind.isMarkdown && mdMode === "split" && !this.truncated);
    const softWrap = (kind.isMarkdown && mdMode === "split") ? true : this.softWrap();
    const fontFamily = settings()?.font_family;
    const codeTheme = this.activeEditorStyle(kind, theme());
    const sig = editorSignature(this.file, kind.language, readonly, softWrap, fontFamily, codeTheme);
    const state = EditorState.new({
      language: kind.language,
      readonly,
      soft_wrap: softWrap,
      value: this.liveText,
      font_family: fontFamily,
      font_size: 12.5,
      colors: codeTheme.colors,
      syntax: codeTheme.syntax,
    });
    if (!readonly) {
      state.on("change", (_event, cx) => {
        if (this.code !== state) return;
        this.liveText = state.value();
        this.dirty = this.liveText !== this.diskText;
        cx.notify();
      });
    }
    this.code = state;
    this.editorSig = sig;
  }

  pollOpenFile(cx) {
    const path = this.file;
    if (!path || this.loadingFile || this.reloadingFile || this.saving || this.dirty) return;
    const context = this.context, root = this.root, on = this.on;
    this.reloadingFile = true;
    cx.spawn(async cx => {
      try {
        const text = await readTextFile(`${root}/${path}`, { on });
        if (this.file !== path || !this.current(context) || this.dirty) return;
        if (text === null) {
          this.fileError = tr("This file is no longer available", "文件已不存在");
          this.code?.release(); this.code = null;
          cx.notify();
          return;
        }
        if (text.includes("\0")) return;
        const truncated = text.length > MAX_PREVIEW_CHARS;
        const bounded = truncated ? text.slice(0, MAX_PREVIEW_CHARS) + "\n… (file truncated)" : text;
        if (bounded === this.diskText && !this.fileError) return;
        this.diskText = bounded;
        this.liveText = bounded;
        this.truncated = truncated;
        this.fileError = null;
        if (this.code) {
          try { this.code.set_value(bounded); }
          catch { this.rebuildEditor(); }
        } else {
          this.rebuildEditor();
        }
        cx.notify();
      } catch {}
      finally { this.reloadingFile = false; }
    });
  }

  show(path, cx) {
    this.code?.release(); this.code = null; this.editorSig = null;
    this.file = path; this.fileError = null;
    this.diskText = ""; this.liveText = ""; this.dirty = false; this.truncated = false;
    this.loadingFile = Boolean(path); this.themeMenuOpen = false;
    if (!path || !cx) return;
    const context = this.context, root = this.root, on = this.on;
    cx.spawn(async cx => {
      try {
        const text = await readTextFile(`${root}/${path}`, { on });
        if (this.file !== path || !this.current(context)) return;
        this.loadingFile = false;
        if (text === null) { this.fileError = tr("This file is no longer available", "文件已不存在"); return; }
        if (text.includes("\0")) { this.fileError = tr("Binary file · no text preview", "二进制文件 · 无法文本预览"); return; }
        this.truncated = text.length > MAX_PREVIEW_CHARS;
        const bounded = this.truncated ? text.slice(0, MAX_PREVIEW_CHARS) + "\n… (file truncated)" : text;
        this.diskText = bounded;
        this.liveText = bounded;
        this.rebuildEditor();
      } catch {
        if (this.file === path && this.current(context)) {
          this.loadingFile = false;
          this.fileError = tr("Cannot preview this file (binary, too large, or unreadable)", "无法预览此文件（二进制、过大或不可读）");
        }
      }
      finally { cx.notify(); }
    });
  }

  saveFile(cx) {
    const path = this.file;
    if (!path || !this.dirty || this.saving || this.on || this.truncated) return;
    const nextText = this.liveText, expected = this.diskText, root = this.root;
    this.saving = true;
    cx.notify();
    cx.spawn(async cx => {
      try {
        await writeTextFile(`${root}/${path}`, nextText, expected);
        if (this.file === path) {
          this.diskText = nextText;
          this.dirty = this.liveText !== nextText;
          this.refresh(cx);
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
      const path = prefix ? `${prefix}/${entry.name}` : entry.name, open = this.folders.has(path), mark = this.marks[`${this.root}/${path}`];
      out.push(h_flex().id(`file-${path}`).h(28).flex_none().pl(8 + depth * 18).pr(8).gap(7).items_center().rounded(7).cursor_pointer().hover(s => s.bg(t.hover))
        .on_click((_e, cx) => entry.dir ? this.toggle(path, cx) : this.show(path, cx))
        .child(entry.dir ? svg(`mods/files/chevron-${open ? "down" : "right"}.svg`).size(12).flex_none().text_color(t.dim) : div().w(12).flex_none())
        .child(svg(`mods/files/${entry.dir ? "folder" : "file"}.svg`).size(14).flex_none().text_color(t.muted))
        .child(div().flex_1().min_w(0).truncate().child(entry.name))
        .when(Boolean(mark), el => el.child(div().text_size(11).text_color(mark === "A" ? t.ansi2 : mark === "D" || mark === "U" ? t.ansi1 : t.blocked).child(mark))));
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
            if (!useMd || mdMode === "split") this.rebuildEditor();
            cx.notify();
          })
          .child(h_flex().size(12).rounded(999).overflow_hidden().border_1().border_color(t.border)
            .child(div().w(6).h(12).bg(item.bgSwatch))
            .child(div().w(6).h(12).bg(item.swatch)))
          .child(div().text_size(11.5).text_color(active ? t.foreground : t.muted).child(label));
      })));
  }

  renderToolbar(t, kind, mdMode) {
    const lines = this.liveText ? this.liveText.split("\n").length : 0;
    const lineText = tr(`${lines} lines`, `${lines} 行`);
    const useMdTheme = kind.isMarkdown && mdMode !== "code";
    const activeTheme = useMdTheme ? resolveMdTheme(this.mdThemeId(), t) : resolveCodeTheme(this.codeThemeId(), t);
    const themeLabel = tr(activeTheme.name, activeTheme.zhName);
    const copied = Date.now() < this.copiedUntil;
    const mono = settings()?.font_family;

    return h_flex()
      .id("files-toolbar")
      .h(32)
      .flex_none()
      .px(10)
      .gap(6)
      .items_center()
      .justify_between()
      .bg(t.raised ?? t.surface)
      .border_b(1)
      .border_color(t.border)
      .child(
        h_flex().gap(6).items_center().min_w(0)
          .child(div().px(6).py(1).rounded(4).bg(t.background).border_1().border_color(t.border)
            .when(Boolean(mono), el => el.font_family(mono))
            .text_size(10.5).text_color(t.foreground).child(kind.label))
          .when(!this.loadingFile && !this.fileError, el =>
            el.child(div().text_size(11).text_color(t.dim).child(lineText)))
          .when(this.dirty, el =>
            el.child(div().text_size(10.5).text_color(t.accent ?? t.working).child(tr("Modified", "已修改"))))
      )
      .child(
        h_flex().gap(4).items_center().flex_none()
          .when(kind.isMarkdown, el => el
            .child(this.pillButton("files-md-preview", tr("Preview", "预览"), mdMode === "preview", t, (_e, cx) => this.setMdMode("preview", cx)))
            .child(this.pillButton("files-md-split", tr("Split", "分栏"), mdMode === "split", t, (_e, cx) => this.setMdMode("split", cx)))
            .child(this.pillButton("files-md-code", tr("Code", "源码"), mdMode === "code", t, (_e, cx) => this.setMdMode("code", cx)))
          )
          .when(this.dirty && !this.on && !this.truncated && mdMode === "split", el =>
            el.child(this.pillButton("files-save", this.saving ? tr("Saving…", "保存中…") : tr("Save", "保存"), true, t, (_e, cx) => this.saveFile(cx)))
          )
          .when(!kind.isMarkdown || mdMode === "code", el =>
            el.child(this.pillButton("files-wrap", tr("Wrap", "换行"), this.softWrap(), t, (_e, cx) => {
              setShared("files.wordWrap", !this.softWrap());
              this.rebuildEditor();
              cx.notify();
            }))
          )
          .child(
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
          )
          .when(!this.loadingFile && !this.fileError, el =>
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
      );
  }

  renderFileBody(t, kind, mdMode) {
    if (this.loadingFile || this.fileError) {
      return div().flex_1().min_h(0).p(14).text_color(t.dim)
        .child(this.fileError || tr("Loading file…", "正在加载文件…"));
    }
    const mdTheme = resolveMdTheme(this.mdThemeId(), t);
    const codeTheme = this.activeEditorStyle(kind, t);

    if (kind.isMarkdown && mdMode === "preview") {
      return div()
        .id("files-md-view")
        .flex_1()
        .min_h(0)
        .overflow_hidden()
        .bg(mdTheme.background)
        .text_color(mdTheme.textView.foreground)
        .child(
          TextView.markdown(`files-md-${this.file}`, prepareMarkdown(this.liveText), mdTheme.textView)
            .size_full()
            .px(20)
            .py(16)
            .text_color(mdTheme.textView.foreground)
            .selectable(true)
            .scrollable(true)
            .on_link_click((url, cx) => this.openLink(url, cx))
        );
    }

    if (kind.isMarkdown && mdMode === "split") {
      return h_flex()
        .id("files-md-split-view")
        .flex_1()
        .min_h(0)
        .overflow_hidden()
        .child(
          div()
            .flex_1()
            .min_w(0)
            .h_full()
            .overflow_hidden()
            .border_r(1)
            .border_color(t.border)
            .bg(codeTheme.colors.background)
            .child(this.code ? Editor.new(this.code).size_full().bg(codeTheme.colors.background) : div().p(12).text_color(t.dim).child(tr("Preparing editor…", "正在准备编辑器…")))
        )
        .child(
          div()
            .flex_1()
            .min_w(0)
            .h_full()
            .overflow_hidden()
            .bg(mdTheme.background)
            .text_color(mdTheme.textView.foreground)
            .child(
              TextView.markdown(`files-md-split-${this.file}`, prepareMarkdown(this.liveText), mdTheme.textView)
                .size_full()
                .px(16)
                .py(14)
                .text_color(mdTheme.textView.foreground)
                .selectable(true)
                .scrollable(true)
                .on_link_click((url, cx) => this.openLink(url, cx))
            )
        );
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
    if (this.file && !this.fileError && !this.loadingFile && (!kind.isMarkdown || mdMode !== "preview")) {
      const codeTheme = this.activeEditorStyle(kind, t);
      const readonly = !(kind.isMarkdown && mdMode === "split" && !this.truncated);
      const softWrap = (kind.isMarkdown && mdMode === "split") ? true : this.softWrap();
      const sig = editorSignature(this.file, kind.language, readonly, softWrap, settings()?.font_family, codeTheme);
      if (this.editorSig !== sig && !this.pendingRebuild) {
        this.pendingRebuild = true;
        cx.spawn(async cx => {
          this.pendingRebuild = false;
          this.rebuildEditor();
          cx.notify();
        });
      }
    }

    const mark = this.file ? this.marks[`${this.root}/${this.file}`] : null;
    return v_flex().id("files").size_full().min_h(0).rounded(10).overflow_hidden().bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13)
      .child(h_flex().h(38).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
        .when(Boolean(this.file), el => el.child(div().id("files-back").w(22).h(22).flex().items_center().justify_center().rounded(5).cursor_pointer().text_color(t.muted).hover(s => s.bg(t.hover).text_color(t.foreground)).on_click((_e, cx) => { this.show(null); cx.notify(); }).child("←")))
        .child(div().font_semibold().child(this.file ? this.file.split("/").pop() : tr("Files", "文件")))
        .when(Boolean(mark), el => el.child(div().text_size(11).font_semibold().text_color(mark === "A" ? t.ansi2 : mark === "D" || mark === "U" ? t.ansi1 : t.blocked).child(mark)))
        .child(div().flex_1().min_w(0).truncate().text_right().text_size(11.5).text_color(t.dim).child(this.file ?? this.root?.split(/[\\/]/).pop() ?? "")))
      .when(!this.file, el => el.child(v_flex().id("files-tree").flex_1().min_h(0).overflow_y_scrollbar().p(6).children(this.rows("", 0, [], t))
        .when(Boolean(this.error) || !this.entries.get("")?.length, el => el.child(div().p(8).text_size(12).text_color(t.dim).child(this.error || (this.loading ? tr("Loading files…", "正在加载文件…") : this.root ? tr("No files", "暂无文件") : tr("No folder", "无工作目录")))))
        .when(this.limited, el => el.child(div().p(8).text_size(11).text_color(t.dim).child(tr("File list limited to 500 entries per folder and 5,000 total", "文件列表限制为每目录最多 500 项，总计 5,000 项"))))))
      .when(Boolean(this.file), el => el
        .child(this.renderToolbar(t, kind, mdMode))
        .when(this.themeMenuOpen, panel => panel.child(this.renderThemePicker(t, kind, mdMode)))
        .child(this.renderFileBody(t, kind, mdMode)));
  }
}
