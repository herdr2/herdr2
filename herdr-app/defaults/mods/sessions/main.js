import { View, div, svg } from "gpui-kit";
import { v_flex, h_flex, v_resizable, resizable_panel, InputState, Input, Scrollbar } from "gpui-base";
import { spaces, panes, focused, terminals, theme, settings, h2, checking, openOverlay } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);
const zoomScale = () => Math.max(0.25, Number(typeof settings === "function" ? settings()?.zoom : 1) || 1);

const INITIAL_VISIBLE_ITEMS = 60;
const VISIBLE_ITEMS_STEP = 60;
const MAX_ENTRY_CHARS = 12000;
const DEFAULT_SPLIT_TOP_HEIGHT = 208;
const MIN_SPLIT_TOP_HEIGHT = 84;
const MAX_SPLIT_TOP_HEIGHT = 680;

function readSavedTopHeight() {
  try {
    if (typeof localStorage !== "undefined" && localStorage?.getItem) {
      const raw = Number(localStorage.getItem("sessions.topHeight"));
      if (Number.isFinite(raw) && raw >= MIN_SPLIT_TOP_HEIGHT && raw <= MAX_SPLIT_TOP_HEIGHT) {
        return Math.round(raw);
      }
    }
  } catch {}
  return DEFAULT_SPLIT_TOP_HEIGHT;
}

function isGenericProgramKey(key) {
  const k = String(key ?? "").trim().toLowerCase();
  return !k || k === "agent" || k === "program" || k === "shell" || k === "terminal";
}

function pickCliAgentKey(pane, term) {
  if (pane?.program?.agent && !pane?.program?.shell && !isGenericProgramKey(pane.program.key)) {
    return String(pane.program.key).trim();
  }
  if (term?.saved_cli?.kind && !isGenericProgramKey(term.saved_cli.kind)) {
    return String(term.saved_cli.kind).trim();
  }
  for (const raw of [pane?.app, term?.report?.app]) {
    if (raw && !isGenericProgramKey(raw)) return String(raw).trim();
  }
  if (term?.program?.agent && !term?.program?.shell && !isGenericProgramKey(term.program.key)) {
    return String(term.program.key).trim();
  }
  if (Array.isArray(term?.argv) && term.argv.length > 0) {
    const first = String(term.argv[0] ?? "").trim();
    const base = (first.split(/[\\/]/).pop() ?? first).replace(/\.exe$/i, "").toLowerCase();
    if (base && !["sh", "bash", "zsh", "fish", "pwsh", "powershell", "cmd", "nu", "dash"].includes(base)) {
      return base;
    }
  }
  return null;
}

function isCliCandidate(pane, term) {
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

function resolveActiveCliContext() {
  const sList = typeof spaces === "function" ? spaces() : [];
  const pList = typeof panes === "function" ? panes() : [];
  const tList = typeof terminals === "function" ? terminals() : [];
  const foc = typeof focused === "function" ? focused() : null;

  const termByAddr = new Map();
  if (Array.isArray(tList)) {
    for (const rec of tList) {
      if (rec?.address) termByAddr.set(rec.address, rec);
    }
  }

  const activeSpace = Array.isArray(sList) ? sList.find((s) => s?.active) : null;
  const activeTab = activeSpace?.tabs?.find((t) => t?.active) ?? null;
  const tabPanes = activeTab && Array.isArray(pList)
    ? pList.filter((p) => p?.tab === activeTab.id && p?.terminal)
    : [];

  let chosenPane = null;
  const focusedPane = tabPanes.find((p) => p.focused) ?? null;
  if (focusedPane) {
    const fTerm = termByAddr.get(focusedPane.terminal) ?? null;
    if (focusedPane.acp || fTerm?.acp) {
      return { target: null, agent: null, cwd: activeSpace?.folder ?? "", machine: activeSpace?.machine ?? null, activeSessionId: null, turn: 0 };
    }
    if (isCliCandidate(focusedPane, fTerm)) {
      chosenPane = focusedPane;
    }
  }
  if (!chosenPane) {
    chosenPane = tabPanes.find((p) => isCliCandidate(p, termByAddr.get(p.terminal) ?? null)) ?? null;
  }

  let target = chosenPane?.terminal ?? null;
  let term = target ? (termByAddr.get(target) ?? null) : null;

  if (!target) {
    const fallbackAddr = foc?.shown || foc?.terminal || null;
    if (fallbackAddr) {
      const fbTerm = termByAddr.get(fallbackAddr) ?? null;
      const fbPane = Array.isArray(pList) ? pList.find((p) => p?.terminal === fallbackAddr) ?? null : null;
      if (isCliCandidate(fbPane, fbTerm)) {
        target = fallbackAddr;
        term = fbTerm;
        chosenPane = fbPane;
      }
    }
  }

  if (!target) {
    return {
      target: null,
      agent: null,
      cwd: activeSpace?.folder ?? "",
      machine: activeSpace?.machine ?? null,
      activeSessionId: null,
      turn: 0,
    };
  }

  const agent = pickCliAgentKey(chosenPane, term);
  const cwd = String(
    chosenPane?.cwd
      || term?.foreground?.cwd
      || term?.cwd
      || foc?.cwd
      || activeSpace?.folder
      || "",
  ).trim();
  const slash = target.indexOf("/");
  const machine = slash > 0 ? target.slice(0, slash) : (activeSpace?.machine ?? null);

  return {
    target,
    agent,
    cwd,
    machine,
    activeSessionId: term?.saved_cli?.session_id ?? null,
    turn: Number(term?.turn ?? 0) || 0,
  };
}

function folderBasename(rawPath) {
  const s = String(rawPath ?? "").trim().replace(/[\\/]+$/, "");
  if (!s) return "";
  const parts = s.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? s;
}

function formatRelativeTime(tsMs) {
  const ts = Number(tsMs) || 0;
  if (ts <= 0) return "";
  const diffSec = Math.max(0, Math.floor((Date.now() - ts) / 1000));
  if (diffSec < 45) return tr("now", "刚刚");
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h`;
  const days = Math.floor(diffSec / 86400);
  if (days < 7) return `${days}d`;
  const d = new Date(ts);
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  return `${mm}-${dd}`;
}

function formatEntryClock(tsMs) {
  const ts = Number(tsMs) || 0;
  if (ts <= 0) return "";
  const d = new Date(ts);
  const hh = String(d.getHours()).padStart(2, "0");
  const mi = String(d.getMinutes()).padStart(2, "0");
  return `${hh}:${mi}`;
}

function cleanSingleLine(raw, maxChars = 140) {
  const s = String(raw ?? "")
    .replace(/<\/?pasted_content\b[^>]*>/gi, " ")
    .replace(/<\/?herdr2-handoff\b[^>]*>/gi, " ")
    .replace(/[\r\n\t]+/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();
  return s.length > maxChars ? `${s.slice(0, maxChars)}…` : s;
}

function clipBodyText(raw) {
  const s = String(raw ?? "").replace(/\r\n/g, "\n").trim();
  if (s.length <= MAX_ENTRY_CHARS) return s;
  return `${s.slice(0, MAX_ENTRY_CHARS)}\n…`;
}

function shortSessionId(sid) {
  const s = String(sid ?? "").trim();
  if (s.length <= 14) return s;
  return `${s.slice(0, 8)}…${s.slice(-4)}`;
}

/**
 * Groups consecutive intermediate steps (`thought`, `tool_call`, `plan`) into
 * collapsible `activity_group` items so the conversation detail view shows only
 * dialogue (`user` / `assistant`) by default while keeping steps accessible on demand.
 */
export function groupTranscriptItems(rawEntries) {
  const entries = Array.isArray(rawEntries) ? rawEntries : [];
  const items = [];
  let pendingGroup = null;

  const flushGroup = () => {
    if (pendingGroup && pendingGroup.steps.length > 0) {
      items.push(pendingGroup);
    }
    pendingGroup = null;
  };

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    if (!entry || typeof entry !== "object") continue;
    const kind = String(entry.kind || entry.type || "");

    if (kind === "thought") {
      const text = clipBodyText(entry.text);
      if (!text) continue;
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.thoughtCount += 1;
      pendingGroup.steps.push({ kind: "thought", idx: i, text, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "tool_call") {
      const title = cleanSingleLine(entry.title || entry.tool_kind || tr("Tool call", "工具调用"), 160);
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.toolCount += 1;
      pendingGroup.steps.push({ kind: "tool_call", idx: i, title, status: entry.status, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "plan" && Array.isArray(entry.items) && entry.items.length > 0) {
      const summary = cleanSingleLine(entry.items.join(" · "), 180);
      if (!summary) continue;
      if (!pendingGroup) {
        pendingGroup = {
          kind: "activity_group",
          id: `act-${i}`,
          turn: Number(entry.turn) || 0,
          thoughtCount: 0,
          toolCount: 0,
          planCount: 0,
          steps: [],
        };
      }
      pendingGroup.planCount += 1;
      pendingGroup.steps.push({ kind: "plan", idx: i, summary, ts_ms: entry.ts_ms });
      continue;
    }

    if (kind === "user") {
      flushGroup();
      items.push({ kind: "user", idx: i, entry });
      continue;
    }

    if (kind === "assistant") {
      const body = clipBodyText(entry.text);
      if (!body) continue;
      flushGroup();
      items.push({ kind: "assistant", idx: i, entry, body });
      continue;
    }

    if (kind === "error") {
      const msg = cleanSingleLine(entry.message || entry.text, 240);
      if (!msg) continue;
      flushGroup();
      items.push({ kind: "error", idx: i, msg });
    }
  }

  flushGroup();
  return items;
}

function formatActivityGroupSummary(group) {
  const parts = [];
  if (group.thoughtCount > 0) {
    parts.push(
      group.thoughtCount > 1
        ? tr(`Thinking (${group.thoughtCount})`, `思考过程 (${group.thoughtCount})`)
        : tr("Thinking", "思考过程"),
    );
  }
  if (group.toolCount > 0) {
    parts.push(
      tr(
        `${group.toolCount} tool call${group.toolCount === 1 ? "" : "s"}`,
        `${group.toolCount} 次工具调用`,
      ),
    );
  }
  if (group.planCount > 0) {
    parts.push(
      tr(
        `${group.planCount} plan${group.planCount === 1 ? "" : "s"}`,
        `${group.planCount} 项计划`,
      ),
    );
  }
  return parts.length > 0 ? parts.join(" · ") : tr("Steps", "执行过程");
}

export default class Sessions extends View {
  init(_props, cx) {
    this.sessions = [];
    this.selectedId = null;
    this.selectedSession = null;
    this.loadedSessionKey = null;
    this.entries = [];
    this.visibleLimit = INITIAL_VISIBLE_ITEMS;
    this.expandedGroups = new Set();
    this.expandAllSteps = false;
    this.splitTopHeight = readSavedTopHeight();
    this.listPaneHeight = DEFAULT_SPLIT_TOP_HEIGHT;
    this.hoveredSession = null;
    this.hoverAtBottom = true;
    this.hasPointerY = false;
    this.hoverSuppressedUntil = 0;

    this.agent = null;
    this.agentName = null;
    this.cwd = null;
    this.loadingList = false;
    this.loadingTranscript = false;
    this.error = null;
    this.transcriptError = null;
    this.context = null;
    this.reqSeq = 0;
    this.readSeq = 0;
    this.handle = cx.focus_handle();

    this.searchInput = InputState.new({
      placeholder: tr("Filter sessions…", "搜索历史会话…"),
    });
    this.searchFocused = false;
    this.searchInput.on("focus", (_e, cx) => {
      this.searchFocused = true;
      this.hoveredSession = null;
      cx.notify();
    });
    this.searchInput.on("blur", (_e, cx) => { this.searchFocused = false; cx.notify(); });
    this.searchInput.on("change", (_e, cx) => {
      this.hoveredSession = null;
      cx.notify();
    });

    if (typeof checking !== "function" || !checking()) {
      this.refresh(cx, false);
      cx.timer.every(15000, (cx) => this.refresh(cx, true));
    }
  }

  computeContextKey(ctx) {
    return `sessions:${ctx.target ?? ""}:${ctx.agent ?? ""}:${ctx.cwd ?? ""}:${ctx.activeSessionId ?? ""}:${ctx.turn ?? 0}`;
  }

  dismissHover(cx, suppressMs = 0) {
    const hadHover = this.hoveredSession !== null;
    this.hoveredSession = null;
    if (suppressMs > 0) {
      this.hoverSuppressedUntil = Date.now() + suppressMs;
    }
    if (hadHover) cx?.notify?.();
  }

  openSessionMenu(session, cx) {
    if (!session) return;
    cx?.stop_propagation?.();
    // Dismiss any hover card immediately before opening the popup menu so the two never overlap.
    this.hoveredSession = null;
    this.hoverSuppressedUntil = Date.now() + 900;
    this.selectSession(session, cx);
    openOverlay({
      mod: "menu",
      kind: "popup",
      width: 300,
      props: {
        tab: "session",
        target: {
          sessionId: session.session_id,
          title: session.title,
          agent: session.agent || this.agent,
          agentName: this.agentName || session.agent || this.agent,
          resumeCommand: session.resume_command,
        },
      },
    });
  }

  selectSession(session, cx) {
    if (!session || !session.session_id) return;
    const changed = this.selectedId !== session.session_id;
    this.selectedId = session.session_id;
    this.selectedSession = session;
    if (changed) {
      this.visibleLimit = INITIAL_VISIBLE_ITEMS;
      this.expandedGroups = new Set();
      this.expandAllSteps = false;
      this.transcriptError = null;
    }
    const loadKey = `${session.agent || this.agent || ""}:${session.session_id}:${session.updated_ms || 0}`;
    if (changed || this.loadedSessionKey !== loadKey) {
      this.loadSessionTranscript(session, resolveActiveCliContext(), cx);
    }
    cx?.notify?.();
  }

  toggleActivityGroup(groupId, cx) {
    if (this.expandAllSteps) {
      // Switching out of expand-all keeps all other groups expanded except the clicked one.
      const all = groupTranscriptItems(this.entries)
        .filter((it) => it.kind === "activity_group")
        .map((it) => it.id);
      this.expandAllSteps = false;
      this.expandedGroups = new Set(all.filter((id) => id !== groupId));
    } else if (this.expandedGroups.has(groupId)) {
      this.expandedGroups.delete(groupId);
    } else {
      this.expandedGroups.add(groupId);
    }
    cx?.notify?.();
  }

  toggleAllActivitySteps(cx) {
    if (this.expandAllSteps || this.expandedGroups.size > 0) {
      this.expandAllSteps = false;
      this.expandedGroups = new Set();
    } else {
      this.expandAllSteps = true;
    }
    cx?.notify?.();
  }

  buildListArgs(ctx) {
    const args = ["session", "ls", "--json"];
    if (ctx.target) args.push(ctx.target);
    if (ctx.cwd) args.push("--cwd", ctx.cwd);
    if (ctx.agent) args.push("--agent", ctx.agent);
    if (!ctx.target && ctx.machine) args.push("--on", ctx.machine);
    return args;
  }

  buildReadArgs(session, ctx) {
    const args = ["session", "read", "--json", String(session.session_id)];
    const ag = session.agent || this.agent || ctx.agent;
    if (ag) args.push("--agent", String(ag));
    if (ctx.target) args.push("--target", ctx.target);
    const cwd = this.cwd || ctx.cwd;
    if (cwd) args.push("--cwd", cwd);
    if (!ctx.target && ctx.machine) args.push("--on", ctx.machine);
    return args;
  }

  refresh(cx, background = false) {
    const ctx = resolveActiveCliContext();
    const nextKey = this.computeContextKey(ctx);
    const keyChanged = nextKey !== this.context;
    if (background && !keyChanged && this.loadingList) return;
    this.context = nextKey;

    if (!ctx.target && !ctx.agent) {
      this.reqSeq += 1;
      this.readSeq += 1;
      this.loadingList = false;
      this.loadingTranscript = false;
      this.sessions = [];
      this.selectedId = null;
      this.selectedSession = null;
      this.hoveredSession = null;
      this.loadedSessionKey = null;
      this.entries = [];
      this.expandedGroups = new Set();
      this.expandAllSteps = false;
      this.agent = null;
      this.agentName = null;
      this.cwd = ctx.cwd || null;
      this.error = tr("Switch to a CLI agent tab to inspect its historical sessions", "切换到 CLI Agent 标签页以查看当前工作区历史会话");
      cx?.notify?.();
      return;
    }

    if (keyChanged && !background) {
      this.error = null;
      this.hoveredSession = null;
    }

    const seq = ++this.reqSeq;
    this.loadingList = true;
    const args = this.buildListArgs(ctx);
    cx.spawn(async (acx) => {
      try {
        const res = await h2(args);
        if (seq !== this.reqSeq) return;
        if (!res || res.code !== 0) {
          this.sessions = [];
          this.entries = [];
          this.selectedSession = null;
          this.loadedSessionKey = null;
          this.error = String(res?.stderr || res?.stdout || tr("Failed to load sessions", "加载历史会话失败")).trim();
          return;
        }
        const parsed = JSON.parse(res.stdout || "{}");
        const list = Array.isArray(parsed.sessions) ? parsed.sessions : [];
        const prevAgent = this.agent;
        this.agent = parsed.agent || ctx.agent || null;
        this.agentName = parsed.agent_name || parsed.agent || ctx.agent || "CLI";
        this.cwd = parsed.cwd || ctx.cwd || null;
        this.sessions = list;
        this.error = null;

        if (prevAgent && this.agent && prevAgent !== this.agent) {
          this.selectedId = null;
          this.loadedSessionKey = null;
          this.expandedGroups = new Set();
          this.expandAllSteps = false;
        }

        let sel = this.selectedId ? list.find((s) => s?.session_id === this.selectedId) : null;
        if (!sel) {
          sel = list[0] ?? null;
          this.selectedId = sel?.session_id ?? null;
          this.visibleLimit = INITIAL_VISIBLE_ITEMS;
          this.expandedGroups = new Set();
          this.expandAllSteps = false;
        }
        this.selectedSession = sel;

        if (sel) {
          const loadKey = `${sel.agent || this.agent || ""}:${sel.session_id}:${sel.updated_ms || 0}`;
          if (this.loadedSessionKey !== loadKey) {
            await this.loadSessionTranscriptAsync(sel, ctx, acx);
          }
        } else {
          this.entries = [];
          this.loadedSessionKey = null;
          this.transcriptError = null;
        }
      } catch (e) {
        if (seq !== this.reqSeq) return;
        this.error = String(e?.message || e || tr("Failed to load sessions", "加载历史会话失败"));
      } finally {
        if (seq === this.reqSeq) {
          this.loadingList = false;
          acx.notify();
        }
      }
    });
  }

  loadSessionTranscript(session, ctx, cx) {
    if (!cx?.spawn) return;
    cx.spawn(async (acx) => {
      await this.loadSessionTranscriptAsync(session, ctx, acx);
    });
  }

  async loadSessionTranscriptAsync(session, ctx, cx) {
    const seq = ++this.readSeq;
    this.loadingTranscript = true;
    this.transcriptError = null;
    cx?.notify?.();
    const args = this.buildReadArgs(session, ctx);
    try {
      const res = await h2(args);
      if (seq !== this.readSeq) return;
      if (!res || res.code !== 0) {
        this.entries = [];
        this.transcriptError = String(res?.stderr || res?.stdout || tr("Failed to read session transcript", "读取会话内容失败")).trim();
        return;
      }
      const parsed = JSON.parse(res.stdout || "{}");
      this.entries = Array.isArray(parsed.entries) ? parsed.entries : [];
      if (parsed.session_id && this.selectedId === session.session_id) {
        this.selectedSession = {
          ...session,
          title: parsed.title || session.title,
          model: parsed.model || session.model,
          resume_command: parsed.resume_command || session.resume_command,
        };
      }
      this.loadedSessionKey = `${session.agent || this.agent || ""}:${session.session_id}:${session.updated_ms || 0}`;
      this.transcriptError = null;
    } catch (e) {
      if (seq !== this.readSeq) return;
      this.entries = [];
      this.transcriptError = String(e?.message || e || tr("Failed to read session transcript", "读取会话内容失败"));
    } finally {
      if (seq === this.readSeq) {
        this.loadingTranscript = false;
        cx?.notify?.();
      }
    }
  }

  filteredSessions() {
    const q = String(this.searchInput?.value?.() ?? "").trim().toLowerCase();
    if (!q) return this.sessions;
    return this.sessions.filter((s) => {
      const title = String(s?.title ?? "").toLowerCase();
      const preview = String(s?.preview ?? "").toLowerCase();
      const sid = String(s?.session_id ?? "").toLowerCase();
      const branch = String(s?.git_branch ?? "").toLowerCase();
      return title.includes(q) || preview.includes(q) || sid.includes(q) || branch.includes(q);
    });
  }

  renderActivityStep(step, t) {
    if (step.kind === "thought") {
      return v_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .px(8)
        .py(5)
        .gap(2)
        .rounded(5)
        .bg(t.background)
        .border_l(2)
        .border_color(t.border)
        .child(
          div()
            .text_size(10)
            .font_medium()
            .text_color(t.dim)
            .child(tr("💭 Thinking", "💭 思考过程")),
        )
        .child(
          div()
            .w_full()
            .min_w(0)
            .text_size(11.5)
            .line_height("17px")
            .text_color(t.muted)
            .whitespace_normal()
            .child(step.text),
        );
    }

    if (step.kind === "tool_call") {
      return h_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .px(7)
        .py(3)
        .gap(6)
        .items_center()
        .rounded(5)
        .bg(t.background)
        .child(div().flex_none().text_size(10).text_color(t.dim).child("▸"))
        .child(div().flex_1().min_w(0).truncate().text_size(11).text_color(t.muted).child(step.title));
    }

    if (step.kind === "plan") {
      return h_flex()
        .id(`sess-step-${step.idx}`)
        .w_full()
        .min_w(0)
        .px(7)
        .py(3)
        .gap(6)
        .items_center()
        .rounded(5)
        .bg(t.background)
        .child(div().flex_none().text_size(10).text_color(t.dim).child("☰"))
        .child(div().flex_1().min_w(0).truncate().text_size(11).text_color(t.muted).child(step.summary));
    }

    return null;
  }

  renderGroupedItem(item, t) {
    if (item.kind === "activity_group") {
      const isOpen = this.expandAllSteps || this.expandedGroups.has(item.id);
      const summaryLabel = formatActivityGroupSummary(item);
      return v_flex()
        .id(`sess-act-${item.id}`)
        .w_full()
        .min_w(0)
        .gap(4)
        .child(
          h_flex()
            .id(`sess-act-toggle-${item.id}`)
            .w_full()
            .min_w(0)
            .h(24)
            .px(8)
            .gap(6)
            .items_center()
            .rounded(6)
            .bg(t.surface ?? t.raised)
            .border_1()
            .border_color(t.border)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => this.toggleActivityGroup(item.id, cx))
            .child(
              div()
                .flex_none()
                .text_size(10)
                .text_color(t.dim)
                .child(isOpen ? "▾" : "▸"),
            )
            .child(
              div()
                .flex_1()
                .min_w(0)
                .truncate()
                .text_size(11)
                .text_color(t.muted)
                .child(summaryLabel),
            )
            .child(
              div()
                .flex_none()
                .text_size(10)
                .text_color(t.dim)
                .child(isOpen ? tr("Collapse", "收起") : tr("Expand", "展开")),
            ),
        )
        .when(isOpen, (el) =>
          el.child(
            v_flex()
              .id(`sess-act-body-${item.id}`)
              .w_full()
              .min_w(0)
              .pl(8)
              .pr(4)
              .py(4)
              .gap(4)
              .rounded(6)
              .bg(t.surface ?? t.raised)
              .children(item.steps.map((st) => this.renderActivityStep(st, t)).filter(Boolean)),
          ),
        );
    }

    if (item.kind === "user") {
      const entry = item.entry;
      const clock = formatEntryClock(entry?.ts_ms);
      const turn = Number(entry?.turn) || 0;
      const body = clipBodyText(entry?.text);
      return v_flex()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(10)
        .py(8)
        .gap(4)
        .rounded(8)
        .bg(t.message ?? t.raised)
        .border_1()
        .border_color(t.border)
        .child(
          h_flex()
            .w_full()
            .gap(6)
            .items_center()
            .child(
              div()
                .text_size(11)
                .font_semibold()
                .text_color(t.accent ?? t.foreground)
                .child(tr("You", "用户")),
            )
            .when(turn > 0, (el) =>
              el.child(
                div()
                  .px(4)
                  .py(0.5)
                  .rounded(4)
                  .bg(t.background)
                  .text_size(9.5)
                  .text_color(t.dim)
                  .child(`T${turn}`),
              ),
            )
            .child(div().flex_1())
            .when(Boolean(clock), (el) => el.child(div().text_size(10).text_color(t.dim).child(clock))),
        )
        .child(
          div()
            .w_full()
            .min_w(0)
            .text_size(12.5)
            .line_height("19px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(body || " "),
        );
    }

    if (item.kind === "assistant") {
      const clock = formatEntryClock(item.entry?.ts_ms);
      return v_flex()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(4)
        .py(4)
        .gap(3)
        .child(
          h_flex()
            .w_full()
            .gap(6)
            .items_center()
            .child(
              div()
                .text_size(11)
                .font_semibold()
                .text_color(t.muted)
                .child(this.agentName || tr("Assistant", "助手")),
            )
            .child(div().flex_1())
            .when(Boolean(clock), (el) => el.child(div().text_size(10).text_color(t.dim).child(clock))),
        )
        .child(
          div()
            .w_full()
            .min_w(0)
            .text_size(12.5)
            .line_height("19px")
            .text_color(t.foreground)
            .whitespace_normal()
            .child(item.body),
        );
    }

    if (item.kind === "error") {
      return div()
        .id(`sess-entry-${item.idx}`)
        .w_full()
        .min_w(0)
        .px(8)
        .py(4)
        .rounded(6)
        .bg(t.raised)
        .text_size(11.5)
        .text_color(t.ansi1 ?? t.foreground)
        .whitespace_normal()
        .child(item.msg);
    }

    return null;
  }

  renderHoverCard(hovered, t) {
    if (!hovered) return null;
    const fullTitle = cleanSingleLine(hovered.title || hovered.session_id, 220);
    const metaParts = [
      `ID: ${shortSessionId(hovered.session_id)}`,
      hovered.git_branch ? `${tr("Branch", "分支")}: ${hovered.git_branch}` : "",
      tr("Right-click to copy full-access resume command", "右键点击可复制 Full Access 恢复命令"),
    ].filter(Boolean);

    return v_flex()
      .id("sessions-hover-tip")
      .absolute()
      .left(8)
      .right(14)
      .when(this.hoverAtBottom, (el) => el.bottom(6))
      .when(!this.hoverAtBottom, (el) => el.top(6))
      .px(9)
      .py(6)
      .gap(2)
      .rounded(7)
      .bg(t.popover ?? t.raised)
      .border_1()
      .border_color(t.border)
      .child(
        div()
          .w_full()
          .min_w(0)
          .text_size(11.5)
          .line_height("16px")
          .font_medium()
          .text_color(t.foreground)
          .whitespace_normal()
          .child(fullTitle),
      )
      .child(
        div()
          .w_full()
          .min_w(0)
          .truncate()
          .text_size(10.5)
          .text_color(t.muted)
          .child(metaParts.join(" · ")),
      );
  }

  render(cx) {
    const liveCtx = resolveActiveCliContext();
    if (this.computeContextKey(liveCtx) !== this.context && (typeof checking !== "function" || !checking())) {
      this.refresh(cx, false);
    }

    const t = theme();
    const accent = t.accent ?? "#3b82f6";
    const filtered = this.filteredSessions();
    const query = String(this.searchInput?.value?.() ?? "");
    const wsFolder = this.cwd || liveCtx.cwd || "";
    const wsName = folderBasename(wsFolder) || tr("Workspace", "当前工作区");
    const agentBadge = this.agentName || liveCtx.agent || "CLI";

    const rowHeight = 36;
    const rowGap = 3;

    const groupedItems = groupTranscriptItems(this.entries);
    const activityGroupCount = groupedItems.reduce((n, it) => n + (it.kind === "activity_group" ? 1 : 0), 0);
    const anyStepsOpen = this.expandAllSteps || this.expandedGroups.size > 0;

    const totalItems = groupedItems.length;
    const hiddenCount = Math.max(0, totalItems - this.visibleLimit);
    const visibleItems = totalItems > 0 ? groupedItems.slice(hiddenCount) : [];
    const renderedEntries = [];
    for (let i = 0; i < visibleItems.length; i++) {
      const node = this.renderGroupedItem(visibleItems[i], t);
      if (node) renderedEntries.push(node);
    }

    const selected = this.selectedSession
      || (this.selectedId ? this.sessions.find((s) => s?.session_id === this.selectedId) : null)
      || null;

    const activeHover =
      this.hoveredSession
      && Date.now() >= this.hoverSuppressedUntil
      && filtered.some((s) => s.session_id === this.hoveredSession.session_id)
        ? this.hoveredSession
        : null;

    const sessionListPane = div()
      .id("sessions-list-pane")
      .size_full()
      .min_h(0)
      .relative()
      .on_size_change((sz) => {
        const h = sz && typeof sz.height === "number" ? sz.height / zoomScale() : 0;
        if (h > 0) this.listPaneHeight = h;
      })
      .on_mouse_move((e, cx) => {
        const rawY = e?.local_position?.y;
        if (typeof rawY === "number" && this.listPaneHeight > 0) {
          this.hasPointerY = true;
          const wantBottom = (rawY / zoomScale()) < this.listPaneHeight * 0.52;
          if (wantBottom !== this.hoverAtBottom) {
            this.hoverAtBottom = wantBottom;
            if (this.hoveredSession) cx.notify();
          }
        }
      })
      .on_hover((inside, cx) => {
        if (!inside) {
          this.hasPointerY = false;
          if (this.hoveredSession) {
            this.hoveredSession = null;
            cx.notify();
          }
        }
      })
      .child(
        v_flex()
          .id("sessions-list")
          .absolute()
          .inset_0()
          .overflow_y_scroll()
          .p(6)
          .gap(rowGap)
          .children(
            filtered.map((s, idx) => {
              const isSel = selected?.session_id === s.session_id;
              const relTime = formatRelativeTime(s.updated_ms || s.created_ms);
              const turns = Number(s.turns) || 0;
              const title = cleanSingleLine(s.title || s.session_id, 120);

              return h_flex()
                .id(`session-row-${idx}`)
                .h(rowHeight)
                .flex_none()
                .w_full()
                .min_w(0)
                .px(8)
                .gap(7)
                .items_center()
                .rounded(7)
                .overflow_hidden()
                .cursor_pointer()
                .when(isSel, (e) => e.bg(t.selected))
                .hover((st) => st.bg(isSel ? t.selected : t.hover))
                .on_hover((hovered, cx) => {
                  if (hovered) {
                    if (Date.now() < this.hoverSuppressedUntil) return;
                    if (this.hoveredSession?.session_id !== s.session_id) {
                      this.hoveredSession = s;
                      if (!this.hasPointerY && this.listPaneHeight > 0) {
                        const estCenterY = 6 + idx * (rowHeight + rowGap) + rowHeight * 0.5;
                        this.hoverAtBottom = estCenterY < this.listPaneHeight * 0.52;
                      }
                      cx.notify();
                    }
                  } else if (this.hoveredSession?.session_id === s.session_id) {
                    this.hoveredSession = null;
                    cx.notify();
                  }
                })
                .on_mouse_down("left", (_e, cx) => this.dismissHover(cx, 300))
                .on_click((_e, cx) => this.selectSession(s, cx))
                .on_mouse_down("right", (_e, cx) => this.openSessionMenu(s, cx))
                .child(
                  div()
                    .w(3)
                    .h(16)
                    .flex_none()
                    .rounded(999)
                    .bg(isSel ? accent : (s.current ? (t.ansi2 ?? accent) : "#00000000")),
                )
                .child(
                  div()
                    .flex_1()
                    .min_w(0)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .truncate()
                    .text_size(12.5)
                    .line_height("20px")
                    .font_medium()
                    .text_color(isSel ? t.foreground : (t.foreground ?? "#f4f4f5"))
                    .child(title),
                )
                .when(Boolean(s.current), (e) =>
                  e.child(
                    div()
                      .flex_none()
                      .whitespace_nowrap()
                      .px(5)
                      .py(1)
                      .rounded(4)
                      .bg(t.raised)
                      .text_size(10)
                      .text_color(t.ansi2 ?? accent)
                      .child(tr("Current", "当前")),
                  ),
                )
                .when(turns > 0, (e) =>
                  e.child(
                    div()
                      .flex_none()
                      .whitespace_nowrap()
                      .text_size(10.5)
                      .text_color(t.dim)
                      .child(`T${turns}`),
                  ),
                )
                .when(Boolean(relTime), (e) =>
                  e.child(
                    div()
                      .flex_none()
                      .whitespace_nowrap()
                      .text_size(10.5)
                      .text_color(t.dim)
                      .child(relTime),
                  ),
                );
            }),
          )
          .when(filtered.length === 0, (el) =>
            el.child(
              div()
                .p(10)
                .text_size(12)
                .text_color(t.dim)
                .whitespace_normal()
                .child(
                  this.error
                    || (this.loadingList
                      ? tr("Scanning workspace sessions…", "正在扫描当前工作区历史会话…")
                      : query.length > 0
                        ? tr("No matching sessions", "未找到匹配的历史会话")
                        : tr("No historical sessions found for this workspace and CLI agent", "当前工作区与当前 CLI Agent 暂无历史会话")),
                ),
            ),
          ),
      )
      .child(Scrollbar.vertical("sessions-list").absolute().inset_0())
      .when(Boolean(activeHover), (el) => el.child(this.renderHoverCard(activeHover, t)));

    const transcriptDetailPane = v_flex()
      .id("sessions-detail-pane")
      .size_full()
      .min_w(0)
      .min_h(0)
      // Selected session toolbar
      .child(
        h_flex()
          .id("sessions-detail-toolbar")
          .h(30)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(10)
          .gap(6)
          .items_center()
          .bg(t.raised)
          .border_b(1)
          .border_color(t.border)
          .when(Boolean(selected), (el) =>
            el.on_mouse_down("right", (_e, cx) => this.openSessionMenu(selected, cx)),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11)
              .font_medium()
              .text_color(t.muted)
              .child(
                selected
                  ? cleanSingleLine(selected.title || selected.session_id, 100)
                  : tr("Conversation History", "会话内容"),
              ),
          )
          .when(activityGroupCount > 0, (el) =>
            el.child(
              div()
                .id("sessions-toggle-steps")
                .flex_none()
                .px(6)
                .py(1.5)
                .rounded(4)
                .bg(anyStepsOpen ? (t.selected ?? t.background) : t.background)
                .border_1()
                .border_color(t.border)
                .text_size(10)
                .text_color(anyStepsOpen ? t.foreground : t.muted)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover).text_color(t.foreground))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => this.toggleAllActivitySteps(cx))
                .child(anyStepsOpen ? tr("Hide steps", "折叠过程") : tr("Show steps", "展开过程")),
            ),
          )
          .when(Boolean(selected?.model), (el) =>
            el.child(
              div()
                .flex_none()
                .max_w(130)
                .truncate()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(t.background)
                .text_size(10)
                .text_color(t.dim)
                .child(String(selected.model)),
            ),
          )
          .when(Boolean(selected?.session_id), (el) =>
            el.child(
              div()
                .flex_none()
                .px(5)
                .py(1)
                .rounded(4)
                .bg(t.background)
                .text_size(10)
                .text_color(t.dim)
                .child(shortSessionId(selected.session_id)),
            ),
          ),
      )
      // Conversation content viewer
      .child(
        div()
          .flex_1()
          .w_full()
          .min_w(0)
          .min_h(0)
          .relative()
          .overflow_hidden()
          .when(Boolean(selected), (el) =>
            el.on_mouse_down("right", (_e, cx) => this.openSessionMenu(selected, cx)),
          )
          .child(
            v_flex()
              .id("sessions-transcript")
              .absolute()
              .inset_0()
              .overflow_y_scroll()
              .p(10)
              .gap(8)
              .when(hiddenCount > 0, (el) =>
                el.child(
                  div()
                    .id("sessions-load-earlier")
                    .w_full()
                    .py(5)
                    .px(8)
                    .rounded(6)
                    .bg(t.raised)
                    .border_1()
                    .border_color(t.border)
                    .text_size(11)
                    .text_color(t.muted)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => {
                      this.visibleLimit += VISIBLE_ITEMS_STEP;
                      cx.notify();
                    })
                    .child(
                      tr(
                        `Show earlier items (${hiddenCount} hidden)`,
                        `显示更早内容（还有 ${hiddenCount} 项）`,
                      ),
                    ),
                ),
              )
              .children(renderedEntries)
              .when(renderedEntries.length === 0, (el) =>
                el.child(
                  div()
                    .p(8)
                    .text_size(12)
                    .text_color(t.dim)
                    .whitespace_normal()
                    .child(
                      this.transcriptError
                        || (this.loadingTranscript
                          ? tr("Loading session conversation…", "正在加载历史对话内容…")
                          : selected
                            ? tr("No text messages recorded in this session", "该会话暂无可展示的对话消息")
                            : tr("Select a session above to view its conversation history", "在上方选择一条历史会话以查看对话内容")),
                    ),
                ),
              ),
          )
          .child(Scrollbar.vertical("sessions-transcript").absolute().inset_0()),
      );

    return v_flex()
      .id("sessions")
      .size_full()
      .min_h(0)
      .rounded(10)
      .overflow_hidden()
      .bg(t.background)
      .border_1()
      .border_color(t.border)
      .text_color(t.foreground)
      .text_size(13)
      .track_focus(this.handle)
      // Header bar: agent + workspace badge + refresh
      .child(
        h_flex()
          .id("sessions-header")
          .h(36)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(10)
          .gap(6)
          .items_center()
          .bg(t.raised)
          .border_b(1)
          .border_color(t.border)
          .child(
            div()
              .flex_none()
              .px(6)
              .py(1.5)
              .rounded(5)
              .bg(t.selected ?? t.background)
              .text_size(11)
              .font_semibold()
              .text_color(t.foreground)
              .child(agentBadge),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_size(11.5)
              .text_color(t.muted)
              .tooltip(wsFolder || wsName)
              .child(wsName),
          )
          .when(this.sessions.length > 0, (el) =>
            el.child(
              div()
                .flex_none()
                .px(5)
                .py(1)
                .rounded(999)
                .bg(t.background)
                .text_size(10.5)
                .text_color(t.dim)
                .child(String(filtered.length)),
            ),
          )
          .child(
            div()
              .id("sessions-refresh")
              .size(24)
              .flex_none()
              .flex()
              .items_center()
              .justify_center()
              .rounded(5)
              .text_color(t.muted)
              .cursor_pointer()
              .hover((s) => s.bg(t.hover).text_color(t.foreground))
              .tooltip(tr("Refresh sessions", "刷新历史会话"))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click((_e, cx) => this.refresh(cx, false))
              .child(
                svg(this.loadingList ? "mods/workspace-chrome/spinner.svg" : "frame/settings/refresh.svg")
                  .size(13)
                  .when(this.loadingList, (s) => s.spin(900)),
              ),
          ),
      )
      // Search bar
      .child(
        h_flex()
          .id("sessions-search-bar")
          .h(34)
          .flex_none()
          .w_full()
          .min_w(0)
          .px(8)
          .gap(6)
          .items_center()
          .border_b(1)
          .border_color(t.border)
          .bg(t.surface ?? t.background)
          .child(
            h_flex()
              .id("sessions-search-wrap")
              .flex_1()
              .min_w(0)
              .h(24)
              .px(8)
              .gap(5)
              .items_center()
              .rounded(6)
              .bg(t.background)
              .border_1()
              .border_color(this.searchFocused ? accent : t.border)
              .cursor_text()
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .child(Input.new(this.searchInput).h_full().text_size(11.5).text_color(t.foreground))
              .when(query.length > 0, (el) =>
                el.child(
                  div()
                    .id("sessions-search-clear")
                    .flex_none()
                    .px(4)
                    .rounded(4)
                    .text_size(11)
                    .text_color(t.dim)
                    .cursor_pointer()
                    .hover((s) => s.bg(t.hover).text_color(t.foreground))
                    .on_click((_e, cx) => {
                      this.searchInput.set_value("");
                      cx.notify();
                    })
                    .child("×"),
                ),
              ),
          ),
      )
      // Vertically resizable split between session list (top) and conversation detail (bottom)
      .child(
        v_resizable("sessions-split")
          .flex_1()
          .min_h(0)
          .w_full()
          .on_resize((sizes) => {
            if (Array.isArray(sizes) && typeof sizes[0] === "number") {
              const clamped = Math.max(MIN_SPLIT_TOP_HEIGHT, Math.min(MAX_SPLIT_TOP_HEIGHT, Math.round(sizes[0])));
              this.splitTopHeight = clamped;
              try {
                if (typeof localStorage !== "undefined" && localStorage?.setItem) {
                  localStorage.setItem("sessions.topHeight", String(clamped));
                }
              } catch {}
            }
          })
          .child(
            resizable_panel()
              .size(this.splitTopHeight)
              .size_range(MIN_SPLIT_TOP_HEIGHT, MAX_SPLIT_TOP_HEIGHT)
              .child(sessionListPane),
          )
          .child(
            resizable_panel()
              .size_range(120)
              .child(transcriptDetailPane),
          ),
      );
  }
}
