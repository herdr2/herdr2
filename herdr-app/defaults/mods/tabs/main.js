import { View, div, svg, image } from "gpui-kit";
import { h_flex, ScrollHandle } from "gpui-base";
import { spaces, panes, terminals, agents, focused, run, openOverlay, theme, window, shared, layout, settings, keys, updates } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function shortcutFor(action) {
  if (typeof keys !== "function" || !action) return "";
  const list = keys();
  if (!Array.isArray(list)) return "";
  const k = list.find((b) => b.section === "global" && b.action === action);
  if (!k?.keys?.trim()) return "";
  const mac = typeof window === "function" && window()?.platform === "macos";
  const names = mac ? { ctrl: "⌃", alt: "⌥", shift: "⇧", cmd: "⌘", fn: "fn" } : { ctrl: "Ctrl", alt: "Alt", shift: "Shift", cmd: "Super", fn: "Fn" };
  const alias = { secondary: mac ? "cmd" : "ctrl", super: "cmd", win: "cmd" };
  return k.keys.trim().split(/\s+/).map((stroke) => {
    const parts = stroke.split("-");
    let key = parts.pop();
    if (key === "") { key = "-"; parts.pop(); }
    const held = new Set(parts.map((m) => alias[m.toLowerCase()] ?? m.toLowerCase()));
    if (/^[A-Z]$/.test(key)) held.add("shift");
    const label = key.length === 1 ? key.toUpperCase() : key[0].toUpperCase() + key.slice(1).toLowerCase();
    return [...Object.keys(names).filter((m) => held.has(m)).map((m) => names[m]), label].join(mac ? "" : "+");
  }).join(" ");
}

function withShortcut(text, action) {
  const sc = shortcutFor(action);
  return sc ? `${text} (${sc})` : text;
}
const zoomScale = () => Math.max(0.25, Number(typeof settings === "function" ? settings()?.zoom : 1) || 1);

function resolvePeerAddr(raw, sibling) {
  const trimmed = String(raw || "").trim();
  if (!trimmed || trimmed.includes("/")) return trimmed;
  const slash = typeof sibling === "string" ? sibling.lastIndexOf("/") : -1;
  return slash >= 0 ? `${sibling.slice(0, slash + 1)}${trimmed}` : trimmed;
}

function focusedMainSideChatCount() {
  const termList = typeof terminals === "function" ? terminals() : [];
  if (!Array.isArray(termList) || termList.length === 0) return 0;
  const pList = typeof panes === "function" ? panes() : [];
  const aList = typeof agents === "function" ? agents() : [];
  const foc = typeof focused === "function" ? focused() : null;
  const mainAddr =
    pList.find((p) => p?.focused && p?.terminal)?.terminal
    || aList.find((a) => a?.focused && a?.address)?.address
    || foc?.shown
    || foc?.terminal
    || foc?.agent
    || null;
  if (!mainAddr) return 0;
  let count = 0;
  for (const x of termList) {
    if (!x?.address || x.state !== "live" || x.labels?.["herdr.trashed"]) continue;
    if (String(x.lineage?.kind || "").toLowerCase().replace("_", "-") !== "side-chat") continue;
    const parent = resolvePeerAddr(x.lineage?.parent || x.parent || "", x.address);
    if (parent === mainAddr) count += 1;
  }
  return count;
}

function find(node, id) {
  if (node?.id === id) return node;
  for (const child of node?.children ?? []) {
    const found = find(child, id);
    if (found) return found;
  }
  return null;
}

function isCliAgentPaneOrTerm(pane, term) {
  if (pane?.acp || term?.acp) return false;
  return Boolean(
    pane?.structured
      || pane?.program?.agent
      || term?.program?.agent
      || term?.saved_cli?.kind
      || pane?.app
      || term?.report?.app,
  );
}

function activeTabHasCliAgent(space) {
  if (!space) return false;
  const activeTab = (space.tabs ?? []).find((t) => t?.active);
  if (!activeTab) return false;
  const pList = typeof panes === "function" ? panes() : [];
  const tList = typeof terminals === "function" ? terminals() : [];
  const termByAddr = new Map();
  if (Array.isArray(tList)) {
    for (const rec of tList) {
      if (rec?.address) termByAddr.set(rec.address, rec);
    }
  }
  const tabPanes = Array.isArray(pList) ? pList.filter((p) => p?.tab === activeTab.id) : [];
  if (tabPanes.length === 0) return false;
  const focusedPane = tabPanes.find((p) => p.focused && p.terminal);
  if (focusedPane) {
    const fTerm = termByAddr.get(focusedPane.terminal) ?? null;
    if (focusedPane.acp || fTerm?.acp) return false;
    if (isCliAgentPaneOrTerm(focusedPane, fTerm)) return true;
  }
  return tabPanes.some((p) => p.terminal && isCliAgentPaneOrTerm(p, termByAddr.get(p.terminal) ?? null));
}

function resolveEnabledPanels(defaultIds) {
  const saved = shared("workspace.panels");
  if (!Array.isArray(saved)) return defaultIds;
  const out = [...saved];
  if (!shared("workspace.panels_has_checkpoints") && !out.includes("checkpoints")) {
    out.push("checkpoints");
  }
  if (!shared("workspace.panels_has_sessions") && !out.includes("sessions")) {
    out.push("sessions");
  }
  if (!shared("workspace.panels_has_side_chat") && !out.includes("side-chat")) {
    out.push("side-chat");
  }
  return out;
}

// Same as the agents mod: drops a status glyph, keeps a leading bracket or quote.
const clean = (s) => (s ?? "").replace(/^[^\p{L}\p{N}\p{Ps}\p{Pi}]+/u, "").replace(/^\[ [!.] \] /, "").trim();

const AUTO_TAB_NAMES = new Set([
  "terminal", "终端", "shell", "zsh", "bash", "sh", "fish", "nu", "nushell", "dash",
  "pwsh", "powershell", "powershell 7", "windows powershell", "cmd", "command prompt", "git bash", "wsl", "dos prompt", "headless",
  "claude", "claude code", "claude-code", "codex", "antigravity", "agy", "agy.real", "opencode",
  "cursor", "cursor-agent", "pi", "pig", "qwen", "kimi", "kimi-code", "kiro", "kiro-cli",
  "qoder", "qodercli", "copilot", "github-copilot", "amp", "grok", "hermes", "goose", "crush",
  "omp", "oh-my-pi", "dsh", "dsh-tui", "deepseek", "deepseek harness", "zcode", "zcode-cli", "glm", "agent",
]);

function isAutoName(rawName, prog, lead = null) {
  const name = String(rawName ?? "").trim();
  if (!name) return true;
  const lower = name.toLowerCase();
  if (prog && lower === String(prog).trim().toLowerCase()) return true;
  if (lead?.program?.key && lower === String(lead.program.key).trim().toLowerCase()) return true;
  if (lead?.terminal) {
    const addr = String(lead.terminal).trim().toLowerCase();
    const slash = addr.lastIndexOf("/");
    const shortAddr = slash >= 0 ? addr.slice(slash + 1) : addr;
    if (lower === addr || (shortAddr && lower === shortAddr)) return true;
  }
  return AUTO_TAB_NAMES.has(lower);
}

function isAutoTabName(tab, prog, lead = null) {
  if (!tab.named) return true;
  return isAutoName(tab.name, prog, lead);
}

function isLaunchCommandTitle(live, prog, lead = null) {
  const parts = String(live ?? "").trim().toLowerCase().split(/\s+/);
  if (parts.length < 2 || !parts[1].startsWith("-")) return false;
  const base = parts[0].split(/[\\/]/).pop() ?? parts[0];
  const cleanCmd = base.replace(/\.exe$/i, "").replace(/\.real$/i, "");
  if (AUTO_TAB_NAMES.has(cleanCmd)) return true;
  if (prog && cleanCmd === String(prog).trim().toLowerCase()) return true;
  if (lead?.program?.key && cleanCmd === String(lead.program.key).trim().toLowerCase()) return true;
  return false;
}

// A tab Herdr named (unnamed, or after a default shell/agent program) follows its agent's
// semantic title or live title, as the agents list does; a name the person gave stays.
function label(tab, lead, rec = null) {
  const prog = lead?.program?.name;
  const custom = lead?.custom_name || rec?.labels?.["herdr.name"];
  if (custom && String(custom).trim() && !isAutoName(custom, prog, lead)) return String(custom).trim();
  const base = tab.named ? tab.name : prog ?? "Terminal";
  if (!lead?.program?.agent && !lead?.acp) return base;
  const auto = isAutoTabName(tab, prog, lead);
  const semantic = String(lead?.semantic_title ?? rec?.semantic_title ?? "").trim();
  if (auto && semantic) return semantic;
  for (const raw of [lead?.title, rec?.title]) {
    const live = clean(raw);
    // clean() would strip a path's leading ~ or /: test the title with only its status glyph gone.
    const path = /^[~/]|^[A-Za-z]:\\/.test((raw ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
    const telling = live && live !== lead.terminal && live !== prog && !path && !live.includes("@") && live.toLowerCase() !== lead.program?.key && !isLaunchCommandTitle(live, prog, lead);
    if (auto && telling) return live;
  }
  if (auto) {
    const perm = String(rec?.pending_permission?.title ?? "").trim();
    if (perm) return perm;
    const micro = String(lead?.micro_summary ?? rec?.micro_summary ?? "").trim();
    if (micro) return micro;
  }
  return auto && prog ? prog : base;
}

function paneDisplayName(p) {
  if (p?.custom_name && !isAutoName(p.custom_name, p?.program?.name, p)) return p.custom_name;
  const semantic = isAgentPane(p) && typeof p?.semantic_title === "string" ? p.semantic_title.trim() : "";
  if (semantic) return semantic;
  if (p?.acp || p?.structured) return p.program?.name || clean(p.title) || p.terminal || "Agent";
  if (p?.program?.agent) {
    const live = clean(p.title);
    const path = /^[~/]|^[A-Za-z]:\\/.test((p.title ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
    const telling = live && live !== p.terminal && live !== p.program.name && !path && !live.includes("@") && live.toLowerCase() !== p.program.key && !isLaunchCommandTitle(live, p.program.name, p);
    return telling ? live : p.program.name;
  }
  return p?.program?.shell && p.program?.name
    ? p.program.name
    : (clean(p?.title) || p?.program?.name || p?.mod || "Terminal");
}

function buildTabMicroTooltip(tab, lead, displayName, elapsed, termByAddr) {
  if (!lead) return "";
  const rec = lead.terminal ? termByAddr.get(lead.terminal) : null;
  const st = lead.status ?? tab.status ?? "idle";
  const stMap = {
    working: tr("Working", "运行中"),
    blocked: tr("Needs you", "待确认"),
    done: tr("Done", "已完成"),
    error: tr("Error", "异常"),
    idle: tr("Idle", "空闲"),
  };
  const isManual = Boolean(lead.manual_status ?? rec?.manual_status);
  const prog = lead.program?.name ?? (lead.acp ? "ACP" : "Terminal");
  const elapsedText = elapsed && elapsed.ms > 0 ? ` ${formatElapsed(elapsed.ms)}` : "";
  const manualTag = isManual ? ` (${tr("manual", "手动")})` : "";
  const head = [displayName, prog !== displayName ? prog : "", `${stMap[st] ?? st}${manualTag}${elapsedText}`]
    .filter(Boolean)
    .join(" · ");
  const perm = !isManual && rec?.pending_permission?.title ? String(rec.pending_permission.title).trim() : "";
  const reason = !isManual ? String(lead.status_reason ?? rec?.status_reason ?? rec?.ai_status_summary ?? "").trim() : "";
  const micro = String(lead.micro_summary ?? rec?.micro_summary ?? reason ?? "").trim();
  const progress = isManual
    ? (micro ? `${micro} · ${tr("resumes on next action", "新操作后恢复自动识别")}` : tr("Manual status · resumes on next action", "手动设置 · 新操作后自动恢复"))
    : perm
      ? tr(`Waiting approval: ${perm}`, `等待审批：${perm}`)
      : st === "blocked" && reason
        ? tr(`Needs you: ${reason}`, `待确认：${reason}`)
        : micro
          ? micro
          : st === "working"
            ? tr("Actively working in background…", "后台执行中…")
            : st === "done"
              ? tr("Finished — click tab to inspect", "已完成 — 点击标签页检视")
              : st === "blocked"
                ? tr("Needs your input or approval to advance", "需你的确认或输入后进入下一状态")
                : "";
  const cwd = String(lead.cwd ?? rec?.cwd ?? "").trim();
  const branch = String(lead.branch ?? "").trim();
  const shortCwd = cwd.length > 44 ? `…${cwd.slice(-42)}` : cwd;
  const meta = shortCwd ? (branch ? `${shortCwd} (${branch})` : shortCwd) : (branch ? `(${branch})` : "");
  return [head, progress ? `▸ ${progress}` : "", meta].filter(Boolean).join("\n");
}

const TAB_GAP = 3;
const MIN_EQUAL_TAB_WIDTH = 36;
const DEFAULT_NATURAL_TAB_WIDTH = 150;

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

export default class Tabs extends View {
  init(_props, cx) {
    this.widths = {};
    this.naturalWidths = {};
    this.stripWidth = 0;
    this.equalTabWidth = null;
    this.seenTabsSpace = null;
    this.sessionsAutoHidden = new Set();
    this.seenTabIds = new Set();
    this.drag = null;
    this.settling = null;
    this.settleTask = null;
    this.snapFromOrder = null;
    this.snapUntil = 0;
    this.hoveredTab = null;
    this.localWorkingSince = {};
    this.localWorkMs = {};
    this.scroll = ScrollHandle.new();
    try { this.panels = JSON.parse(localStorage.getItem("panels") ?? "{}"); } catch { this.panels = {}; }
    if (!this.panels || typeof this.panels !== "object" || Array.isArray(this.panels)) this.panels = {};
    this.swipe = { acc: 0, steps: 0, at: 0, last: 0 };
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

  ensureNewTabsAtTail(space, shown) {
    const spaceId = space?.id ?? null;
    if (!spaceId) {
      this.seenTabsSpace = null;
      this.seenTabIds = new Set();
      return;
    }
    const ids = shown.map((c) => c.id);
    if (this.seenTabsSpace !== spaceId) {
      this.seenTabsSpace = spaceId;
      this.seenTabIds = new Set(ids);
      return;
    }
    const prev = this.seenTabIds ?? new Set();
    const added = [];
    for (let i = 0; i < shown.length; i++) {
      if (!prev.has(shown[i].id)) added.push({ id: shown[i].id, index: i });
    }
    this.seenTabIds = new Set(ids);
    if (added.length === 0 || this.drag || this.settling) return;
    // If any newly added tab was inserted before the tail (e.g. after active tab), move it to the end.
    const tailStart = shown.length - added.length;
    const allAtTail = added.every((item, k) => item.index === tailStart + k);
    if (!allAtTail) {
      for (const item of added) {
        run("tab.move", { id: item.id, index: shown.length });
      }
    }
  }

  computeEqualTabWidth(shown) {
    const count = shown.length;
    if (count <= 1 || !(this.stripWidth > 0)) return null;
    const avail = this.stripWidth - TAB_GAP * (count - 1);
    if (avail <= 0) return null;
    let knownSum = 0;
    let knownCount = 0;
    for (const c of shown) {
      const nw = this.naturalWidths?.[c.id];
      if (typeof nw === "number" && nw > 0) {
        knownSum += nw;
        knownCount += 1;
      }
    }
    const fallbackNatural = knownCount > 0 ? knownSum / knownCount : DEFAULT_NATURAL_TAB_WIDTH;
    let totalNatural = 0;
    for (const c of shown) {
      totalNatural += this.naturalWidths?.[c.id] ?? this.widths?.[c.id] ?? fallbackNatural;
    }
    if (totalNatural <= avail + 1) return null;
    return Math.max(MIN_EQUAL_TAB_WIDTH, Math.floor(avail / count));
  }

  tabWidth(id) {
    return this.equalTabWidth ?? this.widths[id] ?? 136;
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
    if (!panel.hidden && body?.active === "sessions" && !enabled.includes("sessions")) {
      this.sessionsAutoHidden.add(space.id);
      this.pending = { want: null, until: Date.now() + 1000 };
      run("layout.hide", { id: "panel" });
      return;
    }
    if (panel.hidden && this.sessionsAutoHidden.has(space.id)) {
      if (enabled.includes("sessions") && this.panels[space.id] === "sessions") {
        this.sessionsAutoHidden.delete(space.id);
        this.pending = { want: "sessions", until: Date.now() + 1000 };
        run("layout.select", { stack: "panel-body", child: "sessions" });
        run("layout.show", { id: "panel" });
        return;
      }
      if (!enabled.includes("sessions")) return;
      this.sessionsAutoHidden.delete(space.id);
    }
    if (!panel.hidden && body?.active && body.active !== "sessions") {
      this.sessionsAutoHidden.delete(space.id);
    }
    if (this.panels[space.id] === now) return;
    this.panels[space.id] = now;
    localStorage.setItem("panels", JSON.stringify(this.panels));
  }

  render() {
    const t = theme();
    const space = spaces().find(p => p.active);
    const shown = space?.tabs ?? [];
    this.ensureNewTabsAtTail(space, shown);
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
    const shownIds = new Set(shown.map((c) => c.id));
    if (this.naturalWidths) {
      for (const id of Object.keys(this.naturalWidths)) {
        if (!shownIds.has(id)) delete this.naturalWidths[id];
      }
    }
    const equalTabWidth = this.computeEqualTabWidth(shown);
    this.equalTabWidth = equalTabWidth;
    if ((selected !== this.selected || active !== this.selectedIndex) && !this.drag && !this.settling) {
      this.selected = selected;
      this.selectedIndex = active;
      if (active >= 0) this.scroll.scroll_to_item(active);
    }
    const alpha = (hex, a) => (typeof hex === "string" && /^#[0-9a-fA-F]{6}$/.test(hex) ? hex + a : null);
    const stop = (_e, cx) => cx.stop_propagation();
    const button = (id, label, action, tip = "") => div().id(id).size(26).flex_none().flex().items_center().justify_center().rounded(6).text_color(t.dim).hover(s => s.bg(t.hover).text_color(t.foreground))
      .when(Boolean(tip), el => el.tooltip(tip))
      .on_mouse_down("left", stop).on_click(action).child(label);
    const motion = this.drag?.moving ? this.drag : this.settling;
    const now = Date.now();
    const termByAddr = new Map((typeof terminals === "function" ? terminals() : []).filter((x) => x?.address).map((x) => [x.address, x]));
    const timeTooltip = (info) => {
      if (!info) return "";
      const cur = formatElapsed(info.ms);
      const base = info.working
        ? tr(`Working for ${cur}`, `已运行 ${cur}`)
        : info.ran
          ? tr(`Last run: ${cur}`, `最近执行：${cur}`)
          : tr("Not run yet", "尚未执行");
      if (info.totalMs > info.ms + 1000) {
        return `${base} · ${tr(`Total ${formatElapsed(info.totalMs)}`, `累计 ${formatElapsed(info.totalMs)}`)}`;
      }
      return base;
    };
    const renderCard = (c, showClose, { isFloating = false, width = null, interactive = true, isMergeTarget = false, showMergeBadge = false, showSeparator = false } = {}) => {
      const tabPanes = panes().filter(p => p.tab === c.id);
      const lead = tabPanes.find(p => p.program?.agent) ?? tabPanes[0];
      const paneCount = tabPanes.length;
      const isMerged = paneCount > 1;
      const mergedNames = isMerged ? tabPanes.map(paneDisplayName).filter(Boolean) : [];
      const combinedName = mergedNames.length > 1 ? mergedNames.join(" + ") : null;
      const allAcp = paneCount > 0 && tabPanes.every(p => p.acp);
      const hasAcpAgent = tabPanes.some(isAcpPane);
      const hasStructuredTermAgent = tabPanes.some(p => !p?.acp && Boolean(p?.structured));
      const hasRawTermAgent = tabPanes.some(p => !p?.acp && !p?.structured && Boolean(p?.program?.agent));
      const hasTermAgent = hasStructuredTermAgent || hasRawTermAgent;
      const agentPanes = tabPanes.filter(isAgentPane);
      const elapsed = this.tabElapsed(c, tabPanes, agentPanes, now);
      const program = lead?.program;
      const leadRec = lead?.terminal ? termByAddr.get(lead.terminal) : null;
      const name = c.named ? label(c, lead, leadRec) : (combinedName || label(c, lead, leadRec));
      const hoverTip = !isFloating && !c.active && isAgentPane(lead)
        ? buildTabMicroTooltip(c, lead, name, elapsed, termByAddr)
        : (!isFloating && width > 0 ? (c.zoomed ? `${name} Z` : name) : "");
      const accent = t.accent ?? "#3b82f6";
      const constrained = !isFloating && width > 0;
      const cardW = width > 0 ? width : null;
      const tight = Boolean(constrained && cardW < 110);
      const showIcon = Boolean(program?.icon) && (!constrained || cardW >= 56);
      const showTypePills = !constrained || cardW >= 118;
      const showElapsedPill = Boolean(elapsed) && (!constrained || cardW >= 155);
      const sepColor = alpha(t.foreground, "26") ?? t.border;
      let el = h_flex()
        .id(isFloating ? `tab-float-card-${c.id}` : `tab-${c.id}`)
        .h(29)
        .px(tight ? 6 : 10)
        .gap(tight ? 4 : 6)
        .rounded(7)
        .items_center()
        .flex_none()
        .when(Boolean(cardW), node => node.w(cardW).overflow_hidden())
        .when(Boolean(hoverTip), (node) => node.tooltip(hoverTip))
        .text_size(12.8)
        .border(1)
        .border_color(isFloating || isMergeTarget ? accent : (c.active ? t.border : t.chrome))
        .bg(isMergeTarget ? (alpha(accent, "28") ?? t.selected) : (c.active || isFloating ? t.raised : t.chrome))
        .text_color(c.active || isFloating || isMergeTarget ? t.foreground : t.muted)
        .when(isFloating, node => node.shadow_md().cursor_grabbing())
        .when(!isFloating && interactive, node => node.hover(s => s.bg(t.hover)))
        .when(showIcon, node => node.child(program.tint ? svg(program.icon).size(14).flex_none().text_color(t.muted) : image(program.icon).size(14).flex_none()))
        .child(
          constrained
            ? div().flex_1().min_w(0).truncate().child(c.zoomed ? `${name} Z` : name)
            : div().max_w(isMerged ? 210 : 160).truncate().child(c.zoomed ? `${name} Z` : name),
        )
        .when(!isFloating && isMerged && showTypePills, node => node.child(
          div()
            .id(`tab-merged-${c.id}`)
            .px(4.5)
            .h(15)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha(accent, "1c") ?? t.raised)
            .border_1()
            .border_color(alpha(accent, "55") ?? t.border)
            .text_size(9.5)
            .font_medium()
            .text_color(accent)
            .tooltip(combinedName
              ? tr(`Merged ${paneCount} panes: ${combinedName}`, `已合并 ${paneCount} 个分栏：${combinedName}`)
              : tr(`Merged ${paneCount} panes`, `已合并 ${paneCount} 个分栏`))
            .child(tr(`◫ ${paneCount}`, `合并·${paneCount}`)),
        ))
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
        .when(hasAcpAgent && showTypePills, node => node.child(
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
            .when(!isFloating, el => el.tooltip(tr("ACP Agent", "ACP 智能体")))
            .child("ACP"),
        ))
        .when(hasTermAgent && showTypePills, node => node.child(
          div()
            .px(4.5)
            .h(15)
            .flex_none()
            .flex()
            .items_center()
            .rounded(4)
            .bg(hasStructuredTermAgent ? (alpha(accent, "22") ?? t.raised) : t.raised)
            .border_1()
            .border_color(hasStructuredTermAgent ? (alpha(accent, "66") ?? t.border) : t.border)
            .text_size(9.5)
            .font_semibold()
            .text_color(hasStructuredTermAgent ? accent : t.dim)
            .when(!isFloating, el => el.tooltip(hasStructuredTermAgent ? tr("Terminal Agent (Structured View)", "终端智能体（结构化视图）") : tr("Terminal Agent", "终端智能体")))
            .child(tr("CLI", "终端")),
        ))
        .when(showElapsedPill, node => node.child(
          h_flex()
            .id(isFloating ? `tab-float-time-${c.id}` : `tab-time-${c.id}`)
            .px(4.5)
            .h(15)
            .gap(3)
            .flex_none()
            .items_center()
            .rounded(4)
            .bg(elapsed.working ? (alpha(t.working, "1f") ?? t.raised) : t.raised)
            .border_1()
            .border_color(elapsed.working ? (alpha(t.working, "55") ?? t.border) : t.border)
            .text_size(9.5)
            .font_medium()
            .text_color(elapsed.working ? (t.working ?? accent) : (elapsed.ran ? t.muted : t.dim))
            .when(!isFloating, el => el.tooltip(timeTooltip(elapsed)))
            .child(svg("mods/workspace-chrome/clock.svg").size(9.5).flex_none())
            .child(formatElapsed(elapsed.ms)),
        ))
        .when((agentPanes.length > 0 || tabPanes.length === 0) && c.status === "working", node => node.child(svg("mods/workspace-chrome/spinner.svg").size(12).flex_none().text_color(t.working).spin(900)))
        .when((agentPanes.length > 0 || tabPanes.length === 0) && (c.status === "blocked" || c.status === "done"), node => node.child(div().size(6).flex_none().rounded(3).bg(t[c.status])))
        .when(!isFloating && showClose, node => node.child(
          div()
            .id(`close-${c.id}`)
            .flex_none()
            .text_color(t.dim)
            .hover(s => s.text_color(t.foreground))
            .tooltip(withShortcut(allAcp && tabPanes.some(p => p.status !== "ended") ? tr("Move to background", "转入后台") : tr("Close tab", "关闭标签页"), "tab.close"))
            .on_mouse_down("left", stop)
            .on_click((_e, cx) => { cx.stop_propagation(); run("tab.close", { id: c.id }); })
            .child(svg("mods/pane/close.svg").size(12)),
        ))
        .when(!isFloating && showSeparator, node => node.child(
          div()
            .id(`tab-sep-${c.id}`)
            .absolute()
            .right(0)
            .top(7)
            .w(1)
            .h(14)
            .rounded(1)
            .bg(sepColor),
        ));
      return el;
    };
    const item = (c, i) => {
      const isMergeTarget = Boolean(this.drag?.moving && this.drag.mergeTarget === c.id);
      const showClose = (c.active || this.hoveredTab === c.id) && !(motion && motion.from === i) && !this.drag?.moving;
      const isHiddenSource = Boolean(motion && motion.from === i);
      const nextTab = shown[i + 1] ?? null;
      const nextHidden = Boolean(motion && motion.from === i + 1);
      const showSeparator =
        !isHiddenSource &&
        !isMergeTarget &&
        !c.active &&
        this.hoveredTab !== c.id &&
        (!nextTab || (!nextTab.active && this.hoveredTab !== nextTab.id && !nextHidden));
      let shift = 0;
      if (motion && i !== motion.from) {
        if (motion.from < motion.to && i > motion.from && i <= motion.to) {
          shift = -(motion.dragWidth + TAB_GAP);
        } else if (motion.from > motion.to && i >= motion.to && i < motion.from) {
          shift = motion.dragWidth + TAB_GAP;
        }
      }
      return renderCard(c, showClose, { width: equalTabWidth, interactive: !isHiddenSource, isMergeTarget, showSeparator })
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
          if (!equalTabWidth && w > 0 && Math.abs((this.naturalWidths[c.id] ?? 0) - w) > 0.5) {
            this.naturalWidths[c.id] = w;
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
    const enabled = resolveEnabledPanels(["files", "changes", "checkpoints", "sessions", "side-chat"]);
    const hasCliAgent = activeTabHasCliAgent(space);
    const hasSideChats = focusedMainSideChatCount() > 0;
    const effectiveEnabled = enabled.filter(
      (id) => (id !== "sessions" || hasCliAgent) && (id !== "side-chat" || hasSideChats),
    );
    this.syncPanel(space, panel, body, effectiveEnabled);
    const panelButton = (id, label, icon) => {
      const on = panel && !panel.hidden && body?.active === id;
      return h_flex().id(`panel-${id}`).h(26).px(8).gap(5).flex_none().items_center().rounded(6)
        .text_size(12).bg(on ? t.selected : t.chrome).text_color(on ? t.foreground : t.muted)
        .hover(s => s.bg(t.hover))
        .tooltip(withShortcut(on ? tr(`Hide ${label} panel`, `收起${label}面板`) : tr(`Open ${label} panel`, `打开${label}面板`), "layout.toggle:panel"))
        .on_mouse_down("left", stop)
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
    const updateState = typeof updates === "function" ? updates() : null;
    const renderUpdateButton = () => {
      if (!updateState) return null;
      const ui = updateState.ui ?? {};
      const app = updateState.app ?? {};
      const core = updateState.core ?? {};
      const machines = Array.isArray(updateState.machines) ? updateState.machines : [];
      const remoteUpdating = machines.some((m) => m?.updating);
      const remotePending = machines.filter((m) => m && (m.ready || m.raw_state === "needs_update"));
      const avail = updateState.available ?? null;
      const coreCanUpdate = Boolean(core.ready && (core.can_update ?? !core.managed));
      const busy = Boolean(updateState.updating || app.updating || ui.applying || core.updating || remoteUpdating);
      const hasUpdate = Boolean(avail || app.ready || ui.ready || coreCanUpdate || remotePending.length > 0);
      if (!busy && !hasUpdate) return null;
      const accent = t.accent ?? "#3b82f6";
      if (busy) {
        const busyTip = remoteUpdating
          ? tr("Syncing local and SSH remote updates…", "正在后台同步更新本机与远程主机…")
          : tr("Updating in background…", "正在后台更新中…");
        return h_flex()
          .id("top-update-btn")
          .size(26)
          .flex_none()
          .items_center()
          .justify_center()
          .rounded(6)
          .bg(t.raised ?? t.surface ?? "#27272a")
          .border_1()
          .border_color(t.border)
          .text_color(accent)
          .tooltip(busyTip)
          .child(svg("mods/workspace-chrome/spinner.svg").size(14).flex_none().spin(900));
      }
      const nextVer = avail?.version ?? app.ready ?? ui.ready ?? core.ready ?? remotePending[0]?.ready ?? core.version ?? "";
      const hasRemotes = machines.length > 0;
      const tip = app.ready
        ? (hasRemotes
            ? tr(
                `Herdr2 v${nextVer} is available. Click to update this computer and all SSH remote hosts in background.`,
                `发现新版本 v${nextVer}，点击后后台自动同步更新本机与所有 SSH 远程主机。`,
              )
            : tr(
                `Herdr2 v${nextVer} is available. Click to download and update in background.`,
                `发现新版本 v${nextVer}，点击后后台自动更新。`,
              ))
        : ui.ready
          ? tr(
              `UI hot-update v${nextVer} is available. Click to update live without restarting.`,
              `发现 UI 热更新 v${nextVer}，点击后后台自动免重启更新。`,
            )
          : coreCanUpdate
            ? (hasRemotes
                ? tr(
                    `h2 Core v${nextVer} is available. Click to update local and all SSH remote hosts in background.`,
                    `发现 Core 更新 v${nextVer}，点击后后台自动同步更新本机与所有 SSH 远程主机。`,
                  )
                : tr(
                    `h2 Core v${nextVer} is available. Click to update in background.`,
                    `发现 Core 更新 v${nextVer}，点击后后台自动更新。`,
                  ))
            : tr(
                `${remotePending.length} SSH remote host(s) need herdr2-core update${nextVer ? ` (v${nextVer})` : ""}. Click to sync in background.`,
                `检测到 ${remotePending.length} 台 SSH 远程主机需要更新 herdr2-core${nextVer ? `（v${nextVer}）` : ""}，点击后后台自动同步更新。`,
              );
      return h_flex()
        .id("top-update-btn")
        .h(26)
        .px(10)
        .gap(5)
        .flex_none()
        .items_center()
        .rounded(6)
        .bg(accent)
        .text_color(t.accent_foreground ?? "#ffffff")
        .text_size(12)
        .font_semibold()
        .tooltip(tip)
        .cursor_pointer()
        .hover((s) => s.opacity(0.9))
        .on_mouse_down("left", stop)
        .on_click(() => run("app.update"))
        .child(svg("frame/settings/refresh.svg").size(12).flex_none())
        .child(tr("Update", "更新"));
    };
    const updateBtn = renderUpdateButton();
    const sidebarHidden = Boolean(sidebar?.hidden);
    const openSidebar = () => {
      if (typeof sidebar?.size?.px === "number" && sidebar.size.px < 160) {
        run("layout.resize", { id: "sidebar", px: shared("sidebar.width") ?? 268 });
      }
      run("layout.show", { id: "sidebar" });
    };
    return h_flex().relative().size_full().min_w(0).px(10).pl(lightsPadding).gap(3).items_center().bg(t.chrome).border_b(1).border_color(t.border)
      .on_mouse_move((e, cx) => this.handleDragMove(e, space, shown, cx))
      .on_mouse_up("left", (_e, cx) => this.finishDrag(space, shown, cx))
      .on_scroll_wheel(e => this.wheel(e))
      .when(sidebarHidden, el => el.child(
        div().id("sidebar-toggle").size(26).flex_none().flex().items_center().justify_center().rounded(6).text_color(t.muted).hover(s => s.bg(t.hover).text_color(t.foreground))
          .tooltip(withShortcut(tr("Show sidebar", "展开侧边栏"), "layout.toggle:sidebar"))
          .on_mouse_down("left", stop)
          .on_click(openSidebar)
          .child(svg("mods/workspace-chrome/sidebar.svg").size(15)),
      ))
      .child(h_flex().id("tab-scroll").relative().flex_1().min_w(0).h_full().items_center().gap(TAB_GAP).overflow_x_scrollbar().track_scroll(this.scroll)
        .on_size_change((size, cx) => {
          const z = zoomScale();
          const sw = size && typeof size.width === "number" ? size.width / z : 0;
          let changed = false;
          if (sw > 0 && Math.abs(this.stripWidth - sw) > 0.5) {
            this.stripWidth = sw;
            changed = true;
          }
          if (active >= 0 && !this.drag && !this.settling) {
            this.scroll.scroll_to_item(active);
            changed = true;
          }
          if (changed) cx.notify();
        })
        .children(shown.map(item))
        .when(Boolean(floatingTab), el => el.child(floatingTab)))
      .child(button("tab-new", "+", () => openOverlay({ mod: "new-menu", kind: "popup", width: 250, height: 512 }), withShortcut(tr("New agent or terminal", "新建智能体或终端"), "mod.open:new-menu")))
      .when(enabled.includes("files"), el => el.child(panelButton("files", tr("Files", "文件"), "mods/files/file.svg")))
      .when(enabled.includes("changes"), el => el.child(panelButton("changes", tr("Changes", "变更"), "mods/changes/changes.svg")))
      .when(enabled.includes("checkpoints"), el => el.child(panelButton("checkpoints", tr("Checkpoints", "快照"), "mods/checkpoints/checkpoints.svg")))
      .when(enabled.includes("sessions") && hasCliAgent, el => el.child(panelButton("sessions", tr("Sessions", "会话"), "mods/sessions/sessions.svg")))
      .when(enabled.includes("side-chat") && hasSideChats, el => el.child(panelButton("side-chat", tr("Side Chat", "旁路"), "mods/peek/messages-square.svg")))
      .when(Boolean(updateBtn), el => el.child(updateBtn))
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
