import { View, div, svg, image } from "gpui-kit";
import { h_flex, ScrollHandle } from "gpui-base";
import { spaces, panes, run, openOverlay, theme, window, shared, layout, settings } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const zoomScale = () => Math.max(0.25, Number(typeof settings === "function" ? settings()?.zoom : 1) || 1);

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}

// Same as the agents mod: drops a status glyph, keeps a leading bracket or quote.
const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}\p{Ps}\p{Pi}]+/u, "").replace(/^\[ [!.] \] /, "").trim();

// A tab Herdr named (unnamed, or after its program) follows its agent's live
// title, as the agents list does; a name the person gave stays.
function label(tab, lead) {
  const prog = lead?.program?.name;
  const base = tab.named ? tab.name : prog ?? "Terminal";
  if (!lead?.program?.agent) return base;
  const live = clean(lead.title);
  // clean() would strip a path's leading ~ or /: test the title with only its status glyph gone.
  const path = /^[~/]|^[A-Za-z]:\\/.test((lead.title ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
  const telling = live && live !== lead.terminal && live !== prog && !path && !live.includes("@") && live.toLowerCase() !== lead.program.key;
  return (tab.name === prog || !tab.named) && telling ? live : base;
}

const TAB_GAP = 3;

export default class Tabs extends View {
  init() {
    this.widths = {};
    this.drag = null;
    this.settling = null;
    this.settleTask = null;
    this.snapFromOrder = null;
    this.snapUntil = 0;
    this.hoveredTab = null;
    this.scroll = ScrollHandle.new();
    try { this.panels = JSON.parse(localStorage.getItem("panels") ?? "{}"); } catch { this.panels = {}; }
    if (!this.panels || typeof this.panels !== "object" || Array.isArray(this.panels)) this.panels = {};
    this.swipe = { acc: 0, steps: 0, at: 0, last: 0 };
  }

  tabWidth(id) {
    return this.widths[id] ?? 136;
  }

  slotLeft(shown, index) {
    let x = 0;
    for (let k = 0; k < index; k++) x += this.tabWidth(shown[k].id) + TAB_GAP;
    return x;
  }

  slotOffset(shown, from, to) {
    let off = 0;
    if (to > from) {
      for (let j = from + 1; j <= to; j++) off += this.tabWidth(shown[j].id) + TAB_GAP;
    } else if (to < from) {
      for (let j = to; j < from; j++) off -= this.tabWidth(shown[j].id) + TAB_GAP;
    }
    return off;
  }

  computeDragState(shown, from, startLeft, dragWidth, dx) {
    let to = from;
    let mergeTarget = null;
    const center = startLeft + dx + dragWidth * 0.5;
    if (dx > 0) {
      for (let j = from + 1; j < shown.length; j++) {
        const left = this.slotLeft(shown, j);
        const w = this.tabWidth(shown[j].id);
        const right = left + w;
        if (center > right - w * 0.2) {
          to = j;
        } else {
          if (center >= left + w * 0.2) mergeTarget = shown[j].id;
          break;
        }
      }
    } else if (dx < 0) {
      for (let j = from - 1; j >= 0; j--) {
        const left = this.slotLeft(shown, j);
        const w = this.tabWidth(shown[j].id);
        const right = left + w;
        if (center < left + w * 0.2) {
          to = j;
        } else {
          if (center <= right - w * 0.2) mergeTarget = shown[j].id;
          break;
        }
      }
    }
    return { to, mergeTarget };
  }

  clampDragDx(shown, from, dragWidth, dx) {
    let minDx = 0;
    for (let j = 0; j < from; j++) minDx -= this.tabWidth(shown[j].id) + TAB_GAP;
    let maxDx = 0;
    for (let j = from + 1; j < shown.length; j++) maxDx += this.tabWidth(shown[j].id) + TAB_GAP;
    const extra = Math.max(48, Math.round((dragWidth || 136) * 0.58));
    return Math.max(minDx - extra, Math.min(maxDx + extra, dx));
  }

  flushSettle() {
    this.settleTask?.cancel?.();
    this.settleTask = null;
    const s = this.settling;
    if (!s) return;
    this.settling = null;
    if (s.moveArgs) {
      this.snapFromOrder = s.orderSig;
      this.snapUntil = Date.now() + 200;
      run("tab.move", s.moveArgs);
    }
    if (s.activate) run("tab.select", { id: s.id });
  }

  handleDragMove(e, space, shown, cx) {
    if (!this.drag) return;
    if (e && "pressed_button" in e && e.pressed_button !== undefined && e.pressed_button !== "left") {
      this.finishDrag(space, shown, cx);
      return;
    }
    const rawX = e?.position?.x;
    if (typeof rawX !== "number") return;
    const rawDx = rawX / zoomScale() - this.drag.startX;
    if (!this.drag.moving && Math.abs(rawDx) < 4) return;
    if (!this.drag.moving) {
      this.drag.moving = true;
    }
    this.drag.dx = this.clampDragDx(shown, this.drag.from, this.drag.dragWidth, rawDx);
    const state = this.computeDragState(shown, this.drag.from, this.drag.startLeft, this.drag.dragWidth, this.drag.dx);
    this.drag.to = state.to;
    this.drag.mergeTarget = state.mergeTarget;
    cx?.notify?.();
  }

  finishDrag(space, shown, cx) {
    if (!this.drag) return;
    const d = this.drag;
    this.drag = null;
    if (!d.moving) {
      run("tab.select", { id: d.id });
      cx?.notify?.();
      return;
    }
    if (d.mergeTarget && d.mergeTarget !== d.id) {
      run("tab.merge", { source: d.id, target: d.mergeTarget, direction: "right" });
      cx?.notify?.();
      return;
    }
    const destDx = this.slotOffset(shown, d.from, d.to);
    let moveArgs = null;
    if (d.to !== d.from && space && shown[d.to]) {
      const targetId = shown[d.to].id;
      const rawIdx = (space.tabs ?? []).findIndex((x) => x.id === targetId);
      if (rawIdx >= 0) moveArgs = { id: d.id, index: rawIdx + 1 };
    }
    const orderSig = shown.map((x) => x.id).join(",");
    const activate = !shown[d.from]?.active;
    this.settling = {
      id: d.id,
      from: d.from,
      to: d.to,
      dragWidth: d.dragWidth,
      startLeft: d.startLeft,
      destDx,
      orderSig,
      moveArgs,
      activate,
    };
    if (typeof cx?.spawn === "function") {
      this.settleTask = cx.spawn(async (acx) => {
        await acx.sleep(170);
        this.settleTask = null;
        this.flushSettle();
        acx.notify();
      });
    } else {
      this.flushSettle();
    }
    cx?.notify?.();
  }

  // One tab per wheel notch. A touchpad or smooth-scrolling mouse sends many
  // small events per gesture: a short swipe or one smoothed notch (about
  // 25 px) switches one tab, a longer swipe keeps going a tab per 60 px, at
  // most one every 100 ms so each shows.
  wheel(e) {
    if (e.delta_lines) {
      if (e.delta_lines.y) run(e.delta_lines.y > 0 ? "tab.prev" : "tab.next");
      return;
    }
    const s = this.swipe, now = Date.now();
    // Phases often arrive only as moves (a macOS touchpad included): a pause
    // or a turn starts a new gesture.
    if (e.touch_phase === "started" || now - s.at > 250 || e.delta.y * s.acc < 0) Object.assign(s, { acc: 0, steps: 0 });
    s.at = now;
    s.acc += e.delta.y;
    const need = s.steps ? 60 : 20;
    if (Math.abs(s.acc) < need || now - s.last < 100) return;
    s.acc -= Math.sign(s.acc) * need;
    s.steps++;
    s.last = now;
    run(e.delta.y > 0 ? "tab.prev" : "tab.next");
  }

  // The side panel is remembered per space: switching shows that space's choice
  // (closed if it never had one), and opening or closing it records it.
  syncPanel(space, panel, body, enabled) {
    if (!space || !panel) return;
    const now = panel.hidden ? null : body?.active ?? null;
    if (space.id !== this.space) {
      const first = this.space === undefined;
      this.space = space.id; this.pending = null;
      if (!(first && !(space.id in this.panels))) {
        const stored = this.panels[space.id] ?? null, want = enabled.includes(stored) ? stored : null;
        if (want !== now) {
          this.pending = { want, until: Date.now() + 1000 };
          if (want) { run("layout.select", { stack: "panel-body", child: want }); run("layout.show", { id: "panel" }); }
          else run("layout.hide", { id: "panel" });
          return;
        }
      }
    }
    // Until the restore lands (or something else won), the old state isn't a choice.
    if (this.pending) {
      if (this.pending.want !== now && Date.now() < this.pending.until) return;
      this.pending = null;
    }
    if (this.panels[space.id] === now) return;
    this.panels[space.id] = now;
    localStorage.setItem("panels", JSON.stringify(this.panels));
  }

  render() {
    const t = theme();
    const space = spaces().find(p => p.active);
    const shown = space?.tabs ?? [];
    const orderSig = shown.map((x) => x.id).join(",");
    let snapNeighbors = false;
    if (this.snapFromOrder) {
      if (orderSig !== this.snapFromOrder) {
        snapNeighbors = true;
        this.snapFromOrder = null;
      } else if (Date.now() > this.snapUntil) {
        this.snapFromOrder = null;
      }
    }
    if (this.drag && !shown.some((x) => x.id === this.drag.id)) {
      this.drag = null;
    }
    if (this.settling && !shown.some((x) => x.id === this.settling.id)) {
      this.settleTask?.cancel?.();
      this.settleTask = null;
      this.settling = null;
    }
    const active = shown.findIndex(tab => tab.active);
    const selected = shown[active]?.id;
    if ((selected !== this.selected || active !== this.selectedIndex) && !this.drag && !this.settling) {
      this.selected = selected;
      this.selectedIndex = active;
      if (active >= 0) this.scroll.scroll_to_item(active);
    }
    const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);
    const stop = (_e, cx) => cx.stop_propagation();
    const button = (id, label, action) => div().id(id).size(26).flex_none().flex().items_center().justify_center().rounded(6).text_color(t.dim).hover(s => s.bg(t.hover).text_color(t.foreground))
      .on_mouse_down("left", stop).on_click(action).child(label);
    const motion = this.drag?.moving ? this.drag : this.settling;
    const renderCard = (c, showClose, { isFloating = false, width = null, interactive = true, isMergeTarget = false, showMergeBadge = false } = {}) => {
      const tabPanes = panes().filter(p => p.tab === c.id);
      const lead = tabPanes.find(p => p.program?.agent) ?? tabPanes[0];
      const allAcp = tabPanes.length > 0 && tabPanes.every(p => p.acp);
      const isAcpTab = Boolean(lead?.acp) || allAcp;
      const isStructuredTermTab = !isAcpTab && Boolean(lead?.structured);
      const isTermAgentTab = !isAcpTab && !isStructuredTermTab && Boolean(lead?.program?.agent);
      const program = lead?.program;
      const name = label(c, lead);
      const accent = t.accent ?? "#3b82f6";
      let el = h_flex()
        .id(isFloating ? `tab-float-card-${c.id}` : `tab-${c.id}`)
        .h(29)
        .px(10)
        .gap(6)
        .rounded(7)
        .items_center()
        .flex_none()
        .text_size(12.8)
        .when(Boolean(isFloating && width > 0), node => node.w(width))
        .border(1)
        .border_color(isFloating || isMergeTarget ? accent : (c.active ? t.border : t.chrome))
        .bg(isMergeTarget ? (alpha(accent, "28") ?? t.selected) : (c.active || isFloating ? t.raised : t.chrome))
        .text_color(c.active || isFloating || isMergeTarget ? t.foreground : t.muted)
        .when(isFloating, node => node.shadow_md().cursor_grabbing())
        .when(!isFloating && interactive, node => node.hover(s => s.bg(t.hover)))
        .when(Boolean(program?.icon), node => node.child(program.tint ? svg(program.icon).size(14).text_color(t.muted) : image(program.icon).size(14)))
        .child(div().max_w(160).truncate().child(c.zoomed ? `${name} Z` : name))
        .when(isFloating && showMergeBadge, node => node.child(
          div()
            .id("tab-merge-badge")
            .px(5)
            .h(16)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(accent)
            .text_size(9.5)
            .font_semibold()
            .text_color(t.accent_foreground ?? "#ffffff")
            .child(tr("◫ Split", "◫ 分栏")),
        ))
        .when(isAcpTab, node => node.child(
          div()
            .px(4.5)
            .h(15)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha(accent, "22") ?? t.raised)
            .border_1()
            .border_color(alpha(accent, "66") ?? t.border)
            .text_size(9.5)
            .font_semibold()
            .text_color(accent)
            .tooltip(tr("ACP Agent", "ACP 智能体"))
            .child("ACP"),
        ))
        .when(isStructuredTermTab, node => node.child(
          div()
            .px(4.5)
            .h(15)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha(accent, "22") ?? t.raised)
            .border_1()
            .border_color(alpha(accent, "66") ?? t.border)
            .text_size(9.5)
            .font_semibold()
            .text_color(accent)
            .tooltip(tr("Terminal Agent (Structured View)", "终端智能体（结构化视图）"))
            .child(tr("CLI", "终端")),
        ))
        .when(isTermAgentTab, node => node.child(
          div()
            .px(4.5)
            .h(15)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_size(9.5)
            .font_medium()
            .text_color(t.dim)
            .tooltip(tr("Terminal Agent", "终端智能体"))
            .child(tr("CLI", "终端")),
        ))
        .when(c.status === "working", node => node.child(svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)))
        .when(c.status === "blocked" || c.status === "done", node => node.child(div().size(6).rounded(3).bg(t[c.status])))
        .when(!isFloating && showClose, node => node.child(
          div()
            .id(`close-${c.id}`)
            .text_color(t.dim)
            .hover(s => s.text_color(t.foreground))
            .tooltip(allAcp && tabPanes.some(p => p.status !== "ended") ? tr("Move to background", "转入后台") : tr("Close tab", "关闭标签页"))
            .on_mouse_down("left", stop)
            .on_click((_e, cx) => { cx.stop_propagation(); run("tab.close", { id: c.id }); })
            .child(svg("mods/pane/close.svg").size(12)),
        ));
      return el;
    };
    const item = (c, i) => {
      const isMergeTarget = Boolean(this.drag?.moving && this.drag.mergeTarget === c.id);
      const showClose = (c.active || this.hoveredTab === c.id) && !(motion && motion.from === i) && !this.drag?.moving;
      const isHiddenSource = Boolean(motion && motion.from === i);
      let shift = 0;
      if (motion && i !== motion.from) {
        if (motion.from < motion.to && i > motion.from && i <= motion.to) {
          shift = -(motion.dragWidth + TAB_GAP);
        } else if (motion.from > motion.to && i >= motion.to && i < motion.from) {
          shift = motion.dragWidth + TAB_GAP;
        }
      }
      return renderCard(c, showClose, { interactive: !isHiddenSource, isMergeTarget })
        .relative()
        .left(shift)
        .spring("left", { response: snapNeighbors || isHiddenSource ? 0 : 165, damping: 0.86, epsilon: 0.25 })
        .when(isHiddenSource, el => el.opacity(0))
        // A rename or status dot on this or an earlier tab can push the active one out.
        .on_size_change((size, cx) => {
          const z = zoomScale();
          const w = size && typeof size.width === "number" ? size.width / z : 0;
          let changed = false;
          if (w > 0 && Math.abs((this.widths[c.id] ?? 0) - w) > 0.5) {
            this.widths[c.id] = w;
            changed = true;
          }
          if (i <= active && !this.drag && !this.settling) {
            this.scroll.scroll_to_item(active);
            changed = true;
          }
          if (changed) cx.notify();
        })
        .on_hover((hovered, cx) => {
          if (this.drag?.moving) return;
          const next = hovered ? c.id : (this.hoveredTab === c.id ? null : this.hoveredTab);
          if (next !== this.hoveredTab) {
            this.hoveredTab = next;
            cx.notify();
          }
        })
        .on_mouse_down("left", (e, cx) => {
          cx.stop_propagation();
          this.flushSettle();
          if (e?.click_count === 2) {
            this.drag = null;
            run("tab.rename", { id: c.id });
            return;
          }
          const z = zoomScale();
          const rawW = e?.bounds?.width;
          const w = (typeof rawW === "number" && rawW > 0 ? rawW / z : this.widths[c.id]) ?? 136;
          if (w > 0) this.widths[c.id] = w;
          this.drag = {
            id: c.id,
            from: i,
            to: i,
            mergeTarget: null,
            startX: (e?.position?.x ?? 0) / z,
            startLeft: this.slotLeft(shown, i),
            dx: 0,
            moving: false,
            dragWidth: w,
          };
        })
        .on_mouse_up("left", (_e, cx) => {
          cx.stop_propagation();
          this.finishDrag(space, shown, cx);
        })
        .on_mouse_down("right", (_e, cx) => {
          cx.stop_propagation();
          const hasAcp = panes().some(p => p.tab === c.id && p.acp);
          openOverlay({ mod: "menu", kind: "popup", width: hasAcp ? 280 : 200, props: { tab: "tab", target: { id: c.id, space: space.id } } });
        });
    };
    const root = layout(), sidebar = find(root, "sidebar"), w = window();
    // Old saved rails and a dragged-narrow sidebar can be smaller than the window buttons.
    const lightsPadding = w?.platform === "macos" && w.titlebar === "none" && !w.fullscreen && root?.traffic_lights
      ? Math.max(10, (root.traffic_lights[0] + 64) / settings().zoom - (sidebar?.hidden ? 0 : sidebar?.size?.px ?? 0))
      : 10;
    const panel = find(root, "panel"), body = find(root, "panel-body");
    const enabled = shared("workspace.panels") ?? ["files", "changes"];
    this.syncPanel(space, panel, body, enabled);
    const panelButton = (id, label, icon) => {
      const on = panel && !panel.hidden && body?.active === id;
      return h_flex().id(`panel-${id}`).h(26).px(8).gap(5).flex_none().items_center().rounded(6)
        .text_size(12).bg(on ? t.selected : t.chrome).text_color(on ? t.foreground : t.muted)
        .hover(s => s.bg(t.hover)).on_mouse_down("left", stop)
        .on_click(() => {
          if (on) run("layout.hide", { id: "panel" });
          else { run("layout.select", { stack: "panel-body", child: id }); run("layout.show", { id: "panel" }); }
        }).child(svg(icon).size(14)).child(label);
    };
    const floatingTab = motion && shown[motion.from]
      ? div()
          .id(`tab-float-${motion.id}`)
          .absolute()
          .top(0)
          .bottom(0)
          .left(motion.startLeft + (this.drag?.moving ? this.drag.dx : motion.destDx))
          .spring("left", { response: this.drag?.moving ? 0 : 160, damping: 0.88, epsilon: 0.25 })
          .flex()
          .items_center()
          .child(renderCard(shown[motion.from], false, {
          isFloating: true,
          width: motion.dragWidth,
          showMergeBadge: Boolean(this.drag?.moving && this.drag.mergeTarget),
        }))
      : null;
    return h_flex().relative().size_full().min_w(0).px(10).pl(lightsPadding).gap(3).items_center().bg(t.chrome).border_b(1).border_color(t.border)
      .on_mouse_move((e, cx) => this.handleDragMove(e, space, shown, cx))
      .on_mouse_up("left", (_e, cx) => this.finishDrag(space, shown, cx))
      .on_scroll_wheel(e => this.wheel(e))
      .child(h_flex().id("tab-scroll").relative().flex_1().min_w(0).h_full().items_center().gap(TAB_GAP).overflow_x_scrollbar().track_scroll(this.scroll)
        .on_size_change((_size, cx) => { if (active >= 0 && !this.drag && !this.settling) { this.scroll.scroll_to_item(active); cx.notify(); } })
        .children(shown.map(item))
        .when(Boolean(floatingTab), el => el.child(floatingTab)))
      .child(button("tab-new", "+", () => openOverlay({ mod: "new-menu", kind: "popup", width: 250, height: 512 })))
      .when(enabled.includes("files"), el => el.child(panelButton("files", tr("Files", "文件"), "mods/files/file.svg")))
      .when(enabled.includes("changes"), el => el.child(panelButton("changes", tr("Changes", "变更"), "mods/changes/changes.svg")))
      .when(w?.buttons, el => el.child(button("minimize", w.platform === "windows" ? svg("mods/tabs/win-min.svg").size(16) : "−", () => run("window.minimize")))
        .child(button("maximize", w.platform === "windows" ? svg(`mods/tabs/${w.maximized ? "win-restore" : "win-max"}.svg`).size(16) : "□", () => run("window.maximize")))
        .child(button("close-window", w.platform === "windows" ? svg("mods/tabs/win-close.svg").size(16) : svg("mods/pane/close.svg").size(13), () => run("window.close"))))
      .when(Boolean(this.drag?.moving), el => el.child(
        div()
          .id("tab-drag-shield")
          .absolute()
          .inset(0)
          .bg("#00000000")
          .cursor_grabbing()
          .on_mouse_move((e, cx) => this.handleDragMove(e, space, shown, cx))
          .on_mouse_up("left", (_e, cx) => { cx.stop_propagation(); this.finishDrag(space, shown, cx); }),
      ));
  }
}
