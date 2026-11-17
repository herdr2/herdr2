// A pane's header, for layouts that frame their panes:
//   { "tab": "active", "pane": { "header": { "content": { "mod": "pane" } } } }
// Shows the program's icon (the app's program table), the agent's name or
// the terminal's title, the folder, and the agent's status; buttons split, zoom and close
// this pane. Props from the app: { pane, terminal }. Click: focus the pane.

import { View, div, svg, image } from "gpui-kit";
import { h_flex } from "gpui-base";
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

export default class Pane extends View {
  init() { this.hovered = false; }

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
    const term = p.terminal ? terminals().find((x) => x.address === p.terminal) : null;
    const parent = term?.parent ? terminals().find((x) => x.address === term.parent) : null;
    const tabPaneCount = panes().filter((x) => x.tab === p.tab).length;
    const singleStructuredTab = isStructured && tabPaneCount <= 1;
    const customName = term?.labels?.["herdr.name"];
    const name = isStructured
      ? (customName || term?.name || term?.template || p.program?.name || p.title || p.terminal || "Agent")
      : (customName || ((p.program?.agent || p.program?.shell) && p.program.name) || p.title || p.program?.name || "shell");
    const pic = icon(p.program, 14, t.dim);
    const st = p.status;
    const pane = p.pane;
    const usage = p.context_usage ?? term?.context_usage ?? null;
    const hasUsage = isAcp && usage && typeof usage.size === "number" && usage.size > 0;
    const pct = hasUsage ? Math.min(100, Math.round(((usage.used ?? 0) / usage.size) * 100)) : 0;
    const pctColor = pct >= 85 ? (t.blocked ?? "#ef4444") : pct >= 65 ? (t.done ?? "#f59e0b") : t.muted;
    const bottom = paneFrame(layout())?.header?.position === "bottom";
    return h_flex()
      .id(`pane-head-${pane}`)
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
      .when(!singleStructuredTab, (el) => el.child(div().flex_none().truncate().max_w(220).font_medium().text_color(t.foreground).child(name)))
      .when(isAcp && !singleStructuredTab, el => el.child(div().flex_none().px(5).py(1).rounded(4).bg(t.raised).border_1().border_color(t.border).text_size(10).text_color(t.muted).child("ACP")))
      .when(isStructured && Boolean(p.terminal), el => el.child(div().flex_none().text_size(10.5).text_color(t.dim).child(p.terminal)))
      .when(isAcp && Boolean(parent), el => el.child(div().flex_none().text_size(11).text_color(t.muted).child(tr(`from ${parent.name || parent.address}`, `来自 ${parent.name || parent.address}`))))
      .child(div().flex_1().min_w(0).truncate().text_color(t.dim).child(folder(p.cwd)))
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
