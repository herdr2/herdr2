// Pure catalog, formatting, and keys.json mutation helpers for Settings -> Shortcuts.

export const SHORTCUT_GROUPS = [
  {
    id: "ai",
    title: "AI & Prompts",
    zh: "AI 与提示词",
    items: [
      { action: "prompt.polish", section: "global", title: "AI Polish prompt", zh: "AI 润色提示词（ACP / CLI / 闪念）", defaultLinux: "ctrl-alt-p", defaultMac: "cmd-alt-p" },
      { action: "prompt.polish_undo", section: "global", title: "Undo AI Polish (restore original)", zh: "反悔恢复润色前的内容（ACP / CLI / 闪念）", defaultLinux: "ctrl-alt-u", defaultMac: "cmd-alt-u" },
      { action: "mod.open:flash-note", section: "global", title: "Quick Flash Note", zh: "快速记录闪念", defaultLinux: "ctrl-alt-f", defaultMac: "cmd-alt-f" },
      { action: "agent.handoff", section: "global", title: "Context Handoff / Fork / Side Chat", zh: "上下文接力 / 分叉 / 旁路讨论", defaultLinux: "ctrl-shift-h", defaultMac: "cmd-shift-h" },
      { action: "agent.briefing", section: "global", title: "Catch-up Briefing", zh: "打开待办简报", defaultLinux: "ctrl-shift-i", defaultMac: "cmd-shift-i" },
      { action: "agent.next", section: "global", title: "Next agent", zh: "切换到下一个 Agent", defaultLinux: "alt-down", defaultMac: "cmd-]" },
      { action: "agent.prev", section: "global", title: "Previous agent", zh: "切换到上一个 Agent", defaultLinux: "alt-up", defaultMac: "cmd-[" },
    ],
  },
  {
    id: "tabs",
    title: "Tabs & Terminals",
    zh: "标签页与终端",
    items: [
      { action: "tab.new", section: "global", title: "New tab", zh: "新建标签页", defaultLinux: "ctrl-shift-t", defaultMac: "cmd-t" },
      { action: "terminal.open", section: "global", title: "New terminal", zh: "新建终端", defaultLinux: "ctrl-shift-enter", defaultMac: "cmd-alt-t" },
      { action: "mod.open:new-menu", section: "global", title: "Open New menu (Agent / Terminal)", zh: "打开新建菜单（Agent / 终端）", defaultLinux: "ctrl-alt-n", defaultMac: "cmd-shift-n" },
      { action: "tab.close", section: "global", title: "Close tab", zh: "关闭当前标签页", defaultLinux: "ctrl-shift-w", defaultMac: "cmd-w" },
      { action: "tab.next", section: "global", title: "Next tab", zh: "下一个标签页", defaultLinux: "ctrl-tab", defaultMac: "ctrl-tab" },
      { action: "tab.prev", section: "global", title: "Previous tab", zh: "上一个标签页", defaultLinux: "ctrl-shift-tab", defaultMac: "ctrl-shift-tab" },
      { action: "tab.rename", section: "global", title: "Rename tab / agent", zh: "重命名标签页 / Agent", defaultLinux: "f2", defaultMac: "f2" },
      { action: "terminal.view:toggle", section: "global", title: "Toggle chat & terminal view", zh: "切换对话视图 / 原始终端", defaultLinux: "ctrl-shift-m", defaultMac: "cmd-shift-m" },
    ],
  },
  {
    id: "panes",
    title: "Panes & Spaces",
    zh: "分屏与工作区",
    items: [
      { action: "pane.split:right", section: "global", title: "Split pane right", zh: "向右分屏", defaultLinux: "ctrl-shift-o", defaultMac: "cmd-d" },
      { action: "pane.split:down", section: "global", title: "Split pane down", zh: "向下分屏", defaultLinux: "ctrl-shift-e", defaultMac: "cmd-shift-d" },
      { action: "pane.close", section: "global", title: "Close pane", zh: "关闭当前分屏", defaultLinux: "ctrl-shift-x", defaultMac: "cmd-shift-w" },
      { action: "pane.zoom", section: "global", title: "Zoom / restore pane", zh: "放大 / 还原当前分屏", defaultLinux: "ctrl-shift-z", defaultMac: "cmd-shift-enter" },
      { action: "pane.detach", section: "global", title: "Detach pane to new tab", zh: "将分屏最大化为独立标签页", defaultLinux: "ctrl-alt-z", defaultMac: "cmd-alt-z" },
      { action: "space.new", section: "global", title: "New space", zh: "新建工作区", defaultLinux: "ctrl-shift-n", defaultMac: "cmd-n" },
      { action: "space.next", section: "global", title: "Next space", zh: "下一个工作区", defaultLinux: "ctrl-shift-pagedown", defaultMac: "cmd-alt-]" },
      { action: "space.prev", section: "global", title: "Previous space", zh: "上一个工作区", defaultLinux: "ctrl-shift-pageup", defaultMac: "cmd-alt-[" },
      { action: "space.rename", section: "global", title: "Rename space", zh: "重命名工作区", defaultLinux: "shift-f2", defaultMac: "shift-f2" },
    ],
  },
  {
    id: "window",
    title: "Panels & App",
    zh: "面板与应用",
    items: [
      { action: "layout.toggle:sidebar", section: "global", title: "Toggle left sidebar", zh: "切换左侧边栏", defaultLinux: "ctrl-shift-b", defaultMac: "cmd-b" },
      { action: "layout.toggle:panel", section: "global", title: "Toggle right panel (Files / Changes / Checkpoints)", zh: "切换右侧面板（文件 / 变更 / 快照）", defaultLinux: "ctrl-shift-j", defaultMac: "cmd-j" },
      { action: "mod.open:picker", section: "global", title: "Quick switcher", zh: "快速跳转（工作区 / 标签 / Agent）", defaultLinux: "ctrl-shift-k", defaultMac: "cmd-p" },
      { action: "app.palette", section: "global", title: "Command palette", zh: "打开命令面板", defaultLinux: "ctrl-shift-p", defaultMac: "cmd-shift-p" },
      { action: "app.settings", section: "global", title: "Open Settings", zh: "打开设置", defaultLinux: "ctrl-,", defaultMac: "cmd-," },
      { action: "app.help", section: "global", title: "Keybinds cheatsheet", zh: "快捷键速查表", defaultLinux: "ctrl-shift-/", defaultMac: "cmd-/" },
      { action: "layout.switch", section: "global", title: "Switch layout", zh: "切换界面布局", defaultLinux: "ctrl-shift-l", defaultMac: "cmd-shift-l" },
      { action: "app.zoom_in", section: "global", title: "Zoom in", zh: "放大界面", defaultLinux: "ctrl-=", defaultMac: "cmd-=" },
      { action: "app.zoom_out", section: "global", title: "Zoom out", zh: "缩小界面", defaultLinux: "ctrl--", defaultMac: "cmd--" },
      { action: "app.zoom_reset", section: "global", title: "Reset zoom", zh: "重置缩放", defaultLinux: "ctrl-0", defaultMac: "cmd-0" },
    ],
  },
  {
    id: "terminal",
    title: "Terminal",
    zh: "终端内操作",
    items: [
      { action: "terminal.copy", section: "terminal", title: "Copy selection", zh: "复制终端选区", defaultLinux: "ctrl-shift-c", defaultMac: "cmd-c" },
      { action: "terminal.paste", section: "terminal", title: "Paste into terminal", zh: "粘贴到终端", defaultLinux: "ctrl-shift-v", defaultMac: "cmd-v" },
      { action: "terminal.search", section: "terminal", title: "Find in terminal", zh: "终端内搜索", defaultLinux: "ctrl-shift-f", defaultMac: "cmd-f" },
      { action: "terminal.paste_image", section: "terminal", title: "Paste clipboard image", zh: "粘贴剪贴板图片", defaultLinux: "ctrl-v", defaultMac: "ctrl-v" },
    ],
  },
];

export const ALL_CATALOG_ITEMS = SHORTCUT_GROUPS.flatMap((g) => g.items);

export function defaultChordFor(item, isMac) {
  return isMac ? item.defaultMac : item.defaultLinux;
}

export function buildDefaultKeysDoc(isMac) {
  const doc = { global: {}, terminal: {} };
  for (const item of ALL_CATALOG_ITEMS) {
    const chord = defaultChordFor(item, isMac);
    if (chord) doc[item.section][chord] = item.action;
  }
  // Keep numbered tab/space switches & pane navigation defaults intact.
  for (let i = 1; i <= 9; i++) {
    doc.global[isMac ? `cmd-${i}` : `ctrl-${i}`] = `tab.select:${i}`;
    doc.global[isMac ? `ctrl-${i}` : `alt-${i}`] = `space.select:${i}`;
  }
  if (isMac) {
    Object.assign(doc.global, {
      "cmd-q": "app.quit",
      "cmd-h": "app.hide",
      "cmd-alt-h": "app.hide_others",
      "cmd-m": "window.minimize",
      "cmd-shift-]": "tab.next",
      "cmd-shift-[": "tab.prev",
      "cmd-alt-left": "pane.go:left",
      "cmd-alt-right": "pane.go:right",
      "cmd-alt-up": "pane.go:up",
      "cmd-alt-down": "pane.go:down",
      "cmd-alt-shift-left": "pane.swap:left",
      "cmd-alt-shift-right": "pane.swap:right",
      "cmd-alt-shift-up": "pane.swap:up",
      "cmd-alt-shift-down": "pane.swap:down",
      "cmd-shift-/": "app.help",
      "cmd-+": "app.zoom_in",
    });
    Object.assign(doc.terminal, {
      "cmd-a": "terminal.select_all",
      "alt-left": "send:\u001bb",
      "alt-right": "send:\u001bf",
      "alt-backspace": "send:\u0017",
      "cmd-left": "send:\u0001",
      "cmd-right": "send:\u0005",
      "cmd-backspace": "send:\u0015",
    });
  } else {
    Object.assign(doc.global, {
      "alt-h": "pane.go:left",
      "alt-j": "pane.go:down",
      "alt-k": "pane.go:up",
      "alt-l": "pane.go:right",
      "alt-left": "pane.go:left",
      "alt-right": "pane.go:right",
      "alt-shift-h": "pane.swap:left",
      "alt-shift-j": "pane.swap:down",
      "alt-shift-k": "pane.swap:up",
      "alt-shift-l": "pane.swap:right",
      "ctrl-+": "app.zoom_in",
      "ctrl-pagedown": "tab.next",
      "ctrl-pageup": "tab.prev",
      "alt-]": "tab.next",
      "alt-[": "tab.prev",
      "ctrl-?": "app.help",
      "ctrl-shift-?": "app.help",
    });
    Object.assign(doc.terminal, {
      "ctrl-backspace": "send:\u0017",
      "ctrl-left": "send:\u001bb",
      "ctrl-right": "send:\u001bf",
      "ctrl-insert": "terminal.copy",
      "shift-insert": "terminal.paste",
    });
  }
  return doc;
}

export function keysArrayToDoc(list) {
  const doc = { global: {}, terminal: {} };
  for (const row of Array.isArray(list) ? list : []) {
    if (!row?.section || !row?.keys || !row?.action) continue;
    if (row.action === "app.reset") continue;
    if (!doc[row.section]) doc[row.section] = {};
    doc[row.section][row.keys] = row.action;
  }
  return doc;
}

export function parseKeysDoc(text, fallbackList, isMac) {
  if (typeof text === "string" && text.trim()) {
    try {
      const parsed = JSON.parse(text);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
        const out = {};
        for (const [sec, map] of Object.entries(parsed)) {
          if (map && typeof map === "object" && !Array.isArray(map)) {
            out[sec] = { ...map };
          }
        }
        if (!out.global) out.global = {};
        if (!out.terminal) out.terminal = {};
        return out;
      }
    } catch {}
  }
  if (Array.isArray(fallbackList) && fallbackList.length > 0) {
    return keysArrayToDoc(fallbackList);
  }
  return buildDefaultKeysDoc(isMac);
}

/**
 * Find all bound chords for a given (section, action) in keysDoc.
 */
export function chordsForAction(doc, section, action) {
  const sec = doc?.[section];
  if (!sec || typeof sec !== "object") return [];
  const out = [];
  for (const [k, v] of Object.entries(sec)) {
    if (v === action) out.push(k);
  }
  return out;
}

/**
 * Primary displayed chord for an action (prefers default chord if present, else first bound).
 */
export function primaryChordFor(doc, item, isMac) {
  const bound = chordsForAction(doc, item.section, item.action);
  if (!bound.length) return "";
  const def = defaultChordFor(item, isMac);
  if (def && bound.includes(def)) return def;
  return bound[0];
}

/**
 * Bind `chord` to `item.action` inside `item.section`, removing old bindings for that action
 * and reporting any displaced action that previously owned `chord`.
 */
export function setActionBinding(doc, section, action, chord) {
  const cleanChord = String(chord ?? "").trim().toLowerCase();
  const next = {};
  for (const [sec, map] of Object.entries(doc || {})) {
    next[sec] = { ...(map || {}) };
  }
  if (!next[section]) next[section] = {};

  const displacedAction = cleanChord && next[section][cleanChord] && next[section][cleanChord] !== action
    ? String(next[section][cleanChord])
    : null;

  for (const [k, v] of Object.entries(next[section])) {
    if (v === action) delete next[section][k];
  }
  if (cleanChord) {
    next[section][cleanChord] = action;
  }
  return { doc: next, displacedAction };
}

/**
 * Unbind all chords for `(section, action)`.
 */
export function clearActionBinding(doc, section, action) {
  return setActionBinding(doc, section, action, "");
}

/**
 * Format a GPUI chord string (`ctrl-alt-p`, `cmd-shift-enter`) into human-readable keycaps.
 */
export function formatChord(chord, isMac = false) {
  const raw = String(chord ?? "").trim();
  if (!raw) return "";
  const parts = [];
  let rest = raw;
  const mods = [
    ["ctrl-", isMac ? "⌃" : "Ctrl"],
    ["alt-", isMac ? "⌥" : "Alt"],
    ["shift-", isMac ? "⇧" : "Shift"],
    ["cmd-", isMac ? "⌘" : "Super"],
    ["super-", isMac ? "⌘" : "Super"],
  ];
  let matched = true;
  while (matched) {
    matched = false;
    for (const [prefix, label] of mods) {
      if (rest.startsWith(prefix)) {
        parts.push(label);
        rest = rest.slice(prefix.length);
        matched = true;
        break;
      }
    }
  }
  const keyMap = {
    enter: "Enter",
    escape: "Esc",
    backspace: "Backspace",
    tab: "Tab",
    space: "Space",
    pageup: "PgUp",
    pagedown: "PgDn",
    up: "↑",
    down: "↓",
    left: "←",
    right: "→",
    insert: "Ins",
    delete: "Del",
  };
  const keyLabel = keyMap[rest] ?? (rest.length === 1 ? rest.toUpperCase() : rest.replace(/^f(\d+)$/, "F$1"));
  parts.push(keyLabel);
  return isMac ? parts.join(" ") : parts.join(" + ");
}

export function findActionLabel(action, isZh) {
  const item = ALL_CATALOG_ITEMS.find((it) => it.action === action);
  if (item) return isZh ? item.zh : item.title;
  return action;
}
