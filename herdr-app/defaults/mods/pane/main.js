// A pane's header, for layouts that frame their panes:
//   { "tab": "active", "pane": { "header": { "content": { "mod": "pane" } } } }
// Shows the program's icon (the app's program table), the agent's name or
// the terminal's title, the folder, and the agent's status; buttons split, zoom and close
// this pane. Props from the app: { pane, terminal }. Click: focus the pane.

import { View, div, svg, image } from "gpui-kit";
import { h_flex, InputState, Input } from "gpui-base";
import { panes, terminals, props, run, h2, openOverlay, theme, home, layout, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

// Match the first visible tab frame, including the active child of a stack.
function paneFrame(node) {
  if (!node || node.hidden) return null;
  if (node.content?.tab && node.content.pane) return node.content.pane;
  let children = node.children ?? [];
  if (node.type === "stack") {
    const visible = children.filter(child => !child.hidden);
    children = [visible.find(child => child.id === node.active) ?? visible[0]];
  }
  for (const child of children) {
    const frame = paneFrame(child);
    if (frame) return frame;
  }
  return null;
}

function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

function folder(cwd) {
  if (!cwd) return "";
  const h = home();
  return cwd === h ? "~" : cwd.startsWith(h + "/") ? "~" + cwd.slice(h.length) : cwd;
}

function formatTokens(n) {
  if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return "0";
  if (n >= 1000000) return `${(n / 1000000).toFixed(1).replace(/\.0$/, "")}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}k`;
  return String(Math.round(n));
}

function formatElapsed(ms) {
  const totalSec = Math.max(0, Math.floor((Number(ms) || 0) / 1000));
  const h = Math.floor(totalSec / 3600);
  const m = Math.floor((totalSec % 3600) / 60);
  const s = String(totalSec % 60).padStart(2, "0");
  if (h >= 24) return `${Math.floor(h / 24)}d ${h % 24}h`;
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${s}`;
  return `${m}:${s}`;
}

export default class Pane extends View {
  init(_props, cx) {
    this.hovered = false;
    this.editing = false;
    this.editingTerminal = null;
    this.renameText = "";
    this.localWorkingSince = {};
    this.localWorkMs = {};
    this.renameInput = InputState.new({
      placeholder: tr("Pane name (empty: default)", "分栏名称（留空恢复默认）"),
    });
    this.renameInput.on("change", () => {
      this.renameText = this.renameInput.value();
    });
    this.renameInput.on("submit", (_e, cx) => this.commitRename(cx));
    this.renameInput.on("blur", (_e, cx) => this.commitRename(cx));
    this.handle = cx?.focus_handle?.() ?? null;
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

  startRename(terminal, current, cx) {
    if (!terminal) return;
    this.editing = true;
    this.editingTerminal = terminal;
    this.renameText = current ?? "";
    this.renameInput.set_value(this.renameText);
    if (this.renameText) {
      this.renameInput.set_selected_range({ start: 0, end: this.renameText.length });
    }
    this.renameInput.focus();
    cx?.notify?.();
  }

  commitRename(cx) {
    if (!this.editing || !this.editingTerminal) return;
    const terminal = this.editingTerminal;
    const name = this.renameText.trim();
    this.editing = false;
    this.editingTerminal = null;
    run("agent.rename", { terminal, name });
    cx?.notify?.();
  }

  cancelRename(cx) {
    if (!this.editing) return;
    this.editing = false;
    this.editingTerminal = null;
    this.handle?.focus?.();
    cx?.notify?.();
  }

  me() {
    return panes().find((p) => p.pane === props()?.pane);
  }

  button(t, id, icon, command, args, tip = null) {
    return div()
      .id(id)
      .p(3)
      .rounded(4)
      .text_color(t.dim)
      .hover((s) => s.bg(t.hover).text_color(t.foreground))
      .when(Boolean(tip), (el) => el.tooltip(tip))
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        run(command, args);
      })
      .child(svg(`mods/pane/${icon}.svg`).size(14));
  }

  render(cx) {
    const t = theme();
    const p = this.me();
    if (!p) return div();
    const isAcp = Boolean(p.acp);
    const isStructured = isAcp || Boolean(p.structured);
    const isTermAgent = !isAcp && Boolean(p.structured || p.program?.agent);
    const isAgent = isAcp || isTermAgent;
    const term = p.terminal ? terminals().find((x) => x.address === p.terminal) : null;
    const parent = term?.parent ? terminals().find((x) => x.address === term.parent) : null;
    const lineageParent = term?.lineage?.parent || null;
    const tabPaneCount = panes().filter((x) => x.tab === p.tab).length;
    const singleStructuredTab = isStructured && tabPaneCount <= 1;
    const customName = p.custom_name || term?.labels?.["herdr.name"];
    const name = isStructured
      ? (customName || term?.name || term?.template || p.program?.name || p.title || p.terminal || "Agent")
      : (customName || ((p.program?.agent || p.program?.shell) && p.program.name) || p.title || p.program?.name || "shell");
    const isEditing = this.editing && Boolean(p.terminal) && this.editingTerminal === p.terminal;
    const pic = icon(p.program, 14, t.dim);
    const st = p.status;
    const pane = p.pane;
    const usage = p.context_usage ?? term?.context_usage ?? null;
    const hasUsage = (isAcp || isStructured) && usage && typeof usage.size === "number" && usage.size > 0;
    const showViewToggle = !isAcp && Boolean(p.terminal) && (isStructured || Boolean(p.program?.agent) || this.hovered);
    const pct = hasUsage ? Math.min(100, Math.round(((usage.used ?? 0) / usage.size) * 100)) : 0;
    const pctColor = pct >= 85 ? (t.blocked ?? "#ef4444") : pct >= 65 ? (t.done ?? "#f59e0b") : t.muted;
    const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);
    const accent = t.accent ?? "#3b82f6";
    const showHandoffBtns = Boolean(p.terminal) && st !== "ended" && (isAgent || isStructured || Boolean(lineageParent));
    const headerPill = (id, label, iconPath, command, args, tip, highlighted = false) =>
      h_flex()
        .id(id)
        .flex_none()
        .h(20)
        .px(6)
        .gap(4)
        .items_center()
        .rounded(6)
        .bg(highlighted ? (alpha(accent, "18") ?? t.raised) : t.raised)
        .border_1()
        .border_color(highlighted ? (alpha(accent, "66") ?? t.border) : t.border)
        .text_size(10.5)
        .font_medium()
        .text_color(highlighted ? accent : t.muted)
        .cursor_pointer()
        .hover((s) => s.bg(highlighted ? (alpha(accent, "2a") ?? t.hover) : t.hover).text_color(highlighted ? accent : t.foreground))
        .when(Boolean(tip), (el) => el.tooltip(tip))
        .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
        .on_click((_e, cx) => {
          cx.stop_propagation();
          run(command, args);
        })
        .when(Boolean(iconPath), (el) => el.child(svg(iconPath).size(11).flex_none().when(highlighted, (s) => s.text_color(accent))))
        .child(label);
    const elapsed = isAgent
      ? this.agentElapsed(
          p.terminal || pane,
          st,
          p.working_since ?? term?.working_since,
          p.work_ms ?? term?.work_ms,
          p.total_work_ms ?? term?.total_work_ms,
          Date.now(),
        )
      : null;
    const timeTooltip = (info) => {
      if (!info) return "";
      const cur = formatElapsed(info.ms);
      const base = info.working
        ? tr(`Execution time: ${cur} (running)`, `执行时间：${cur}（运行中）`)
        : info.ran
          ? tr(`Execution time: ${cur}`, `执行时间：${cur}`)
          : tr("Execution time: 0:00 (idle)", "执行时间：0:00（尚未执行）");
      if (info.totalMs > info.ms + 1000) {
        return `${base} · ${tr(`Total ${formatElapsed(info.totalMs)}`, `累计 ${formatElapsed(info.totalMs)}`)}`;
      }
      return base;
    };
    const bottom = paneFrame(layout())?.header?.position === "bottom";
    return h_flex()
      .id(`pane-head-${pane}`)
      .when(Boolean(this.handle), (el) => el.track_focus(this.handle))
      .size_full()
      .px(10)
      .gap(8)
      .items_center()
      .bg(t.background)
      .when(bottom, el => el.border_t(1))
      .when(!bottom, el => el.border_b(1))
      .border_color(t.border)
      .text_size(12)
      .on_hover((hovered, cx) => { this.hovered = hovered; cx.notify(); })
      .on_mouse_down("right", (_e, cx) => {
        cx.stop_propagation();
        openOverlay({ mod: "menu", kind: "popup", width: isAcp ? 280 : 220, props: { tab: "pane", target: { pane, terminal: p.terminal } } });
      })
      .on_click(() => run("pane.focus", { pane }))
      .when(!singleStructuredTab && Boolean(pic), (el) => el.child(pic))
      .when(!singleStructuredTab && isEditing, (el) =>
        el.child(
          div()
            .flex_none()
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_key_down((e, cx) => {
              if (e.key === "escape") {
                cx.stop_propagation();
                this.cancelRename(cx);
              }
            })
            .child(
              Input.new(this.renameInput)
                .id(`ph-ren-input-${pane}`)
                .w(160)
                .h(22)
                .px(6)
                .rounded(4)
                .border_1()
                .border_color(t.accent ?? t.border)
                .bg(t.raised)
                .text_size(11.5)
                .text_color(t.foreground),
            ),
        ),
      )
      .when(!singleStructuredTab && !isEditing, (el) =>
        el.child(
          div()
            .id(`ph-name-${pane}`)
            .flex_none()
            .truncate()
            .max_w(220)
            .font_medium()
            .text_color(t.foreground)
            .when(Boolean(p.terminal), (el) =>
              el.on_mouse_down("left", (e, cx) => {
                if (e?.click_count === 2) {
                  cx.stop_propagation();
                  this.startRename(p.terminal, customName ?? name, cx);
                }
              }),
            )
            .child(name),
        ),
      )
      .when(!singleStructuredTab && !isEditing && Boolean(p.terminal) && (tabPaneCount > 1 || this.hovered), (el) =>
        el.child(
          div()
            .id(`ph-ren-${pane}`)
            .p(2)
            .rounded(4)
            .flex_none()
            .cursor_pointer()
            .text_color(t.dim)
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .tooltip(tr("Rename pane", "重命名分栏"))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              this.startRename(p.terminal, customName ?? name, cx);
            })
            .child(svg("mods/workspace-chrome/square-pen.svg").size(12)),
        ),
      )
      .when(isAcp, el => el.child(
        div()
          .id(`ph-type-${pane}`)
          .flex_none()
          .px(5)
          .h(16)
          .flex()
          .items_center()
          .rounded(4)
          .bg(alpha(accent, "22") ?? t.raised)
          .border_1()
          .border_color(alpha(accent, "66") ?? t.border)
          .text_size(10)
          .font_semibold()
          .text_color(accent)
          .tooltip(tr("ACP Agent", "ACP 智能体"))
          .child("ACP"),
      ))
      .when(isTermAgent, el => el.child(
        div()
          .id(`ph-type-${pane}`)
          .flex_none()
          .px(5)
          .h(16)
          .flex()
          .items_center()
          .rounded(4)
          .bg(isStructured ? (alpha(accent, "22") ?? t.raised) : t.raised)
          .border_1()
          .border_color(isStructured ? (alpha(accent, "66") ?? t.border) : t.border)
          .text_size(10)
          .font_semibold()
          .text_color(isStructured ? accent : t.muted)
          .tooltip(isStructured ? tr("Terminal Agent (Structured View)", "终端智能体（结构化视图）") : tr("Terminal Agent", "终端智能体"))
          .child(tr("CLI", "终端")),
      ))
      .when(isStructured && Boolean(p.terminal), el => el.child(div().flex_none().text_size(10.5).text_color(t.dim).child(p.terminal)))
      .when(isAcp && Boolean(parent), el => el.child(div().flex_none().text_size(11).text_color(t.muted).child(tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`))))
      .when(Boolean(lineageParent) && lineageParent !== parent?.address, (el) =>
        el.child(
          div()
            .id(`ph-lin-${pane}`)
            .flex_none()
            .px(5)
            .py(1)
            .rounded(4)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_size(10.5)
            .text_color(accent)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .tooltip(tr(`Focus parent session ${lineageParent}`, `跳转到来源父会话 ${lineageParent}`))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.focus", { terminal: lineageParent });
            })
            .child(`↳ ${lineageParent}`),
        ),
      )
      .child(div().flex_1().min_w(0).truncate().text_color(t.dim).child(folder(p.cwd)))
      .when(showHandoffBtns && Boolean(lineageParent), (el) =>
        el.child(
          headerPill(
            `ph-merge-${pane}`,
            tr("Merge", "合回"),
            "mods/peek/corner-up-right.svg",
            "agent.handoff",
            { source: p.terminal, into: lineageParent, kind: "merge-back" },
            tr(`Merge conclusions back into ${lineageParent}`, `一键将结论合回 ${lineageParent}`),
            true,
          ),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el
          .child(
            headerPill(
              `ph-side-${pane}`,
              tr("Side Chat", "旁路讨论"),
              "mods/peek/messages-square.svg",
              "agent.handoff",
              { source: p.terminal, kind: "side-chat" },
              tr("Open isolated side-chat with current context", "带当前上下文开启旁路讨论（不污染主会话）"),
            ),
          )
          .child(
            headerPill(
              `ph-ho-${pane}`,
              tr("Handoff", "接力"),
              "mods/workspace-chrome/git-branch.svg",
              "agent.handoff",
              { source: p.terminal, kind: "handoff" },
              tr("Context handoff / Fork / Merge", "上下文接力 / 分支探索 / 合回"),
            ),
          ),
      )
      .when(showViewToggle, el => el.child(
        h_flex()
          .id(`ph-view-${pane}`)
          .flex_none()
          .h(20)
          .px(7)
          .gap(4)
          .items_center()
          .rounded(6)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .text_size(11)
          .font_medium()
          .text_color(t.muted)
          .cursor_pointer()
          .hover((s) => s.bg(t.hover).text_color(t.foreground))
          .tooltip(isStructured ? tr("Switch back to raw terminal view", "切回原始终端视图") : tr("Switch to structured chat view", "切换到结构化对话视图"))
          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
          .on_click((_e, cx) => {
            cx.stop_propagation();
            run("terminal.view", { terminal: p.terminal, view: "toggle" });
          })
          .child(svg(isStructured ? "mods/workspace-chrome/terminal.svg" : "mods/workspace-chrome/layers.svg").size(12).flex_none())
          .child(isStructured ? tr("Terminal", "终端") : tr("Chat view", "对话视图")),
      ))
      .when(hasUsage, el => el.child(
        div()
          .id(`ph-ctx-${pane}`)
          .flex_none()
          .px(6)
          .h(18)
          .flex()
          .items_center()
          .rounded(5)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .text_size(10.5)
          .font_medium()
          .text_color(pctColor)
          .tooltip(tr(`Context window: ${formatTokens(usage.used ?? 0)} / ${formatTokens(usage.size)} (${pct}%)`, `上下文占用：${formatTokens(usage.used ?? 0)} / ${formatTokens(usage.size)} (${pct}%)`))
          .child(tr(`Ctx ${pct}%`, `上下文 ${pct}%`)),
      ))
      .when(Boolean(elapsed), el => el.child(
        h_flex()
          .id(`ph-time-${pane}`)
          .flex_none()
          .h(18)
          .px(6)
          .gap(4)
          .items_center()
          .rounded(5)
          .bg(elapsed.working ? (alpha(t.working, "1f") ?? t.raised) : t.raised)
          .border_1()
          .border_color(elapsed.working ? (alpha(t.working, "55") ?? t.border) : t.border)
          .text_size(10.5)
          .font_medium()
          .text_color(elapsed.working ? (t.working ?? accent) : (elapsed.ran ? t.muted : t.dim))
          .tooltip(timeTooltip(elapsed))
          .child(svg("mods/workspace-chrome/clock.svg").size(10.5).flex_none())
          .child(formatElapsed(elapsed.ms)),
      ))
      .when(st === "working", el => el.child(svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)))
      .when(isStructured && st === "working" && Boolean(p.terminal), el =>
        el.child(
          div()
            .id(`ph-stop-${pane}`)
            .px(7)
            .py(2)
            .rounded(5)
            .flex_none()
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_size(10.5)
            .text_color(t.blocked ?? "#ef4444")
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              h2(["interrupt", "--json", p.terminal]);
            })
            .child(tr("Stop", "停止")),
        ),
      )
      .when(st === "ended", el => el.child(div().flex_none().px(6).h(18).flex().items_center().rounded(5).bg(t.raised).border_1().border_color(t.border).text_size(10.5).font_medium().text_color(t.muted).child(tr("Ended", "已结束"))))
      .when(st === "blocked", el => el.child(div().flex_none().px(6).h(18).flex().items_center().rounded(5).bg(t.blocked + "26").text_size(10.5).font_semibold().text_color(t.blocked).child(tr("Needs you", "待确认"))))
      .when(st === "done", el => el.child(div().size(7).flex_none().rounded(4).bg(t.done)))
      .when(this.hovered || isStructured, el => el
        .child(this.button(t, `ph-r-${pane}`, "split-right", "pane.split", { direction: "right", pane }, tr("Split right", "向右分栏")))
        .child(this.button(t, `ph-d-${pane}`, "split-down", "pane.split", { direction: "down", pane }, tr("Split down", "向下分栏")))
        .when(tabPaneCount > 1, el => el.child(this.button(t, `ph-z-${pane}`, "zoom", "pane.detach", { pane }, tr("Maximize to new tab", "最大化为独立标签页"))))
        .child(this.button(t, `ph-x-${pane}`, "close", "pane.close", { pane }, st === "ended" ? tr("Close tab", "关闭标签页") : isAcp ? tr("Move to background", "转入后台") : tr("Close pane", "关闭窗格"))));
  }
}
