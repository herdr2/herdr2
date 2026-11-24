import { View, div, list } from "gpui-kit";
import { v_flex, h_flex, InputState, Input, TextareaState, Textarea, Popup, Scrollbar } from "gpui-base";
import { spaces, panes, focused, agents, terminals, theme, settings, h2, shared, setShared, run } from "herdr";
import {
  annotateDiffRows,
  buildSelectionRange,
  extractSnippet,
  buildDisplayItems,
  buildSplitDisplayItems,
  matchCommentRowIndex,
  resolveTargetCandidates,
  makeDraftScopeKey,
  readScopeDrafts,
  writeScopeDrafts,
  formatLineTag,
  withAlpha,
} from "./review_model.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function activeTargetAddress() {
  const f = focused();
  if (f?.terminal) return f.terminal;
  if (f?.shown) return f.shown;
  if (f?.agent) return f.agent;
  const pane = f?.pane ? panes().find((p) => p.pane === f.pane) : null;
  if (pane?.terminal) return pane.terminal;
  const activeSpace = spaces().find((s) => s.active);
  const activeTab = activeSpace?.tabs?.find((t) => t.active);
  if (activeTab) {
    const tabPane = panes().find((p) => p.tab === activeTab.id && p.terminal);
    if (tabPane?.terminal) return tabPane.terminal;
  }
  return null;
}

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

// Normalizes multi-file unified diff output into lines compatible with diffRows().
function parseUnifiedPatchLines(patch) {
  if (!patch || !String(patch).trim()) return ["@@ No text changes @@"];
  const rawLines = String(patch).replace(/\n$/, "").split("\n");
  const out = [];
  let inHunk = false;
  for (const line of rawLines) {
    if (line.startsWith("diff --git ")) {
      inHunk = false;
      const bMatch = / b\/(.+)$/.exec(line);
      out.push(`File: ${bMatch ? bMatch[1] : line.slice(11)}`);
      continue;
    }
    if (!inHunk) {
      if (line.startsWith("@@")) {
        inHunk = true;
        out.push(line);
      }
      continue;
    }
    out.push(line);
  }
  return out.length ? out : ["@@ Binary or metadata-only changes @@"];
}

// Chronological timeline: T0 initial state on top, followed by T1 -> T2 -> ...
function normalizeVisibleCheckpoints(rawList, includeInitialZero = false) {
  const list = Array.isArray(rawList) ? [...rawList] : [];
  const sorted = list
    .filter((c) => c != null && Number.isFinite(Number(c.turn)) && Number(c.turn) >= 0)
    .sort((a, b) => Number(a.turn) - Number(b.turn) || Number(a.ts_ms ?? 0) - Number(b.ts_ms ?? 0));
  if (includeInitialZero && !sorted.some((c) => Number(c.turn) === 0)) {
    sorted.unshift({ turn: 0, synthetic: true, files: [] });
  }
  return sorted;
}

// Collapses multiline prompts / wrapper tags into a clean single-line summary so GPUI rows never overflow vertically.
function formatCheckpointSummary(c, turn) {
  const raw = c?.label || c?.prompt || "";
  const cleaned = String(raw)
    .replace(/<\/?pasted_content\b[^>]*>/gi, " ")
    .replace(/<\/?herdr2-handoff\b[^>]*>/gi, " ")
    .replace(/[\r\n\t]+/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();
  if (cleaned) return cleaned;
  return turn === 0
    ? tr("Initial state (before Turn 1)", "初始状态 (第 1 轮前)")
    : tr(`Turn ${turn}`, `第 ${turn} 轮`);
}

function isManualCheckpointBookmark(c) {
  return Boolean(c?.label && c?.label_source !== "ai");
}

function isCurrentCheckpoint(c, turn, maxTurn) {
  return turn === maxTurn && (turn === 0 || !isManualCheckpointBookmark(c));
}

function formatCheckpointTooltip(c, turn) {
  const summary = formatCheckpointSummary(c, turn);
  const base = summary.length > 260 ? `${summary.slice(0, 260)}…` : summary;
  if (c?.label && c?.label_source === "ai" && c?.prompt) {
    const rawPrompt = String(c.prompt)
      .replace(/<\/?pasted_content\b[^>]*>/gi, " ")
      .replace(/<\/?herdr2-handoff\b[^>]*>/gi, " ")
      .replace(/[\r\n\t]+/g, " ")
      .replace(/\s{2,}/g, " ")
      .trim();
    if (rawPrompt && rawPrompt !== summary) {
      const clipped = rawPrompt.length > 160 ? `${rawPrompt.slice(0, 160)}…` : rawPrompt;
      return `${base}\n${tr("Prompt", "原始指令")}: ${clipped}`;
    }
  }
  return base;
}

export default class Checkpoints extends View {
  init(_props, cx) {
    this.checkpoints = [];
    this.isolation = null;
    this.selectedTurn = null;
    this.confirmTurn = null;
    this.targetAddr = null;
    this.rows = [];
    this.annotatedRows = [];
    this.diffNumWidth = 30;
    this.rollbackBusy = false;
    this.snapping = false;
    this.loading = false;
    this.error = null;
    this.note = null;
    this.handle = cx.focus_handle();

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
    this.loadedCommitOid = null;

    this.bisectInput = InputState.new({
      placeholder: tr("Locate bug turn, e.g. \"which turn broke auth?\"", "反查故障引入轮次，如“哪一轮导致鉴权测试失败？”"),
    });
    this.bisectTyping = false;
    this.bisectBusy = false;
    this.bisectResult = null;
    this.bisectError = null;
    this.bisectInput.on("focus", (_e, cx) => { this.bisectTyping = true; cx.notify(); });
    this.bisectInput.on("blur", (_e, cx) => { this.bisectTyping = false; cx.notify(); });
    this.bisectInput.on("change", (_e, cx) => { this.bisectError = null; cx.notify(); });
    this.bisectInput.on("submit", (_e, cx) => this.runBisect(cx));

    this.refresh(cx);
    cx.timer.every(10000, (cx) => this.refresh(cx));
  }

  viewKey() {
    const target = activeTargetAddress();
    const tRec = target ? (terminals().find((x) => x.address === target) ?? agents().find((x) => x.address === target)) : null;
    return `checkpoints:${target ?? ""}:${tRec?.turn ?? 0}:${tRec?.foreground?.pid ?? ""}:${tRec?.checkpoint_seq ?? 0}:${shared("checkpoint.sync") ?? ""}`;
  }

  targetMachine() {
    if (typeof this.targetAddr === "string" && this.targetAddr.includes("/")) {
      return this.targetAddr.slice(0, this.targetAddr.indexOf("/"));
    }
    return spaces().find((s) => s.active)?.machine ?? null;
  }

  scopeKey() {
    if (!this.targetAddr || this.selectedTurn == null) return null;
    return makeDraftScopeKey(this.targetMachine(), `ckpt:${this.targetAddr}:T${this.selectedTurn}`);
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

  setLines(lines) {
    this.rows = diffRows(lines);
    this.annotatedRows = annotateDiffRows(this.rows, null);
    const maxNum = this.rows.reduce((m, row) => (typeof row.number === "number" && row.number > m ? row.number : m), 0);
    const numCols = Math.max(3, String(maxNum).length);
    this.diffNumWidth = Math.max(30, numCols * 8 + 6);
  }

  refresh(cx) {
    if (this.loading || this.rollbackBusy) return;
    const target = activeTargetAddress();
    const context = this.viewKey();
    const targetChanged = this.targetAddr !== target;
    if (context !== this.context) {
      this.context = context;
      this.note = null;
      this.error = null;
      if (targetChanged) {
        this.checkpoints = [];
        this.isolation = null;
        this.confirmTurn = null;
        this.selectedTurn = null;
        this.loadedCommitOid = null;
        this.composer = null;
        this.anchorRowIdx = null;
        this.targetOverride = null;
        this.targetPopupOpen = false;
        this.reviewError = null;
        this.bisectResult = null;
        this.bisectError = null;
        this.bisectInput.set_value("");
        this.setLines([]);
      }
      this.targetAddr = target;
    }
    if (!target) {
      this.targetAddr = null;
      this.checkpoints = [];
      this.error = tr("Focus an agent or terminal pane to inspect checkpoints", "聚焦一个智能体或终端窗格以查看快照时间轴");
      cx.notify();
      return;
    }
    this.loading = true;
    cx.spawn(async (cx) => {
      try {
        const res = await h2(["checkpoint", "ls", "--json", target]);
        if (this.viewKey() !== context) return;
        if (res.code !== 0) {
          this.checkpoints = [];
          this.isolation = null;
          this.loadedCommitOid = null;
          this.error = (res.stderr || res.stdout || "No checkpoints").trim();
          this.setLines([]);
          return;
        }
        const parsed = JSON.parse(res.stdout || "{}");
        const list = normalizeVisibleCheckpoints(parsed.checkpoints, true);
        this.checkpoints = list;
        this.isolation = parsed.isolation ?? null;
        this.targetAddr = parsed.target || target;
        this.error = null;
        if (this.selectedTurn == null || !list.some((c) => Number(c.turn) === Number(this.selectedTurn))) {
          const latest = list[list.length - 1] ?? null;
          this.selectedTurn = latest ? Number(latest.turn) : null;
          this.loadedCommitOid = null;
        }
        if (this.selectedTurn != null) {
          const selMeta = list.find((c) => Number(c.turn) === Number(this.selectedTurn));
          const selOid = selMeta?.commit_oid ?? `t${this.selectedTurn}`;
          if (targetChanged || this.rows.length === 0 || this.loadedCommitOid !== selOid) {
            await this.loadCheckpointDiff(this.selectedTurn, context);
            if (this.viewKey() === context) this.loadedCommitOid = selOid;
          }
        } else {
          this.loadedCommitOid = null;
          this.setLines([]);
        }
      } catch (e) {
        if (this.viewKey() === context) this.error = String(e);
      } finally {
        this.loading = false;
        cx.notify();
      }
    });
  }

  async loadCheckpointDiff(turn, context) {
    const target = this.targetAddr;
    if (!target) return;
    if (Number(turn) === 0) {
      if (this.viewKey() === context) {
        this.setLines([tr("@@ Initial state (T0 — clean workspace baseline before Turn 1) @@", "@@ 初始状态 (T0 — 第 1 轮对话开始前的工作区基线) @@")]);
      }
      return;
    }
    const res = await h2(["checkpoint", "diff", "--json", target, "--to", String(turn)]);
    if (this.viewKey() !== context) return;
    if (res.code !== 0) {
      this.setLines([`Unable to load diff: ${(res.stderr || res.stdout || "").trim()}`]);
      return;
    }
    const diff = JSON.parse(res.stdout || "{}");
    const lines = parseUnifiedPatchLines(diff.patch).slice(0, 1500).map((line) =>
      line.length > 4000 ? `${line.slice(0, 4000)}… (line truncated)` : line,
    );
    if (this.viewKey() === context) this.setLines(lines);
  }

  pickCheckpoint(turn, cx) {
    this.selectedTurn = turn;
    this.confirmTurn = null;
    this.composer = null;
    this.anchorRowIdx = null;
    this.note = null;
    this.reviewError = null;
    this.setLines([]);
    const selMeta = this.checkpoints.find((c) => Number(c.turn) === Number(turn));
    this.loadedCommitOid = selMeta?.commit_oid ?? `t${turn}`;
    const context = this.viewKey();
    cx.spawn(async (cx) => {
      try { await this.loadCheckpointDiff(turn, context); }
      catch (e) { if (this.viewKey() === context) this.note = String(e); }
      cx.notify();
    });
    cx.notify();
  }

  runBisect(cx) {
    const target = this.targetAddr;
    const q = String(this.bisectInput?.value?.() ?? "").trim();
    if (!target || !q || this.bisectBusy) return;
    this.bisectBusy = true;
    this.bisectError = null;
    const context = this.viewKey();
    cx.spawn(async (cx) => {
      try {
        const res = await h2(
          ["checkpoint", "bisect", "--json", target, "--query", q, "--lang", isZh() ? "zh" : "en"],
          { timeout: 25000 },
        );
        if (this.targetAddr !== target) return;
        if (res.code !== 0) {
          let msg = (res.stderr || res.stdout || tr("Bisect failed", "故障定位失败")).trim();
          try {
            const p = JSON.parse(res.stderr || res.stdout || "{}");
            if (p?.error?.message) msg = p.error.message;
          } catch {}
          this.bisectError = msg;
          this.bisectResult = null;
        } else {
          const parsed = JSON.parse(res.stdout || "{}");
          this.bisectResult = parsed;
          this.bisectError = null;
          if (typeof parsed?.suspectTurn === "number" && this.viewKey() === context) {
            this.pickCheckpoint(parsed.suspectTurn, cx);
          }
        }
      } catch (e) {
        if (this.targetAddr === target) {
          this.bisectError = String(e);
          this.bisectResult = null;
        }
      } finally {
        this.bisectBusy = false;
        cx.notify();
      }
    });
    cx.notify();
  }

  snapNow(cx) {
    const target = activeTargetAddress() || this.targetAddr;
    if (!target || this.snapping || this.loading) return;
    this.snapping = true;
    this.note = null;
    cx.spawn(async (cx) => {
      try {
        const res = await h2(["checkpoint", "snap", "--json", target]);
        if (res.code !== 0) {
          this.note = (res.stderr || res.stdout || "Snapshot failed").trim();
        } else {
          const parsed = JSON.parse(res.stdout || "{}");
          this.selectedTurn = parsed?.checkpoint?.turn ?? this.selectedTurn;
          this.context = "";
          setShared("checkpoint.sync", `${target}:${parsed?.checkpoint?.turn ?? 0}:${Date.now()}`);
          this.refresh(cx);
        }
      } catch (e) {
        this.note = String(e);
      } finally {
        this.snapping = false;
        cx.notify();
      }
    });
    cx.notify();
  }

  rollbackCheckpoint(turn, restoreFiles, cx) {
    const target = this.targetAddr;
    if (!target || this.rollbackBusy) return;
    this.rollbackBusy = true;
    this.note = null;
    cx.spawn(async (cx) => {
      try {
        const args = [
          "checkpoint",
          "rollback",
          "--json",
          target,
          "--to",
          String(turn),
          ...(restoreFiles ? ["--confirm"] : ["--no-files"]),
        ];
        const res = await h2(args, { timeout: 90000 });
        if (res.code !== 0) {
          let msg = (res.stderr || res.stdout || "Rollback failed").trim();
          try {
            const p = JSON.parse(res.stderr || "{}");
            msg = p?.error?.message || msg;
          } catch {}
          this.note = msg;
        } else {
          this.confirmTurn = null;
          this.selectedTurn = turn;
          this.composer = null;
          this.anchorRowIdx = null;
          this.rollbackBusy = false;
          this.context = "";
          setShared("checkpoint.sync", `${target}:${turn}:${Date.now()}`);
          this.refresh(cx);
          run("agent.focus", { terminal: target });
        }
      } catch (e) {
        this.note = String(e);
      } finally {
        this.rollbackBusy = false;
        cx.notify();
      }
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
    const preferred = this.targetOverride || this.targetAddr;
    const { candidates, defaultTarget } = resolveTargetCandidates({
      panesList: panes(),
      agentsList: agents(),
      terminalsList: terminals(),
      machine: this.targetMachine(),
      focusedInfo: focused(),
      repoRoot: null,
      preferredAddr: preferred,
    });
    const chosen = candidates.find((c) => c.address === preferred) ?? candidates.find((c) => c.address === defaultTarget) ?? null;
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
    const reqId = `rev-ckpt-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
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
      source: "checkpoints",
      ...(this.selectedTurn != null ? { checkpoint_turn: Number(this.selectedTurn) } : {}),
      comments,
      steer: Boolean(this.steerMode && chosen.kind === "acp" && chosen.status === "working"),
      focus: false,
    });
    cx.notify();
  }

  diffMode() {
    try {
      return shared("diff.viewMode") === "split" ? "split" : "unified";
    } catch {
      return "unified";
    }
  }

  setDiffMode(mode, cx) {
    const next = mode === "split" ? "split" : "unified";
    if (this.diffMode() === next) return;
    setShared("diff.viewMode", next);
    cx.notify();
  }

  handleVirtualItemClick(itemKey, cx) {
    if (typeof itemKey !== "string") return;
    if (itemKey.startsWith("r-") || itemKey.startsWith("s-")) {
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
    if (row.isFileHeader) {
      return h_flex()
        .w_full()
        .min_w(0)
        .px(12)
        .py(4)
        .min_h(24)
        .items_center()
        .bg(t.raised)
        .border_y(1)
        .border_color(t.border)
        .child(
          div()
            .flex_1()
            .min_w(0)
            .font_semibold()
            .text_size(11)
            .line_height("16px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(row.text),
        );
    }
    if (row.kind === "@") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(2)
        .min_h(20)
        .line_height("16px")
        .whitespace_normal()
        .bg(t.raised)
        .text_color(t.dim)
        .child(row.text);
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
      .min_w(0)
      .min_h(20)
      .pr(14)
      .gap(6)
      .items_start()
      .when(Boolean(rowBg), (e) => e.bg(rowBg))
      .when(row.commentable, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(accent, "18") ?? t.hover)))
      .child(
        div()
          .w(3)
          .self_stretch()
          .flex_none()
          .when(inSel || hasComment, (e) => e.bg(accent)),
      )
      .child(
        div()
          .w(14)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_center()
          .text_size(11)
          .text_color(inSel || hasComment ? accent : t.dim)
          .child(row.commentable ? (hasComment ? "●" : "+") : ""),
      )
      .child(
        div()
          .w(this.diffNumWidth ?? 30)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_right()
          .text_color(inSel ? t.foreground : t.dim)
          .child(row.number === undefined ? "" : String(row.number)),
      )
      .child(div().flex_1().min_w(0).py(1).line_height("18px").whitespace_normal().child(row.text));
  }

  renderSplitCell(row, side, t, selectedRange, commentedSet, padRight) {
    const accent = t.accent ?? "#3b82f6";
    if (!row) {
      return h_flex()
        .flex_1()
        .min_w(0)
        .min_h(20)
        .items_start()
        .bg(withAlpha(t.raised, "99") ?? t.surface ?? t.background)
        .child(div().w(3).self_stretch().flex_none())
        .child(div().w(this.diffNumWidth ?? 30).flex_none().py(1).line_height("18px").child(""))
        .child(div().w(12).flex_none().py(1).line_height("18px").child(""))
        .child(div().flex_1().min_w(0).py(1).pr(padRight).line_height("18px").child(" "));
    }

    const inSel = Boolean(
      selectedRange && row.file === selectedRange.file && row.rowIndex >= selectedRange.startRowIdx && row.rowIndex <= selectedRange.endRowIdx,
    );
    const hasComment = commentedSet.has(row.rowIndex);
    const isDel = side === "left" && row.kind === "-";
    const isAdd = side === "right" && row.kind === "+";
    const cellBg = inSel
      ? (withAlpha(accent, "24") ?? t.selected)
      : isAdd
        ? (t.ansi2 + "18")
        : isDel
          ? (t.ansi1 + "18")
          : null;
    const lineNum = side === "left"
      ? (row.oldLine ?? (row.kind === "-" ? row.number : null))
      : (row.newLine ?? (row.kind === "+" ? row.number : null));
    const sign = isDel ? "−" : isAdd ? "+" : "";
    const signColor = isDel ? t.ansi1 : isAdd ? t.ansi2 : t.dim;

    return h_flex()
      .flex_1()
      .min_w(0)
      .min_h(20)
      .gap(4)
      .items_start()
      .when(Boolean(cellBg), (e) => e.bg(cellBg))
      .child(
        div()
          .w(3)
          .self_stretch()
          .flex_none()
          .when(inSel || hasComment, (e) => e.bg(accent)),
      )
      .child(
        div()
          .w(this.diffNumWidth ?? 30)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_right()
          .text_color(inSel ? t.foreground : t.dim)
          .child(lineNum == null ? "" : String(lineNum)),
      )
      .child(
        div()
          .w(12)
          .flex_none()
          .py(1)
          .line_height("18px")
          .text_center()
          .text_color(hasComment ? accent : signColor)
          .child(hasComment ? "●" : sign),
      )
      .child(
        div()
          .flex_1()
          .min_w(0)
          .py(1)
          .pr(padRight)
          .line_height("18px")
          .whitespace_normal()
          .child(row.text),
      );
  }

  renderSplitRow(srow, t, selectedRange, commentedSet) {
    if (srow.kind === "file") {
      return h_flex()
        .w_full()
        .min_w(0)
        .px(12)
        .py(4)
        .min_h(24)
        .items_center()
        .bg(t.raised)
        .border_y(1)
        .border_color(t.border)
        .child(
          div()
            .flex_1()
            .min_w(0)
            .font_semibold()
            .text_size(11)
            .line_height("16px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(srow.row.text),
        );
    }
    if (srow.kind === "hunk") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(2)
        .min_h(20)
        .line_height("16px")
        .whitespace_normal()
        .bg(t.raised)
        .text_color(t.dim)
        .child(srow.row.text);
    }
    if (srow.kind === "meta") {
      return div()
        .w_full()
        .min_w(0)
        .px(12)
        .py(1)
        .min_h(20)
        .line_height("18px")
        .whitespace_normal()
        .text_color(t.muted)
        .child(srow.row.text);
    }

    const accent = t.accent ?? "#3b82f6";
    const commentable = Boolean(srow.right?.commentable || srow.left?.commentable);
    return h_flex()
      .w_full()
      .min_w(0)
      .items_stretch()
      .when(commentable, (e) => e.cursor_pointer().hover((s) => s.bg(withAlpha(accent, "12") ?? t.hover)))
      .child(this.renderSplitCell(srow.left, "left", t, selectedRange, commentedSet, 6))
      .child(div().w(1).self_stretch().flex_none().bg(t.border))
      .child(this.renderSplitCell(srow.right, "right", t, selectedRange, commentedSet, 14));
  }

  renderDiffModeToggle(t, mode) {
    const pill = (id, label, active, nextMode) =>
      div()
        .id(id)
        .flex_none()
        .px(7)
        .py(2)
        .rounded(4)
        .text_size(10.5)
        .font_medium()
        .cursor_pointer()
        .when(active, (e) => e.bg(t.selected ?? t.raised))
        .text_color(active ? t.foreground : t.muted)
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => this.setDiffMode(nextMode, cx))
        .child(label);

    return h_flex()
      .id("ckpt-diff-mode-toggle")
      .flex_none()
      .p(2)
      .gap(2)
      .items_center()
      .rounded(6)
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .child(pill("ckpt-diff-mode-unified", tr("Unified", "直接"), mode === "unified", "unified"))
      .child(pill("ckpt-diff-mode-split", tr("Split", "分栏"), mode === "split", "split"));
  }

  renderSavedComment(item, t) {
    const { comment, outdated, editing } = item;
    const accent = t.accent ?? "#3b82f6";
    return div()
      .w_full()
      .min_w(0)
      .px(10)
      .py(4)
      .flex_none()
      .cursor_pointer()
      .child(
        v_flex()
          .w_full()
          .min_w(0)
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
                  .child(`${comment.path} · ${formatLineTag(comment)}`),
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
      .id("ckpt-inline-composer")
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
                .id("ckpt-comp-delete")
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
              .id("ckpt-comp-cancel")
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
              .id("ckpt-comp-save")
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
              .id("ckpt-comp-send")
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
      .id("ckpt-review-target-btn")
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
      .id("ckpt-review-bar")
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
                  `T${this.selectedTurn} · ${allDrafts.length} note${allDrafts.length === 1 ? "" : "s"} (${fileSet.size}f)`,
                  `T${this.selectedTurn} · ${allDrafts.length} 条批注（${fileSet.size} 文件）`,
                ),
              ),
          )
          .child(
            div()
              .id("ckpt-review-clear")
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
            Popup.new("ckpt-review-target-popup", trigger)
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
                              .id(`ckpt-target-${c.address}`)
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
                .id("ckpt-review-steer")
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
              .id("ckpt-review-send")
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
    const t = theme();
    const activeTarget = activeTargetAddress();
    const targetRec = this.targetAddr
      ? (terminals().find((x) => x.address === this.targetAddr) ?? agents().find((x) => x.address === this.targetAddr))
      : null;
    const ckptMax = this.checkpoints.reduce((m, c) => Math.max(m, Number(c.turn) || 0), 0);
    const maxTurn = this.checkpoints.length > 0 ? ckptMax : Math.max(Number(targetRec?.turn ?? 0), 0);
    const isoLevel = this.isolation?.level ?? "unknown";
    const filesBlocked = isoLevel === "shared_blocked";
    const rowHeight = 34;
    const rowGap = 4;
    const listHeight = this.checkpoints.length > 0
      ? this.checkpoints.length * rowHeight + Math.max(0, this.checkpoints.length - 1) * rowGap + 14
      : 48;

    const allDrafts = this.drafts();
    const diffMode = this.diffMode();
    const { items } = diffMode === "split"
      ? buildSplitDisplayItems(this.annotatedRows, allDrafts, this.composer)
      : buildDisplayItems(this.annotatedRows, allDrafts, this.composer);
    const commentedSet = new Set();
    for (const c of allDrafts) {
      const anchor = matchCommentRowIndex(this.annotatedRows, c);
      if (anchor >= 0) commentedSet.add(anchor);
    }
    const diffListId = diffMode === "split" ? "checkpoints-diff-split" : "checkpoints-diff";

    return v_flex().id("checkpoints").size_full().min_h(0).rounded(10).overflow_hidden().bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13).track_focus(this.handle)
      .on_key_down((e, cx) => {
        if (e.key === "escape") {
          if (this.targetPopupOpen) {
            this.targetPopupOpen = false;
            cx.stop_propagation();
            cx.notify();
          } else if (this.composer) {
            this.closeComposer(cx);
            cx.stop_propagation();
          } else if (this.bisectTyping) {
            this.bisectTyping = false;
            this.handle.focus();
            cx.stop_propagation();
            cx.notify();
          }
        }
      })
      .child(
        h_flex().h(42).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
          .child(div().flex_none().whitespace_nowrap().font_semibold().child(tr("Checkpoints", "快照")))
          .when(Boolean(this.targetAddr), (el) =>
            el.child(div().flex_none().whitespace_nowrap().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(this.targetAddr)),
          )
          .when(this.checkpoints.length > 0, (el) =>
            el.child(div().flex_none().whitespace_nowrap().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(tr(`${this.checkpoints.length} turns`, `${this.checkpoints.length} 个节点`))),
          )
          .child(div().flex_1())
          .when(Boolean(activeTarget || this.targetAddr), (el) =>
            el.child(
              div().id("checkpoints-snap").flex_none().whitespace_nowrap().px(8).py(3).rounded(6).border_1().border_color(t.border).text_size(11).text_color(t.foreground).cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => this.snapNow(cx))
                .child(this.snapping ? tr("Snapping…", "抓取中…") : tr("+ Snap", "+ 存档")),
            ),
          ),
      )
      .when(this.checkpoints.some((c) => Number(c.turn) > 0) || Boolean(this.bisectResult || this.bisectError), (el) => {
        const hasQuery = Boolean(String(this.bisectInput?.value?.() ?? "").trim());
        const canBisect = Boolean(hasQuery && this.targetAddr && !this.bisectBusy);
        const accent = t.accent ?? "#3b82f6";
        return el.child(
          h_flex()
            .id("ckpt-bisect-bar")
            .flex_none()
            .px(10)
            .py(6)
            .gap(6)
            .items_center()
            .border_b(1)
            .border_color(t.border)
            .bg(t.raised)
            .child(
              div()
                .id("ckpt-bisect-input-wrap")
                .flex_1()
                .min_w(0)
                .h(26)
                .px(8)
                .flex()
                .items_center()
                .rounded(6)
                .bg(t.background)
                .border_1()
                .border_color(this.bisectTyping ? accent : t.border)
                .cursor_text()
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .child(Input.new(this.bisectInput).h_full().text_size(11.5).text_color(t.foreground)),
            )
            .child(
              div()
                .id("ckpt-bisect-run")
                .flex_none()
                .h(26)
                .px(9)
                .flex()
                .items_center()
                .rounded(6)
                .bg(canBisect ? t.solid : t.background)
                .border_1()
                .border_color(canBisect ? t.solid : t.border)
                .text_size(11)
                .font_medium()
                .text_color(canBisect ? t.solid_foreground : t.dim)
                .when(canBisect, (e) => e.cursor_pointer().on_click((_e, cx) => this.runBisect(cx)))
                .child(this.bisectBusy ? tr("Locating…", "定位中…") : tr("Locate", "定位故障")),
            ),
        );
      })
      .when(Boolean(this.bisectError), (el) =>
        el.child(
          h_flex()
            .flex_none()
            .px(10)
            .py(6)
            .gap(6)
            .items_center()
            .border_b(1)
            .border_color(t.border)
            .bg(t.raised)
            .child(div().flex_1().min_w(0).text_size(11).text_color(t.ansi1).whitespace_normal().child(this.bisectError))
            .child(
              div()
                .id("ckpt-bisect-err-close")
                .flex_none()
                .px(5)
                .py(1)
                .rounded(4)
                .text_size(11)
                .text_color(t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_click((_e, cx) => { this.bisectError = null; cx.notify(); })
                .child("×"),
            ),
        ),
      )
      .when(Boolean(this.bisectResult), (el) => {
        const br = this.bisectResult;
        const suspect = Number(br.suspectTurn ?? 0);
        const safe = Number(br.recommendedRollbackTurn ?? 0);
        const pct = Math.round(Number(br.confidence ?? 0) * 100);
        const files = Array.isArray(br.evidenceFiles) ? br.evidenceFiles : [];
        return el.child(
          v_flex()
            .id("ckpt-bisect-card")
            .flex_none()
            .px(10)
            .py(8)
            .gap(5)
            .border_b(1)
            .border_color(withAlpha(t.blocked, "66") ?? t.border)
            .bg(withAlpha(t.blocked, "12") ?? t.raised)
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
                    .bg(withAlpha(t.ansi1, "22") ?? t.background)
                    .text_size(10)
                    .font_semibold()
                    .text_color(t.ansi1)
                    .child(tr(`Suspect T${suspect} (${pct}%)`, `疑似引入 T${suspect} (${pct}%)`)),
                )
                .child(
                  div()
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .bg(withAlpha(t.ansi2, "22") ?? t.background)
                    .text_size(10)
                    .font_semibold()
                    .text_color(t.ansi2)
                    .child(tr(`Safe anchor T${safe}`, `推荐回滚 T${safe}`)),
                )
                .child(div().flex_1())
                .child(
                  div()
                    .id("ckpt-bisect-clear")
                    .px(5)
                    .py(1)
                    .rounded(4)
                    .text_size(11)
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => { this.bisectResult = null; cx.notify(); })
                    .child("×"),
                ),
            )
            .child(
              div()
                .w_full()
                .text_size(11.5)
                .line_height("16px")
                .text_color(t.foreground)
                .whitespace_normal()
                .child(br.reason || ""),
            )
            .when(files.length > 0, (fe) =>
              fe.child(
                div()
                  .w_full()
                  .text_size(10.5)
                  .text_color(t.muted)
                  .truncate()
                  .child(`${tr("Evidence", "涉及文件")}: ${files.join(", ")}`),
              ),
            )
            .child(
              h_flex()
                .w_full()
                .gap(6)
                .items_center()
                .child(
                  div()
                    .id("ckpt-bisect-view")
                    .px(8)
                    .py(3)
                    .rounded(5)
                    .border_1()
                    .border_color(t.border)
                    .bg(t.background)
                    .text_size(10.5)
                    .text_color(t.foreground)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover))
                    .on_click((_e, cx) => this.pickCheckpoint(suspect, cx))
                    .child(tr(`Inspect T${suspect} Diff`, `查看 T${suspect} 变更`)),
                )
                .child(
                  div()
                    .id("ckpt-bisect-rb")
                    .px(8)
                    .py(3)
                    .rounded(5)
                    .bg(t.solid)
                    .text_size(10.5)
                    .font_medium()
                    .text_color(t.solid_foreground)
                    .cursor_pointer()
                    .on_click((_e, cx) => {
                      this.confirmTurn = safe;
                      this.note = null;
                      cx.notify();
                    })
                    .child(tr(`Prepare Rollback to T${safe}`, `准备回滚到 T${safe}`)),
                ),
            ),
        );
      })
      .child(
        div().flex_1().min_h(48).max_h(listHeight).relative().border_b(1).border_color(t.border)
          .child(
            v_flex().id("checkpoints-list").absolute().inset_0().overflow_y_scroll().p(6).gap(rowGap)
              .children(this.checkpoints.map((c) => {
                const turn = Number(c.turn);
                const files = Array.isArray(c.files) ? c.files : [];
                const cAdd = files.reduce((n, f) => n + (Number(f.added) || 0), 0);
                const cDel = files.reduce((n, f) => n + (Number(f.deleted) || 0), 0);
                const summary = formatCheckpointSummary(c, turn);
                const tip = formatCheckpointTooltip(c, turn);
                const selected = Number(this.selectedTurn) === turn;
                const isCurrent = isCurrentCheckpoint(c, turn, maxTurn);
                const isAiLabel = Boolean(c?.label && c?.label_source === "ai");
                const isBisectSuspect = Boolean(this.bisectResult && Number(this.bisectResult.suspectTurn) === turn);
                const isBisectSafe = Boolean(this.bisectResult && Number(this.bisectResult.recommendedRollbackTurn) === turn);
                return h_flex().id(`ckpt-${turn}`).h(rowHeight).flex_none().px(8).gap(8).items_center().rounded(7).overflow_hidden().cursor_pointer()
                  .tooltip(tip)
                  .when(selected, (e) => e.bg(t.selected)).hover((s) => s.bg(t.hover))
                  .on_click((_e, cx) => this.pickCheckpoint(turn, cx))
                  .child(div().min_w(28).flex_none().whitespace_nowrap().text_size(11).line_height("18px").text_color(t.accent ?? t.muted).child(`T${turn}`))
                  .child(div().flex_1().min_w(0).overflow_hidden().whitespace_nowrap().truncate().text_size(12.5).line_height("20px").child(summary))
                  .when(isAiLabel && !isBisectSuspect && !isBisectSafe, (e) =>
                    e.child(div().flex_none().whitespace_nowrap().px(4).py(0.5).rounded(4).bg(t.raised).text_size(9.5).text_color(t.dim).child("AI")),
                  )
                  .when(isBisectSuspect, (e) =>
                    e.child(div().flex_none().whitespace_nowrap().px(5).py(1).rounded(4).bg(withAlpha(t.ansi1, "22") ?? t.raised).text_size(9.5).font_semibold().text_color(t.ansi1).child(tr("Suspect", "疑似引入"))),
                  )
                  .when(isBisectSafe, (e) =>
                    e.child(div().flex_none().whitespace_nowrap().px(5).py(1).rounded(4).bg(withAlpha(t.ansi2, "22") ?? t.raised).text_size(9.5).font_semibold().text_color(t.ansi2).child(tr("Safe", "推荐回滚"))),
                  )
                  .when(cAdd > 0, (e) => e.child(div().flex_none().whitespace_nowrap().text_size(11).line_height("18px").text_color(t.ansi2).child(`+${cAdd}`)))
                  .when(cDel > 0, (e) => e.child(div().flex_none().whitespace_nowrap().text_size(11).line_height("18px").text_color(t.ansi1).child(`−${cDel}`)))
                  .when(isCurrent, (e) =>
                    e.child(div().flex_none().whitespace_nowrap().px(6).py(2).rounded(5).bg(t.raised).text_size(10.5).line_height("14px").text_color(t.dim).child(tr("Current", "当前"))),
                  )
                  .when(!isCurrent, (e) =>
                    e.child(
                      div().id(`ckpt-rb-${turn}`).flex_none().whitespace_nowrap().px(6).py(2).rounded(5).border_1().border_color(t.border).text_size(10.5).line_height("14px").text_color(t.muted)
                        .hover((s) => s.bg(t.hover))
                        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                        .on_click((_e, cx) => {
                          this.confirmTurn = this.confirmTurn === turn ? null : turn;
                          this.note = null;
                          cx.notify();
                        })
                        .child(tr("Restore…", "恢复…")),
                    ),
                  );
              }))
              .when(!this.checkpoints.length, (e) =>
                e.child(div().p(8).text_color(t.dim).child(this.error || (this.loading ? tr("Loading checkpoints…", "正在加载快照…") : tr("No checkpoints yet", "暂无轮次快照")))),
              ),
          )
          .child(Scrollbar.vertical("checkpoints-list").absolute().inset_0()),
      )
      .when(this.confirmTurn != null, (panel) => {
        const rbTurn = Number(this.confirmTurn);
        const confirmCkpt = this.checkpoints.find((c) => Number(c.turn) === rbTurn);
        const noFileSnap = Boolean(confirmCkpt?.synthetic) || !this.isolation;
        const disableFiles = filesBlocked || noFileSnap || this.rollbackBusy;
        const blockersText = Array.isArray(this.isolation?.blockers) ? this.isolation.blockers.map((b) => b.detail || b.addr).join(", ") : "";
        return panel.child(
          h_flex().p(10).gap(8).items_center().flex_wrap().border_b(1).border_color(t.border).bg(t.raised)
            .child(div().text_size(11.5).line_height("17px").text_color(filesBlocked ? t.ansi1 : t.foreground).child(
              filesBlocked
                ? (rbTurn === 0
                    ? tr(`Blocked (${blockersText}): session-only reset to initial state T0`, `同目录有活跃程序（${blockersText}），仅允许清空对话恢复到初始状态 T0`)
                    : tr(`Blocked (${blockersText}): session-only restore to T${rbTurn}`, `同目录有活跃程序（${blockersText}），仅允许恢复记忆到 T${rbTurn}`))
                : noFileSnap
                  ? (rbTurn === 0
                      ? tr("No Git file snapshot for T0: reset session only to initial state T0?", "当前无 T0 文件快照：仅清空对话恢复到初始状态 T0？")
                      : tr(`No Git file snapshot for T${rbTurn}: restore session only?`, `当前无 T${rbTurn} 文件快照：仅恢复会话记忆？`))
                  : rbTurn === 0
                    ? tr("Restore to initial state T0 (clears all turns & reverts workspace edits)?", "确认恢复到初始状态 T0（清空全部对话并还原工作区修改）？")
                    : isoLevel === "exclusive_confirm"
                      ? tr(`Keep T${rbTurn} & earlier on main checkout (reverts edits & turns after T${rbTurn})?`, `确认恢复到 T${rbTurn}（保留 T${rbTurn} 及之前会话，撤回后续会话与修改）？`)
                      : tr(`Keep T${rbTurn} & earlier in worktree (reverts edits & turns after T${rbTurn})?`, `确认恢复到 T${rbTurn}（保留 T${rbTurn} 及之前会话，撤回后续会话与修改）？`),
            ))
            .child(div().flex_1())
            .child(
              div().id("ckpt-confirm-files").px(9).py(4).rounded(6).bg(disableFiles ? t.background : t.solid)
                .text_size(11.5).text_color(disableFiles ? t.dim : t.solid_foreground)
                .when(!disableFiles, (e) => e.cursor_pointer().on_click((_e, cx) => this.rollbackCheckpoint(rbTurn, true, cx)))
                .child(this.rollbackBusy ? tr("Restoring…", "恢复中…") : tr("Files & session", "文件+会话")),
            )
            .child(
              div().id("ckpt-confirm-mem").px(9).py(4).rounded(6).border_1().border_color(t.border).text_size(11.5)
                .when(!this.rollbackBusy, (e) => e.cursor_pointer().hover((s) => s.bg(t.hover)).on_click((_e, cx) => this.rollbackCheckpoint(rbTurn, false, cx)))
                .child(tr("Session only", "仅会话")),
            )
            .child(
              div().id("ckpt-confirm-cancel").px(7).py(4).text_size(11.5).text_color(t.muted).cursor_pointer()
                .on_click((_e, cx) => { this.confirmTurn = null; cx.notify(); })
                .child(tr("Cancel", "取消")),
            ),
        );
      })
      .child(
        h_flex()
          .id("ckpt-diff-toolbar")
          .h(30)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(10)
          .gap(6)
          .items_center()
          .bg(t.raised)
          .border_b(1)
          .border_color(t.border)
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11)
              .font_medium()
              .text_color(t.muted)
              .child(this.selectedTurn != null ? tr(`Turn T${this.selectedTurn} Diff`, `T${this.selectedTurn} 变更详情`) : tr("Diff", "变更详情")),
          )
          .child(this.renderDiffModeToggle(t, diffMode)),
      )
      .when(diffMode === "split" && this.rows.length > 0, (el) =>
        el.child(
          h_flex()
            .id("ckpt-split-header")
            .h(22)
            .flex_none()
            .w_full()
            .min_w(0)
            .items_center()
            .bg(t.surface ?? t.raised)
            .border_b(1)
            .border_color(t.border)
            .child(div().flex_1().min_w(0).px(10).truncate().text_size(10.5).font_medium().text_color(t.muted).child(tr("Before", "修改前")))
            .child(div().w(1).h_full().flex_none().bg(t.border))
            .child(div().flex_1().min_w(0).px(10).truncate().text_size(10.5).font_medium().text_color(t.muted).child(tr("After", "修改后"))),
        ),
      )
      .child(
        div().flex_1().w_full().min_w(0).min_h(0).relative().overflow_hidden().font_family(settings().font_family ?? "JetBrains Mono").text_size(11)
          .on_mouse_down("left", (e) => { this.lastShift = Boolean(e?.modifiers?.shift); })
          .child(
            list(
              diffListId,
              items.length,
              (i) => items[i]?.key ?? String(i),
              (i) => {
                const item = items[i];
                if (!item) return div().h(0);
                if (item.type === "comment") return this.renderSavedComment(item, t);
                if (item.type === "split") return this.renderSplitRow(item.srow, t, this.composer, commentedSet);
                return this.renderCodeRow(item.row, t, this.composer, commentedSet);
              },
            ).on_item_click((itemKey, cx) => this.handleVirtualItemClick(itemKey, cx)).size_full(),
          )
          .child(Scrollbar.vertical(diffListId).viewport_from_layout().absolute().inset_0())
          .when(!this.checkpoints.length && !this.loading && !this.error, (e) =>
            e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Turn diffs appear after each agent turn completes", "每轮对话完成后将在此显示轮次 diff"))),
          ),
      )
      .when(Boolean(this.composer), (e) => e.child(this.renderDockedComposer(this.composer, t)))
      .when(allDrafts.length > 0 || Boolean(this.reviewError), (e) => e.child(this.renderReviewBar(t, allDrafts)))
      .when(Boolean(this.note), (e) => e.child(div().px(10).pb(8).text_size(11).text_color(t.ansi1).whitespace_normal().child(this.note)));
  }
}
