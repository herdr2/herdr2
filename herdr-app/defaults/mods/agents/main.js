// Agents in grouped or priority views; parents take their most urgent sub-agent's place.

import { View, div, image, svg } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { agents, terminals, panes, focused, spaces, run, h2, checking, theme, shared, setShared, setAgentOrder, openOverlay, closeOverlay, props, window, layout, settings } from "herdr";
import { resolveSemanticLabel, buildHoverTooltip, buildLocalFallbackBriefing, mergeBriefingSnapshots } from "./briefing_model.js";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function shortAgentId(addr) {
  const s = String(addr || "").trim();
  const idx = s.lastIndexOf("/");
  return idx >= 0 ? s.slice(idx + 1) : s;
}

function lineageBadgeInfo(rec) {
  const kind = String(rec?.lineage?.kind || "").toLowerCase();
  if (kind === "side-chat" || kind === "side_chat") {
    return { label: tr("Side", "旁路"), color: "#14b8a6", bg: "#14b8a622", border: "#14b8a655" };
  }
  if (kind === "fork") {
    return { label: tr("Fork", "分叉"), color: "#a855f7", bg: "#a855f722", border: "#a855f755" };
  }
  if (kind === "handoff") {
    return { label: tr("Handoff", "接力"), color: "#3b82f6", bg: "#3b82f622", border: "#3b82f655" };
  }
  return null;
}

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}

// The sidebar's rail goes by its width, not a flag: dragging its edge narrows it too.
const narrow = () => (find(layout(), "sidebar")?.size?.px ?? Infinity) < 160;

// To the rail (on macOS wide enough for the traffic lights), or back to the width it had.
function toggleSidebar() {
  const width = find(layout(), "sidebar")?.size?.px;
  if (!narrow()) setShared("sidebar.width", width);
  // Mods edited before the rail went by width still read the flag.
  setShared("sidebar.collapsed", !narrow());
  run("layout.resize", { id: "sidebar", px: narrow() ? (shared("sidebar.width") ?? 268) : window()?.platform === "macos" ? 76 : 44 });
}

// Drops what an agent puts before its title: a status glyph or spinner and the separator
// after it ("✦  Working", "⠋ - Thinking"), and Codex's blinking "[ ! ] " / "[ . ] " mark.
// Stops at an opening bracket or quote, so a title that starts with one ("[web] login") keeps it.
const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}\p{Ps}\p{Pi}]+/u, "").replace(/^\[ [!.] \] /, "").trim();

function leadPanes(all) {
  const leads = new Map();
  for (const p of all) {
    if (!p.terminal) continue;
    const current = leads.get(p.tab);
    // First agent, else first focused terminal, else first terminal in the tab.
    if (!current || (!current.program?.agent && (p.program?.agent || (!current.focused && p.focused)))) leads.set(p.tab, p);
  }
  return leads;
}

function title(tab, l) {
  const prog = l?.program?.name;
  const live = clean(l?.title);
  // clean() would strip a path's leading ~ or /: test the title with only its status glyph gone.
  const path = /^[~/]|^[A-Za-z]:\\/.test((l?.title ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
  const telling = live && live !== (l?.terminal ?? l?.address) && live !== prog && !path && !live.includes("@") && live.toLowerCase() !== l?.program?.key;
  return (tab.name === prog || !tab.named) && telling ? live : tab.name;
}
const RANK = { blocked: 4, done: 3, working: 2, idle: 1 };
// A sub-agent without a pane isn't in agents(): its raw word, error counting as
// blocked. It's never done: it reports to its parent, and the parent's done is the news.
const kidStatus = (k, listed) => {
  if (k.manual_status) return k.manual_status;
  const s = listed.get(k.address) ?? k.status;
  return s === "error" ? "blocked" : s === "done" ? "idle" : s;
};
const statusLabels = () => ({ idle: tr("Idle", "空闲"), working: tr("Working", "运行中"), blocked: tr("Needs you", "待确认"), done: tr("Done", "已完成") });
const statusItems = () => [
  { id: "idle", label: tr("Idle", "空闲") },
  { id: "working", label: tr("Working", "运行中") },
  { id: "blocked", label: tr("Needs you", "待确认") },
  { id: "done", label: tr("Done", "已完成") },
  { id: "auto", label: tr("Auto detect", "自动识别") },
];
const sorts = () => [
  { id: "priority", label: tr("Priority", "按优先级"), icon: "clock" },
  { id: "grouped", label: tr("Grouped by space", "按工作区分组"), icon: "layers" },
];
const sortOf = () => {
  const list = sorts();
  return list.find((s) => s.id === shared("agents.sort")) ?? list[0];
};
const sectionLabel = (r) => ({ 4: tr("Needs you", "待确认"), 3: tr("Done", "已完成"), 2: tr("Working", "运行中"), 1: tr("Idle", "空闲") }[r]);
const SECTION_STATUS = { 4: "blocked", 3: "done", 2: "working" };

function duration(ms) {
  const m = Math.floor(ms / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h` : `${Math.floor(h / 24)}d`;
}

// The agent's icon from the app's program table: a one-color glyph or a picture.
function icon(p, size, color) {
  if (!p?.icon) return null;
  return p.tint ? svg(p.icon).size(size).flex_none().text_color(color) : image(p.icon).w(size).h(size).flex_none();
}

const trashedList = () => {
  const v = typeof shared === "function" ? shared("agents.trash") : null;
  return Array.isArray(v) ? v : [];
};
const trashTitle = (it) => {
  const rawTitle = clean(it.title);
  const meaningfulTitle = rawTitle && rawTitle !== it.address && rawTitle !== it.program?.name ? rawTitle : null;
  return it.custom_name || it.name || (it.acp ? meaningfulTitle : null) || it.tab_name || it.program?.name || it.address;
};

export default class Agents extends View {
  init(_props, cx) {
    if (props()?.sortMenu || props()?.statusMenu || props()?.trashMenu) {
      this.at = 0;
      this.handle = cx.focus_handle?.();
      this.handle?.focus?.();
      return;
    }
    if (props()?.briefing) {
      this.handle = cx.focus_handle?.();
      this.handle?.focus?.();
      this.briefingLoading = false;
      this.coreBriefing = null;
      this.briefingError = "";
      this.workingExpanded = false;
      this.batchBusy = new Set();
      this.resolvedIds = new Set();
      this.replyInputs = new Map();
      this.replyTexts = new Map();
      const awayShared = typeof shared === "function" ? shared("briefing.away") : null;
      this.awayMinutes = Number(props()?.awayMinutes ?? awayShared?.away_minutes ?? 0) || null;
      if (awayShared && typeof setShared === "function") {
        setShared("briefing.away", null);
      }
      if (!(typeof checking === "function" && checking())) {
        this.refreshBriefing(cx);
      }
      return;
    }
    this.folded = new Set();
    // Each agent's last state and when it changed (ms), kept across restarts.
    try { this.changed = JSON.parse(localStorage.getItem("changed") ?? "{}"); } catch { this.changed = {}; }
    if (!this.changed || typeof this.changed !== "object" || Array.isArray(this.changed)) this.changed = {};
    try { this.pendingPins = JSON.parse(localStorage.getItem("pins") ?? "[]"); } catch { this.pendingPins = []; }
    if (!Array.isArray(this.pendingPins)) this.pendingPins = [];
    if (!this.pendingPins.length) localStorage.removeItem("pins");
    this.migratingPins = new Set();
    this.firstSeen = {};
    this.firstSeenSeq = 0;
    this.hovered = null;
    this.hoveredSub = null;
    this.digestInflight = new Set();
    this.digestLastFetch = new Map();
    this.hoverDigestTask = null;
    this.listFocus = cx?.focus_handle?.();
    cx.timer.every(30000, (cx) => cx.notify());
  }

  refreshBriefing(cx) {
    if (this.briefingLoading || typeof h2 !== "function") return;
    this.briefingLoading = true;
    this.briefingError = "";
    cx?.notify?.();
    cx?.spawn?.(async (acx) => {
      try {
        const args = ["ai", "briefing", "--json", "--lang", isZh() ? "zh-CN" : "en"];
        if (this.awayMinutes && this.awayMinutes > 0) {
          args.push("--away-minutes", String(this.awayMinutes));
        }
        const res = await h2(args);
        if (res && res.code === 0 && res.stdout) {
          this.coreBriefing = JSON.parse(res.stdout);
        }
      } catch (err) {
        this.briefingError = String(err?.message ?? err ?? "");
      }
      this.briefingLoading = false;
      acx?.notify?.();
    });
  }

  scheduleHoverDigest(addr, rec, cx) {
    this.hoverDigestTask?.cancel?.();
    this.hoverDigestTask = null;
    if (!addr || !rec || typeof h2 !== "function" || (typeof checking === "function" && checking())) return;
    if (this.digestInflight?.has(addr)) return;
    const now = Date.now();
    const last = this.digestLastFetch?.get(addr) ?? 0;
    if (rec.micro_summary && now - last < 30000) return;
    if (now - last < 15000) return;
    if (typeof cx?.spawn !== "function") return;
    this.hoverDigestTask = cx.spawn(async (acx) => {
      await acx.sleep(350);
      if (this.hovered !== addr && this.hoveredSub !== addr) return;
      this.digestInflight.add(addr);
      this.digestLastFetch.set(addr, Date.now());
      try {
        await h2(["ai", "summarize-agent", addr, "--json", "--lang", isZh() ? "zh-CN" : "en"]);
      } catch {}
      this.digestInflight.delete(addr);
      acx?.notify?.();
    });
  }

  openBriefing() {
    const away = typeof shared === "function" ? shared("briefing.away") : null;
    const mins = Number(away?.away_minutes ?? 0) || null;
    if (away && typeof setShared === "function") setShared("briefing.away", null);
    openOverlay({
      mod: "agents",
      kind: "dialog",
      key: "catchup-briefing",
      width: 740,
      height: 580,
      props: { briefing: true, ...(mins ? { awayMinutes: mins } : {}) },
    });
  }

  // Stamps agents whose state changed. Done and idle count as one: looking at a
  // finished agent turns done into idle, and looking isn't the agent doing anything.
  track(list, now) {
    let dirty = false;
    for (const a of list) {
      const s = a.status === "done" ? "idle" : a.status;
      if (this.changed[a.address]?.s === s) continue;
      this.changed[a.address] = { s, at: now };
      dirty = true;
    }
    if (!dirty) return;
    // Gone and old: the list alone is empty right after a start.
    const listed = new Set(list.map((a) => a.address));
    for (const [k, c] of Object.entries(this.changed)) if (!listed.has(k) && now - c.at > 30 * 86400000) delete this.changed[k];
    localStorage.setItem("changed", JSON.stringify(this.changed));
  }

  // Keep offline pins until their terminal returns, and keep each pending until
  // core echoes the label so a failed write doesn't erase the old pin.
  migratePins(all) {
    const listed = new Map(all.filter((t) => t.state === "live").map((t) => [t.address, t]));
    const pending = this.pendingPins.filter((address) => {
      const terminal = listed.get(address);
      if (!terminal) return true;
      if (terminal.labels?.["herdr.pinned"]) return false;
      if (!this.migratingPins.has(address)) {
        this.migratingPins.add(address);
        run("terminal.pin", { terminal: address, pinned: true });
      }
      return true;
    });
    if (pending.length === this.pendingPins.length) return;
    this.pendingPins = pending;
    if (pending.length) localStorage.setItem("pins", JSON.stringify(pending));
    else localStorage.removeItem("pins");
  }

  since(a) {
    return this.changed[a.address]?.at ?? 0;
  }

  rank(a) {
    return RANK[a.status] ?? 0;
  }

  sortMenu(t) {
    const on = sortOf().id;
    return v_flex()
      .w_full()
      .p(5)
      .bg(t.popover)
      .text_size(12.8)
      .child(div().px(8).pt(4).pb(6).text_size(11).text_color(t.dim).child(tr("Sort agents", "排序方式")))
      .children(sorts().map((s) => h_flex()
        .id(`sort-${s.id}`)
        .h(31)
        .px(8)
        .gap(9)
        .rounded(7)
        .items_center()
        .text_color(t.foreground)
        .hover((el) => el.bg(t.hover))
        .on_click(() => {
          setShared("agents.sort", s.id);
          closeOverlay(props().overlay);
        })
        .child(svg(`mods/workspace-chrome/${s.icon}.svg`).size(14).flex_none().text_color(t.muted))
        .child(div().flex_1().child(s.label))
        .when(s.id === on, (el) => el.child(svg("mods/workspace-chrome/check.svg").size(14).flex_none().text_color(t.foreground)))));
  }

  statusMenu(t) {
    const terminal = props()?.terminal;
    const ag = agents().find((x) => x.address === terminal);
    const term = terminals().find((x) => x.address === terminal);
    const manual = ag?.manual_status ?? term?.manual_status ?? null;
    const active = manual ?? "auto";
    const items = statusItems();
    const select = (id) => {
      if (terminal) run("agent.status", { terminal, status: id });
      closeOverlay(props()?.overlay);
    };
    const glyph = (it) => {
      if (it.id === "auto") return svg("mods/workspace-chrome/clock.svg").size(14).flex_none().text_color(t.muted);
      return div().w(14).flex_none().flex().items_center().justify_center().child(this.status(t, { status: it.id }, true));
    };
    return v_flex()
      .id("ag-status-menu")
      .track_focus(this.handle)
      .w_full()
      .p(5)
      .bg(t.popover)
      .text_size(12.8)
      .on_key_down((e, cx) => {
        if (e.key === "down") this.at = (this.at + 1) % items.length;
        else if (e.key === "up") this.at = (this.at + items.length - 1) % items.length;
        else if (e.key === "enter") select(items[this.at]?.id ?? "auto");
        else if (e.key === "escape") closeOverlay(props()?.overlay);
        else return;
        cx.notify();
      })
      .child(div().px(8).pt(4).pb(6).text_size(11).text_color(t.dim).child(tr("Agent status", "智能体状态")))
      .children(items.map((it, i) => {
        const picked = it.id === active;
        const hint = it.id === "auto" ? (manual ? "" : tr("Default", "默认")) : picked ? tr("Manual", "手动") : "";
        return v_flex()
          .when(it.id === "auto", (el) => el.child(div().mx(7).my(4).h(1).bg(t.border)))
          .child(
            h_flex()
              .id(`st-item-${it.id}`)
              .h(30)
              .px(8)
              .gap(9)
              .rounded(7)
              .items_center()
              .text_color(t.foreground)
              .cursor_pointer()
              .when(i === this.at, (el) => el.bg(t.selected ?? t.hover))
              .hover((el) => el.bg(t.hover))
              .on_click(() => select(it.id))
              .child(glyph(it))
              .child(div().flex_1().child(it.label))
              .when(Boolean(hint), (el) => el.child(div().text_size(10.5).text_color(t.dim).child(hint)))
              .when(picked, (el) => el.child(svg("mods/workspace-chrome/check.svg").size(13).flex_none().text_color(t.accent ?? t.foreground))),
          );
      }));
  }

  statusButton(t, addr, a, compact = false) {
    const label = statusLabels()[a.status] ?? tr("Idle", "空闲");
    const aiSummary = !a.manual_status && typeof a.ai_status_summary === "string" ? a.ai_status_summary.trim() : "";
    const aiKind = !a.manual_status && typeof a.ai_status_kind === "string" ? a.ai_status_kind.trim() : "";
    const aiTag = aiSummary
      ? ` · ${aiKind === "error" ? tr("Error", "异常") : "AI"}: ${aiSummary}`
      : "";
    const tip = tr(
      `Status: ${label}${a.manual_status ? " (manual)" : ""}${aiTag} — click to change`,
      `状态：${label}${a.manual_status ? "（手动）" : ""}${aiTag} — 点击更改`,
    );
    const active = this.hovered === addr || this.hoveredSub === addr || Boolean(a.manual_status);
    const btn = h_flex()
      .id(`status-${addr}`)
      .min_w(24)
      .h(22)
      .px(5)
      .rounded(5)
      .items_center()
      .justify_center()
      .flex_none()
      .when(active, (el) => el.bg(t.raised).border_1().border_color(a.manual_status ? (t.accent ?? t.border) : t.border))
      .cursor_pointer()
      .hover((s) => s.bg(t.raised).border_1().border_color(t.border))
      .tooltip(tip)
      .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
      .on_click((_e, cx) => {
        cx.stop_propagation();
        openOverlay({ mod: "agents", kind: "popup", width: 215, height: 220, props: { statusMenu: true, terminal: addr } });
      })
      .child(this.status(t, a, compact));
    return btn;
  }

  peek(addr) {
    run("terminal.peek", { terminal: addr });
  }

  selectAgent(addr) {
    if (addr && typeof setShared === "function") setShared("agents.selected", addr);
  }

  batchResolveCluster(group, approve, cx) {
    if (!group?.addresses?.length || this.batchBusy.has(group.clusterKey)) return;
    this.batchBusy.add(group.clusterKey);
    cx?.notify?.();
    cx?.spawn?.(async (acx) => {
      const cmd = approve ? "agent.approve" : "agent.deny";
      for (const addr of group.addresses) {
        try {
          run(cmd, { agent: addr });
        } catch {}
      }
      this.resolvedIds.add(`cluster:${group.clusterKey}`);
      for (const addr of group.addresses) this.resolvedIds.add(`agent:${addr}`);
      this.batchBusy.delete(group.clusterKey);
      acx?.notify?.();
      await acx.sleep(600);
      this.refreshBriefing(acx);
    });
  }

  resolveSinglePermission(item, approve, cx) {
    if (!item?.address) return;
    run(approve ? "agent.approve" : "agent.deny", { agent: item.address });
    this.resolvedIds.add(item.id);
    cx?.notify?.();
  }

  replyDroveQuestion(item, cx) {
    const text = String(this.replyTexts.get(item.id) ?? "").trim();
    if (!text || !item?.messageId || typeof h2 !== "function" || this.batchBusy.has(item.id)) return;
    this.batchBusy.add(item.id);
    cx?.notify?.();
    cx?.spawn?.(async (acx) => {
      try {
        const args = ["drove", "reply", item.messageId, text];
        if (item.machine) args.push("--on", item.machine);
        const r = await h2(args);
        if (r && r.code === 0) {
          this.resolvedIds.add(item.id);
          this.replyTexts.delete(item.id);
        }
      } catch {}
      this.batchBusy.delete(item.id);
      acx?.notify?.();
    });
  }

  focusBriefingTarget(item, openChanges = false) {
    const overlay = props()?.overlay;
    if (item?.address) {
      this.selectAgent(item.address);
      const p = panes().find((x) => x.terminal === item.address);
      if (p?.pane) run("pane.focus", { pane: p.pane });
      else if (item.kind?.startsWith("acp_")) run("terminal.peek", { terminal: item.address });
      else run("agent.focus", { terminal: item.address });
      if (openChanges) {
        run("layout.select", { stack: "panel-body", child: "changes" });
        run("layout.show", { id: "panel" });
      }
    } else if (item?.runId && typeof h2 === "function") {
      const args = ["drove", "web", "--run", item.runId];
      if (item.machine) args.push("--on", item.machine);
      h2(args).catch?.(() => {});
    }
    if (overlay) closeOverlay(overlay);
  }

  briefingView(t, cx) {
    const overlay = props()?.overlay;
    const localSnap = buildLocalFallbackBriefing({
      agents: agents(),
      terminals: terminals(),
      awayMinutes: this.awayMinutes,
      isZh: isZh(),
      now: Date.now(),
    });
    const snap = mergeBriefingSnapshots(localSnap, this.coreBriefing);
    const accent = t.accent ?? "#3b82f6";
    const blockedColor = t.blocked ?? "#f59e0b";
    const doneColor = t.done ?? "#22c55e";
    const workingColor = t.working ?? accent;
    const errColor = "#ef4444";

    const blockers = (snap.criticalBlockers ?? []).filter((x) => !this.resolvedIds.has(x.id));
    const clusters = (snap.permissionClusters ?? []).filter((g) => !this.resolvedIds.has(`cluster:${g.clusterKey}`));
    const decisions = (snap.pendingDecisions ?? []).filter((x) => !this.resolvedIds.has(x.id));
    const ready = (snap.readyToReview ?? []).filter((x) => !this.resolvedIds.has(x.id));
    const working = snap.workingItems ?? [];

    const pill = (text, color, bg) =>
      div()
        .px(6)
        .py(1.5)
        .flex_none()
        .rounded(999)
        .bg(bg ?? `${color}22`)
        .text_size(10.5)
        .font_semibold()
        .text_color(color)
        .child(text);

    const actionBtn = (id, label, onClick, { primary = false, danger = false, disabled = false } = {}) =>
      div()
        .id(id)
        .h(24)
        .px(9)
        .flex_none()
        .flex()
        .items_center()
        .rounded(6)
        .border_1()
        .border_color(primary ? accent : danger ? `${errColor}66` : t.border)
        .bg(primary ? `${accent}26` : danger ? `${errColor}1a` : t.raised)
        .text_size(11)
        .font_medium()
        .text_color(disabled ? t.dim : danger ? errColor : t.foreground)
        .when(!disabled, (el) => el.cursor_pointer().hover((s) => s.bg(t.hover)))
        .on_mouse_down("left", (_e, evCx) => evCx.stop_propagation())
        .on_click((_e, evCx) => {
          evCx.stop_propagation();
          if (!disabled) onClick(evCx);
        })
        .child(label);

    const sectionTitle = (id, titleText, count, color) =>
      h_flex()
        .id(id)
        .pt(6)
        .pb(2)
        .gap(6)
        .items_center()
        .child(div().size(7).rounded(4).bg(color))
        .child(div().text_size(11.5).font_semibold().text_color(t.foreground).child(titleText))
        .child(div().text_size(11).text_color(t.dim).child(String(count)));

    const renderItemCard = (item, badgeColor, buttons) =>
      h_flex()
        .id(`br-item-${item.id}`)
        .px(10)
        .py(8)
        .gap(10)
        .flex_none()
        .rounded(8)
        .bg(t.raised)
        .border_1()
        .border_color(t.border)
        .items_center()
        .child(
          v_flex()
            .flex_1()
            .min_w(0)
            .gap(3)
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .min_w(0)
                .child(div().truncate().text_size(12.5).font_semibold().text_color(t.foreground).child(item.title))
                .when(Boolean(item.address), (el) =>
                  el.child(div().px(4).py(0.5).rounded(4).bg(t.hover).text_size(9.5).text_color(t.dim).child(item.address)),
                )
                .when(Boolean(item.spaceName), (el) =>
                  el.child(div().truncate().max_w(120).text_size(10.5).text_color(t.dim).child(item.spaceName)),
                )
                .when((item.blockedDownstreamCount ?? 0) > 0, (el) =>
                  el.child(
                    pill(
                      tr(`Blocks +${item.blockedDownstreamCount} downstream`, `阻塞 +${item.blockedDownstreamCount} 下游`),
                      errColor,
                    ),
                  ),
                ),
            )
            .child(div().truncate().text_size(11.5).text_color(badgeColor ?? t.muted).child(item.summary)),
        )
        .child(h_flex().gap(5).flex_none().items_center().children(buttons));

    const renderClusterCard = (g) => {
      const busy = this.batchBusy.has(g.clusterKey);
      return v_flex()
        .id(`br-cluster-${g.clusterKey}`)
        .px(10)
        .py(9)
        .gap(7)
        .flex_none()
        .rounded(8)
        .bg(`${blockedColor}14`)
        .border_1()
        .border_color(`${blockedColor}55`)
        .child(
          h_flex()
            .gap(8)
            .items_center()
            .justify_between()
            .child(
              h_flex()
                .flex_1()
                .min_w(0)
                .gap(6)
                .items_center()
                .child(pill(tr(`Batch · ${g.addresses.length}`, `合并批复 · ${g.addresses.length}`), blockedColor))
                .child(div().truncate().text_size(12.5).font_semibold().text_color(t.foreground).child(g.title)),
            )
            .child(
              h_flex()
                .gap(5)
                .flex_none()
                .child(
                  actionBtn(
                    `br-batch-ok-${g.clusterKey}`,
                    busy ? "…" : tr(`Approve All (${g.addresses.length})`, `一键全批 (${g.addresses.length})`),
                    (evCx) => this.batchResolveCluster(g, true, evCx),
                    { primary: true, disabled: busy },
                  ),
                )
                .child(
                  actionBtn(
                    `br-batch-no-${g.clusterKey}`,
                    tr("Deny All", "全部拒绝"),
                    (evCx) => this.batchResolveCluster(g, false, evCx),
                    { danger: true, disabled: busy },
                  ),
                ),
            ),
        )
        .child(
          h_flex()
            .gap(5)
            .flex_wrap()
            .children(
              g.addresses.map((addr, idx) =>
                div()
                  .id(`br-cluster-chip-${g.clusterKey}-${addr}`)
                  .px(6)
                  .py(1.5)
                  .rounded(5)
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .text_size(10.5)
                  .text_color(t.muted)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .on_click(() => this.focusBriefingTarget({ address: addr, kind: "acp_permission" }))
                  .child(`${g.labels?.[idx] || addr} (${addr})`),
              ),
            ),
        );
    };

    const renderQuestionCard = (item) => {
      let state = this.replyInputs.get(item.id);
      if (!state) {
        state = InputState.new({ placeholder: tr("Type quick reply and press Enter…", "输入简短回复后按回车或点发送…") });
        state.on("change", (_e, icx) => {
          this.replyTexts.set(item.id, state.value());
          icx?.notify?.();
        });
        state.on("submit", (_e, icx) => this.replyDroveQuestion(item, icx));
        this.replyInputs.set(item.id, state);
      }
      const busy = this.batchBusy.has(item.id);
      return v_flex()
        .id(`br-q-${item.id}`)
        .px(10)
        .py(8)
        .gap(6)
        .flex_none()
        .rounded(8)
        .bg(t.raised)
        .border_1()
        .border_color(t.border)
        .child(
          h_flex()
            .gap(6)
            .items_center()
            .child(pill("Drove", accent))
            .child(div().flex_1().min_w(0).truncate().text_size(12.5).font_semibold().text_color(t.foreground).child(item.title))
            .when((item.blockedDownstreamCount ?? 0) > 0, (el) =>
              el.child(pill(tr(`Blocks +${item.blockedDownstreamCount}`, `阻塞 +${item.blockedDownstreamCount}`), errColor)),
            ),
        )
        .child(div().text_size(11.5).text_color(t.muted).child(item.summary))
        .child(
          h_flex()
            .gap(6)
            .items_center()
            .child(
              Input.new(state)
                .flex_1()
                .h(26)
                .px(8)
                .rounded(5)
                .border_1()
                .border_color(t.input)
                .bg(t.background)
                .text_size(11.5)
                .text_color(t.foreground),
            )
            .child(
              actionBtn(
                `br-q-send-${item.id}`,
                busy ? "…" : tr("Reply", "回复"),
                (evCx) => this.replyDroveQuestion(item, evCx),
                { primary: true, disabled: busy },
              ),
            ),
        );
    };

    const nothingUrgent = blockers.length === 0 && clusters.length === 0 && decisions.length === 0 && ready.length === 0;

    return v_flex()
      .id("ag-briefing-dialog")
      .when(Boolean(this.handle), (el) => el.track_focus(this.handle))
      .size_full()
      .min_h(0)
      .p(14)
      .gap(10)
      .bg(t.popover)
      .text_size(12.5)
      .on_key_down((e) => {
        if (e?.key === "escape" && overlay) closeOverlay(overlay);
      })
      // Header bar
      .child(
        h_flex()
          .pb(8)
          .gap(8)
          .flex_none()
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(8)
              .items_center()
              .child(svg("mods/workspace-chrome/layers.svg").size(15).text_color(accent))
              .child(div().text_size(14).font_semibold().text_color(t.foreground).child(tr("Catch-up Briefing", "全局进展速览")))
              .when(Boolean(this.awayMinutes), (el) =>
                el.child(pill(tr(`Away ${this.awayMinutes}m`, `离开 ${this.awayMinutes} 分钟`), accent)),
              )
              .when(Boolean(snap.headlineFromAi), (el) => el.child(pill("AI", doneColor))),
          )
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(
                actionBtn(
                  "br-refresh",
                  this.briefingLoading ? tr("Syncing…", "刷新中…") : tr("Refresh", "刷新"),
                  (evCx) => this.refreshBriefing(evCx),
                  { disabled: this.briefingLoading },
                ),
              )
              .child(
                actionBtn(
                  "br-mark-read",
                  tr("Mark Caught Up", "全部已阅"),
                  () => {
                    if (typeof setShared === "function") setShared("briefing.away", null);
                    if (overlay) closeOverlay(overlay);
                  },
                ),
              )
              .child(
                div()
                  .id("br-close")
                  .size(24)
                  .flex()
                  .items_center()
                  .justify_center()
                  .rounded(5)
                  .text_color(t.dim)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .on_click(() => overlay && closeOverlay(overlay))
                  .child(svg("mods/pane/close.svg").size(12)),
              ),
          ),
      )
      // Executive summary banner
      .child(
        v_flex()
          .px(12)
          .py(10)
          .gap(6)
          .flex_none()
          .rounded(8)
          .bg(t.raised)
          .border_1()
          .border_color(t.border)
          .child(div().text_size(12.5).text_color(t.foreground).child(snap.headline || tr("Workspace is quiet.", "当前工作区暂无待处理事项。")))
          .child(
            h_flex()
              .gap(6)
              .flex_wrap()
              .child(pill(tr(`${blockers.length} Critical`, `${blockers.length} 异常阻塞`), blockers.length > 0 ? errColor : t.dim))
              .child(pill(tr(`${clusters.reduce((s, g) => s + g.addresses.length, 0) + decisions.length} Needs You`, `${clusters.reduce((s, g) => s + g.addresses.length, 0) + decisions.length} 待拍板`), clusters.length + decisions.length > 0 ? blockedColor : t.dim))
              .child(pill(tr(`${ready.length} Ready Diff`, `${ready.length} 可验收`), ready.length > 0 ? doneColor : t.dim))
              .child(pill(tr(`${working.length} Working`, `${working.length} 进行中`), working.length > 0 ? workingColor : t.dim)),
          ),
      )
      // Scrollable body
      .child(
        v_flex()
          .id("br-scroll")
          .flex_1()
          .min_h(0)
          .gap(8)
          .overflow_y_scrollbar()
          .when(nothingUrgent && working.length === 0, (el) =>
            el.child(
              v_flex()
                .py(28)
                .items_center()
                .justify_center()
                .child(div().text_size(12.5).text_color(t.dim).child(tr("All clear! No pending approvals, blockers, or unread completions.", "一切就绪！当前没有待审批项、异常阻塞或未检视的完成项。"))),
            ),
          )
          // 1. Critical Blockers
          .when(blockers.length > 0, (el) =>
            el
              .child(sectionTitle("br-sec-crit", tr("Critical Blockers (Highest Impact First)", "阻塞大局（按影响下游数排序）"), blockers.length, errColor))
              .children(
                blockers.map((item) =>
                  renderItemCard(item, errColor, [
                    actionBtn(`br-go-${item.id}`, tr("Inspect", "立即排查"), () => this.focusBriefingTarget(item), { danger: true }),
                  ]),
                ),
              ),
          )
          // 2. Needs Your Decision (Batch Clusters + Single Items)
          .when(clusters.length > 0 || decisions.length > 0, (el) =>
            el
              .child(sectionTitle("br-sec-dec", tr("Needs Your Decision", "待拍板与审批"), clusters.reduce((s, g) => s + g.addresses.length, 0) + decisions.length, blockedColor))
              .children(clusters.map(renderClusterCard))
              .children(
                decisions.map((item) => {
                  if (item.kind === "drove_question") return renderQuestionCard(item);
                  const btns = item.kind === "acp_permission" && item.address
                    ? [
                        actionBtn(`br-ok-${item.id}`, tr("Approve", "允许"), (evCx) => this.resolveSinglePermission(item, true, evCx), { primary: true }),
                        actionBtn(`br-no-${item.id}`, tr("Deny", "拒绝"), (evCx) => this.resolveSinglePermission(item, false, evCx), { danger: true }),
                        actionBtn(`br-peek-${item.id}`, tr("Open", "查看"), () => this.focusBriefingTarget(item)),
                      ]
                    : [
                        actionBtn(`br-focus-${item.id}`, tr("Focus", "跳转处理"), () => this.focusBriefingTarget(item), { primary: true }),
                      ];
                  return renderItemCard(item, blockedColor, btns);
                }),
              ),
          )
          // 3. Ready to Review
          .when(ready.length > 0, (el) =>
            el
              .child(sectionTitle("br-sec-done", tr("Ready to Review", "已完成可验收"), ready.length, doneColor))
              .children(
                ready.map((item) =>
                  renderItemCard(item, t.muted, [
                    ...(item.address
                      ? [
                          actionBtn(`br-diff-${item.id}`, tr("Review Diff", "检视 Diff"), () => this.focusBriefingTarget(item, true), { primary: true }),
                          actionBtn(`br-open-${item.id}`, tr("Open", "打开"), () => this.focusBriefingTarget(item, false)),
                        ]
                      : [actionBtn(`br-open-${item.id}`, tr("Open Run", "查看任务"), () => this.focusBriefingTarget(item, false))]),
                  ]),
                ),
              ),
          )
          // 4. Working (collapsible)
          .when(working.length > 0, (el) =>
            el
              .child(
                h_flex()
                  .id("br-sec-work-toggle")
                  .pt(6)
                  .pb(2)
                  .gap(6)
                  .items_center()
                  .cursor_pointer()
                  .on_click((_e, evCx) => {
                    this.workingExpanded = !this.workingExpanded;
                    evCx?.notify?.();
                  })
                  .child(svg(`mods/workspace-chrome/chevron-${this.workingExpanded ? "down" : "right"}.svg`).size(12).text_color(t.dim))
                  .child(div().size(7).rounded(4).bg(workingColor))
                  .child(div().text_size(11.5).font_semibold().text_color(t.muted).child(tr("Working in Background", "后台推进中")))
                  .child(div().text_size(11).text_color(t.dim).child(String(working.length))),
              )
              .when(this.workingExpanded, (el2) =>
                el2.children(
                  working.map((item) =>
                    renderItemCard(item, t.dim, [
                      actionBtn(`br-w-open-${item.id}`, tr("Peek", "查看"), () => this.focusBriefingTarget(item)),
                    ]),
                  ),
                ),
              ),
          ),
      );
  }

  trashMenu(t) {
    const list = trashedList();
    const now = Date.now();
    const overlay = props()?.overlay;
    const accent = t.accent ?? "#3b82f6";
    const menuH = list.length === 0 ? 132 : Math.min(340, 52 + list.length * 54);
    return v_flex()
      .id("ag-trash-menu")
      .when(Boolean(this.handle), (el) => el.track_focus(this.handle))
      .w_full()
      .h(menuH)
      .min_h(0)
      .p(6)
      .gap(6)
      .bg(t.popover)
      .text_size(12.5)
      .on_key_down((e) => {
        if (e?.key === "escape" && overlay) closeOverlay(overlay);
      })
      .child(
        h_flex()
          .px(6)
          .pt(2)
          .pb(4)
          .flex_none()
          .items_center()
          .justify_between()
          .border_b(1)
          .border_color(t.border)
          .child(
            h_flex()
              .gap(6)
              .items_center()
              .child(svg("mods/workspace-chrome/archive.svg").size(14).flex_none().text_color(t.muted))
              .child(div().font_semibold().text_color(t.foreground).child(tr(`Recycle Bin (${list.length})`, `智能体回收站 (${list.length})`))),
          )
          .when(list.length > 0, (el) =>
            el.child(
              div()
                .id("trash-empty")
                .px(7)
                .py(3)
                .rounded(5)
                .text_size(11)
                .text_color(t.dim)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("agent.empty_trash", {});
                })
                .child(tr("Empty", "清空")),
            ),
          ),
      )
      .when(list.length === 0, (el) =>
        el.child(
          v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .px(16)
            .child(div().text_size(12).text_color(t.dim).child(tr("Recycle bin is empty. Deleted agents can be restored here.", "回收站为空，删除的智能体可在这里恢复。"))),
        ),
      )
      .when(list.length > 0, (el) =>
        el.child(
          v_flex()
            .id("trash-list")
            .flex_1()
            .min_h(0)
            .gap(4)
            .overflow_y_scrollbar()
            .children(
              list.map((it) => {
                const pic = icon(it.program, 15, t.foreground) ?? svg("mods/workspace-chrome/terminal.svg").size(14).flex_none().text_color(t.muted);
                const live = Boolean(it.live);
                const age = it.trashed_at ? duration(Math.max(0, now - it.trashed_at)) : "";
                const meta = [
                  it.space_name || it.machine || "",
                  live ? tr("Live · lossless", "会话存活 · 无损恢复") : tr("Ended · relaunch", "已退出 · 重新启动"),
                  age,
                ].filter(Boolean).join(" · ");
                return h_flex()
                  .id(`trash-row-${it.address}`)
                  .h(48)
                  .px(8)
                  .gap(8)
                  .flex_none()
                  .rounded(7)
                  .items_center()
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .child(pic)
                  .child(
                    v_flex()
                      .flex_1()
                      .min_w(0)
                      .gap(2)
                      .child(
                        h_flex()
                          .gap(5)
                          .items_center()
                          .min_w(0)
                          .child(div().truncate().text_size(12.5).font_medium().text_color(t.foreground).child(trashTitle(it)))
                          .child(
                            div()
                              .px(4)
                              .py(1)
                              .flex_none()
                              .rounded(4)
                              .bg(t.hover)
                              .text_size(9.5)
                              .text_color(t.dim)
                              .child(it.acp ? "ACP" : "CLI"),
                          ),
                      )
                      .child(div().truncate().text_size(10.5).text_color(live ? (t.done ?? t.muted) : t.dim).child(meta)),
                  )
                  .child(
                    h_flex()
                      .gap(4)
                      .flex_none()
                      .items_center()
                      .child(
                        div()
                          .id(`trash-restore-${it.address}`)
                          .px(8)
                          .h(22)
                          .flex()
                          .items_center()
                          .rounded(5)
                          .bg(t.selected ?? t.hover)
                          .border_1()
                          .border_color(accent)
                          .text_size(11)
                          .font_medium()
                          .text_color(t.foreground)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover))
                          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                          .on_click((_e, cx) => {
                            cx.stop_propagation();
                            run("agent.restore", { terminal: it.address });
                            if (overlay) closeOverlay(overlay);
                          })
                          .child(tr("Restore", "恢复")),
                      )
                      .child(
                        div()
                          .id(`trash-purge-${it.address}`)
                          .size(22)
                          .flex()
                          .items_center()
                          .justify_center()
                          .rounded(5)
                          .text_color(t.dim)
                          .cursor_pointer()
                          .hover((s) => s.bg(t.hover).text_color(t.foreground))
                          .tooltip(tr("Delete permanently", "彻底删除"))
                          .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                          .on_click((_e, cx) => {
                            cx.stop_propagation();
                            run("agent.purge", { terminal: it.address });
                          })
                          .child(svg("mods/pane/close.svg").size(11)),
                      ),
                  );
              }),
            ),
        ),
      );
  }

  // Live sub-agents of `addr`, depth first, each with the tree lines in front of it.
  // Handoff and Fork sessions are standalone top-level sessions even if legacy records carry `parent`.
  tree(all, addr, lead = []) {
    const kids = all
      .filter((k) => {
        if (k.parent !== addr || k.state !== "live" || k.labels?.["herdr.trashed"]) return false;
        const lk = String(k.lineage?.kind || "").toLowerCase().replace("_", "-");
        return lk !== "handoff" && lk !== "fork";
      })
      .sort((x, y) => {
        const tx = x.program_started_ms || x.created_ms || 0;
        const ty = y.program_started_ms || y.created_ms || 0;
        if (tx > 0 && ty > 0 && tx !== ty) return tx - ty;
        return 0;
      });
    return kids.flatMap((k, i) => {
      const last = i === kids.length - 1;
      return [
        { t: k, lines: [...lead, last ? "└" : "├"] },
        ...this.tree(all, k.address, [...lead, last ? "" : "│"]),
      ];
    });
  }

  subRow(t, s, focusedSet, compact) {
    const k = s.t;
    const isAcp = Boolean(k.acp);
    const isSideChat = String(k.lineage?.kind || "").toLowerCase().replace("_", "-") === "side-chat";
    const pane = this.paneByAddr?.get(k.address) ?? null;
    const canFocusPane = !isSideChat && (Boolean(pane) || (!isAcp && this.listed.has(k.address)));
    const st = kidStatus(k, this.listed);
    const pic = icon(k.program, 14, t.foreground);
    const on = focusedSet instanceof Set ? focusedSet.has(k.address) : k.address === focusedSet;
    const lBadge = lineageBadgeInfo(k);
    const hasWt = Boolean(k.lineage?.worktree_path);
    const subLabel = k.labels?.["herdr.name"] || k.labels?.["drove.display"] || k.name || k.semantic_title || (isAcp && clean(k.title) && clean(k.title) !== k.address && clean(k.title) !== k.program?.name ? clean(k.title) : null) || k.template || k.program?.name || k.address;
    const subShortId = shortAgentId(k.address);
    const showSubId = Boolean(subShortId) && subLabel !== subShortId && subLabel !== k.address;
    const mono = (typeof settings === "function" ? settings()?.font_family : null) ?? "JetBrains Mono";
    const subHoverTip = buildHoverTooltip({ ...k, status: st }, k, {
      isZh: isZh(),
      now: Date.now(),
      since: this.since(k),
      displayLabel: subLabel,
    });
    return h_flex()
      .id(`sub-${k.address}`)
      .pl(39)
      .pr(8)
      .rounded(8)
      .h(26)
      .flex_none()
      .gap(0)
      .items_center()
      .text_size(12.5)
      .cursor_pointer()
      .when(on, (el) => el.bg(t.selected))
      .hover((s) => s.bg(t.hover))
      .tooltip(subHoverTip)
      .on_hover((on, cx) => {
        if (on) {
          this.hoveredSub = k.address;
          this.scheduleHoverDigest(k.address, k, cx);
        } else if (this.hoveredSub === k.address) {
          this.hoveredSub = null;
        }
        cx.notify();
      })
      .on_mouse_down("right", (_e, cx) => {
        cx.stop_propagation();
        this.selectAgent(k.address);
        openOverlay({ mod: "menu", kind: "popup", width: k.acp ? 280 : 220, props: { tab: "agent", target: { terminal: k.address, pane } } });
      })
      .on_click(() => {
        this.selectAgent(k.address);
        canFocusPane ? run("pane.focus", pane ? { pane } : { terminal: k.address }) : this.peek(k.address);
      })
      .children(s.lines.map((l, idx) => div().self_start().mt(-4).w(9).h(17).mr(4).flex_none()
        .when(Boolean(l), el => el.border_l(1.5).border_color(on && idx === s.lines.length - 1 ? (t.accent ?? t.foreground) : t.input))
        .when(l === "└" || l === "├", el => el.border_b(1.5).rounded_bl(6))))
      .when(Boolean(pic), el => el.child(div().mr(8).child(pic)))
      .child(
        h_flex()
          .flex_1()
          .min_w(0)
          .gap(4)
          .items_center()
          .child(div().min_w(0).truncate().when(on, (el) => el.font_medium()).text_color(on ? t.foreground : t.muted).child(subLabel))
          .when(showSubId, (el) =>
            el.child(
              div()
                .id(`sub-id-${k.address}`)
                .flex_none()
                .px(3.5)
                .py(0.5)
                .rounded(4)
                .bg(t.raised)
                .border_1()
                .border_color(t.border)
                .font_family(mono)
                .text_size(9.5)
                .text_color(t.dim)
                .tooltip(tr(`Agent ID: ${k.address}`, `智能体编号：${k.address}`))
                .child(subShortId),
            ),
          ),
      )
      .when(Boolean(lBadge), (el) =>
        el.child(
          div()
            .id(`sub-kind-${k.address}`)
            .flex_none()
            .mr(4)
            .px(4)
            .py(1)
            .rounded(4)
            .bg(lBadge.bg)
            .border_1()
            .border_color(lBadge.border)
            .text_size(9.5)
            .font_semibold()
            .text_color(lBadge.color)
            .child(lBadge.label),
        ),
      )
      .when(hasWt, (el) =>
        el.child(
          div()
            .id(`sub-wt-${k.address}`)
            .flex_none()
            .mr(4)
            .px(4)
            .py(1)
            .rounded(4)
            .bg("#f59e0b22")
            .border_1()
            .border_color("#f59e0b55")
            .text_size(9.5)
            .font_semibold()
            .text_color("#f59e0b")
            .tooltip(tr(`Isolated Worktree: ${k.lineage.worktree_path}`, `独立 Worktree：${k.lineage.worktree_path}`))
            .child("WT"),
        ),
      )
      .child(
        div()
          .id(`sub-rt-${k.address}`)
          .flex_none()
          .mr(4)
          .px(4)
          .py(1)
          .rounded(4)
          .bg(t.hover)
          .text_size(9.5)
          .text_color(t.dim)
          .child(isAcp ? "ACP" : "CLI"),
      )
      .when(this.hoveredSub === k.address, (el) =>
        el.child(
          div()
            .id(`rm-sub-${k.address}`)
            .mr(4)
            .flex_none()
            .p(2)
            .rounded(4)
            .text_color(t.dim)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover).text_color(t.foreground))
            .tooltip(tr("Move to recycle bin", "移入回收站"))
            .on_click((_e, cx) => {
              cx.stop_propagation();
              this.hoveredSub = null;
              run("agent.trash", { terminal: k.address });
              cx.notify();
            })
            .child(svg("mods/pane/close.svg").size(11)),
        ),
      )
      .child(this.statusButton(t, k.address, {
        status: st,
        manual_status: k.manual_status,
        ai_status_summary: k.ai_status_summary,
        ai_status_kind: k.ai_status_kind,
      }, compact));
  }

  // compact: a ! for blocked where a header already says "Needs you".
  status(t, a, compact = false) {
    const color = t[a.status] ?? t.dim;
    if (a.status === "working") return svg("mods/workspace-chrome/spinner.svg").size(12).mx(1).flex_none().text_color(t.working).spin(900);
    if (a.status === "blocked" && compact) return div().size(14).flex_none().rounded(4).bg(t.blocked + "26").text_size(10.5).font_bold().text_color(t.blocked).flex().items_center().justify_center().child("!");
    if (a.status === "blocked") {
      const aiSummary = !a.manual_status && typeof a.ai_status_summary === "string" ? a.ai_status_summary.trim() : "";
      const isErr = !a.manual_status && a.ai_status_kind === "error";
      const badgeLabel = aiSummary || (isErr ? tr("Error", "异常") : tr("Needs you", "待确认"));
      return div().flex_none().max_w(130).truncate().px(6).h(18).rounded(5).bg(t.blocked + "26").text_size(10.5).font_semibold().text_color(t.blocked).flex().items_center().child(badgeLabel);
    }
    // Idle: a ring; done: a dot.
    if (a.status === "idle") return div().size(7).mx(3).flex_none().rounded(4).border(1.5).border_color(t.idle ?? t.dim);
    return div().size(7).mx(3).flex_none().rounded(4).bg(a.status === "done" ? color : t.dim + "00");
  }

  render(cx) {
    const t = theme();
    if (props()?.sortMenu) return this.sortMenu(t);
    if (props()?.statusMenu) return this.statusMenu(t);
    if (props()?.trashMenu) return this.trashMenu(t);
    if (props()?.briefing) return this.briefingView(t, cx);
    const trashed = trashedList();
    const collapsed = narrow();
    const sort = sortOf();
    let list = agents();
    this.listed = new Map(list.map((a) => [a.address, a.status]));
    const all = terminals();
    this.migratePins(all);
    // Read the pane snapshot once: every host read copies the whole list into JS.
    const allPanes = panes();
    this.paneByAddr = new Map(allPanes.filter((p) => p.terminal && p.pane).map((p) => [p.terminal, p.pane]));
    const focusedAddrs = new Set();
    for (const a of list) {
      if (a.focused) focusedAddrs.add(a.address);
    }
    const foc = focused();
    const activePaneAddr = allPanes.find((p) => p.focused)?.terminal ?? null;
    if (foc?.shown) focusedAddrs.add(foc.shown);
    else if (foc?.terminal) focusedAddrs.add(foc.terminal);
    else if (activePaneAddr) focusedAddrs.add(activePaneAddr);
    else if (foc?.agent) focusedAddrs.add(foc.agent);
    const now = Date.now();
    this.track(list, now);
    // A kept sub-agent (one with a pane) is in agents() too: where a tree shows
    // it, it gets no row of its own, unless it's pinned.
    const nested = new Set(collapsed ? [] : list.flatMap((a) => this.tree(all, a.address).map((s) => s.t.address)));
    list = list.filter((a) => a.pinned || !nested.has(a.address));
    const tabCounts = new Map();
    for (const a of list) tabCounts.set(a.tab, (tabCounts.get(a.tab) ?? 0) + 1);
    const byPriority = sort.id === "priority";
    const allByAddr = new Map(all.map((x) => [x.address, x]));
    const indexOf = new Map(list.map((a, i) => [a.address, i]));
    this.firstSeen ??= {};
    this.firstSeenSeq ??= 0;
    for (const a of list) {
      if (!(a.address in this.firstSeen)) this.firstSeen[a.address] = ++this.firstSeenSeq;
    }
    const appearedAt = (a) => {
      const rec = allByAddr.get(a.address);
      const ts = a.created_ms || rec?.program_started_ms || rec?.created_ms || 0;
      return ts > 0 ? ts : (this.firstSeen[a.address] ?? 0);
    };
    const byAppearance = (x, y) => appearedAt(x) - appearedAt(y) || (indexOf.get(x.address) ?? 0) - (indexOf.get(y.address) ?? 0);
    const urgency = new Map(list.map((a) => [a.address, Math.max(this.rank(a), ...this.tree(all, a.address).map((s) => RANK[kidStatus(s.t, this.listed)] ?? 0))]));
    const rankOf = (a) => urgency.get(a.address);
    const pinned = list.filter((a) => a.pinned).sort(byAppearance);
    let rest = list.filter((a) => !a.pinned);
    if (byPriority) {
      rest = [...rest].sort((x, y) => rankOf(y) - rankOf(x) || byAppearance(x, y));
    } else {
      // Group by space in space order, keeping agents within each space in appearance order (oldest first).
      const bySpace = new Map();
      for (const a of rest) {
        if (!bySpace.has(a.space)) bySpace.set(a.space, []);
        bySpace.get(a.space).push(a);
      }
      rest = [];
      for (const members of bySpace.values()) {
        members.sort(byAppearance);
        rest.push(...members);
      }
    }
    list = [...pinned, ...rest];
    setAgentOrder(list.map((a) => a.address));

    const withKids = (a, i) => {
      const subs = collapsed ? [] : this.tree(all, a.address);
      const r = row(a, i, subs);
      if (this.folded.has(a.address)) return [r];
      return [r, ...subs.map((s) => this.subRow(t, s, focusedAddrs, byPriority))];
    };

    // The fold chevron; folded, the count too, colored by the most urgent one.
    const fold = (a, subs) => {
      const sts = subs.map((s) => kidStatus(s.t, this.listed));
      const top = sts.reduce((m, s) => ((RANK[s] ?? 0) > (RANK[m] ?? 0) ? s : m), null);
      const open = !this.folded.has(a.address);
      const hasFocusedSub = !open && subs.some((s) => focusedAddrs.has(s.t.address));
      return div()
        .id(`fold-${a.address}`)
        .h(18)
        .px(3)
        .gap(3)
        .flex()
        .items_center()
        .rounded(4)
        .text_size(11)
        .when(hasFocusedSub, (el) => el.bg(t.selected))
        .text_color(hasFocusedSub ? t.foreground : open ? t.dim : (t[top] ?? t.dim))
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .on_click((_e, cx) => {
          cx.stop_propagation();
          open ? this.folded.add(a.address) : this.folded.delete(a.address);
          cx.notify();
        })
        .child(svg(`mods/workspace-chrome/chevron-${open ? "down" : "right"}.svg`).size(11).flex_none())
        .when(!open, (el) => el.child(String(subs.length)));
    };

    // A row reads "space \u00b7 detail": the detail is a tab someone named or the
    // agent's own title, never an automatic tab number unless two rows would
    // otherwise read the same.
    const leads = leadPanes(allPanes);
    const tabs = new Map(spaces().flatMap(s => s.tabs).map(tab => [tab.id, tab]));
    const labels = new Map(list.map(a => {
      const rec = allByAddr.get(a.address);
      const tab = tabs.get(a.tab);
      const many = tabCounts.get(a.tab) > 1;
      return [a.address, resolveSemanticLabel(a, rec, tab, leads.get(a.tab), { manyInTab: many })];
    }));
    const label = (a) => labels.get(a.address);
    const detail = new Map(list.map(a => [a.address, /^\d+$/.test(label(a)) ? null : label(a)]));
    const seen = {};
    for (const a of list) { const k = `${a.space_name}\u0000${detail.get(a.address) ?? ""}`; seen[k] = (seen[k] ?? 0) + 1; }
    for (const a of list) {
      if (detail.get(a.address) == null && seen[`${a.space_name}\u0000`] > 1) detail.set(a.address, label(a));
    }

    const row = (a, i, subs = []) => {
      const pic = icon(a.program, 16, t.foreground);
      const more = detail.get(a.address);
      const rowTerm = all.find((x) => x.address === a.address);
      const isSideChat = String(rowTerm?.lineage?.kind || "").toLowerCase().replace("_", "-") === "side-chat";
      // An agent on another machine (`vps/t7`) says which.
      const machine = a.address.includes("/") ? a.address.split("/")[0] : null;
      if (collapsed) {
        return h_flex()
          .id(`ag-${a.address}`)
          .h(26)
          .mx(6)
          .px(2)
          .rounded(6)
          .flex_none()
          .items_center()
          .gap(4)
          .when(a.focused, (el) => el.bg(t.selected))
          .hover((s) => s.bg(t.hover))
          .on_mouse_down("right", (_e, cx) => {
            cx.stop_propagation();
            this.selectAgent(a.address);
            openOverlay({ mod: "menu", kind: "popup", width: a.acp ? 280 : 220, props: { tab: "agent", target: { terminal: a.address, pane: a.pane ?? null } } });
          })
          .on_click(() => {
            this.selectAgent(a.address);
            if (isSideChat) this.peek(a.address);
            else if (a.pane) run("pane.focus", { pane: a.pane });
            else if (a.acp) this.peek(a.address);
            else run("terminal.keep", { terminal: a.address, place: "tab" });
          })
          .child(div().flex_1().min_w(0).whitespace_nowrap().text_right().text_color(a.focused ? t.foreground : t.muted).child(String(i + 1)))
          .child(h_flex().w(18).flex_none().items_center().justify_center().child(this.statusButton(t, a.address, a, true)));
      }
      const when = this.since(a);
      const idle = a.status === "idle" && !a.focused;
      const isPinned = a.pinned;
      const pin = div()
        .id(`pin-${a.address}`)
        .flex_none()
        .p(2)
        .rounded(4)
        .text_color(t.dim)
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .tooltip(isPinned ? tr("Unpin", "取消置顶") : tr("Pin to the top", "置顶"))
        .on_click((_e, cx) => {
          cx.stop_propagation();
          run("terminal.pin", { terminal: a.address, pinned: !isPinned });
          // The row moves away from under the pointer, and no hover-out comes.
          this.hovered = null;
          cx.notify();
        })
        .child(svg(`mods/workspace-chrome/${isPinned ? "pin-off" : "pin"}.svg`).size(13));
      const remove = div()
        .id(`rm-${a.address}`)
        .flex_none()
        .p(2)
        .rounded(4)
        .text_color(t.dim)
        .cursor_pointer()
        .hover((s) => s.bg(t.hover).text_color(t.foreground))
        .tooltip(tr("Move to recycle bin", "移入回收站"))
        .on_click((_e, cx) => {
          cx.stop_propagation();
          this.hovered = null;
          run("agent.trash", { terminal: a.address });
          cx.notify();
        })
        .child(svg("mods/pane/close.svg").size(12));
      // Under a space header the space goes without saying; an automatic tab number says nothing.
      // Without a space header above (priority, pinned) the row names its space.
      const full = byPriority || isPinned;
      const name = full ? a.space_name || label(a) : more ?? (/^\d+$/.test(label(a)) ? a.program?.name || label(a) : label(a));
      const shortId = shortAgentId(a.address);
      const showId = Boolean(shortId) && name !== shortId && name !== a.address;
      const mono = (typeof settings === "function" ? settings()?.font_family : null) ?? "JetBrains Mono";
      const rowHoverTip = buildHoverTooltip(a, rowTerm, {
        isZh: isZh(),
        now,
        since: when,
        displayLabel: more || name,
      });
      return h_flex()
        .id(`ag-${a.address}`)
        .px(8)
        .h(30)
        .flex_none()
        .rounded(8)
        .items_center()
        .gap(9)
        .when(a.focused, (el) => el.bg(t.selected))
        .cursor_pointer()
        .hover((s) => s.bg(t.hover))
        .tooltip(rowHoverTip)
        .on_hover((on, cx) => {
          if (on) {
            this.hovered = a.address;
            this.scheduleHoverDigest(a.address, rowTerm ?? a, cx);
          } else if (this.hovered === a.address) {
            this.hovered = null;
          }
          cx.notify();
        })
        .on_mouse_down("right", (_e, cx) => {
          cx.stop_propagation();
          this.selectAgent(a.address);
          openOverlay({ mod: "menu", kind: "popup", width: a.acp ? 280 : 220, props: { tab: "agent", target: { terminal: a.address, pane: a.pane ?? null } } });
        })
        .on_click(() => {
          this.selectAgent(a.address);
          if (isSideChat) this.peek(a.address);
          else if (a.pane) run("pane.focus", { pane: a.pane });
          else if (a.acp) this.peek(a.address);
          else run("terminal.keep", { terminal: a.address, place: "tab" });
        })
        .when(Boolean(pic), el => el.child(div().id(`agent-fold-${a.address}`)
          .when(subs.length > 0, el => el.on_click((_e, cx) => {
            cx.stop_propagation();
            this.folded.has(a.address) ? this.folded.delete(a.address) : this.folded.add(a.address);
            cx.notify();
          })).child(pic)))
        .child(h_flex().flex_1().min_w(0).gap(5).items_center()
          .child(div().min_w(0).max_w(full && more ? 120 : 200).truncate().text_color(idle ? t.muted : t.foreground).child(name))
          .when(showId, (el) =>
            el.child(
              div()
                .id(`ag-id-${a.address}`)
                .flex_none()
                .px(3.5)
                .py(0.5)
                .rounded(4)
                .bg(t.raised)
                .border_1()
                .border_color(t.border)
                .font_family(mono)
                .text_size(9.5)
                .text_color(t.dim)
                .tooltip(tr(`Agent ID: ${a.address}`, `智能体编号：${a.address}`))
                .child(shortId),
            ),
          )
          .when(full && Boolean(more), el => el.child(div().flex_1().min_w(0).truncate().text_color(t.dim).child(`\u00b7 ${more}`))))
        .when(Boolean(lineageBadgeInfo(all.find((x) => x.address === a.address))), (el) => {
          const rec = all.find((x) => x.address === a.address);
          const lb = lineageBadgeInfo(rec);
          const rawParent = String(rec?.lineage?.parent || "").trim();
          const slash = typeof a.address === "string" ? a.address.lastIndexOf("/") : -1;
          const prefix = slash >= 0 ? a.address.slice(0, slash + 1) : "";
          const parentAddr = rawParent ? (rawParent.includes("/") ? rawParent : `${prefix}${rawParent}`) : "";
          const parentRec = parentAddr ? all.find((x) => x.address === parentAddr) : null;
          const shortParent = parentAddr
            ? (parentRec?.labels?.["herdr.name"]
                || parentRec?.name
                || (prefix && parentAddr.startsWith(prefix) ? parentAddr.slice(prefix.length) : parentAddr))
            : "";
          const badgeText = shortParent ? `${lb.label} ← ${shortParent}` : lb.label;
          return el.child(
            div()
              .id(`ag-kind-${a.address}`)
              .flex_none()
              .max_w(130)
              .truncate()
              .px(4)
              .py(1)
              .rounded(4)
              .bg(lb.bg)
              .border_1()
              .border_color(lb.border)
              .text_size(9.5)
              .font_semibold()
              .text_color(lb.color)
              .when(Boolean(parentAddr), (b) =>
                b
                  .cursor_pointer()
                  .tooltip(tr(`From ${parentAddr} (click to focus)`, `来自 ${parentAddr}（点击定位上级）`))
                  .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                  .on_click((_e, cx) => {
                    cx.stop_propagation();
                    run("agent.focus", { terminal: parentAddr });
                  }),
              )
              .child(badgeText),
          );
        })
        .when(Boolean(all.find((x) => x.address === a.address)?.lineage?.worktree_path), (el) => {
          const wtPath = all.find((x) => x.address === a.address)?.lineage?.worktree_path;
          return el.child(
            div()
              .id(`ag-wt-${a.address}`)
              .flex_none()
              .px(4)
              .py(1)
              .rounded(4)
              .bg("#f59e0b22")
              .border_1()
              .border_color("#f59e0b55")
              .text_size(9.5)
              .font_semibold()
              .text_color("#f59e0b")
              .tooltip(tr(`Isolated Worktree: ${wtPath}`, `独立 Worktree：${wtPath}`))
              .child("WT"),
          );
        })
        .child(
          div()
            .id(`ag-rt-${a.address}`)
            .flex_none()
            .px(4)
            .py(1)
            .rounded(4)
            .bg(t.hover)
            .text_size(9.5)
            .text_color(t.dim)
            .child(a.acp ? "ACP" : "CLI"),
        )
        .when(Boolean(machine), el => el.child(div().text_size(10).text_color(t.dim).child(machine)))
        .when(subs.length > 0, el => el.child(fold(a, subs)))
        .when(this.hovered === a.address, el => el.child(pin).child(remove))
        .when(this.hovered !== a.address && Boolean(when), el => el.child(div().flex_none().text_size(11).text_color(t.dim).child(duration(now - when))))
        .child((() => {
          const liveRec = all.find((x) => x.address === a.address);
          const merged = liveRec
            ? {
                ...a,
                ai_status_summary: a.ai_status_summary ?? liveRec.ai_status_summary,
                ai_status_kind: a.ai_status_kind ?? liveRec.ai_status_kind,
              }
            : a;
          return this.statusButton(t, a.address, merged, byPriority && !isPinned);
        })());
    };

    // Groups in list order: spaces (grouped) or priority sections. Agents of a
    // space are next to each other in agents(), so neighbours make a group.
    const groups = pinned.length ? [{ id: "pinned", label: tr("Pinned", "已置顶"), status: null, members: pinned }] : [];
    for (const a of rest) {
      const r = rankOf(a) || RANK.idle;
      const id = byPriority ? `sec-${r}` : `sp-${a.space}`;
      if (groups.at(-1)?.id === id) groups.at(-1).members.push(a);
      else groups.push({ id, label: byPriority ? sectionLabel(r) : a.space_name || "", status: byPriority ? SECTION_STATUS[r] : null, members: [a] });
    }

    // A group's header; it folds the group. Folded, it says how many and the most urgent state.
    const header = (g) => {
      const open = !this.folded.has(g.id);
      const top = g.members.reduce((m, a) => ((RANK[a.status] ?? 0) > (RANK[m] ?? 0) ? a.status : m), null);
      return h_flex()
        .id(g.id)
        .px(8)
        .pt(8)
        .h(30)
        .flex_none()
        .gap(6)
        .items_center()
        .text_size(11)
        .font_medium()
        .cursor_pointer()
        .hover((s) => s.text_color(t.foreground))
        .on_click((_e, cx) => {
          open ? this.folded.add(g.id) : this.folded.delete(g.id);
          cx.notify();
        })
        .child(div().truncate().text_color(t[g.status] ?? t.dim).child(g.label))
        .when(byPriority || !open, (el) => el.child(div().text_color(t.dim).child(String(g.members.length))))
        .child(div().flex_1())
        .when(!open, (el) => el
          .when(!byPriority && top && top !== "idle", (el) => el.child(this.status(t, { status: top }, true)))
          .child(svg("mods/workspace-chrome/chevron-right.svg").size(11).flex_none().text_color(t.dim)));
    };
    const rows = collapsed
      ? list.flatMap(withKids)
      : groups.flatMap((g) => [header(g), ...(this.folded.has(g.id) ? [] : g.members.flatMap(withKids))]);

    const awayBanner = typeof shared === "function" ? shared("briefing.away") : null;
    const awayChangedCount = Number(awayBanner?.changed_count ?? 0) || 0;
    const urgentCount = list.filter((a) => {
      const r = rankOf(a);
      return r >= RANK.done;
    }).length;
    const briefingBadgeCount = awayChangedCount > 0 ? awayChangedCount : urgentCount;
    const briefingHot = awayChangedCount > 0 || list.some((a) => rankOf(a) >= RANK.blocked);

    return v_flex()
      .size_full()
      .min_h(0)
      .bg(t.chrome)
      .border_t(1)
      .border_color(t.border)
      .text_size(13)
      .when(Boolean(this.listFocus), (el) => el.track_focus(this.listFocus))
      .on_key_down((e, cx) => {
        if (e?.key === "f2") {
          const sel = (typeof shared === "function" ? shared("agents.selected") : null) || focused()?.agent;
          if (sel) {
            cx?.stop_propagation?.();
            run("agent.rename", { terminal: sel });
          }
        }
      })
      .when(!collapsed, (el) =>
        el.child(
          h_flex()
            .px(16)
            .h(41)
            .pt(14)
            .pb(5)
            .flex_none()
            .items_center()
            .justify_between()
            .child(div().text_size(11.5).font_medium().text_color(t.dim).child(tr("Agents", "智能体")))
            .child(
              h_flex()
                .gap(6)
                .items_center()
                .child(
                  h_flex()
                    .id("ag-briefing")
                    .h(20)
                    .px(briefingBadgeCount > 0 ? 6 : 5)
                    .gap(4)
                    .items_center()
                    .rounded(5)
                    .text_size(10.5)
                    .when(briefingHot, (b) => b.bg(`${t.blocked ?? "#f59e0b"}24`).border_1().border_color(`${t.blocked ?? "#f59e0b"}66`).text_color(t.blocked ?? "#f59e0b"))
                    .when(!briefingHot, (b) => b.text_color(briefingBadgeCount > 0 ? t.foreground : t.dim))
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click(() => this.openBriefing())
                    .tooltip(
                      awayChangedCount > 0
                        ? tr(`Catch-up Briefing (${awayChangedCount} updates while away)`, `离开重连简报（离开期间 ${awayChangedCount} 项新动态）`)
                        : tr("Catch-up Briefing: priority blockers, batch approvals & diffs", "全局进展速览：阻塞排序、同类审批一键合批、完成项验收"),
                    )
                    .child(svg("mods/workspace-chrome/layers.svg").size(12))
                    .child(div().font_medium().child(tr("Brief", "速览")))
                    .when(briefingBadgeCount > 0, (b) => b.child(div().font_semibold().child(String(briefingBadgeCount)))),
                )
                .child(
                  h_flex()
                    .id("ag-recycle")
                    .h(20)
                    .px(trashed.length > 0 ? 5 : 3)
                    .gap(4)
                    .items_center()
                    .rounded(5)
                    .text_size(10.5)
                    .text_color(trashed.length > 0 ? t.muted : t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 360, height: 340, props: { trashMenu: true } }))
                    .tooltip(trashed.length > 0 ? tr(`Recycle bin (${trashed.length})`, `回收站 (${trashed.length})`) : tr("Recycle bin", "回收站"))
                    .child(svg("mods/workspace-chrome/archive.svg").size(13))
                    .when(trashed.length > 0, (el) => el.child(div().font_semibold().text_color(t.foreground).child(String(trashed.length)))),
                )
                .child(
                  div()
                    .id("ag-sort")
                    .text_size(11)
                    .text_color(t.dim)
                    .hover((s) => s.text_color(t.foreground))
                    .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 200, height: 140, props: { sortMenu: true } }))
                    .tooltip(tr(`Sort agents: ${sort.label}`, `排序：${sort.label}`))
                    .child(svg(`mods/workspace-chrome/${sort.icon}.svg`).size(14)),
                ),
            ),
        ),
      )
      .when(list.length === 0 && !collapsed, (el) =>
        el.child(
          v_flex()
            .px(12)
            .gap(8)
            .child(div().text_size(12).text_color(t.dim).child(tr("No active agents yet. Headless ACP agents and interactive coding CLIs appear here automatically.", "暂无活跃智能体。后台 ACP 智能体和交互式终端 CLI 会自动显示在这里。")))
            .when(trashed.length > 0, (el2) =>
              el2.child(
                h_flex()
                  .id("ag-recycle-empty-btn")
                  .px(8)
                  .h(26)
                  .gap(6)
                  .items_center()
                  .rounded(6)
                  .bg(t.raised)
                  .border_1()
                  .border_color(t.border)
                  .text_size(11.5)
                  .text_color(t.foreground)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover))
                  .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 360, height: 340, props: { trashMenu: true } }))
                  .child(svg("mods/workspace-chrome/archive.svg").size(13).text_color(t.muted))
                  .child(tr(`Recycle bin (${trashed.length} recoverable)`, `打开回收站（${trashed.length} 个可恢复）`)),
              ),
            ),
        ),
      )
      .child(v_flex().id("ag-list").flex_1().min_h(0).px(collapsed ? 0 : 8).overflow_y_scrollbar().mode("always").children(rows))
      .when(collapsed, (el) =>
        el.child(
          v_flex()
            .items_center()
            .gap(4)
            .pb(4)
            .child(
              div()
                .id("ag-briefing-rail")
                .p(4)
                .rounded(5)
                .text_color(briefingHot ? (t.blocked ?? "#f59e0b") : t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .tooltip(tr("Catch-up Briefing", "全局进展速览"))
                .on_click(() => this.openBriefing())
                .child(svg("mods/workspace-chrome/layers.svg").size(13)),
            )
            .when(trashed.length > 0, (el2) =>
              el2.child(
                div()
                  .id("ag-recycle-rail")
                  .p(4)
                  .rounded(5)
                  .text_color(t.muted)
                  .cursor_pointer()
                  .hover((s) => s.bg(t.hover).text_color(t.foreground))
                  .tooltip(tr(`Recycle bin (${trashed.length})`, `回收站 (${trashed.length})`))
                  .on_click(() => openOverlay({ mod: "agents", kind: "popup", width: 360, height: 340, props: { trashMenu: true } }))
                  .child(svg("mods/workspace-chrome/archive.svg").size(13)),
              ),
            )
            .child(
              h_flex()
                .h(24)
                .items_center()
                .justify_center()
                .child(
                  div()
                    .id("collapse")
                    .text_size(13)
                    .text_color(t.dim)
                    .hover((s) => s.text_color(t.foreground))
                    .on_click(toggleSidebar)
                    .child("»"),
                ),
            ),
        ),
      );
  }
}
