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

  button(id, icon, command, args) {
    return div()
      .id(id)
      .p(4)
      .rounded(5)
      .text_color("#5d6168")
      .cursor_pointer()
      .hover((s) => s.bg("#ffffff0d").text_color("#d6d8db"))
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
      .child(div().flex_1().truncate().text_color("#8b8f95").child(dir))
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
      .child(this.button(`th-r-${pane}`, "split-right", "pane.split", { direction: "right", pane }))
      .child(this.button(`th-d-${pane}`, "split-down", "pane.split", { direction: "down", pane }))
      .when(tabPaneCount > 1, (el) => el.child(this.button(`th-z-${pane}`, "zoom", "pane.detach", { pane })))
      .child(this.button(`th-x-${pane}`, "close", "pane.close", { pane }));
  }
}
