// Terminals layout's draggable top bar: space picker, tabs, and window controls.

import { View, div, svg, image } from "gpui-kit";
import { h_flex } from "gpui-base";
import { spaces, panes, run, theme, home, openOverlay, window as win, settings } from "herdr";

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

const isAcpPane = (p) => Boolean(p?.acp);
const isTermAgentPane = (p) => !p?.acp && Boolean(p?.structured || p?.program?.agent);
const isAgentPane = (p) => isAcpPane(p) || isTermAgentPane(p);

const C = {
  tray: "#1d1f22",
  // The shown tab stands out; a hovered one only hints, so the two never look alike.
  tab: "#34373c",
  tabEdge: "#4a4d53",
  hover: "#ffffff07",
  edge: "#383b40",
  chip: "#252628",
  text: "#e8e9eb",
  dim: "#8b8f95",
};

function folder(cwd) {
  if (!cwd) return "";
  const h = home();
  return cwd === h ? "~" : cwd.startsWith(h + "/") ? "~" + cwd.slice(h.length) : cwd;
}

// The pane a tab's tab speaks for: the focused one, else its agent, else the first.
function lead(all, id) {
  const mine = all.filter((p) => p.tab === id);
  return mine.find((p) => p.focused) ?? mine.find((p) => p.program?.agent) ?? mine[0];
}

// The program's tile from the app's table: its color, its glyph (or logo) on it.
function tile(t, p) {
  const g = p?.program;
  const dot = p?.status && p.status !== "idle" ? t[p.status] : null;
  return div()
    .relative()
    .flex_none()
    .w(26)
    .h(26)
    .rounded(7)
    .border_1()
    .border_color("#ffffff14")
    .bg(g?.tile ?? C.chip)
    .flex()
    .items_center()
    .justify_center()
    .when(Boolean(g?.icon), (el) =>
      el.child(g.tint ? svg(g.icon).size(15).text_color(g.color ?? C.text) : image(g.icon).w(15).h(15)),
    )
    .when(Boolean(dot), (el) =>
      el.child(div().absolute().top(-4).right(-4).w(11).h(11).rounded_full().border_2().border_color(C.tray).bg(dot)),
    );
}

// A control in the draggable bar: the press is its own, not the window's
// (a right press there would open the system's window menu).
function own(el) {
  return el.on_mouse_down("left", (_e, cx) => cx.stop_propagation()).on_mouse_down("right", (_e, cx) => cx.stop_propagation());
}

function windowButton(id, icon, command) {
  return own(
    div()
      .id(id)
      .flex_none()
      .w(24)
      .h(24)
      .rounded_full()
      .flex()
      .items_center()
      .justify_center()
      .bg(C.chip)
      .text_color(C.dim)
      .cursor_pointer()
      .hover((s) => s.bg(C.edge).text_color(C.text))
      .on_click(() => run(command))
      .child(svg(`mods/terminals-bar/${icon}.svg`).size(14)),
  );
}

function iconButton(id, icon, onClick) {
  return own(div())
    .id(id)
    .flex_none()
    .p(7)
    .rounded(8)
    .text_color(C.dim)
    .cursor_pointer()
    .hover((s) => s.bg(C.chip).text_color(C.text))
    .on_click(onClick)
    .child(svg(`mods/terminals-bar/${icon}.svg`).size(17));
}

export default class TerminalsBar extends View {
  init(_props, cx) {
    this.localWorkingSince = {};
    this.localWorkMs = {};
    cx?.timer?.every?.(1000, (tcx) => {
      const space = spaces().find((p) => p.active);
      const activeIds = new Set((space?.tabs ?? []).map((c) => c.id));
      const working = (space?.tabs ?? []).some((c) => c.status === "working")
        || panes().some((p) => activeIds.has(p.tab) && isAgentPane(p) && p.status === "working");
      if (working) tcx?.notify?.();
    });
  }

  paneElapsed(key, status, workingSince, workMs, totalWorkMs, now) {
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

  tabElapsed(c, tabPanes, agentPanes, now) {
    for (const p of tabPanes) {
      if (!isAgentPane(p)) {
        const key = p.pane || p.terminal;
        if (key) {
          delete this.localWorkingSince[key];
          delete this.localWorkMs[key];
        }
      }
    }
    if (agentPanes.length === 0) {
      if (tabPanes.length > 0) {
        delete this.localWorkingSince[`tab:${c.id}`];
        delete this.localWorkMs[`tab:${c.id}`];
        return null;
      }
      if (typeof c.working_since === "number" || typeof c.work_ms === "number" || c.status === "working") {
        return this.paneElapsed(`tab:${c.id}`, c.status, c.working_since, c.work_ms, c.total_work_ms, now);
      }
      return null;
    }
    const entries = agentPanes.map((p) => {
      const key = p.pane || p.terminal || `tab:${c.id}`;
      const ws = p.working_since ?? (p.status === "working" && agentPanes.length === 1 ? c.working_since : null);
      const wm = p.work_ms ?? (agentPanes.length === 1 ? c.work_ms : null);
      const tm = p.total_work_ms ?? (agentPanes.length === 1 ? c.total_work_ms : 0);
      return { p, info: this.paneElapsed(key, p.status, ws, wm, tm, now) };
    });
    const working = entries.filter((e) => e.info.working);
    if (working.length > 0) {
      return working.reduce((best, cur) => (cur.info.ms >= best.info.ms ? cur : best)).info;
    }
    const focusedRan = entries.find((e) => e.p.focused && e.info.ran);
    if (focusedRan) return focusedRan.info;
    const ran = entries.filter((e) => e.info.ran);
    if (ran.length > 0) {
      return ran.reduce((best, cur) => (cur.info.ms >= best.info.ms ? cur : best)).info;
    }
    return entries[0].info;
  }

  render() {
    const t = theme();
    const space = spaces().find((p) => p.active);
    if (!space) return div();
    const all = panes();
    const w = win() ?? {};
    const now = Date.now();
    const accent = t.accent ?? "#3b82f6";

    const tab = (c) => {
      const tabPanes = all.filter((p) => p.tab === c.id);
      const p = lead(all, c.id);
      const where = folder(p?.cwd);
      const hasAcpAgent = tabPanes.some(isAcpPane);
      const hasTermAgent = tabPanes.some(isTermAgentPane);
      const agentPanes = tabPanes.filter(isAgentPane);
      const elapsed = this.tabElapsed(c, tabPanes, agentPanes, now);
      return own(h_flex())
        .id(`tm-${c.id}`)
        .flex_shrink(1)
        .w(260)
        .min_w(0)
        .h_full()
        .pl(5)
        .pr(12)
        .gap(7)
        .items_center()
        .rounded(13)
        .border_1()
        .cursor_pointer()
        .text_size(13)
        .when(c.active, (el) => el.bg(C.tab).border_color(C.tabEdge))
        .when(!c.active, (el) => el.border_color("#00000000").hover((s) => s.bg(C.hover)))
        .on_click(() => run("tab.select", { id: c.id }))
        .on_mouse_down("right", () =>
          openOverlay({ mod: "menu", kind: "popup", width: 200, props: { tab: "tab", target: { id: c.id, space: space.id } } }),
        )
        .child(tile(t, p))
        .child(
          h_flex()
            .flex_1()
            .min_w(0)
            .gap(6)
            .truncate()
            .child(div().flex_none().text_color(c.active ? C.text : "#c4c6ca").child(c.name))
            .when(Boolean(where), (el) => el.child(div().truncate().text_color(C.dim).child(where))),
        )
        .when(hasAcpAgent, (el) => el.child(
          div()
            .px(4.5)
            .h(16)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(C.chip)
            .border_1()
            .border_color(C.edge)
            .text_size(9.5)
            .font_semibold()
            .text_color(accent)
            .child("ACP"),
        ))
        .when(hasTermAgent, (el) => el.child(
          div()
            .px(4.5)
            .h(16)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(C.chip)
            .border_1()
            .border_color(C.edge)
            .text_size(9.5)
            .font_medium()
            .text_color(C.dim)
            .child(tr("CLI", "终端")),
        ))
        .when(Boolean(elapsed), (el) => el.child(
          h_flex()
            .id(`tm-time-${c.id}`)
            .px(4.5)
            .h(16)
            .gap(3)
            .flex_none()
            .items_center()
            .rounded(4)
            .bg(C.chip)
            .border_1()
            .border_color(C.edge)
            .text_size(9.5)
            .font_medium()
            .text_color(elapsed.working ? (t.working ?? accent) : C.dim)
            .child(svg("mods/workspace-chrome/clock.svg").size(9.5).flex_none())
            .child(formatElapsed(elapsed.ms)),
        ));
    };

    return h_flex()
      .size_full()
      .gap(8)
      .items_center()
      // macOS: the window buttons sit here (the layout's traffic_lights).
      .when(w.platform === "macos" && w.titlebar === "none" && !w.fullscreen, (el) => el.pl(72))
      .child(
        own(h_flex())
          .id("tm-space")
          .flex_none()
          .h(30)
          .pl(10)
          .pr(12)
          .gap(8)
          .items_center()
          .rounded_full()
          .bg(C.chip)
          .cursor_pointer()
          .text_size(13)
          .text_color(C.dim)
          .hover((s) => s.text_color(C.text))
          .on_click(() => openOverlay({ mod: "picker", kind: "popup", at: "top", offset: 40, width: 640, height: 0.8 }))
          .child(svg("mods/terminals-bar/stack.svg").size(15))
          .child(div().max_w(160).truncate().text_color(C.text).child(space.name)),
      )
      .child(
        h_flex()
          .flex_shrink(1)
          .min_w(0)
          .h(36)
          .p(3)
          .gap(3)
          .rounded(16)
          .bg(C.tray)
          .children(space.tabs.map(tab)),
      )
      .child(div().flex_1())
      .child(iconButton("tm-keys", "command", () => run("app.palette")))
      .child(iconButton("tm-new", "plus", () => run("tab.new", { space: space.id })))
      .when(Boolean(w.buttons), (el) =>
        el.child(
          h_flex()
            .flex_none()
            .pl(6)
            .pr(4)
            .gap(8)
            .child(windowButton("tm-min", "win-min", "window.minimize"))
            .child(windowButton("tm-max", w.maximized ? "win-restore" : "win-max", "window.maximize"))
            .child(windowButton("tm-close", "win-close", "window.close")),
        ),
      );
  }
}
