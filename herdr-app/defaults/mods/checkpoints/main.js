import { View, div } from "gpui-kit";
import { v_flex, h_flex, Scrollbar, v_virtual_list } from "gpui-base";
import { spaces, panes, focused, agents, terminals, theme, settings, h2, shared, setShared, run } from "herdr";

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

export default class Checkpoints extends View {
  init(_props, cx) {
    this.checkpoints = [];
    this.isolation = null;
    this.selectedTurn = null;
    this.confirmTurn = null;
    this.targetAddr = null;
    this.rows = [];
    this.diffWidth = 0;
    this.rollbackBusy = false;
    this.snapping = false;
    this.loading = false;
    this.error = null;
    this.note = null;
    this.handle = cx.focus_handle();
    this.refresh(cx);
    cx.timer.every(10000, (cx) => this.refresh(cx));
  }

  viewKey() {
    const target = activeTargetAddress();
    const tRec = target ? (terminals().find((x) => x.address === target) ?? agents().find((x) => x.address === target)) : null;
    return `checkpoints:${target ?? ""}:${tRec?.turn ?? 0}:${tRec?.foreground?.pid ?? ""}:${tRec?.checkpoint_seq ?? 0}:${shared("checkpoint.sync") ?? ""}`;
  }

  setLines(lines) {
    this.rows = diffRows(lines);
    this.diffWidth = Math.max(300, ...this.rows.map((row) => (row.text.length + 1) * 7 + 50));
  }

  refresh(cx) {
    if (this.loading || this.rollbackBusy) return;
    const target = activeTargetAddress();
    const context = this.viewKey();
    if (context !== this.context) {
      this.context = context;
      this.checkpoints = [];
      this.isolation = null;
      this.confirmTurn = null;
      this.note = null;
      this.error = null;
      if (this.targetAddr !== target) this.selectedTurn = null;
      this.targetAddr = target;
      this.setLines([]);
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
        }
        if (this.selectedTurn != null) {
          await this.loadCheckpointDiff(this.selectedTurn, context);
        } else {
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
    this.note = null;
    const context = this.viewKey();
    cx.spawn(async (cx) => {
      try { await this.loadCheckpointDiff(turn, context); }
      catch (e) { if (this.viewKey() === context) this.note = String(e); }
      cx.notify();
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

  diffRow({ kind, number, text }, t) {
    if (kind === "@") return div().w_full().px(12).h(20).flex_none().truncate().bg(t.raised).text_color(t.dim).child(text);
    return h_flex().w_full().h(20).flex_none().gap(10).when(kind === "+", (e) => e.bg(t.ansi2 + "18")).when(kind === "-", (e) => e.bg(t.ansi1 + "18"))
      .child(div().w(30).flex_none().text_right().text_color(t.dim).child(number === undefined ? "" : String(number)))
      .child(div().flex_1().min_w(0).truncate().child(text));
  }

  render(cx) {
    if (this.viewKey() !== this.context) this.refresh(cx);
    const t = theme();
    const activeTarget = activeTargetAddress();
    const targetRec = this.targetAddr
      ? (terminals().find((x) => x.address === this.targetAddr) ?? agents().find((x) => x.address === this.targetAddr))
      : null;
    const ckptMax = this.checkpoints.reduce((m, c) => Math.max(m, Number(c.turn) || 0), 0);
    const maxTurn = this.checkpoints.length > 0 ? ckptMax : Math.max(Number(targetRec?.turn ?? 0), 0);
    const isoLevel = this.isolation?.level ?? "unknown";
    const filesBlocked = isoLevel === "shared_blocked";
    const listHeight = this.checkpoints.length * 30 + 12;

    return v_flex().id("checkpoints").size_full().min_h(0).rounded(10).overflow_hidden().bg(t.background).border_1().border_color(t.border).text_color(t.foreground).text_size(13).track_focus(this.handle)
      .child(
        h_flex().h(42).flex_none().px(12).gap(8).items_center().border_b(1).border_color(t.border)
          .child(div().font_semibold().child(tr("Checkpoints", "快照")))
          .when(Boolean(this.targetAddr), (el) =>
            el.child(div().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(this.targetAddr)),
          )
          .when(this.checkpoints.length > 0, (el) =>
            el.child(div().px(5).py(1).rounded(4).border_1().border_color(t.border).text_size(10).text_color(t.muted).child(tr(`${this.checkpoints.length} turns`, `${this.checkpoints.length} 个节点`))),
          )
          .child(div().flex_1())
          .when(Boolean(activeTarget || this.targetAddr), (el) =>
            el.child(
              div().id("checkpoints-snap").px(8).py(3).rounded(6).border_1().border_color(t.border).text_size(11).text_color(t.foreground).cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .on_click((_e, cx) => this.snapNow(cx))
                .child(this.snapping ? tr("Snapping…", "抓取中…") : tr("+ Snap", "+ 存档")),
            ),
          ),
      )
      .child(
        div().flex_1().min_h(44).max_h(listHeight).relative().border_b(1).border_color(t.border)
          .child(
            v_flex().id("checkpoints-list").absolute().inset_0().overflow_y_scroll().p(6)
              .children(this.checkpoints.map((c) => {
                const turn = Number(c.turn);
                const files = Array.isArray(c.files) ? c.files : [];
                const cAdd = files.reduce((n, f) => n + (Number(f.added) || 0), 0);
                const cDel = files.reduce((n, f) => n + (Number(f.deleted) || 0), 0);
                const summary = c.label || c.prompt || (turn === 0 ? tr("Initial state (before Turn 1)", "初始状态 (第 1 轮前)") : tr(`Turn ${turn}`, `第 ${turn} 轮`));
                const selected = Number(this.selectedTurn) === turn;
                const isCurrent = turn === maxTurn && (turn === 0 || !c.label);
                return h_flex().id(`ckpt-${turn}`).h(30).flex_none().px(8).gap(8).items_center().rounded(7).cursor_pointer()
                  .when(selected, (e) => e.bg(t.selected)).hover((s) => s.bg(t.hover))
                  .on_click((_e, cx) => this.pickCheckpoint(turn, cx))
                  .child(div().w(44).flex_none().text_size(11).text_color(t.accent ?? t.muted).child(`T${turn}`))
                  .child(div().flex_1().min_w(0).truncate().child(summary))
                  .when(cAdd > 0, (e) => e.child(div().text_size(11).text_color(t.ansi2).child(`+${cAdd}`)))
                  .when(cDel > 0, (e) => e.child(div().text_size(11).text_color(t.ansi1).child(`−${cDel}`)))
                  .when(isCurrent, (e) =>
                    e.child(div().px(6).py(2).rounded(5).bg(t.raised).text_size(10.5).text_color(t.dim).child(tr("Current", "当前"))),
                  )
                  .when(!isCurrent, (e) =>
                    e.child(
                      div().id(`ckpt-rb-${turn}`).px(6).py(2).rounded(5).border_1().border_color(t.border).text_size(10.5).text_color(t.muted)
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
            .child(div().text_size(11.5).text_color(filesBlocked ? t.ansi1 : t.foreground).child(
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
        div().flex_1().min_h(0).relative().font_family(settings().font_family ?? "JetBrains Mono").text_size(11)
          .child(
            div().id("checkpoints-diff-x").size_full().overflow_x_scrollbar()
              .child(
                div().w_full().min_w(this.diffWidth).h_full().child(
                  v_virtual_list("checkpoints-diff", this.rows.length, 20, (i) => String(i),
                    (range) => this.rows.slice(range.start, range.end).map((row) => this.diffRow(row, t))).size_full(),
                ),
              ),
          )
          .child(Scrollbar.vertical("checkpoints-diff").viewport_from_layout().absolute().inset_0())
          .when(!this.checkpoints.length && !this.loading && !this.error, (e) =>
            e.child(div().absolute().top(6).px(12).text_color(t.dim).child(tr("Turn diffs appear after each agent turn completes", "每轮对话完成后将在此显示轮次 diff"))),
          ),
      )
      .when(Boolean(this.note), (e) => e.child(div().px(10).pb(8).text_size(11).text_color(t.ansi1).child(this.note)));
  }
}
