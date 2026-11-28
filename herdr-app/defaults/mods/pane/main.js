// A pane's header, for layouts that frame their panes:
//   { "tab": "active", "pane": { "header": { "content": { "mod": "pane" } } } }
// Shows the program's icon (the app's program table), the agent's name or
// the terminal's title, the folder, and the agent's status; buttons split, zoom and close
// this pane. Props from the app: { pane, terminal }. Click: focus the pane.

import { View, div, svg, image } from "gpui-kit";
import { v_flex, h_flex, InputState, Input } from "gpui-base";
import { panes, terminals, spaces, props, run, h2, exec, openOverlay, theme, home, layout, settings, shared, keys, window as appWindow } from "herdr";

const isZh = () => typeof settings === "function" && String(settings()?.language ?? "en").toLowerCase().startsWith("zh");
const tr = (en, zh) => (isZh() ? zh : en);

function shortcutFor(action) {
  if (typeof keys !== "function" || !action) return "";
  const list = keys();
  if (!Array.isArray(list)) return "";
  const k = list.find((b) => b.section === "global" && b.action === action);
  if (!k?.keys?.trim()) return "";
  const mac = typeof appWindow === "function" && appWindow()?.platform === "macos";
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

const GIT_STATUS_ARGS = ["--no-optional-locks", "-c", "core.fsmonitor=false", "status", "--porcelain=v2", "--branch"];

function parseGitPorcelainV2(stdout) {
  let branch = "";
  let ahead = 0;
  let behind = 0;
  let staged = 0;
  let modified = 0;
  let untracked = 0;
  let conflicts = 0;
  const files = [];
  for (const rawLine of String(stdout || "").split("\n")) {
    const line = rawLine.trimEnd();
    if (!line) continue;
    if (line.startsWith("# branch.head ")) {
      branch = line.slice("# branch.head ".length).trim();
    } else if (line.startsWith("# branch.ab ")) {
      const m = /\+(\d+)\s+-(\d+)/.exec(line);
      if (m) {
        ahead = Number(m[1]) || 0;
        behind = Number(m[2]) || 0;
      }
    } else if (line.startsWith("u ")) {
      conflicts += 1;
      const p = line.split(" ").slice(10).join(" ");
      if (p && files.length < 10) files.push(`! ${p}`);
    } else if (line.startsWith("1 ") || line.startsWith("2 ")) {
      const xy = line.slice(2, 4);
      const x = xy[0] || ".";
      const y = xy[1] || ".";
      if (x !== ".") staged += 1;
      if (y !== ".") modified += 1;
      const parts = line.split(" ");
      const pathPart = line.startsWith("2 ") ? parts.slice(9).join(" ").split("\t")[0] : parts.slice(8).join(" ");
      if (pathPart && files.length < 10) files.push(`${xy} ${pathPart}`);
    } else if (line.startsWith("? ")) {
      untracked += 1;
      const p = line.slice(2).trim();
      if (p && files.length < 10) files.push(`?? ${p}`);
    }
  }
  let changedFiles = 0;
  for (const rawLine of String(stdout || "").split("\n")) {
    if (rawLine.startsWith("1 ") || rawLine.startsWith("2 ") || rawLine.startsWith("u ") || rawLine.startsWith("? ")) {
      changedFiles += 1;
    }
  }
  return { branch, ahead, behind, staged, modified, untracked, conflicts, changedFiles, files, ok: true };
}

function formatGitSummary(info, fallbackBranch) {
  const branch = info?.ok ? (info.branch === "(detached)" ? tr("detached", "游离 HEAD") : info.branch) : (fallbackBranch || "");
  if (!branch && !info?.ok) return null;
  const abParts = [];
  if (info?.ahead > 0) abParts.push(`↑${info.ahead}`);
  if (info?.behind > 0) abParts.push(`↓${info.behind}`);
  const ab = abParts.length ? ` ${abParts.join(" ")}` : "";
  if (!info?.ok) {
    return {
      text: `${branch}`,
      statusKind: "unknown",
      tooltip: tr(`Git branch: ${branch} — click to open Changes`, `Git 分支：${branch} — 点击打开右侧「变更」面板`),
    };
  }
  if (info.conflicts > 0) {
    const label = tr(`${info.conflicts} conflicts`, `${info.conflicts} 处冲突`);
    return {
      text: `${branch}${ab} · ${label}`,
      statusKind: "conflict",
      tooltip: [
        `${branch}${ab} · ${label}`,
        ...info.files,
        tr("Click to open Changes panel", "点击打开右侧「变更」面板"),
      ].join("\n"),
    };
  }
  if (info.changedFiles === 0) {
    const cleanText = tr("clean", "干净");
    return {
      text: `${branch}${ab} · ✓ ${cleanText}`,
      statusKind: "clean",
      tooltip: `${branch}${ab} · ${tr("Working tree clean", "工作区干净")} — ${tr("Click to open Changes panel", "点击打开右侧「变更」面板")}`,
    };
  }
  const details = [];
  if (info.staged > 0) details.push(tr(`${info.staged} staged`, `暂存 ${info.staged}`));
  if (info.modified > 0) details.push(tr(`${info.modified} mod`, `修改 ${info.modified}`));
  if (info.untracked > 0) details.push(tr(`${info.untracked} new`, `新增 ${info.untracked}`));
  const detailStr = details.length > 0 ? ` (${details.join(" · ")})` : "";
  const countStr = tr(`${info.changedFiles} changed`, `${info.changedFiles} 改动`);
  return {
    text: `${branch}${ab} · ${countStr}${detailStr}`,
    statusKind: "dirty",
    tooltip: [
      `${branch}${ab} · ${countStr}${detailStr}`,
      ...info.files,
      info.changedFiles > info.files.length ? tr(`… and ${info.changedFiles - info.files.length} more`, `… 其余 ${info.changedFiles - info.files.length} 个文件`) : "",
      tr("Click to open Changes panel", "点击打开右侧「变更」面板"),
    ].filter(Boolean).join("\n"),
  };
}

function buildWorkSummary(p, term, isAgent, displayName) {
  if (!isAgent) {
    const text = p.title && p.title !== displayName ? p.title : tr("Interactive terminal", "交互式终端");
    return { text, tone: "dim", tooltip: text };
  }
  const st = p.manual_status ?? term?.manual_status ?? p.status ?? term?.status ?? "idle";
  const manual = Boolean(p.manual_status || term?.manual_status);
  const permTitle = term?.pending_permission?.title ? String(term.pending_permission.title).trim() : "";
  const micro = String(term?.micro_summary ?? p.micro_summary ?? "").trim();
  const semantic = String(term?.semantic_title ?? p.semantic_title ?? "").trim();
  const aiSummary = !manual ? String(term?.ai_status_summary ?? p.ai_status_summary ?? "").trim() : "";
  const turn = Number(term?.turn ?? 0);

  let text = "";
  let tone = "muted";
  const semPrefix = semantic && semantic !== displayName ? `${semantic} · ` : "";
  if (permTitle && !manual) {
    text = tr(`Waiting approval: ${permTitle}`, `等待审批：${permTitle}`);
    tone = "blocked";
  } else if (micro) {
    const prefix = semPrefix && !micro.startsWith(semantic) ? semPrefix : "";
    text = `${prefix}${micro}`;
    tone = st === "working" ? "working" : st === "blocked" ? "blocked" : st === "done" ? "done" : "muted";
  } else if (aiSummary) {
    text = aiSummary;
    tone = st === "blocked" ? "blocked" : "muted";
  } else if (st === "working") {
    const base = turn > 0
      ? tr(`Working on turn ${turn}…`, `正在执行中（第 ${turn} 轮）…`)
      : tr("Working in progress…", "正在执行任务…");
    text = `${semPrefix}${base}`;
    tone = "working";
  } else if (st === "done") {
    text = `${semPrefix}${tr("Completed — ready to inspect changes", "本轮已完成，可检视改动 Diff")}`;
    tone = "done";
  } else if (st === "blocked") {
    text = `${semPrefix}${tr("Paused — waiting for your input or confirmation", "已暂停，等待你的输入或确认")}`;
    tone = "blocked";
  } else if (st === "ended") {
    text = tr("Session ended", "会话已结束");
    tone = "dim";
  } else {
    text = semantic
      ? `${semantic} · ${tr("Idle, ready for next prompt", "空闲中，随时可发新指令")}`
      : tr("Idle — ready for next prompt", "空闲中，随时可发新指令");
    tone = "dim";
  }

  const tipLines = [];
  if (semantic) tipLines.push(tr(`Task: ${semantic}`, `任务：${semantic}`));
  tipLines.push(text);
  if (turn > 0) tipLines.push(tr(`Turn ${turn}`, `当前第 ${turn} 轮`));
  return { text, tone, tooltip: tipLines.join("\n") };
}

export default class Pane extends View {
  init(_props, cx) {
    this.hovered = false;
    this.editing = false;
    this.editingTerminal = null;
    this.renameText = "";
    this.localWorkingSince = {};
    this.localWorkMs = {};
    this.gitStatus = null;
    this.gitKey = "";
    this.gitLoading = false;
    this.gitLastAt = 0;
    this.renameInput = InputState.new({
      placeholder: tr("Pane name (empty: default)", "分栏名称（留空恢复默认）"),
    });
    this.renameInput.on("change", () => {
      this.renameText = this.renameInput.value();
    });
    this.renameInput.on("submit", (_e, cx) => this.commitRename(cx));
    this.renameInput.on("blur", (_e, cx) => this.commitRename(cx));
    this.handle = cx?.focus_handle?.() ?? null;
    this.refreshGit(cx, true);
    cx?.timer?.every?.(1000, (tcx) => {
      const p = this.me();
      if (p?.status === "working" && (p.acp || p.structured || p.program?.agent)) {
        tcx?.notify?.();
      }
    });
    cx?.timer?.every?.(8000, (tcx) => this.refreshGit(tcx, false));
  }

  gitScope(p) {
    const cwd = String(p?.cwd || "").trim();
    if (!cwd) return null;
    const addr = String(p?.terminal || "");
    const slash = addr.indexOf("/");
    const on = slash > 0 ? addr.slice(0, slash) : (spaces().find((s) => s.active)?.machine ?? null);
    return { cwd, on };
  }

  refreshGit(cx, force = false) {
    if (this.gitLoading || typeof exec !== "function" || typeof cx?.spawn !== "function") return;
    const p = this.me();
    const scope = this.gitScope(p);
    if (!scope) return;
    const syncToken = String(shared("checkpoint.sync") ?? "");
    const key = `${scope.on ?? ""}|${scope.cwd}|${p?.status ?? ""}|${syncToken}`;
    const now = Date.now();
    if (!force && this.gitKey === key && now - this.gitLastAt < 6000) return;
    this.gitKey = key;
    this.gitLoading = true;
    this.gitLastAt = now;
    cx.spawn(async (acx) => {
      try {
        const res = await exec("git", GIT_STATUS_ARGS, scope.on ? { cwd: scope.cwd, on: scope.on } : { cwd: scope.cwd });
        if (this.gitKey !== key) return;
        if (res && res.code === 0) {
          this.gitStatus = parseGitPorcelainV2(res.stdout);
        } else {
          this.gitStatus = { ok: false };
        }
      } catch {
        if (this.gitKey === key) this.gitStatus = { ok: false };
      } finally {
        this.gitLoading = false;
        acx?.notify?.();
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

  button(t, id, icon, command, args, tip = null, danger = false) {
    return div()
      .id(id)
      .p(3)
      .rounded(4)
      .text_color(t.dim)
      .cursor_pointer()
      .hover((s) => s.bg(t.hover).text_color(danger ? (t.blocked ?? "#ef4444") : t.foreground))
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
    const rawLineageParent = (term?.lineage?.parent || "").trim();
    const curSlash = typeof p.terminal === "string" ? p.terminal.lastIndexOf("/") : -1;
    const lineageParent = rawLineageParent
      ? (!rawLineageParent.includes("/") && curSlash > 0 ? `${p.terminal.slice(0, curSlash + 1)}${rawLineageParent}` : rawLineageParent)
      : null;
    const rawTermParent = typeof term?.parent === "string" ? term.parent.trim() : "";
    const termParentAddr = rawTermParent
      ? (!rawTermParent.includes("/") && curSlash > 0 ? `${p.terminal.slice(0, curSlash + 1)}${rawTermParent}` : rawTermParent)
      : null;
    const upstreamAddr = lineageParent || parent?.address || termParentAddr || null;
    const upstreamTerm = upstreamAddr
      ? (parent?.address === upstreamAddr ? parent : terminals().find((x) => x.address === upstreamAddr) ?? null)
      : null;
    const upstreamCustom = upstreamTerm?.labels?.["herdr.name"] || upstreamTerm?.name || null;
    const upstreamLabel = upstreamCustom && upstreamCustom !== upstreamAddr
      ? `${upstreamCustom} (${upstreamAddr})`
      : upstreamAddr;
    const rawLineageKind = String(term?.lineage?.kind || "").toLowerCase();
    const lineageKindInfo = rawLineageKind === "fork"
      ? { label: tr("Fork", "会话分叉"), color: "#a855f7", tip: tr("Forked session from upstream", "由上游会话分叉而来") }
      : rawLineageKind === "side-chat" || rawLineageKind === "side_chat"
        ? { label: tr("Side Chat", "旁路讨论"), color: "#14b8a6", tip: tr("Read-only side-channel discussion", "隔离的旁路讨论会话") }
        : rawLineageKind === "handoff"
          ? { label: tr("Handoff", "上下文接力"), color: "#3b82f6", tip: tr("Context handoff from upstream", "由上游会话上下文接力而来") }
          : null;
    const worktreePath = term?.lineage?.worktree_path || null;
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
    const elapsedKey = p.terminal || pane;
    if (!isAgent) {
      delete this.localWorkingSince[elapsedKey];
      delete this.localWorkMs[elapsedKey];
    }
    const elapsed = isAgent
      ? this.agentElapsed(
          elapsedKey,
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
    // Trigger git refresh lazily when cwd / status / checkpoint sync changes.
    this.refreshGit(cx, false);
    const workSummary = buildWorkSummary(p, term, isAgent, name);
    const gitSummary = formatGitSummary(this.gitStatus, p.branch);
    const workDotColor =
      workSummary.tone === "working"
        ? (t.working ?? accent)
        : workSummary.tone === "blocked"
          ? (t.blocked ?? "#f59e0b")
          : workSummary.tone === "done"
            ? (t.done ?? "#22c55e")
            : t.dim;
    const gitColor =
      gitSummary?.statusKind === "conflict"
        ? "#ef4444"
        : gitSummary?.statusKind === "dirty"
          ? "#f59e0b"
          : gitSummary?.statusKind === "clean"
            ? (t.done ?? "#22c55e")
            : t.muted;
    const topRow = h_flex()
      .w_full()
      .h(30)
      .px(10)
      .gap(8)
      .items_center()
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
      .when((isAgent || isStructured) && Boolean(p.terminal), (el) =>
        el.child(
          div()
            .id(`ph-addr-${pane}`)
            .flex_none()
            .px(5)
            .py(1)
            .rounded(4)
            .bg(t.raised)
            .border_1()
            .border_color(t.border)
            .text_size(10.5)
            .text_color(t.muted)
            .tooltip(tr(`Agent ID: ${p.terminal}`, `智能体编号：${p.terminal}`))
            .child(p.terminal),
        ),
      )
      .when(Boolean(lineageKindInfo), (el) =>
        el.child(
          div()
            .id(`ph-kind-${pane}`)
            .flex_none()
            .px(5)
            .h(18)
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha(lineageKindInfo.color, "20") ?? t.raised)
            .border_1()
            .border_color(alpha(lineageKindInfo.color, "66") ?? t.border)
            .text_size(10)
            .font_semibold()
            .text_color(lineageKindInfo.color)
            .tooltip(lineageKindInfo.tip)
            .child(lineageKindInfo.label),
        ),
      )
      .when(Boolean(worktreePath), (el) =>
        el.child(
          div()
            .id(`ph-wt-${pane}`)
            .flex_none()
            .px(5)
            .h(18)
            .flex()
            .items_center()
            .rounded(4)
            .bg(alpha("#f59e0b", "20") ?? t.raised)
            .border_1()
            .border_color(alpha("#f59e0b", "66") ?? t.border)
            .text_size(10)
            .font_semibold()
            .text_color("#f59e0b")
            .tooltip(tr(`Isolated Git Worktree: ${worktreePath}`, `独立 Git Worktree 隔离目录：${worktreePath}`))
            .child("Worktree"),
        ),
      )
      .when(Boolean(upstreamAddr), (el) =>
        el.child(
          h_flex()
            .id(`ph-lin-${pane}`)
            .flex_none()
            .h(20)
            .px(6)
            .gap(3)
            .items_center()
            .rounded(5)
            .bg(alpha(accent, "14") ?? t.raised)
            .border_1()
            .border_color(alpha(accent, "44") ?? t.border)
            .text_size(10.5)
            .font_medium()
            .text_color(accent)
            .cursor_pointer()
            .hover((s) => s.bg(alpha(accent, "28") ?? t.hover).text_color(t.foreground))
            .tooltip(tr(`Click to return to upstream agent ${upstreamAddr}`, `点击返回上游智能体 ${upstreamAddr}`))
            .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
            .on_click((_e, cx) => {
              cx.stop_propagation();
              run("agent.focus", { terminal: upstreamAddr });
            })
            .child(tr(`↳ from ${upstreamLabel}`, `↳ 来自 ${upstreamLabel}`)),
        ),
      )
      .child(div().flex_1().min_w(0).truncate().text_color(t.dim).child(folder(p.cwd)))
      .when(isAgent && !isStructured && st !== "ended" && Boolean(p.terminal), el => {
        const isPolishing = shared("polish.active") === p.terminal;
        const polishUndoMap = shared("polish.undo");
        const canUndoPolish = !isPolishing && Boolean(polishUndoMap && typeof polishUndoMap === "object" && polishUndoMap[p.terminal]);
        return el
          .child(
            h_flex()
              .id(`ph-polish-${pane}`)
              .flex_none()
              .h(20)
              .px(7)
              .gap(4)
              .items_center()
              .rounded(6)
              .bg(alpha("#a855f7", isPolishing ? "2e" : "1a") ?? t.raised)
              .border_1()
              .border_color(alpha("#a855f7", isPolishing ? "99" : "55") ?? t.border)
              .text_size(11)
              .font_medium()
              .text_color("#a855f7")
              .cursor_pointer()
              .hover((s) => s.bg(alpha("#a855f7", "2e") ?? t.hover))
              .tooltip(withShortcut(tr("Polish current CLI input in-place with Core AI", "使用 Core AI 直接润色当前 CLI Agent 中的输入"), "prompt.polish"))
              .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
              .on_click((_e, cx) => {
                cx.stop_propagation();
                if (!isPolishing) run("prompt.polish", { terminal: p.terminal });
              })
              .child(
                isPolishing
                  ? svg("mods/workspace-chrome/spinner.svg").size(11).flex_none().spin(900).text_color("#a855f7")
                  : svg("mods/peek/sparkles.svg").size(11).flex_none().text_color("#a855f7"),
              )
              .child(isPolishing ? tr("Polishing…", "润色中…") : tr("Polish", "润色输入")),
          )
          .when(canUndoPolish, (e2) =>
            e2.child(
              h_flex()
                .id(`ph-polish-undo-${pane}`)
                .flex_none()
                .h(20)
                .px(7)
                .gap(4)
                .items_center()
                .rounded(6)
                .bg(t.raised)
                .border_1()
                .border_color(alpha("#a855f7", "55") ?? t.border)
                .text_size(11)
                .font_medium()
                .text_color(t.foreground)
                .cursor_pointer()
                .hover((s) => s.bg(t.hover))
                .tooltip(withShortcut(tr("Undo last polish and restore original input", "撤销本次润色并恢复润色前的内容"), "prompt.polish_undo"))
                .on_mouse_down("left", (_e, cx) => cx.stop_propagation())
                .on_click((_e, cx) => {
                  cx.stop_propagation();
                  run("prompt.polish_undo", { terminal: p.terminal });
                })
                .child(tr("↩ Undo", "↩ 反悔恢复")),
            ),
          );
      })
      .when(showHandoffBtns && Boolean(lineageParent), (el) =>
        el.child(
          headerPill(
            `ph-merge-${pane}`,
            tr("Merge", "合回"),
            "mods/peek/corner-up-right.svg",
            "agent.handoff",
            worktreePath
              ? { source: p.terminal, kind: "merge-back" }
              : { source: p.terminal, into: lineageParent, kind: "merge-back" },
            worktreePath
              ? tr(`Notify ${lineageParent} to merge Worktree branch (keeps worktree)`, `通知 ${lineageParent} 合并 Worktree 分支代码（保留 Worktree 与分支）`)
              : tr(`Merge conclusions back into ${lineageParent}`, `一键将结论合回 ${lineageParent}`),
            true,
          ),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el.child(
          headerPill(
            `ph-side-${pane}`,
            tr("Side", "旁路"),
            "mods/peek/messages-square.svg",
            "agent.handoff",
            { source: p.terminal, kind: "side-chat" },
            tr("Open Side Chat in the right panel (read-only)", "在右侧面板打开旁路讨论（只读参谋，不改代码）"),
          ),
        ),
      )
      .when(showHandoffBtns && String(term?.lineage?.kind || "").toLowerCase().replace("_", "-") !== "side-chat", (el) =>
        el.child(
          headerPill(
            `ph-fork-${pane}`,
            tr("Fork", "分叉"),
            "mods/workspace-chrome/git-branch.svg",
            "agent.handoff",
            { source: p.terminal, kind: "fork" },
            tr("Fork Session (optional Git Worktree isolation)", "会话分叉（支持原生无损克隆与可选 Worktree 隔离）"),
          ),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el.child(
          headerPill(
            `ph-ho-${pane}`,
            tr("Handoff", "接力"),
            "mods/peek/corner-up-right.svg",
            "agent.handoff",
            { source: p.terminal, kind: "handoff" },
            withShortcut(tr("Context Handoff to another agent / provider", "上下文接力给其他 Agent 或模型继续执行"), "agent.handoff"),
          ),
        ),
      )
      .when(showHandoffBtns, (el) =>
        el.child(
          headerPill(
            `ph-send-ctx-${pane}`,
            tr("Send Context", "发送上下文"),
            "mods/peek/send.svg",
            "agent.handoff",
            { source: p.terminal, kind: "send-context" },
            tr(
              "Send recent conversation context (with optional note) directly to an open CLI or ACP agent",
              "将当前会话的近期上下文直接发送给已开启的 CLI / ACP Agent（可附带备注信息）",
            ),
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
          .tooltip(withShortcut(isStructured ? tr("Switch back to raw terminal view", "切回原始终端视图") : tr("Switch to structured chat view", "切换到结构化对话视图"), "terminal.view:toggle"))
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
            .tooltip(tr("Stop generation (Esc)", "停止生成 (Esc)"))
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
        .child(this.button(t, `ph-r-${pane}`, "split-right", "pane.split", { direction: "right", pane }, withShortcut(tr("Split right", "向右分栏"), "pane.split:right")))
        .child(this.button(t, `ph-d-${pane}`, "split-down", "pane.split", { direction: "down", pane }, withShortcut(tr("Split down", "向下分栏"), "pane.split:down")))
        .when(tabPaneCount > 1, el => el.child(this.button(t, `ph-z-${pane}`, "zoom", "pane.detach", { pane }, withShortcut(tr("Maximize to new tab", "最大化为独立标签页"), "pane.detach"))))
        .when(Boolean(p.terminal), el => el.child(this.button(t, `ph-trash-${pane}`, "trash", "agent.trash", { terminal: p.terminal }, tr("Move to recycle bin", "移入回收站"), true)))
        .child(this.button(t, `ph-x-${pane}`, "close", "pane.close", { pane }, withShortcut(isAgent && st !== "ended" ? tr("Close tab (keep agent in background)", "关闭标签页（Agent 保留在后台）") : tr("Close pane", "关闭窗格"), "pane.close"))));

    const summaryRow = h_flex()
      .id(`ph-brief-row-${pane}`)
      .w_full()
      .flex_1()
      .min_h(22)
      .px(10)
      .gap(10)
      .items_center()
      .bg(t.raised)
      .border_t(1)
      .border_color(t.border)
      .text_size(11)
      .child(
        h_flex()
          .id(`ph-work-summary-${pane}`)
          .flex_1()
          .min_w(0)
          .gap(6)
          .items_center()
          .tooltip(workSummary.tooltip)
          .child(
            workSummary.tone === "working"
              ? svg("mods/workspace-chrome/spinner.svg").size(10).flex_none().text_color(workDotColor).spin(900)
              : div().size(6).flex_none().rounded(3).bg(workDotColor),
          )
          .child(
            div()
              .flex_1()
              .min_w(0)
              .truncate()
              .text_color(workSummary.tone === "dim" ? t.dim : t.muted)
              .child(workSummary.text),
          ),
      )
      .when(Boolean(gitSummary), (el) =>
        el.child(
          h_flex()
            .id(`ph-git-${pane}`)
            .flex_none()
            .h(18)
            .px(6)
            .gap(4)
            .items_center()
            .rounded(5)
            .bg(gitSummary.statusKind === "clean" || gitSummary.statusKind === "unknown" ? t.background : (alpha(gitColor, "18") ?? t.background))
            .border_1()
            .border_color(gitSummary.statusKind === "clean" || gitSummary.statusKind === "unknown" ? t.border : (alpha(gitColor, "55") ?? t.border))
            .text_size(10.5)
            .font_medium()
            .text_color(gitColor)
            .cursor_pointer()
            .hover((s) => s.bg(t.hover))
            .tooltip(gitSummary.tooltip)
            .on_mouse_down("left", (_e, evCx) => evCx.stop_propagation())
            .on_click((_e, evCx) => {
              evCx.stop_propagation();
              run("layout.select", { stack: "panel-body", child: "changes" });
              run("layout.show", { id: "panel" });
            })
            .child(svg("mods/workspace-chrome/git-branch.svg").size(10.5).flex_none().text_color(gitColor))
            .child(gitSummary.text),
        ),
      );

    return v_flex()
      .id(`pane-head-${pane}`)
      .when(Boolean(this.handle), (el) => el.track_focus(this.handle))
      .size_full()
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
      .child(topRow)
      .child(summaryRow);
  }
}
