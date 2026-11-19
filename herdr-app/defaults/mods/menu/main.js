// Context menus (Herdr's items). Opened as a popup with
// props { tab: "pane" | "tab" | "space" | "global", target }.
// ↑/↓ move, Enter runs, Esc closes. An item is [label, command, args];
// a command "worktree" opens the worktree dialog instead.

import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex } from "gpui-base";
import { run, props, closeOverlay, openOverlay, theme, keys, terminals, spaces, panes, exec, settings, h2 } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function acpPermItem(term) {
  if (!term?.acp || term?.state === "finished") return null;
  const isClaude = term?.agent === "claude"
    || term?.program?.key === "claude"
    || (Array.isArray(term?.argv) && String(term.argv[0] ?? "").toLowerCase().includes("claude"));
  const isBypass = term?.mode === "bypassPermissions"
    || term?.mode === "dangerously-skip-permissions"
    || term?.permission_mode === "approve-all";
  return [
    isBypass
      ? tr("✓ dangerously-skip-permissions (Reset)", "✓ dangerously-skip-permissions（恢复默认）")
      : tr("Permissions: dangerously-skip-permissions", "权限：dangerously-skip-permissions"),
    "acp.config",
    {
      terminal: term.address,
      id: isClaude ? "mode" : "permissions",
      value: isBypass ? (isClaude ? "default" : "approve-reads") : (isClaude ? "bypassPermissions" : "dangerously-skip-permissions"),
    },
  ];
}

// Where a space is now: its first terminal's live folder, else where it was made.
function place(s) {
  const tabs = new Set((s.tabs ?? []).map((c) => c.id));
  return panes().find((p) => tabs.has(p.tab) && p.cwd)?.cwd || s.folder;
}

// A space's checkout: "main" (a repo's own), "linked" (a worktree), or null.
async function checkout(folder, on) {
  const top = await exec("git", ["rev-parse", "--show-toplevel"], { cwd: folder, on });
  const common = await exec("git", ["rev-parse", "--path-format=absolute", "--git-common-dir"], { cwd: folder, on });
  if (top.code !== 0 || common.code !== 0) return null;
  return common.stdout.trim() === `${top.stdout.trim()}/.git` ? "main" : "linked";
}

function items(p, git) {
  p = p ?? {};
  const tg = p.target ?? {};
  switch (p.tab) {
    case "pane": {
      // A kept sub-agent can go back under its parent, still running.
      const all = terminals();
      const term = all.find((t) => t.address === tg.terminal);
      const paneInfo = panes().find((pane) => pane.pane === tg.pane);
      const isAcp = Boolean(term?.acp ?? paneInfo?.acp);
      const isEnded = paneInfo?.status === "ended";
      const parent = term?.parent && all.find((t) => t.address === term.parent);
      // Once they go to the program, Shift+right-click still opens this menu.
      const toProgram = term?.labels?.["herdr.right-click"] === "program";
      // A terminal shown outside a tab (a layout terminal block, a window) has
      // no pane to split, zoom or close: its pane is the block's id.
      const inTab = Boolean(paneInfo);
      const tabPaneCount = inTab ? panes().filter((pane) => pane.tab === paneInfo.tab).length : 0;
      const isStructured = Boolean(paneInfo?.structured);
      const rawLineageParent = (term?.lineage?.parent || "").trim();
      const isSideChat = String(term?.lineage?.kind || "").toLowerCase().replace("_", "-") === "side-chat";
      const hasWorktree = Boolean(term?.lineage?.worktree_path);
      const termSlash = typeof term?.address === "string" ? term.address.lastIndexOf("/") : -1;
      const lineageParent = rawLineageParent
        ? (!rawLineageParent.includes("/") && termSlash > 0 ? `${term.address.slice(0, termSlash + 1)}${rawLineageParent}` : rawLineageParent)
        : null;
      return [
        ...(tg.terminal
          ? [
              [tr("Rename", "重命名"), "agent.rename", { terminal: tg.terminal }],
            ]
          : []),
        ...(term && !isEnded
          ? [
              [tr("Context handoff…", "上下文接力…"), "agent.handoff", { source: term.address, kind: "handoff" }],
              ...(!isSideChat ? [[tr("Fork session…", "会话分叉…"), "agent.handoff", { source: term.address, kind: "fork" }]] : []),
              [tr("Side chat (right panel)…", "旁路讨论（右侧面板）…"), "agent.handoff", { source: term.address, kind: "side-chat" }],
              ...(lineageParent
                ? [[tr(`Merge back into ${lineageParent}`, `合回 ${lineageParent}`), "agent.handoff", { source: term.address, into: lineageParent, kind: "merge-back" }]]
                : []),
            ]
          : []),
        ...(inTab
          ? [
              [tr("Split right", "向右分栏"), "pane.split", { direction: "right", pane: tg.pane }],
              [tr("Split down", "向下分栏"), "pane.split", { direction: "down", pane: tg.pane }],
              ...(tabPaneCount > 1
                ? [
                    [tr("Maximize to new tab", "最大化为独立标签页"), "pane.detach", { pane: tg.pane }],
                    [tr("Zoom in tab", "标签内放大/还原"), "pane.zoom", { pane: tg.pane }],
                  ]
                : []),
            ]
          : []),
        ...(inTab && term && !isAcp
          ? [
              [
                tr("Structured chat view", "结构化对话视图"),
                "terminal.view",
                { terminal: term.address, view: isStructured ? "terminal" : "peek" },
                { toggle: isStructured },
              ],
            ]
          : []),
        ...(term && !isAcp
          ? [
              toProgram
                ? [tr("Use Herdr right-click menu", "使用 Herdr2 右键菜单"), "terminal.right_click", { terminal: term.address, to: "menu" }]
                : [tr("Send right-clicks to pane", "右键直接传给终端程序"), "terminal.right_click", { terminal: term.address, to: "program" }],
            ]
          : []),
        ...(parent && !isAcp ? [[tr(`Send back to ${parent.name || parent.address}`, `放回 ${parent.name || parent.address} 下`), "terminal.park", { terminal: term.address }]] : []),
        ...(term && isAcp && !isEnded ? [acpPermItem(term)].filter(Boolean) : []),
        ...(term?.manual_status ? [[tr("Reset status to Auto", "恢复自动识别状态"), "agent.status", { terminal: term.address, status: "auto" }]] : []),
        ...(inTab ? [[(isAcp || term?.program?.agent || paneInfo?.status) && !isEnded ? tr("Close pane (keep agent in background)", "关闭窗格（转入后台）") : tr("Close pane", "关闭窗格"), "pane.close", { pane: tg.pane }]] : []),
        ...(term && (isAcp || term?.program?.agent || paneInfo?.status) ? [[tr("Move to recycle bin", "移入回收站"), "agent.trash", { terminal: term.address }]] : []),
      ];
    }
    case "agent": {
      const term = terminals().find((t) => t.address === tg.terminal);
      const isAcp = Boolean(term?.acp);
      const isEnded = term?.state === "finished" || term?.status === "ended";
      const rawLineageParent = (term?.lineage?.parent || "").trim();
      const isSideChat = String(term?.lineage?.kind || "").toLowerCase().replace("_", "-") === "side-chat";
      const hasWorktree = Boolean(term?.lineage?.worktree_path);
      const termSlash = typeof term?.address === "string" ? term.address.lastIndexOf("/") : -1;
      const lineageParent = rawLineageParent
        ? (!rawLineageParent.includes("/") && termSlash > 0 ? `${term.address.slice(0, termSlash + 1)}${rawLineageParent}` : rawLineageParent)
        : null;
      const pinned = Boolean(term?.labels?.["herdr.pinned"]);
      return [
        [tr("Rename", "重命名"), "agent.rename", { terminal: tg.terminal }],
        ...(tg.terminal && !isEnded
          ? [
              [tr("Context handoff…", "上下文接力…"), "agent.handoff", { source: tg.terminal, kind: "handoff" }],
              ...(!isSideChat ? [[tr("Fork session…", "会话分叉…"), "agent.handoff", { source: tg.terminal, kind: "fork" }]] : []),
              [tr("Side chat (right panel)…", "旁路讨论（右侧面板）…"), "agent.handoff", { source: tg.terminal, kind: "side-chat" }],
              ...(lineageParent
                ? [[tr(`Merge back into ${lineageParent}`, `合回 ${lineageParent}`), "agent.handoff", { source: tg.terminal, into: lineageParent, kind: "merge-back" }]]
                : []),
            ]
          : []),
        ...(term
          ? [
              ...(!tg.pane ? [[tr("Open as Tab", "挂载为标签页"), "terminal.keep", { terminal: tg.terminal, place: "tab" }]] : []),
              ...(isAcp || !tg.pane ? [[tr("Split right", "右侧分栏"), "terminal.keep", { terminal: tg.terminal, place: "right" }], [tr("Split down", "下方分栏"), "terminal.keep", { terminal: tg.terminal, place: "down" }]] : []),
              ...(isAcp ? [acpPermItem(term)] : []),
            ].filter(Boolean)
          : []),
        ...(term ? [[pinned ? tr("Unpin agent", "取消置顶") : tr("Pin to top", "置顶"), "terminal.pin", { terminal: tg.terminal, pinned: !pinned }]] : []),
        ...(tg.pane ? [[!isEnded ? tr("Close pane (keep in background)", "关闭窗格（转入后台）") : tr("Close pane", "关闭窗格"), "pane.close", { pane: tg.pane }]] : []),
        [tr("Move to recycle bin", "移入回收站"), "agent.trash", { terminal: tg.terminal }],
      ];
    }
    case "tab": {
      const tabPanes = panes().filter((pane) => pane.tab === tg.id);
      const allAcp = tabPanes.length > 0 && tabPanes.every((pane) => pane.acp);
      const allEnded = allAcp && tabPanes.every((pane) => pane.status === "ended");
      const acpPane = tabPanes.find((pane) => pane.acp && pane.terminal && pane.status !== "ended");
      const acpTerm = acpPane ? terminals().find((t) => t.address === acpPane.terminal) : null;
      const ptyPanes = tabPanes.filter((pane) => !pane.acp && pane.terminal);
      const isStructured = ptyPanes.length > 0 && ptyPanes.every((pane) => pane.structured);
      const agentPanes = tabPanes.filter((pane) => {
        if (!pane.terminal) return false;
        if (pane.acp) return true;
        const tRec = terminals().find((t) => t.address === pane.terminal);
        return Boolean(pane.program?.agent || pane.status || tRec?.program?.agent || tRec?.status);
      });
      const primaryAgentPane = agentPanes.find((p) => p.focused) || agentPanes[0] || null;
      return [
        [tr("Rename", "重命名"), "tab.rename", { id: tg.id }],
        [tr("New tab", "新建标签页"), "tab.new", { space: tg.space }],
        ...(ptyPanes.length > 0
          ? [
              [
                tr("Structured chat view", "结构化对话视图"),
                "terminal.view",
                { tab: tg.id, view: isStructured ? "terminal" : "peek" },
                { toggle: isStructured },
              ],
            ]
          : []),
        ...(acpTerm ? [acpPermItem(acpTerm)].filter(Boolean) : []),
        [agentPanes.length > 0 && !allEnded ? tr("Close tab (keep agent)", "关闭标签页（保留 Agent）") : tr("Close tab", "关闭标签页"), "tab.close", { id: tg.id }],
        ...(primaryAgentPane ? [[tr("Move to recycle bin", "移入回收站"), "agent.trash", { terminal: primaryAgentPane.terminal }]] : []),
      ];
    }
    case "space":
      return [
        [tr("Rename", "重命名"), "space.rename", { id: tg.id }],
        ...(git ? [[tr("New worktree", "新建工作树"), "worktree", { space: tg.id, action: "new" }], [tr("Open worktree", "打开工作树"), "worktree", { space: tg.id, action: "open" }]] : []),
        ...(git === "linked" ? [[tr("Remove worktree", "移除工作树"), "worktree", { space: tg.id, action: "remove" }]] : []),
        [tr("Close", "关闭"), "space.close", { id: tg.id }],
      ];
    default:
      return [
        [tr("New space", "新建工作区"), "space.new", {}],
        [tr("Keybinds", "快捷键"), "app.help", {}],
        [tr("Theme: graphite", "主题：graphite"), "app.theme", { name: "graphite" }],
        [tr("Theme: ink", "主题：ink"), "app.theme", { name: "ink" }],
        [tr("Theme: moss", "主题：moss"), "app.theme", { name: "moss" }],
        [tr("Theme: atom one dark", "主题：atom one dark"), "app.theme", { name: "atom-one-dark" }],
        ["Language: English", "app.language", { language: "en" }],
        ["Language: 简体中文", "app.language", { language: "zh-CN" }],
      ];
  }
}

function menuIcon(command, args) {
  if (command === "agent.trash") {
    return "mods/workspace-chrome/archive.svg";
  }
  if (command === "acp.config") {
    return "frame/settings/shield.svg";
  }
  if (command === "agent.status") {
    const s = args?.status;
    return `mods/workspace-chrome/${s === "working" ? "spinner" : s === "done" ? "circle-check" : s === "auto" ? "clock" : "check"}.svg`;
  }
  if (command === "terminal.keep") {
    return `mods/workspace-chrome/${args?.place === "right" ? "panel-right" : args?.place === "down" ? "panel-bottom" : "plus"}.svg`;
  }
  if (command === "terminal.pin") {
    return `mods/workspace-chrome/${args?.pinned === false ? "pin-off" : "pin"}.svg`;
  }
  if (command === "terminal.view") {
    return "mods/workspace-chrome/layers.svg";
  }
  const name = command.endsWith(".close") || command === "terminal.kill" ? "x" : command.endsWith(".rename") ? "square-pen" : command.endsWith(".new") ? "plus" : command === "worktree" ? "git-branch" : command === "app.help" ? "terminal" : command === "app.theme" || command === "app.language" ? "settings" : command === "pane.split" ? (args?.direction === "down" ? "panel-bottom" : "panel-right") : command === "pane.zoom" || command === "pane.detach" ? "square" : "terminal";
  return `mods/workspace-chrome/${name}.svg`;
}

// The key bound to a command, for the hint on the right.
function shortcut(name, args) {
  if (name === "agent.rename") {
    const direct = keys().find((b) => b.section === "global" && b.action === "agent.rename");
    if (direct?.keys) return direct.keys;
    return keys().find((b) => b.section === "global" && b.action === "tab.rename")?.keys ?? "";
  }
  if (name === "space.rename") {
    const f2 = keys().find((b) => b.section === "global" && b.action === "tab.rename")?.keys;
    const shiftF2 = keys().find((b) => b.section === "global" && b.action === "space.rename")?.keys;
    return f2 || shiftF2 || "";
  }
  // `pane.split:right` binds pane.split with { direction: "right" }.
  const value = args && Object.values(args)[0];
  const k = keys().find((b) => b.section === "global" && (b.action === name || b.action === `${name}:${value}`));
  return k ? k.keys : "";
}

export default class Menu extends View {
  init(_props, cx) {
    this.at = 0;
    this.git = null;
    this.handle = cx.focus_handle();
    this.handle.focus();
    const p = props();
    const space = p?.tab === "space" && spaces().find((s) => s.id === p.target?.id);
    const folder = space && place(space);
    if (folder)
      cx.spawn(async (cx) => {
        this.git = await checkout(folder, space.machine).catch(() => null);
        cx.notify();
      });
  }

  choose(i, cx) {
    const it = items(props(), this.git)[i];
    if (!it) return;
    closeOverlay(props()?.overlay);
    if (it[1] === "worktree") openOverlay({ mod: "worktree", kind: "dialog", ...(it[2].action === "open" ? { width: 560, height: 480 } : { width: 440, height: 260 }), props: it[2] });
    else if (it[1] === "acp.config") Promise.resolve(h2(["config", "--json", it[2].terminal, "--id", it[2].id, "--value", String(it[2].value)])).catch(() => {});
    else run(it[1], it[2]);
  }

  render(cx) {
    const t = theme();
    const list = items(props(), this.git);
    return v_flex()
      .id("menu")
      .track_focus(this.handle)
      .w_full()
      .min_h(0)
      .p(5)
      .bg(t.popover)
      .text_size(12.8)
      .on_key_down((e, cx) => {
        if (e.key === "down") this.at = (this.at + 1) % list.length;
        else if (e.key === "up") this.at = (this.at + list.length - 1) % list.length;
        else if (e.key === "enter") this.choose(this.at, cx);
        else if (e.key === "escape") closeOverlay(props().overlay);
        else return;
        cx.notify();
      })
      .children(
        list.map((it, i) =>
          v_flex()
            .when(
              i > 0 && (
                it[1].endsWith(".close")
                || (it[1] === "terminal.keep" && list[i - 1][1] === "agent.rename")
                || (it[1] === "acp.config" && list[i - 1][1] !== "acp.config")
                || ((it[1] === "terminal.kill" || it[1] === "agent.trash") && !list[i - 1][1].endsWith(".close"))
                || it[1] === "terminal.pin"
                || it[1] === "terminal.view"
                || (it[1] === "agent.status" && (it[2]?.status === "auto" || list[i - 1][1] === "terminal.keep" || list[i - 1][1] === "acp.config"))
                || it[1] === "app.help"
                || (it[1] === "app.theme" && list[i - 1][1] !== "app.theme")
                || (it[1] === "app.language" && list[i - 1][1] !== "app.language")
              ),
              el => el.child(div().mx(7).my(4).h(1).bg(t.border)),
            )
            .child(h_flex()
            .id(`mi-${i}`)
            .h(31)
            .px(8)
            .gap(9)
            .rounded(7)
            .items_center()
            .justify_between()
            .text_color(t.foreground)
            .when(i === this.at, (el) => el.bg(t.selected))
            .hover((s) => s.bg(t.hover))
            .on_click((_e, cx) => this.choose(i, cx))
            .child(svg(menuIcon(it[1], it[2])).size(14).flex_none().text_color(t.muted))
            .child(div().flex_1().child(it[0]))
            .child(
              typeof it[3]?.toggle === "boolean"
                ? h_flex()
                    .id(`mi-sw-${i}`)
                    .w(28)
                    .h(16)
                    .p(2)
                    .flex_none()
                    .items_center()
                    .when(it[3].toggle, (el) => el.justify_end())
                    .when(!it[3].toggle, (el) => el.justify_start())
                    .rounded(999)
                    .bg(it[3].toggle ? (t.accent ?? t.selected) : t.raised)
                    .border_1()
                    .border_color(it[3].toggle ? (t.accent ?? t.border) : t.border)
                    .child(div().size(10).rounded(999).bg(it[3].toggle ? (t.solid_foreground ?? "#ffffff") : t.dim))
                : div().text_size(11).text_color(t.dim).child(shortcut(it[1], it[2])),
            )),
        ),
      );
  }
}
