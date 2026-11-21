import { View, div } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextareaState, Textarea, Popup, Scrollbar, v_virtual_list } from "gpui-base";
import { spaces, panes, agents, terminals, focused, exec, readTextFile, watchFolder, theme, settings, shared, setShared, run } from "herdr";
import {
  annotateDiffRows,
  buildSelectionRange,
  extractSnippet,
  buildDisplayItems,
  resolveTargetCandidates,
  makeDraftScopeKey,
  readScopeDrafts,
  writeScopeDrafts,
  formatLineTag,
  withAlpha,
} from "./review_model.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function location() {
  const space = spaces().find((s) => s.active), f = focused();
  const pane = f?.pane ? panes().find((p) => p.pane === f.pane) : null;
  const on = f?.pane === null ? (f.terminal?.includes("/") ? f.terminal.split("/")[0] : null) : space?.machine ?? null;
  return { cwd: pane?.cwd || f?.cwd || space?.folder, on };
}
const key = () => JSON.stringify(location());
// Reads write nothing under .git (no index refresh or lock, no fsmonitor cookie):
// the folder watch would see it, and every refresh would start the next one.
const READ = ["--no-optional-locks", "-c", "core.fsmonitor=false", "-c", "diff.autoRefreshIndex=false"];
const countLines = (text) => text ? text.split("\n").length - Number(text.endsWith("\n")) : 0;

// Text draws a tab as nothing: pad each to the next 4-column stop.
const untab = (text) => text.replace(/[^\t]*\t/g, (s) => s.slice(0, -1).padEnd((s.length - 1) - (s.length - 1) % 4 + 4));

// Numbers each diff line once, so a frame only draws the rows on screen.
function diffRows(lines) {
  let old = 0, next = 0;
  return lines.map((line) => {
    const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(line);
    if (match) { old = +match[1]; next = +match[2]; return { kind: "@", text: line }; }
    const kind = line[0];
    if (kind === "-") return { kind, number: old++, text: untab(line.slice(1)) || " " };
    if (kind === "+") return { kind, number: next++, text: untab(line.slice(1)) || " " };
    if (kind === " ") { old++; return { kind, number: next++, text: untab(line.slice(1)) || " " }; }
    return { kind: "", text: line };
  });
}

export default class Changes extends View {
  init(_props, cx) {
    this.files = [];
    this.rows = [];
    this.annotatedRows = [];
    this.diffWidth = 0;
    this.message = "";
    this.handle = cx.focus_handle();
    this.input = InputState.new({ placeholder: tr("Commit message", "输入提交信息") });
    this.input.on("change", (_e, cx) => {
      const hadMessage = Boolean(this.message.trim());
      this.message = this.input.value();
      if (hadMessage !== Boolean(this.message.trim())) cx.notify();
    });
    this.input.on("submit", (_e, cx) => this.commit(cx));
    this.input.on("focus", (_e, cx) => { this.typing = true; cx.notify(); });
    this.input.on("blur", (_e, cx) => { this.typing = false; cx.notify(); });

    // Single reusable inline comment editor (created once in init(), never inside render()).
    this.commentInput = TextareaState.new({
      placeholder: tr("Ask, note, or comment on selected lines… (Ctrl/Cmd+Enter to add)", "针对选中行提问、批注或写修改说明… (Ctrl/Cmd+Enter 添加)"),
    });
    this.commentInput.set_auto_grow(2, 4);
    this.commentTyping = false;
    this.commentInput.on("focus", (_e, cx) => { this.commentTyping = true; cx.notify(); });
    this.commentInput.on("blur", (_e, cx) => { this.commentTyping = false; cx.notify(); });
    this.commentInput.on("change", (_e, cx) => { cx.notify(); });
    this.commentInput.on("submit", (e, cx) => {
      if (e?.ctrl || e?.meta || (!e?.shift && e?.key === "enter")) {
        this.saveComposer(cx, false);
      }
    });

    this.composer = null;
    this.anchorRowIdx = null;
    this.lastShift = false;
    this.targetOverride = null;
    this.targetPopupOpen = false;
    this.steerMode = false;
    this.sendingReqId = null;
    this.sendingIds = [];
    this.reviewError = null;
    this.lastHandledResultId = null;

    this.refresh(cx);
    cx.timer.every(10000, (cx) => this.refresh(cx));
  }

  watch(root, cx) {
    if (this.on || this.watched === root) return;
    this.watched = root;
    cx.spawn(async (cx) => {
      try {
        while (this.watched === root) {
          await watchFolder(root);
          if (this.watched === root) this.refresh(cx);
        }
      } catch { this.watched = null; }
    });
  }

  viewKey() {
    return `${key()}:${shared("checkpoint.sync") ?? ""}`;
  }

  scopeKey() {
    return this.root ? makeDraftScopeKey(this.on, this.root) : null;
  }

  drafts() {
    const sk = this.scopeKey();
    if (!sk) return [];
    return readScopeDrafts(shared("diff_review.drafts"), sk);
  }

  setDrafts(nextList) {
    const sk = this.scopeKey();
    if (!sk) return;
    const updated = writeScopeDrafts(shared("diff_review.drafts"), sk, nextList);
    setShared("diff_review.drafts", updated);
  }

  syncLastResult() {
    const res = shared("diff_review.last_result");
    if (!res || typeof res !== "object") return;
    const rid = res.request_id;
    if (!rid || rid === this.lastHandledResultId) return;
    if (this.sendingReqId && rid === this.sendingReqId) {
      this.lastHandledResultId = rid;
      this.sendingReqId = null;
      if (res.ok) {
        const sentSet = new Set(this.sendingIds ?? []);
        const remaining = this.drafts().filter((c) => !sentSet.has(c.id));
        this.setDrafts(remaining);
        this.sendingIds = [];
        this.reviewError = null;
      } else {
        this.sendingIds = [];
        this.reviewError = String(res.error || tr("Failed to send line notes", "发送行级消息失败"));
      }
    }
  }

  refresh(cx) {
    if (this.loading || this.busy) return;
    const { cwd, on } = location(), context = this.viewKey();
    if (context !== this.context) {
      const folderChanged = this.folderKey !== key();
      this.folderKey = key();
      this.context = context;
      if (folderChanged) {
        this.root = null;
        this.files = [];
        this.open = null;
        this.composer = null;
        this.anchorRowIdx = null;
        this.targetPopupOpen = false;
        this.reviewError = null;
        this.setLines([]);
        this.message = "";
        this.input.set_value("");
        this.error = null;
        this.note = null;
        this.watched = null;
        this.closed = false;
        this.limited = false;
      }
    }
    if (!cwd) return;
    this.loading = true;
    cx.spawn(async (cx) => {
      try {
        const top = await exec("git", ["rev-parse", "--show-toplevel"], { cwd, on });
        if (this.viewKey() !== context) return;
        if (top.code !== 0) { this.root = null; this.files = []; this.setLines([]); this.open = null; this.error = "Not a git repository"; return; }
        const root = top.stdout.trim(), git = (args) => exec("git", [...READ, ...args], { cwd: root, on });
        const status = await git(["status", "--porcelain=v1", "-z", "--no-renames", "--untracked-files=all"]);
        if (status.code !== 0) throw new Error(status.stderr);
        const head = await git(["rev-parse", "--verify", "HEAD"]);
        const stats = {};
        const results = head.code === 0 ? [await git(["diff", "HEAD", "--numstat", "-z", "--no-renames"])] : [await git(["diff", "--cached", "--numstat", "-z", "--no-renames"]), await git(["diff", "--numstat", "-z", "--no-renames"])];
        for (const result of results) {
          if (result.code !== 0) throw new Error(result.stderr);
          for (const entry of result.stdout.split("\0")) {
            const [a, d, ...parts] = entry.split("\t");
            if (!parts.length) continue;
            const path = parts.join("\t"), old = stats[path] ?? { add: 0, del: 0 };
            stats[path] = { add: old.add + (Number(a) || 0), del: old.del + (Number(d) || 0), binary: a === "-" };
          }
        }
        const files = [];
        const entries = status.stdout.split("\0").filter(Boolean);
        for (const entry of entries.slice(0, 1000)) {
          const xy = entry.slice(0, 2), path = entry.slice(3), fresh = xy === "??";
          const conflict = xy.includes("U") || xy === "AA" || xy === "DD";
          const s = conflict ? "U" : xy.includes("D") ? "D" : fresh || xy.includes("A") ? "A" : "M";
          const stat = stats[path] ?? { add: 0, del: 0 };
          files.push({ path, status: s, fresh, ...stat });
        }
        if (this.viewKey() !== context) return;
        this.root = root; this.on = on; this.hasHead = head.code === 0; this.files = files; this.limited = entries.length > 1000; this.error = null;
        this.watch(root, cx);
        if (this.open && !files.some((f) => f.path === this.open)) { this.open = null; this.composer = null; this.anchorRowIdx = null; this.setLines([]); }
        // The first file's diff shows until you pick another.
        if (!this.open && !this.closed && files.length) this.open = files[0].path;
        if (this.open) {
          try { await this.load(this.open, context); }
          catch { if (this.viewKey() === context) this.setLines(["Cannot preview this diff (too large or unreadable)"]); }
        }
      } catch (e) { if (this.viewKey() === context) this.error = String(e); }
      finally { this.loading = false; cx.notify(); }
    });
  }

  async load(path, context) {
    const file = this.files.find((f) => f.path === path), root = this.root, on = this.on;
    if (!file) return;
    let lines;
    if (file.fresh || (!this.hasHead && file.status === "A")) {
      const text = await readTextFile(`${root}/${path}`, { on }).catch(() => null);
      if (text === null) lines = ["Cannot preview this file (binary, too large, or unreadable)"];
      else if (text.includes("\0")) lines = ["Binary file · no text diff"];
      else {
        file.add = countLines(text);
        lines = [`@@ -0,0 +1,${file.add} @@`, ...text.slice(0, 400000).replace(/\n$/, "").split("\n").slice(0, 1500).map((l) => `+${l}`)];
      }
    } else {
      const r = await exec("git", [...READ, "diff", ...(this.hasHead ? ["HEAD"] : []), "--no-ext-diff", "--no-textconv", "--no-color", "--", path], { cwd: root, on });
      if (r.code !== 0) throw new Error(r.stderr);
      const all = r.stdout.replace(/\n$/, "").split("\n"), first = all.findIndex((l) => l.startsWith("@@"));
      lines = first < 0 ? [file.binary ? "Binary file changed" : "No text diff available"] : all.slice(first);
    }
    if (this.viewKey() === context && this.open === path) {
      this.setLines(lines.slice(0, 1500).map((line) => line.length > 4000 ? line.slice(0, 4000) + "… (line truncated)" : line));
    }
  }

  setLines(lines) {
    this.rows = diffRows(lines);
    this.annotatedRows = annotateDiffRows(this.rows, this.open);
    this.diffWidth = Math.max(300, ...this.rows.map((row) => (row.text.length + 1) * 7 + 72));
  }

  pick(path, cx) {
    this.open = this.open === path ? null : path;
    this.composer = null;
    this.anchorRowIdx = null;
    this.setLines([]);
    this.note = null;
    this.closed = !this.open;
    const context = this.context;
    if (this.open) cx.spawn(async (cx) => {
      try { await this.load(path, context); }
      catch (e) { if (this.viewKey() === context && this.open === path) this.note = `Cannot preview this diff (too large or unreadable): ${e}`; }
      cx.notify();
    });
    cx.notify();
  }

  clickDiffRow(row, e, cx) {
    if (!row?.commentable) return;
    const shift = Boolean(e?.modifiers?.shift || e?.shift);
    const anchor = shift && this.anchorRowIdx != null ? this.anchorRowIdx : row.rowIndex;
    const range = buildSelectionRange(this.annotatedRows, anchor, row.rowIndex);
    if (!range) return;

    if (!shift && this.composer && !this.composer.editingId && this.composer.startRowIdx === range.startRowIdx && this.composer.endRowIdx === range.endRowIdx && !String(this.commentInput.value() ?? "").trim()) {
      this.closeComposer(cx);
      return;
    }

    this.anchorRowIdx = shift && this.anchorRowIdx != null ? this.anchorRowIdx : row.rowIndex;
    const preserveText = Boolean(shift && this.composer);
    this.composer = {
      ...range,
      editingId: preserveText ? (this.composer?.editingId ?? null) : null,
    };
    if (!preserveText) this.commentInput.set_value("");
    this.reviewError = null;
    this.commentInput.focus();
    cx.notify();
  }

  editComment(comment, cx) {
    const idx = this.annotatedRows.findIndex(
      (r) => r.commentable && r.file === comment.path && ((comment.side === "old" ? r.oldLine : (r.newLine ?? r.oldLine)) === (comment.end_line || comment.start_line)),
    );
    const rowIdx = idx >= 0 ? idx : Math.max(0, this.annotatedRows.length - 1);
    this.anchorRowIdx = rowIdx;
    this.composer = {
      file: comment.path,
      startRowIdx: rowIdx,
      endRowIdx: rowIdx,
      side: comment.side || "new",
      startLine: comment.start_line,
      endLine: comment.end_line || comment.start_line,
      hunkHeader: comment.hunk_header || "",
      existingSnippet: comment.snippet || "",
      editingId: comment.id,
    };
    this.commentInput.set_value(comment.body || "");
    this.commentInput.focus();
    cx.notify();
  }

  deleteComment(id, cx) {
    const next = this.drafts().filter((c) => c.id !== id);
    this.setDrafts(next);
    if (this.composer?.editingId === id) {
      this.closeComposer(cx);
      return;
    }
    cx.notify();
  }

  closeComposer(cx) {
    this.composer = null;
    this.anchorRowIdx = null;
    this.commentTyping = false;
    this.commentInput.set_value("");
    this.handle.focus();
    cx.notify();
  }

  saveComposer(cx, sendImmediately = false) {
    if (!this.composer) return;
    const body = String(this.commentInput.value() ?? "").trim();
    if (!body) return;

    const c = this.composer;
    const snippet = c.editingId && c.existingSnippet
      ? c.existingSnippet
      : extractSnippet(this.annotatedRows, c.startRowIdx, c.endRowIdx);
    const item = {
      id: c.editingId || `rc-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
      path: c.file,
      side: c.side || "new",
      start_line: c.startLine,
      end_line: c.endLine > c.startLine ? c.endLine : undefined,
      hunk_header: c.hunkHeader || undefined,
      snippet: snippet || undefined,
      body,
    };

    const current = this.drafts();
    const next = c.editingId
      ? current.map((x) => (x.id === c.editingId ? item : x))
      : [...current, item];
    this.setDrafts(next);
    this.composer = null;
    this.anchorRowIdx = null;
    this.commentTyping = false;
    this.commentInput.set_value("");
    this.reviewError = null;

    if (sendImmediately) {
      this.sendReview(cx, next);
    } else {
      cx.notify();
    }
  }

  targetInfo() {
    const { candidates, defaultTarget } = resolveTargetCandidates({
      panesList: panes(),
      agentsList: agents(),
      terminalsList: terminals(),
      machine: this.on ?? null,
      focusedInfo: focused(),
      repoRoot: this.root,
      preferredAddr: this.targetOverride,
    });
    const chosen = candidates.find((c) => c.address === this.targetOverride) ?? candidates.find((c) => c.address === defaultTarget) ?? null;
    return { candidates, chosen };
  }

  sendReview(cx, explicitList = null) {
    if (this.sendingReqId) return;
    const list = explicitList ?? this.drafts();
    if (!list.length) return;
    const { chosen } = this.targetInfo();
    if (!chosen?.address) {
      this.reviewError = tr("No live agent or terminal on this machine", "当前机器暂无运行中的 Agent 或终端");
      cx.notify();
      return;
    }
    const reqId = `rev-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    this.sendingReqId = reqId;
    this.sendingIds = list.map((c) => c.id);
    this.reviewError = null;
    this.targetPopupOpen = false;
    const comments = list.map((c) => ({
      path: c.path,
      side: c.side || "new",
      start_line: Number(c.start_line) || 1,
      ...(c.end_line && Number(c.end_line) > Number(c.start_line) ? { end_line: Number(c.end_line) } : {}),
      ...(c.hunk_header ? { hunk_header: c.hunk_header } : {}),
      ...(c.snippet ? { snippet: c.snippet } : {}),
      body: c.body,
    }));
    run("agent.send_review", {
      request_id: reqId,
      target: chosen.address,
      source: "changes",
      ...(this.root ? { repo_root: this.root } : {}),
      comments,
      steer: Boolean(this.steerMode && chosen.kind === "acp" && chosen.status === "working"),
      focus: false,
    });
    cx.notify();
  }

  commit(cx) {
    if (!this.message.trim() || !this.root || this.busy || this.loading || this.context !== this.viewKey()) return;
    const root = this.root, on = this.on, context = this.context, message = this.message.trim();
    this.busy = true; this.note = null;
    cx.spawn(async (cx) => {
      try {
        for (const args of [["add", "-A"], ["commit", "-m", message]]) {
          const r = await exec("git", args, { cwd: root, on });
          if (r.code !== 0) throw new Error((r.stderr || r.stdout).trim());
        }
        if (this.viewKey() === context) { this.message = ""; this.input.set_value(""); }
      } catch (e) { if (this.viewKey() === context) this.note = `Commit failed: ${e}`; }
      finally { this.busy = false; this.refresh(cx); cx.notify(); }
    });
    cx.notify();
  }

  handleVirtualItemClick(itemKey, cx) {
    if (typeof itemKey !== "string") return;
    if (itemKey.startsWith("r-")) {
      const idx = Number(itemKey.slice(2));
      const row = Number.isFinite(idx) ? this.annotatedRows[idx] : null;
      if (row?.commentable) {
        this.clickDiffRow(row, { shift: Boolean(this.lastShift) }, cx);
      }
      return;
    }
    if (itemKey.startsWith("c-orphan-")) {
      const id = itemKey.slice("c-orphan-".length);
      const comment = this.drafts().find((c) => c.id === id);
      if (comment) this.editComment(comment, cx);
      return;
    }
    if (itemKey.startsWith("c-")) {
      const id = itemKey.slice(2);
      const comment = this.drafts().find((c) => c.id === id);
      if (comment) this.editComment(comment, cx);
    }
  }

  renderCodeRow(row, t, selectedRange, commentedSet) {
    if (row.kind === "@") {
      return div().w_full().px(12).h(20).flex_none().truncate().bg(t.raised).text_color(t.dim).child(row.text);
    }
    const inSel = Boolean(
      selectedRange && row.file === selectedRange.file && row.rowIndex >= selectedRange.startRowIdx && row.rowIndex <= selectedRange.endRowIdx,
    );
    const hasComment = commentedSet.has(row.rowIndex);
    const accent = t.accent ?? "#3b82f6";
    const rowBg = inSel
      ? (withAlpha(accent, "24") ?? t.selected)
      : row.kind === "+"
        ? (t.ansi2 + "18")
        : row.kind === "-"
          ? (t.ansi1 + "18")
          : null;

    return h_flex()
      .w_full()
      .h(20)
      .flex_none()
      .gap(6)
      .items_center()
      .when(Boolean(rowBg), (e) => e.bg(rowBg))
      .when(row.commentable, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(accent, "18") ?? t.hover)))
      .child(
        div()
          .w(3)
          .h_full()
          .flex_none()
          .when(inSel || hasComment, (e) => e.bg(accent)),
      )
      .child(
        div()
          .w(14)
          .flex_none()
          .text_center()
          .text_size(11)
          .text_color(inSel || hasComment ? accent : t.dim)
          .child(row.commentable ? (hasComment ? "●" : "+") : ""),
      )
      .child(
        div()
          .w(30)
          .flex_none()
          .text_right()
          .text_color(inSel ? t.foreground : t.dim)
          .child(row.number === undefined ? "" : String(row.number)),
      )
      .child(div().flex_1().min_w(0).truncate().child(row.text));
  }

  renderSavedComment(item, t) {
    const { comment, outdated, editing } = item;
    const accent = t.accent ?? "#3b82f6";
    return div()
      .w_full()
      .px(10)
      .py(4)
      .flex_none()
      .cursor_pointer()
      .child(
        v_flex()
          .w_full()
          .max_w(580)
          .p(8)
          .gap(4)
          .rounded(8)
          .bg(editing ? (withAlpha(accent, "1c") ?? t.selected) : t.raised)
          .border_1()
          .border_color(editing ? accent : outdated ? t.blocked : (withAlpha(accent, "66") ?? t.border))
          .font_family("sans-serif")
          .child(
            h_flex()
              .w_full()
              .items_center()
              .gap(6)
              .child(
                div()
                  .px(5)
                  .py(1)
                  .rounded(4)
                  .bg(withAlpha(accent, "1f") ?? t.background)
                  .text_size(10)
                  .font_semibold()
                  .text_color(accent)
                  .child(formatLineTag(comment)),
              )
              .when(outdated, (e) =>
                e.child(
                  div()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha(t.blocked, "22") ?? t.background)
                    .text_size(10)
                    .text_color(t.blocked)
                    .child(tr("Line moved", "行已变动")),
                ),
              )
              .child(div().flex_1())
              .child(
                div()
                  .text_size(10)
                  .text_color(editing ? accent : t.dim)
                  .child(editing ? tr("Editing below…", "正在下方编辑…") : tr("Click to edit / delete", "点击编辑或删除")),
              ),
          )
          .child(
            div()
              .w_full()
              .text_size(12)
              .line_height("16px")
              .text_color(t.foreground)
              .whitespace_normal()
              .child(comment.body),
          ),
      );
  }

  renderDockedComposer(composer, t) {
    const accent = t.accent ?? "#3b82f6";
    const hasText = Boolean(String(this.commentInput.value() ?? "").trim());
    const rangeTag = formatLineTag({
      start_line: composer.startLine,
      end_line: composer.endLine,
      side: composer.side,
    });

    return v_flex()
      .id("chg-inline-composer")
      .flex_none()
      .w_full()
      .p(10)
      .gap(6)
      .bg(t.popover ?? t.raised)
      .border_t(1)
      .border_color(this.commentTyping ? accent : t.border)
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .child(
        h_flex()
          .w_full()
          .items_center()
          .gap(6)
          .child(
            div()
              .px(5)
              .py(1)
              .rounded(4)
              .bg(withAlpha(accent, "22") ?? t.raised)
              .text_size(10)
              .font_semibold()
              .text_color(accent)
              .child(`${composer.file} · ${rangeTag}`),
          )
          .child(div().flex_1())
          .child(
            div()
              .text_size(10)
              .text_color(t.dim)
              .child(tr("Shift+Click a diff line to extend range", "Shift+点击差异行可扩展行区间")),
          ),
      )
      .child(
        div()
          .w_full()
          .h(54)
          .px(8)
          .py(4)
          .rounded(6)
          .bg(t.background)
          .border_1()
          .border_color(this.commentTyping ? accent : t.border)
          .cursor_text()
          .child(Textarea.new(this.commentInput).size_full().text_size(12).text_color(t.foreground)),
      )
      .child(
        h_flex()
          .w_full()
          .items_center()
          .gap(6)
          .when(Boolean(composer.editingId), (e) =>
            e.child(
              div()
                .id("chg-comp-delete")
                .px(8)
                .py(3)
                .rounded(5)
                .text_size(11)
                .text_color(t.ansi1)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => this.deleteComment(composer.editingId, cx))
                .child(tr("Delete", "删除")),
            ),
          )
          .child(div().flex_1())
          .child(
            div()
              .id("chg-comp-cancel")
              .px(8)
              .py(3)
              .rounded(5)
              .text_size(11)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, cx) => this.closeComposer(cx))
              .child(tr("Cancel", "取消")),
          )
          .child(
            div()
              .id("chg-comp-save")
              .px(9)
              .py(3)
              .rounded(5)
              .border_1()
              .border_color(hasText ? accent : t.border)
              .bg(hasText ? (withAlpha(accent, "1f") ?? t.raised) : t.raised)
              .text_size(11)
              .text_color(hasText ? t.foreground : t.dim)
              .when(hasText, (e) => e.cursor_pointer().hover((s) => s.bg(t.hover)).on_click((_e, cx) => this.saveComposer(cx, false)))
              .child(composer.editingId ? tr("Update", "更新") : tr("Add", "添加")),
          )
          .child(
            div()
              .id("chg-comp-send")
              .px(9)
              .py(3)
              .rounded(5)
              .bg(hasText && !this.sendingReqId ? t.solid : t.raised)
              .text_size(11)
              .text_color(hasText && !this.sendingReqId ? t.solid_foreground : t.dim)
              .when(hasText && !this.sendingReqId, (e) => e.cursor_pointer().on_click((_e, cx) => this.saveComposer(cx, true)))
              .child(tr("Send now", "立即发送")),
          ),
      );
  }

  renderReviewBar(t, allDrafts) {
    const { candidates, chosen } = this.targetInfo();
    const fileSet = new Set(allDrafts.map((c) => c.path));
    const accent = t.accent ?? "#3b82f6";
    const isWorkingAcp = Boolean(chosen && chosen.kind === "acp" && chosen.status === "working");
    const canSend = Boolean(allDrafts.length > 0 && chosen && !this.sendingReqId);
    const statusColor = (st) => (st === "working" ? t.working : st === "blocked" ? t.blocked : st === "done" ? t.done : t.dim);

    const trigger = h_flex()
      .id("chg-review-target-btn")
      .h(26)
      .max_w(210)
      .px(8)
      .gap(5)
      .items_center()
      .rounded(6)
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        this.targetPopupOpen = !this.targetPopupOpen;
        cx.notify();
      })
      .when(Boolean(chosen), (e) => e.child(div().w(6).h(6).flex_none().rounded(3).bg(statusColor(chosen.status))))
      .child(
        div()
          .min_w(0)
          .truncate()
          .text_size(11)
          .text_color(chosen ? t.foreground : t.dim)
          .child(chosen ? chosen.label : tr("No live agent", "无存活 Agent")),
      )
      .child(div().flex_none().text_size(9).text_color(t.dim).child("▾"));

    return v_flex()
      .id("chg-review-bar")
      .flex_none()
      .px(10)
      .py(8)
      .gap(6)
      .border_t(1)
      .border_color(withAlpha(accent, "55") ?? t.border)
      .bg(t.raised)
      .when(Boolean(this.reviewError), (e) =>
        e.child(div().text_size(11).text_color(t.ansi1).whitespace_normal().child(this.reviewError)),
      )
      .child(
        h_flex()
          .w_full()
          .gap(6)
          .items_center()
          .flex_wrap()
          .child(
            div()
              .px(6)
              .py(2)
              .rounded(5)
              .bg(withAlpha(accent, "20") ?? t.background)
              .text_size(11)
              .font_semibold()
              .text_color(accent)
              .child(
                tr(
                  `${allDrafts.length} note${allDrafts.length === 1 ? "" : "s"} · ${fileSet.size} file${fileSet.size === 1 ? "" : "s"}`,
                  `${allDrafts.length} 条批注 · ${fileSet.size} 个文件`,
                ),
              ),
          )
          .child(
            div()
              .id("chg-review-clear")
              .px(6)
              .py(2)
              .rounded(5)
              .text_size(11)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .on_click((_e, cx) => {
                this.setDrafts([]);
                this.reviewError = null;
                cx.notify();
              })
              .child(tr("Clear", "清空")),
          )
          .child(div().flex_1())
          .child(
            Popup.new("chg-review-target-popup", trigger)
              .anchor("top_left")
              .when(this.targetPopupOpen, (el) =>
                el.content(
                  v_flex()
                    .w(240)
                    .max_h(220)
                    .p(4)
                    .gap(2)
                    .rounded(8)
                    .bg(t.popover ?? t.background)
                    .border_1()
                    .border_color(t.border)
                    .overflow_y_scroll()
                    .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                    .children(
                      candidates.length
                        ? candidates.map((c) => {
                            const active = chosen?.address === c.address;
                            return h_flex()
                              .id(`chg-target-${c.address}`)
                              .h(26)
                              .px(8)
                              .gap(6)
                              .items_center()
                              .rounded(6)
                              .cursor_pointer()
                              .when(active, (x) => x.bg(t.selected))
                              .hover((s) => s.bg(t.hover))
                              .on_click((_e, cx) => {
                                this.targetOverride = c.address;
                                this.targetPopupOpen = false;
                                cx.notify();
                              })
                              .child(div().w(6).h(6).flex_none().rounded(3).bg(statusColor(c.status)))
                              .child(div().flex_1().min_w(0).truncate().text_size(11.5).text_color(t.foreground).child(c.label))
                              .child(
                                div()
                                  .flex_none()
                                  .px(4)
                                  .py(1)
                                  .rounded(4)
                                  .bg(t.raised)
                                  .text_size(9.5)
                                  .text_color(t.dim)
                                  .child(c.kind === "acp" ? "ACP" : "CLI"),
                              );
                          })
                        : [div().p(8).text_size(11).text_color(t.dim).child(tr("No live sessions", "暂无存活会话"))],
                    ),
                ),
              ),
          )
          .when(isWorkingAcp, (e) =>
            e.child(
              div()
                .id("chg-review-steer")
                .h(26)
                .px(7)
                .flex()
                .items_center()
                .rounded(6)
                .border_1()
                .border_color(this.steerMode ? t.blocked : t.border)
                .bg(this.steerMode ? (withAlpha(t.blocked, "22") ?? t.background) : t.background)
                .text_size(10.5)
                .text_color(this.steerMode ? t.blocked : t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => {
                  this.steerMode = !this.steerMode;
                  cx.notify();
                })
                .child(this.steerMode ? tr("Steer", "抢占") : tr("Queue", "排队")),
            ),
          )
          .child(
            div()
              .id("chg-review-send")
              .h(26)
              .px(11)
              .flex()
              .items_center()
              .rounded(6)
              .bg(canSend ? t.solid : t.background)
              .text_size(11.5)
              .font_medium()
              .text_color(canSend ? t.solid_foreground : t.dim)
              .when(canSend, (e) => e.cursor_pointer().on_click((_e, cx) => this.sendReview(cx)))
              .child(this.sendingReqId ? tr("Sending…", "发送中…") : tr("Send to Agent", "发给 Agent")),
          ),
      );
  }

  render(cx) {
    if (this.viewKey() !== this.context) this.refresh(cx);
    this.syncLastResult();
    const t = theme(), add = this.files.reduce((n, f) => n + f.add, 0), del = this.files.reduce((n, f) => n + f.del, 0);
    const enabled = Boolean(this.message.trim() && this.root && !this.busy && !this.loading);
    const listHeight = this.files.length * 28 + 12;

    const allDrafts = this.drafts();
    const countByFile = new Map();
    for (const d of allDrafts) {
      countByFile.set(d.path, (countByFile.get(d.path) ?? 0) + 1);
    }
    const openComments = this.open ? allDrafts.filter((c) => c.path === this.open) : [];
    const activeComposer = this.composer && this.composer.file === this.open ? this.composer : null;
    const { items, sizes } = buildDisplayItems(this.annotatedRows, openComments, activeComposer);
    const commentedSet = new Set();
    let lastCodeRowIdx = null;
    for (const item of items) {
      if (item.type === "code") lastCodeRowIdx = item.row.rowIndex;
      else if (item.type === "comment" && !item.outdated && lastCodeRowIdx != null) commentedSet.add(lastCodeRowIdx);
    }
    const accent = t.accent ?? "#3b82f6";

    return v_flex().id("changes").size_full().min_h(0).rounded(10).overflow_hidden().bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13).track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          if (this.targetPopupOpen) {
            this.targetPopupOpen = false;
            cx.stop_propagation();
            cx.notify();
          } else if (this.composer) {
            this.closeComposer(cx);
            cx.stop_propagation();
          } else if (this.typing) {
            this.typing = false;
            this.handle.focus();
            cx.stop_propagation();
            cx.notify();
          }
        }
      })
      .child(
        h_flex().h(42).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
          .child(div().font_semibold().child(tr("Changes", "变更")))
          .child(div().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(tr(`${this.files.length} files`, `${this.files.length} 个文件`)))
          .when(allDrafts.length > 0, (el) =>
            el.child(
              div()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(withAlpha(accent, "20") ?? t.raised)
                .text_size(10)
                .font_semibold()
                .text_color(accent)
                .child(tr(`${allDrafts.length} note${allDrafts.length === 1 ? "" : "s"}`, `${allDrafts.length} 条批注`)),
            ),
          )
          .child(div().flex_1())
          .child(div().text_size(11).text_color(t.ansi2).child(`+${add}`))
          .child(div().text_size(11).text_color(t.ansi1).child(`−${del}`)),
      )
      // overflow_y_scrollbar() wraps the list, so its flex sizing never reached the panel's layout.
      .child(div().flex_1().min_h(44).max_h(listHeight).relative().border_b(1).border_color(t.border)
        .child(
          v_flex().id("changes-list").absolute().inset_0().overflow_y_scroll().p(6)
            .children(this.files.map((f) => {
              const parts = f.path.split("/"), name = parts.pop();
              const cCount = countByFile.get(f.path) ?? 0;
              return h_flex().id(`change-${f.path}`).h(28).flex_none().px(8).gap(8).items_center().rounded(7).cursor_pointer().when(this.open === f.path, (e) => e.bg(t.selected)).hover((s) => s.bg(t.hover)).on_click((_e, cx) => this.pick(f.path, cx))
                .child(div().w(14).text_size(11).text_color(f.status === "A" ? t.ansi2 : f.status === "D" ? t.ansi1 : f.status === "U" ? t.ansi1 : t.blocked).child(f.status))
                .child(h_flex().flex_1().min_w(0).child(div().text_color(t.dim).truncate().child(parts.length ? parts.join("/") + "/" : "")).child(div().max_w(260).min_w(0).truncate().child(name)))
                .when(cCount > 0, (e) =>
                  e.child(
                    div()
                      .px(5)
                      .py(1)
                      .rounded(4)
                      .bg(withAlpha(accent, "22") ?? t.raised)
                      .text_size(10)
                      .font_semibold()
                      .text_color(accent)
                      .child(String(cCount)),
                  ),
                )
                .when(f.add > 0, (e) => e.child(div().text_size(11).text_color(t.ansi2).child(`+${f.add}`))).when(f.del > 0, (e) => e.child(div().text_size(11).text_color(t.ansi1).child(`−${f.del}`)));
            }))
            .when(!this.files.length, (e) => e.child(div().p(8).text_color(t.dim).child(this.error || (this.loading ? tr("Loading changes…", "正在加载变更…") : this.root ? tr("No uncommitted changes", "暂无未提交变更") : tr("No folder", "无工作目录"))))),
        )
        .child(Scrollbar.vertical("changes-list").absolute().inset_0()))
      .when(this.limited, (el) => el.child(div().px(12).py(4).text_size(11).text_color(t.dim).child(tr("Showing the first 1,000 changed files", "仅显示前 1,000 个变更文件"))))
      .child(div().flex_1().min_h(0).relative().font_family(settings().font_family ?? "JetBrains Mono").text_size(11)
        .on_mouse_down("left", (e) => { this.lastShift = Boolean(e?.modifiers?.shift); })
        .child(div().id("changes-diff-x").size_full().overflow_x_scrollbar()
          .child(div().w_full().min_w(this.diffWidth).h_full().child(
            v_virtual_list(
              "changes-diff",
              items.length,
              sizes,
              (i) => items[i]?.key ?? String(i),
              (range) => items.slice(range.start, range.end).map((item) => {
                if (item.type === "comment") return this.renderSavedComment(item, t);
                return this.renderCodeRow(item.row, t, activeComposer, commentedSet);
              }),
            ).on_item_click((itemKey, cx) => this.handleVirtualItemClick(itemKey, cx)).size_full(),
          )))
        .child(Scrollbar.vertical("changes-diff").viewport_from_layout().absolute().inset_0())
        .when(!this.open && this.files.length > 0, (e) => e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Select a file to view its changes", "选择一个文件以查看变更详情")))))
      .when(Boolean(activeComposer), (e) => e.child(this.renderDockedComposer(activeComposer, t)))
      .when(allDrafts.length > 0 || Boolean(this.reviewError), (e) => e.child(this.renderReviewBar(t, allDrafts)))
      .when(Boolean(this.note), (e) => e.child(div().px(10).pb(8).text_size(11).text_color(t.ansi1).child(this.note)))
      .child(h_flex().flex_none().p(10).gap(8).border_t(1).border_color(t.border)
        .child(div().id("commit-message").flex_1().min_w(0).h(32).px(10).flex().items_center().rounded(8).border_1().border_color(this.typing ? t.accent : t.border).cursor_text().on_mouse_down("left", (_e, cx) => cx.stop_propagation()).child(Input.new(this.input).h_full().text_size(12).text_color(t.foreground)))
        .child(div().id("commit").h(32).px(14).flex().items_center().rounded(8).bg(enabled ? t.solid : t.raised).text_color(enabled ? t.solid_foreground : t.dim).text_size(12).when(enabled, (e) => e.cursor_pointer()).on_click((_e, cx) => this.commit(cx)).child(this.busy ? tr("Committing…", "提交中…") : tr("Commit", "提交"))));
  }
}
