// Terminals pane header; props: { pane, terminal }.

import { View, div, svg, image } from "gpui-kit";
import { h_flex } from "gpui-base";
import { panes, terminals, props, run, home, theme, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function formatElapsed(ms) {
  const totalSec = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = String(totalSec % 60).padStart(2, "0");
  if (h >= 24) return `${Math.floor(h / 24)}d ${h % 24}h`;
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${s}`;
  return `${m}:${s}`;
}

function folder(cwd) {
  if (!cwd) return "";
  const h = home();
  return cwd === h ? "~" : cwd.startsWith(h + "/") ? "~" + cwd.slice(h.length) : cwd;
}

export default class TerminalsPane extends View {
  init(_props, cx) {
    this.localWorkingSince = {};
    this.localWorkMs = {};
    cx?.timer?.every?.(1000, (tcx) => {
      const p = this.me();
      if (p?.status === "working" && (p.acp || p.structured || p.program?.agent)) {
        tcx?.notify?.();
      }
    });
  }

  agentElapsed(key, status, workingSince, workMs, totalWorkMs, now) {
    const hasWs = typeof workingSince === "number" && workingSince > 0;
    const hasWork = typeof workMs === "number" && workMs >= 0;
    if (status === "working") {
      const start = hasWs ? workingSince : (this.localWorkingSince[key] ?? (this.localWorkingSince[key] = now));
      const ms = Math.max(0, now - start);
      this.localWorkMs[key] = ms;
      return { ms, totalMs: Math.max(ms, Number(totalWorkMs) || 0), working: true, ran: true };
    }
    if (this.localWorkingSince[key]) {
      if (!hasWork) {
        this.localWorkMs[key] = Math.max(this.localWorkMs[key] ?? 0, now - this.localWorkingSince[key]);
      }
      delete this.localWorkingSince[key];
    }
    if (hasWork) {
      this.localWorkMs[key] = workMs;
      return { ms: workMs, totalMs: Math.max(workMs, Number(totalWorkMs) || 0), working: false, ran: true };
    }
    if (typeof this.localWorkMs[key] === "number") {
      const ms = this.localWorkMs[key];
      return { ms, totalMs: Math.max(ms, Number(totalWorkMs) || 0), working: false, ran: true };
    }
    return { ms: 0, totalMs: Number(totalWorkMs) || 0, working: false, ran: false };
  }

  me() {
    return panes().find((p) => p.pane === props()?.pane);
  }

  button(id, icon, command, args, tip = null, danger = false) {
    return div()
      .id(id)
      .p(4)
      .rounded(5)
      .text_color("#5d6168")
      .cursor_pointer()
      .hover((s) => s.bg("#ffffff0d").text_color(danger ? "#ef4444" : "#d6d8db"))
      .when(Boolean(tip), (el) => el.tooltip(tip))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run(command, args);
      })
      .child(svg(`mods/pane/${icon}.svg`).size(15));
  }

  render() {
    const t = theme();
    const p = this.me();
    if (!p) return div();
    const pane = p.pane;
    const tabPaneCount = panes().filter((x) => x.tab === p.tab).length;
    const g = p.program;
    const where = folder(p.cwd);
    const isAcp = Boolean(p.acp);
    const isTermAgent = !isAcp && Boolean(p.structured || g?.agent);
    const isAgent = isAcp || isTermAgent;
    const term = p.terminal ? terminals().find((x) => x.address === p.terminal) : null;
    const rawLineageParent = (term?.lineage?.parent || "").trim();
    const curSlash = typeof p.terminal === "string" ? p.terminal.lastIndexOf("/") : -1;
    const lineageParent = rawLineageParent
      ? (!rawLineageParent.includes("/") && curSlash > 0 ? `${p.terminal.slice(0, curSlash + 1)}${rawLineageParent}` : rawLineageParent)
      : null;
    const showHandoffBtns = Boolean(p.terminal) && p.status !== "ended" && (isAgent || Boolean(lineageParent));
    const accent = t.accent ?? "#3b82f6";
    const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);
    const elapsed = isAgent
      ? this.agentElapsed(
          p.terminal || pane,
          p.status,
          p.working_since ?? term?.working_since,
          p.work_ms ?? term?.work_ms,
          p.total_work_ms ?? term?.total_work_ms,
          Date.now(),
        )
      : null;
    // Shells often put the folder in their title already.
    const title = p.title || "shell";
    const dir = where && !title.includes(where) && !/^[~/]/.test(title) ? where : "";
    return h_flex()
      .id(`tm-head-${pane}`)
      .size_full()
      .pl(12)
      .pr(8)
      .gap(8)
      .items_center()
      .text_size(14)
      .on_click(() => run("pane.focus", { pane }))
      .when(Boolean(g?.icon), (el) =>
        el.child(g.tint ? svg(g.icon).size(16).flex_none().text_color("#d6d8db") : image(g.icon).w(15).h(15).flex_none()),
      )
      .child(div().flex_none().max_w(320).truncate().font_semibold().text_color(p.focused ? "#e8e9eb" : "#a9adb3").child(title))
      .when(isAcp, (el) => el.child(
        div()
          .id(`tm-type-${pane}`)
          .flex_none()
          .px(5)
          .h(18)
          .flex()
          .items_center()
          .rounded(4)
          .bg("#252628")
          .border_1()
          .border_color("#383b40")
          .text_size(10)
          .font_semibold()
          .text_color(t.accent ?? "#3b82f6")
          .child("ACP"),
      ))
      .when(isTermAgent, (el) => el.child(
        div()
          .id(`tm-type-${pane}`)
          .flex_none()
          .px(5)
          .h(18)
          .flex()
          .items_center()
          .rounded(4)
          .bg("#252628")
          .border_1()
          .border_color("#383b40")
          .text_size(10)
          .font_medium()
          .text_color("#a9adb3")
          .child(tr("CLI", "终端")),
      ))
      .when((isAgent || isStructured) && Boolean(p.terminal), (el) =>
        el.child(
          div()
            .id(`tm-addr-${pane}`)
            .flex_none()
            .px(5)
            .py(1)
            .rounded(4)
            .bg("#252628")
            .border_1()
            .border_color("#383b40")
            .text_size(11)
            .text_color("#a9adb3")
            .tooltip(tr(`Agent ID: ${p.terminal}`, `智能体编号：${p.terminal}`))
            .child(p.terminal),
        ),
      )
      .when(Boolean(term?.lineage?.kind), (el) => {
        const k = String(term.lineage.kind).toLowerCase();
        const label = k === "fork" ? tr("Fork", "会话分叉") : (k === "side-chat" || k === "side_chat") ? tr("Side Chat", "旁路讨论") : tr("Handoff", "上下文接力");
        const color = k === "fork" ? "#a855f7" : (k === "side-chat" || k === "side_chat") ? "#14b8a6" : "#3b82f6";
        return el.child(
          div()
            .id(`tm-kind-${pane}`)
            .flex_none()
            .px(5)
            .h(18)
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha(color, "22") ?? "#252628")
            .border_1()
            .border_color(alpha(color, "66") ?? "#383b40")
            .text_size(10)
            .font_semibold()
            .text_color(color)
            .child(label),
        );
      })
      .when(Boolean(term?.lineage?.worktree_path), (el) =>
        el.child(
          div()
            .id(`tm-wt-${pane}`)
            .flex_none()
            .px(5)
            .h(18)
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha("#f59e0b", "22") ?? "#252628")
            .border_1()
            .border_color(alpha("#f59e0b", "66") ?? "#383b40")
            .text_size(10)
            .font_semibold()
            .text_color("#f59e0b")
            .tooltip(tr(`Isolated Git Worktree: ${term.lineage.worktree_path}`, `独立 Git Worktree：${term.lineage.worktree_path}`))
            .child("Worktree"),
        ),
      )
      .when(Boolean(lineageParent), (el) =>
        el.child(
          div()
            .id(`tm-lin-${pane}`)
            .flex_none()
            .px(5)
            .py(1)
            .rounded(4)
            .bg("#252628")
            .border_1()
            .border_color("#383b40")
            .text_size(11)
            .text_color(accent)
            .cursor_pointer()
            .hover((s) => s.bg("#303338").text_color("#ffffff"))
            .tooltip(tr(`Focus parent session ${lineageParent}`, `跳转到来源父会话 ${lineageParent}`))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.focus", { terminal: lineageParent });
            })
            .child(`↳ ${lineageParent}`),
        ),
      )
      .child(div().flex_1().truncate().text_color("#8b8f95").child(dir))
      .when(showHandoffBtns && Boolean(lineageParent), (el) =>
        el.child(
          h_flex()
            .id(`tm-merge-${pane}`)
            .flex_none()
            .h(20)
            .px(6)
            .gap(4)
            .items_center()
            .rounded(5)
            .bg(alpha(accent, "22") ?? "#252628")
            .border_1()
            .border_color(alpha(accent, "66") ?? "#383b40")
            .text_size(11)
            .font_medium()
            .text_color(accent)
            .cursor_pointer()
            .hover((s) => s.bg(alpha(accent, "33") ?? "#303338"))
            .tooltip(tr(`Merge conclusions back into ${lineageParent}`, `一键合回 ${lineageParent}`))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.handoff", { source: p.terminal, into: lineageParent, kind: "merge-back" });
            })
            .child(svg("mods/peek/corner-up-right.svg").size(11).flex_none().text_color(accent))
            .child(tr("Merge", "合回")),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el.child(
          h_flex()
            .id(`tm-side-${pane}`)
            .flex_none()
            .h(20)
            .px(6)
            .gap(4)
            .items_center()
            .rounded(5)
            .bg("#252628")
            .border_1()
            .border_color("#383b40")
            .text_size(11)
            .font_medium()
            .text_color("#c4c7cc")
            .cursor_pointer()
            .hover((s) => s.bg("#303338").text_color("#ffffff"))
            .tooltip(tr("Open Side Chat in right panel", "在右侧面板打开旁路讨论"))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.handoff", { source: p.terminal, kind: "side-chat" });
            })
            .child(svg("mods/peek/messages-square.svg").size(11).flex_none())
            .child(tr("Side", "旁路")),
        ),
      )
      .when(showHandoffBtns && String(term?.lineage?.kind || "").toLowerCase().replace("_", "-") !== "side-chat", (el) =>
        el.child(
          h_flex()
            .id(`tm-fork-${pane}`)
            .flex_none()
            .h(20)
            .px(6)
            .gap(4)
            .items_center()
            .rounded(5)
            .bg("#252628")
            .border_1()
            .border_color("#383b40")
            .text_size(11)
            .font_medium()
            .text_color("#c4c7cc")
            .cursor_pointer()
            .hover((s) => s.bg("#303338").text_color("#ffffff"))
            .tooltip(tr("Fork Session (optional Worktree isolation)", "会话分叉（可选 Worktree 隔离）"))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.handoff", { source: p.terminal, kind: "fork" });
            })
            .child(svg("mods/workspace-chrome/git-branch.svg").size(11).flex_none())
            .child(tr("Fork", "分叉")),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el.child(
          h_flex()
            .id(`tm-ho-${pane}`)
            .flex_none()
            .h(20)
            .px(6)
            .gap(4)
            .items_center()
            .rounded(5)
            .bg("#252628")
            .border_1()
            .border_color("#383b40")
            .text_size(11)
            .font_medium()
            .text_color("#c4c7cc")
            .cursor_pointer()
            .hover((s) => s.bg("#303338").text_color("#ffffff"))
            .tooltip(tr("Context Handoff", "上下文接力"))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.handoff", { source: p.terminal, kind: "handoff" });
            })
            .child(svg("mods/peek/corner-up-right.svg").size(11).flex_none())
            .child(tr("Handoff", "接力")),
        ),
      )
      .when(Boolean(elapsed), (el) => el.child(
        h_flex()
          .id(`tm-time-${pane}`)
          .flex_none()
          .px(6)
          .h(20)
          .gap(4)
          .items_center()
          .rounded(5)
          .bg("#252628")
          .border_1()
          .border_color("#383b40")
          .text_size(11)
          .font_medium()
          .text_color(elapsed.working ? (t.working ?? t.accent ?? "#3b82f6") : (elapsed.ran ? "#a9adb3" : "#8b8f95"))
          .child(svg("mods/workspace-chrome/clock.svg").size(11).flex_none())
          .child(formatElapsed(elapsed.ms)),
      ))
      .child(this.button(`th-r-${pane}`, "split-right", "pane.split", { direction: "right", pane }, tr("Split right", "向右分栏")))
      .child(this.button(`th-d-${pane}`, "split-down", "pane.split", { direction: "down", pane }, tr("Split down", "向下分栏")))
      .when(tabPaneCount > 1, (el) => el.child(this.button(`th-z-${pane}`, "zoom", "pane.detach", { pane }, tr("Maximize to new tab", "最大化为独立标签页"))))
      .when(Boolean(p.terminal), (el) => el.child(this.button(`th-trash-${pane}`, "trash", "agent.trash", { terminal: p.terminal }, tr("Move to recycle bin", "移入回收站"), true)))
      .child(this.button(`th-x-${pane}`, "close", "pane.close", { pane }, isAgent ? tr("Close pane (keep agent in background)", "关闭窗格（Agent 保留在后台）") : tr("Close pane", "关闭窗格")));
  }
}
