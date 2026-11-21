// Pure state/model helpers for Catch-up Briefing, adaptive semantic naming,
// and zero-click hover micro-summaries. Kept free of GPUI imports for unit testing.

const cleanTitle = (s) =>
  String(s ?? "")
    .replace(/^[^\p{L}\p{N}\p{Ps}\p{Pi}]+/u, "")
    .replace(/^\[ [!.] \] /, "")
    .trim();

function isPathLike(raw) {
  return /^[~/]|^[A-Za-z]:\\/.test(String(raw ?? "").replace(/^[^\p{L}\p{N}~/]+/u, ""));
}

export function isTellingTitle(rawTitle, { address, programName, programKey } = {}) {
  const live = cleanTitle(rawTitle);
  if (!live) return false;
  if (live === address || live === programName) return false;
  if (isPathLike(rawTitle) || live.includes("@")) return false;
  if (programKey && live.toLowerCase() === String(programKey).toLowerCase()) return false;
  return true;
}

/**
 * Resolves display label with strict user-first precedence:
 * 1. User custom name (`herdr.name` / `custom_name`)
 * 2. Drove role/task display (`drove.display`)
 * 3. User-named tab (`tab.named && tab.name !== program.name`)
 * 4. Adaptive `semantic_title` (replaces boring process name / path / tab number)
 * 5. Telling live terminal/agent title
 * 6. Fallback (`tab.name` / `program.name` / `address`)
 */
export function resolveSemanticLabel(agent, rec, tab, lead, { manyInTab = false } = {}) {
  const custom = agent?.custom_name || rec?.labels?.["herdr.name"];
  if (custom && String(custom).trim()) return String(custom).trim();

  const droveDisplay = rec?.labels?.["drove.display"];
  if (droveDisplay && String(droveDisplay).trim()) return String(droveDisplay).trim();

  const prog = lead?.program?.name ?? agent?.program?.name ?? rec?.program?.name ?? "";
  const progKey = lead?.program?.key ?? agent?.program?.key ?? rec?.program?.key ?? "";

  if (tab && !manyInTab && tab.named && tab.name && tab.name !== prog) {
    return tab.name;
  }

  const semantic = String(
    agent?.semantic_title ?? rec?.semantic_title ?? lead?.semantic_title ?? "",
  ).trim();
  if (semantic) return semantic;

  const target = manyInTab ? agent : (lead ?? agent);
  const rawTitle = target?.title ?? rec?.title ?? "";
  if (isTellingTitle(rawTitle, { address: target?.terminal ?? agent?.address, programName: prog, programKey: progKey })) {
    return cleanTitle(rawTitle);
  }

  if (tab?.name) return tab.name;
  return cleanTitle(agent?.title) || prog || agent?.address || "";
}

export function formatShortDuration(ms) {
  const m = Math.floor(Math.max(0, Number(ms) || 0) / 60000);
  if (m < 1) return "<1m";
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  return h < 24 ? `${h}h` : `${Math.floor(h / 24)}d`;
}

/**
 * Formats a concise 3-line zero-click hover tooltip for an agent row or tab card.
 */
export function buildHoverTooltip(agent, rec, { isZh = false, now = Date.now(), since = 0, displayLabel = "" } = {}) {
  const tr = (en, zh) => (isZh ? zh : en);
  const statusMap = {
    working: tr("Working", "运行中"),
    blocked: tr("Needs you", "待确认"),
    done: tr("Done", "已完成"),
    error: tr("Error", "异常"),
    idle: tr("Idle", "空闲"),
  };
  const addr = agent?.address ?? rec?.address ?? "";
  const statusKey = agent?.status ?? rec?.status ?? "idle";
  const statusText = statusMap[statusKey] ?? statusKey;
  const elapsed = since > 0 && now >= since ? ` ${formatShortDuration(now - since)}` : "";
  const prog = agent?.program?.name ?? rec?.program?.name ?? (agent?.acp || rec?.acp ? "ACP" : "CLI");
  const title =
    String(displayLabel || agent?.semantic_title || rec?.semantic_title || cleanTitle(agent?.title || rec?.title) || prog).trim();

  const headerParts = [title];
  if (prog && prog !== title) headerParts.push(prog);
  if (addr) headerParts.push(addr);
  headerParts.push(`${statusText}${elapsed}`);
  const line1 = headerParts.join(" · ");

  const permTitle = rec?.pending_permission?.title ? String(rec.pending_permission.title).trim() : "";
  const aiStatusSummary = !agent?.manual_status && typeof (agent?.ai_status_summary ?? rec?.ai_status_summary) === "string"
    ? String(agent?.ai_status_summary ?? rec?.ai_status_summary).trim()
    : "";
  const micro = String(agent?.micro_summary ?? rec?.micro_summary ?? "").trim();

  let progress = "";
  if (permTitle) {
    progress = tr(`Waiting approval: ${permTitle}`, `等待审批：${permTitle}`);
  } else if (micro) {
    progress = micro;
  } else if (aiStatusSummary) {
    progress = aiStatusSummary;
  } else if (statusKey === "working") {
    const turn = Number(rec?.turn ?? 0);
    progress = turn > 0
      ? tr(`Actively working (turn ${turn})`, `正在执行中（第 ${turn} 轮）`)
      : tr("Actively working…", "正在执行中…");
  } else if (statusKey === "done") {
    progress = tr("Finished while away — click to inspect", "已完成，点击查看结果与 Diff");
  } else if (statusKey === "blocked") {
    progress = tr("Paused and waiting for your input", "已暂停，等待你的确认或输入");
  } else {
    progress = tr("Idle and ready for next prompt", "空闲中，随时可发新指令");
  }
  const line2 = `▸ ${progress}`;

  const cwd = String(rec?.cwd ?? agent?.cwd ?? "").trim();
  const branch = String(agent?.branch ?? rec?.branch ?? "").trim();
  const shortCwd = cwd.length > 44 ? `…${cwd.slice(-42)}` : cwd;
  const metaParts = [];
  if (shortCwd) metaParts.push(branch ? `${shortCwd} (${branch})` : shortCwd);
  else if (branch) metaParts.push(`(${branch})`);
  metaParts.push(tr("Click to focus · Right-click menu", "点击聚焦 · 右键菜单"));
  const line3 = metaParts.join(" · ");

  return `${line1}\n${line2}\n${line3}`;
}

export function permissionClusterKey(perm) {
  const kind = String(perm?.kind ?? "").trim().toLowerCase() || "other";
  const cleaned = String(perm?.title ?? "")
    .trim()
    .toLowerCase()
    .replace(/["'`]/g, "");
  const tokens = cleaned.split(/\s+/).filter(Boolean);
  let stem = kind;
  if (tokens.length > 0) {
    if (["run", "execute", "shell", "bash", "command"].includes(tokens[0]) && tokens.length >= 2) {
      stem = `run:${tokens.slice(1, 3).join(" ")}`;
    } else if (["edit", "write", "modify", "delete", "create"].includes(tokens[0])) {
      stem = tokens[0];
    } else {
      stem = tokens.slice(0, 3).join(" ");
    }
  }
  return { kind, key: `${kind}:${stem}` };
}

/**
 * Builds an instant 0ms local Catch-up Briefing snapshot from desktop `agents()` + `terminals()`
 * before Core's `h2 ai briefing --json` (which adds Drove DAG impact & AI executive headline) resolves.
 */
export function buildLocalFallbackBriefing({
  agents = [],
  terminals = [],
  awayMinutes = null,
  isZh = false,
  now = Date.now(),
} = {}) {
  const tr = (en, zh) => (isZh ? zh : en);
  const termByAddr = new Map();
  for (const t of terminals) {
    if (t?.address) termByAddr.set(t.address, t);
  }

  const criticalBlockers = [];
  const pendingDecisions = [];
  const readyToReview = [];
  const workingItems = [];
  const permBuckets = new Map();
  const seen = new Set();

  const processEntry = (addr, status, a, rec) => {
    if (!addr || seen.has(addr)) return;
    if (rec?.labels?.["herdr.trashed"] || rec?.state === "finished") return;
    seen.add(addr);

    const machine = addr.includes("/") ? addr.split("/")[0] : null;
    const isAcp = Boolean(a?.acp ?? rec?.acp);
    const title = resolveSemanticLabel(a, rec, null, null);
    const micro = String(a?.micro_summary ?? rec?.micro_summary ?? "").trim();
    const aiSummary = String(a?.ai_status_summary ?? rec?.ai_status_summary ?? "").trim();
    const aiKind = String(a?.ai_status_kind ?? rec?.ai_status_kind ?? "").trim();
    const perm = rec?.pending_permission ?? null;
    const spaceName = a?.space_name || rec?.labels?.where || null;

    if (status === "error" || (status === "blocked" && aiKind === "error")) {
      criticalBlockers.push({
        id: `${isAcp ? "agent" : "terminal"}:${addr}`,
        kind: "acp_error",
        address: addr,
        machine,
        spaceName,
        title,
        summary: micro || aiSummary || tr("Execution error needs investigation", "执行异常，建议优先排查"),
        status: "error",
        blockedDownstreamCount: 0,
        blockedDownstreamKeys: [],
        hasDiff: false,
      });
      return;
    }

    if (status === "blocked") {
      if (isAcp && perm) {
        const { kind, key } = permissionClusterKey(perm);
        if (!permBuckets.has(key)) permBuckets.set(key, { kind, key, entries: [] });
        permBuckets.get(key).entries.push({ addr, title, perm, machine, spaceName });
      } else {
        pendingDecisions.push({
          id: `${isAcp ? "agent" : "terminal"}:${addr}`,
          kind: isAcp ? "acp_permission" : "pty_blocked",
          address: addr,
          machine,
          spaceName,
          title,
          summary: perm?.title || aiSummary || micro || tr("Waiting for your confirmation", "等待你的输入或确认"),
          status: "blocked",
          blockedDownstreamCount: 0,
          blockedDownstreamKeys: [],
          permissionKind: perm?.kind ?? (aiKind || null),
          permissionTitle: perm?.title ?? null,
          hasDiff: false,
        });
      }
      return;
    }

    if (status === "done") {
      readyToReview.push({
        id: `${isAcp ? "agent" : "terminal"}:${addr}`,
        kind: isAcp ? "acp_done" : "pty_done",
        address: addr,
        machine,
        spaceName,
        title,
        summary: micro || aiSummary || tr("Completed — ready to inspect diff", "已完成，可检视改动 Diff"),
        status: "done",
        blockedDownstreamCount: 0,
        blockedDownstreamKeys: [],
        hasDiff: true,
      });
      return;
    }

    if (status === "working") {
      workingItems.push({
        id: `${isAcp ? "agent" : "terminal"}:${addr}`,
        kind: isAcp ? "acp_working" : "pty_working",
        address: addr,
        machine,
        spaceName,
        title,
        summary: micro || aiSummary || tr("Working in background", "后台推进中"),
        status: "working",
        blockedDownstreamCount: 0,
        blockedDownstreamKeys: [],
        hasDiff: false,
      });
    }
  };

  for (const a of agents) {
    processEntry(a.address, a.status, a, termByAddr.get(a.address) ?? null);
  }
  for (const rec of terminals) {
    if (rec?.state !== "live" || !rec?.status) continue;
    processEntry(rec.address, rec.status, null, rec);
  }

  const permissionClusters = [];
  for (const bucket of permBuckets.values()) {
    if (bucket.entries.length >= 2) {
      permissionClusters.push({
        clusterKey: bucket.key,
        permissionKind: bucket.kind,
        title: String(bucket.entries[0].perm?.title || bucket.kind).slice(0, 60),
        summary: tr(
          `${bucket.entries.length} agents waiting on similar permission; batch approve available`,
          `${bucket.entries.length} 个智能体卡在同类权限审批，可一键合并批复`,
        ),
        addresses: bucket.entries.map((e) => e.addr),
        labels: bucket.entries.map((e) => e.title),
      });
    } else {
      const e = bucket.entries[0];
      pendingDecisions.push({
        id: `agent:${e.addr}`,
        kind: "acp_permission",
        address: e.addr,
        machine: e.machine,
        spaceName: e.spaceName,
        title: e.title,
        summary: String(e.perm?.title || e.perm?.kind || tr("Approval needed", "需要权限审批")),
        status: "blocked",
        blockedDownstreamCount: 0,
        blockedDownstreamKeys: [],
        permissionKind: e.perm?.kind ?? null,
        permissionTitle: e.perm?.title ?? null,
        hasDiff: false,
      });
    }
  }

  const clusteredTotal = permissionClusters.reduce((s, g) => s + g.addresses.length, 0);
  const counts = {
    critical: criticalBlockers.length,
    needsYou: clusteredTotal + pendingDecisions.length,
    batchGroups: permissionClusters.length,
    readyReview: readyToReview.length,
    working: workingItems.length,
    totalActive:
      criticalBlockers.length + clusteredTotal + pendingDecisions.length + readyToReview.length + workingItems.length,
  };

  let headline;
  if (counts.critical === 0 && counts.needsYou === 0 && counts.readyReview === 0) {
    headline = counts.working > 0
      ? tr(`${counts.working} agents running smoothly with no blockers.`, `当前有 ${counts.working} 个智能体正在平稳推进，暂无阻塞。`)
      : tr("All agents are idle; nothing needs attention.", "所有智能体均空闲，暂无待处理事项。");
  } else {
    const parts = [];
    if (awayMinutes && awayMinutes > 0) {
      parts.push(tr(`While away (${awayMinutes}m)`, `离开约 ${awayMinutes} 分钟期间`));
    }
    if (counts.critical > 0) parts.push(tr(`${counts.critical} critical blockers`, `${counts.critical} 项异常待优先排查`));
    if (counts.needsYou > 0) {
      parts.push(
        counts.batchGroups > 0
          ? tr(`${counts.needsYou} waiting on you (${counts.batchGroups} batchable)`, `${counts.needsYou} 项待确认（含 ${counts.batchGroups} 组可合并批复）`)
          : tr(`${counts.needsYou} waiting on you`, `${counts.needsYou} 项待确认`),
      );
    }
    if (counts.readyReview > 0) parts.push(tr(`${counts.readyReview} ready to review`, `${counts.readyReview} 项已完成待看 Diff`));
    headline = isZh ? `${parts.join("；")}。` : `${parts.join("; ")}.`;
  }

  return {
    generatedMs: now,
    headline,
    headlineFromAi: false,
    counts,
    criticalBlockers,
    permissionClusters,
    pendingDecisions,
    readyToReview,
    workingItems,
  };
}

/**
 * Merges Core's `ai.briefing` result (Drove DAG impact, unread worker questions, AI headline)
 * with local desktop terminal state (PTY `done` / `blocked` / `ai_status_summary`).
 */
export function mergeBriefingSnapshots(local, core) {
  if (!core || typeof core !== "object") return local;
  const dedup = (primary, secondary) => {
    const seen = new Set();
    const out = [];
    for (const item of [...(primary ?? []), ...(secondary ?? [])]) {
      if (!item?.id || seen.has(item.id)) continue;
      seen.add(item.id);
      out.push(item);
    }
    return out;
  };

  const clusters = Array.isArray(core.permissionClusters) && core.permissionClusters.length > 0
    ? core.permissionClusters
    : (local?.permissionClusters ?? []);
  const clusteredAddrs = new Set(clusters.flatMap((g) => g.addresses ?? []));

  const criticalBlockers = dedup(core.criticalBlockers, local?.criticalBlockers).sort(
    (a, b) => (b.blockedDownstreamCount ?? 0) - (a.blockedDownstreamCount ?? 0),
  );
  const pendingDecisions = dedup(core.pendingDecisions, local?.pendingDecisions).filter(
    (it) => !it.address || !clusteredAddrs.has(it.address),
  );
  const readyToReview = dedup(core.readyToReview, local?.readyToReview);
  const workingItems = dedup(core.workingItems, local?.workingItems);

  const clusteredTotal = clusters.reduce((s, g) => s + (g.addresses?.length ?? 0), 0);
  const counts = {
    critical: criticalBlockers.length,
    needsYou: clusteredTotal + pendingDecisions.length,
    batchGroups: clusters.length,
    readyReview: readyToReview.length,
    working: workingItems.length,
    totalActive:
      criticalBlockers.length + clusteredTotal + pendingDecisions.length + readyToReview.length + workingItems.length,
  };

  return {
    generatedMs: core.generatedMs || local?.generatedMs || Date.now(),
    headline: core.headline || local?.headline || "",
    headlineFromAi: Boolean(core.headlineFromAi),
    counts,
    criticalBlockers,
    permissionClusters: clusters,
    pendingDecisions,
    readyToReview,
    workingItems,
  };
}
