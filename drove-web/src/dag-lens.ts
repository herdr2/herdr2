import { branchPhase, dataBranchSource, isDataWhen, type BranchPhase } from "./branch";
import { workerNumbers } from "./format";
import {
  depRouteKey,
  dispatchRouteKey,
  execRouteKey,
  forkRouteKey,
  reviewRouteKey,
  ROLE_RANK,
  type Dir,
  type Pt,
} from "./layout";
import {
  COORD_ID,
  allMessages,
  attemptMark,
  byStarted,
  openEscalations,
  openQuestions,
  reviewerTasksInOrder,
  tasksInOrder,
  workerNodeId,
  type AttemptMark,
} from "./relate";
import { authorWorkers, isReviewer, reviewBoard, reviewerNodeId, taskRequiresReview } from "./review";
import { resolveRoleMeta, type RoleVisualKind } from "./roles";
import type { Attempt, RunSnapshot, Task, TaskWhen, Worker } from "./types";

export type LensRole = RoleVisualKind;

export interface DagLens {
  role: LensRole | null;
  phase: string | null;
}

export interface RunProgressSummary {
  total: number;
  done: number;
  running: number;
  ready: number;
  failed: number;
  blocked: number;
  pending: number;
  reviewing: number;
  rework: number;
  pct: number;
  busyAuthors: number;
  busyReviewers: number;
  busyTotal: number;
  pendingActions: number;
}

export interface CanvasRoleChip {
  kind: LensRole;
  count: number;
  busy: number;
  labelKey: string;
  fallbackLabel: string;
  colorVar: string;
}

export interface WorkerCurrentTask {
  task: Task;
  attempt: Attempt;
  startedAt: string;
}

export interface SmartExecLink {
  id: string;
  routeKey: string;
  attemptId: string;
  workerId: string;
  workerNodeId: string;
  taskId: string;
  wNum?: number;
  roleKind: LensRole;
  roleLabelKey: string;
  roleFallback: string;
  live: boolean;
  attempts: number;
  mark: AttemptMark;
  step: number;
  attemptIndex: number;
  attemptTotal: number;
  rework: boolean;
  retry: boolean;
  execKind: "worker" | "coordinator" | "exec";
  colorVar: string;
}

export interface ExecRoutePlan {
  seamAlong: number;
  preSeamAlong?: number;
  preCorridorCross?: number;
  corridorCross?: number;
  dropAlong?: number;
  sourceOffset?: number;
  entryOffset: number;
  labelAlong: number;
  labelCross: number;
}

export type ReviewEdgeMark = "approved" | "changes" | "broken" | "reviewing" | "submitted" | "waiting";

export interface VisibleReviewLink {
  edgeId: string;
  baseEdgeId: string;
  taskId: string;
  reviewerWorkerId: string;
  reviewerNodeId: string;
  round: number;
  maxRounds: number;
  live: boolean;
  mark: ReviewEdgeMark;
  selectedMatch: boolean;
  emphasis: "" | " edge--focus" | " edge--dim";
  parallelOffset?: number;
  labelShift?: number;
}

export interface ForkLink {
  id: string;
  sourceNodeId: string;
  targetNodeId: string;
  sourceWorkerId: string;
  targetWorkerId: string;
  targetNum?: number;
  roleKind: LensRole;
  colorVar: string;
}

export interface DataBranchLink {
  id: string;
  sourceTaskId: string;
  targetTaskId: string;
  sharesDepEdge: boolean;
  phase: BranchPhase;
  label: string;
}

export interface LensSpotlight {
  nodes: Set<string>;
  edges: Set<string>;
}

const ROLE_COLOR_VAR: Record<LensRole, string> = {
  coordinator: "var(--role-coord)",
  coder: "var(--role-coder)",
  tester: "var(--role-tester)",
  writer: "var(--role-writer)",
  reviewer: "var(--role-reviewer)",
  researcher: "var(--role-researcher)",
  custom: "var(--role-custom)",
};

export function roleColorVar(kind: LensRole): string {
  return ROLE_COLOR_VAR[kind] ?? ROLE_COLOR_VAR.custom;
}

export function computeRunProgressSummary(snap: RunSnapshot): RunProgressSummary {
  let done = 0;
  let running = 0;
  let ready = 0;
  let failed = 0;
  let blocked = 0;
  let pending = 0;
  let reviewing = 0;
  let rework = 0;

  for (const task of snap.tasks) {
    if (task.status === "succeeded") done += 1;
    else if (task.status === "running") running += 1;
    else if (task.status === "ready") ready += 1;
    else if (task.status === "failed") failed += 1;
    else if (task.status === "blocked") blocked += 1;
    else pending += 1;

    if (task.execPhase === "rework") {
      rework += 1;
    } else if (task.execPhase === "review") {
      reviewing += 1;
    } else if (task.status === "running" && taskRequiresReview(task, snap.attempts)) {
      const board = reviewBoard(task, snap.attempts, snap.workers);
      if (board?.standing === "reviewing") reviewing += 1;
      else if (board?.standing === "rework") rework += 1;
    }
  }

  let busyAuthors = 0;
  let busyReviewers = 0;
  for (const w of snap.workers) {
    if (w.state !== "busy" && w.state !== "starting") continue;
    if (isReviewer(w)) busyReviewers += 1;
    else busyAuthors += 1;
  }

  const msgs = allMessages(snap);
  const pendingActions = openQuestions(msgs).length + openEscalations(msgs).length;
  const total = snap.tasks.length;
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;

  return {
    total,
    done,
    running,
    ready,
    failed,
    blocked,
    pending,
    reviewing,
    rework,
    pct,
    busyAuthors,
    busyReviewers,
    busyTotal: busyAuthors + busyReviewers,
    pendingActions,
  };
}

export function summarizeCanvasRoles(snap: RunSnapshot): CanvasRoleChip[] {
  const chips = new Map<LensRole, { workers: number; tasks: number; busy: number; labelKey: string; fallbackLabel: string }>();

  const touch = (kind: LensRole, labelKey: string, fallbackLabel: string) => {
    let entry = chips.get(kind);
    if (!entry) {
      entry = { workers: 0, tasks: 0, busy: 0, labelKey, fallbackLabel };
      chips.set(kind, entry);
    }
    return entry;
  };

  if (snap.tasks.length > 0 || snap.workers.length > 0) {
    const coord = touch("coordinator", "role.coordinator", "编排者");
    coord.workers = 1;
    coord.busy = snap.run.status === "active" ? 1 : 0;
  }

  for (const w of snap.workers) {
    const meta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
    const entry = touch(meta.visualKind, meta.labelKey, meta.fallbackLabel);
    entry.workers += 1;
    if (w.state === "busy" || w.state === "starting") entry.busy += 1;
  }

  for (const t of snap.tasks) {
    if (t.assign?.role) {
      const meta = resolveRoleMeta(t.assign.role);
      touch(meta.visualKind, meta.labelKey, meta.fallbackLabel).tasks += 1;
    }
    if (taskRequiresReview(t, snap.attempts)) {
      touch("reviewer", "role.reviewer", "审查员").tasks += 1;
    }
  }

  return [...chips.entries()]
    .map(([kind, entry]) => ({
      kind,
      count: entry.workers > 0 ? entry.workers : entry.tasks,
      busy: entry.busy,
      labelKey: entry.labelKey,
      fallbackLabel: entry.fallbackLabel,
      colorVar: roleColorVar(kind),
    }))
    .sort((a, b) => (ROLE_RANK[a.kind] ?? 99) - (ROLE_RANK[b.kind] ?? 99));
}

export function workerCurrentTask(worker: Worker, snap: RunSnapshot): WorkerCurrentTask | null {
  const byId = worker.currentAttemptId
    ? snap.attempts.find((a) => a.id === worker.currentAttemptId && a.workerId === worker.id)
    : undefined;
  const open = snap.attempts
    .filter((a) => a.workerId === worker.id && !a.endedAt && !a.outcome)
    .sort(byStarted)
    .at(-1);
  const attempt = byId && !byId.endedAt ? byId : open;
  if (!attempt) return null;
  if (worker.state !== "busy" && worker.state !== "starting" && attempt.endedAt) return null;
  const task = snap.tasks.find((t) => t.id === attempt.taskId);
  if (!task) return null;
  return { task, attempt, startedAt: attempt.startedAt };
}

export function computeSmartExecLinks(
  snap: RunSnapshot,
  showHistory: boolean,
  selectedId: string | null,
  lens: DagLens,
): SmartExecLink[] {
  const authors = authorWorkers(snap.workers);
  const authorById = new Map(authors.map((w) => [w.id, w]));
  const nums = workerNumbers(authors);
  const taskById = new Map(snap.tasks.map((t) => [t.id, t]));
  const currentByWorker = new Map(authors.map((w) => [w.id, workerCurrentTask(w, snap)] as const));
  const sortedAll = [...snap.attempts].sort(byStarted);
  const validAttempts = sortedAll.filter((a) => {
    if (a.kind === "review") return false;
    if (!taskById.has(a.taskId)) return false;
    return a.workerId === "exec" || a.kind === "exec" || a.workerId === "coordinator" || authorById.has(a.workerId);
  });

  const pairCounts = new Map<string, number>();
  for (const a of validAttempts) {
    const pk = `${a.workerId}\0${a.taskId}`;
    pairCounts.set(pk, (pairCounts.get(pk) ?? 0) + 1);
  }
  const pairSeen = new Map<string, number>();
  const out: SmartExecLink[] = [];

  for (let i = 0; i < validAttempts.length; i++) {
    const a = validAttempts[i];
    const task = taskById.get(a.taskId)!;
    const pk = `${a.workerId}\0${a.taskId}`;
    const attemptIndex = (pairSeen.get(pk) ?? 0) + 1;
    pairSeen.set(pk, attemptIndex);
    const attemptTotal = pairCounts.get(pk) ?? 1;

    const isExec = a.workerId === "exec" || a.kind === "exec";
    const isCoord = !isExec && a.workerId === "coordinator";
    const execKind: SmartExecLink["execKind"] = isExec ? "exec" : isCoord ? "coordinator" : "worker";
    const w = execKind === "worker" ? authorById.get(a.workerId) : undefined;
    const meta = w ? resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir }) : null;
    const linkWorkerNodeId = w ? workerNodeId(w.id) : COORD_ID;
    const wNum = w ? nums.get(w.id) : undefined;
    const roleKind: LensRole = meta ? meta.visualKind : "coordinator";
    const roleLabelKey = isExec ? "badge.exec" : isCoord ? "task.coordShort" : meta!.labelKey;
    const roleFallback = isExec ? "EXEC" : isCoord ? "主控" : meta!.fallbackLabel;
    const current = w ? currentByWorker.get(w.id) : null;
    const isLive = Boolean(
      !a.endedAt &&
      !a.outcome &&
      (task.status === "running" || (w != null && (w.state === "busy" || w.state === "starting") && current?.attempt.id === a.id)),
    );
    const mark: AttemptMark = isLive ? "run" : attemptMark(a);
    const rework = sortedAll.some((prev) => prev.taskId === a.taskId && prev.kind === "review" && byStarted(prev, a) < 0);
    const retry = Boolean(a.retryOf);

    const selectedMatch = selectedId === linkWorkerNodeId || selectedId === task.id;
    const wLensMatch = lens.role !== null && roleKind === lens.role;
    const pLensMatch = lens.phase !== null && task.phase === lens.phase;
    if (!isLive && !showHistory && !selectedMatch && !wLensMatch && !pLensMatch) continue;

    const routeKey = execRouteKey(a.workerId, task.id);
    out.push({
      id: `${routeKey}__${a.id}`,
      routeKey,
      attemptId: a.id,
      workerId: a.workerId,
      workerNodeId: linkWorkerNodeId,
      taskId: task.id,
      wNum,
      roleKind,
      roleLabelKey,
      roleFallback,
      live: isLive,
      attempts: attemptTotal,
      mark,
      step: i + 1,
      attemptIndex,
      attemptTotal,
      rework,
      retry,
      execKind,
      colorVar: roleColorVar(roleKind),
    });
  }

  return out;
}

/**
 * Task-to-reviewer edges to draw on the DAG canvas.
 * Even when `showReviewEdges` is off, selecting a reviewer or reviewed task reveals its review edges.
 */
export function computeVisibleReviewLinks(
  snap: RunSnapshot,
  showReviewEdges: boolean,
  selectedId: string | null,
  spotlight: LensSpotlight | null = null,
): VisibleReviewLink[] {
  const taskById = new Map(snap.tasks.map((t) => [t.id, t]));
  const focusTask = selectedId != null ? (taskById.get(selectedId) ?? null) : null;
  const focusCoord = selectedId === COORD_ID;
  const selectedWorker = selectedId != null ? (snap.workers.find((w) => workerNodeId(w.id) === selectedId) ?? null) : null;
  const focusReviewer =
    snap.workers.find((w) => isReviewer(w) && reviewerNodeId(w.id) === selectedId) ??
    (selectedWorker && isReviewer(selectedWorker) ? selectedWorker : null);
  const focusWorker = selectedWorker && !focusReviewer ? selectedWorker : null;

  const out: VisibleReviewLink[] = [];
  for (const w of snap.workers.filter(isReviewer)) {
    const rNodeId = reviewerNodeId(w.id);
    const taskIds = reviewerTasksInOrder(w, snap.attempts, snap.tasks).filter((id) => taskById.has(id));
    for (const taskId of taskIds) {
      const task = taskById.get(taskId)!;
      const board = reviewBoard(task, snap.attempts, snap.workers);
      if (!board) continue;
      const baseEdgeId = reviewRouteKey(taskId, rNodeId);
      const selectedMatch =
        focusReviewer?.id === w.id ||
        (focusTask != null && focusTask.id === taskId && taskRequiresReview(focusTask, snap.attempts));
      const lensMatch = Boolean(spotlight?.edges.has(baseEdgeId));
      if (!showReviewEdges && !selectedMatch && !lensMatch) continue;

      const emphasis: VisibleReviewLink["emphasis"] = selectedMatch
        ? " edge--focus"
        : focusReviewer || focusTask || focusWorker || focusCoord
          ? " edge--dim"
          : "";
      const taskRounds = board.rounds.filter((r) => r.attempt.workerId === w.id);
      if (taskRounds.length > 0) {
        taskRounds.forEach((round, roundIdx) => {
          const live = !round.attempt.endedAt && !round.attempt.outcome;
          const mark: ReviewEdgeMark = round.verdict === "approved"
            ? "approved"
            : round.verdict === "changes_requested"
              ? "changes"
              : round.attempt.outcome === "failed"
                ? "broken"
                : live
                  ? "reviewing"
                  : "submitted";
          const maxRounds = round.attempt.reviewRound?.maxRounds ?? board.maxRounds;
          const parallelOffset = taskRounds.length > 1 ? (roundIdx - (taskRounds.length - 1) / 2) * 22 : undefined;
          const labelShift = taskRounds.length > 1 ? (roundIdx - (taskRounds.length - 1) / 2) * 24 : undefined;
          out.push({
            edgeId: taskRounds.length > 1 ? `${baseEdgeId}__${round.attempt.id}` : baseEdgeId,
            baseEdgeId,
            taskId,
            reviewerWorkerId: w.id,
            reviewerNodeId: rNodeId,
            round: round.round,
            maxRounds,
            live,
            mark,
            selectedMatch,
            emphasis,
            parallelOffset,
            labelShift,
          });
        });
      } else if (task.reviewState?.reviewerWorkerId === w.id) {
        const mark: ReviewEdgeMark = board.standing === "approved"
          ? "approved"
          : board.standing === "rejected"
            ? "changes"
            : board.standing === "broken"
              ? "broken"
              : board.standing === "reviewing"
                ? "reviewing"
                : "waiting";
        out.push({
          edgeId: baseEdgeId,
          baseEdgeId,
          taskId,
          reviewerWorkerId: w.id,
          reviewerNodeId: rNodeId,
          round: Math.max(1, board.round || 1),
          maxRounds: board.maxRounds,
          live: board.standing === "reviewing",
          mark,
          selectedMatch,
          emphasis,
        });
      }
    }
  }
  return out;
}

interface AlongRect {
  id: string;
  alongStart: number;
  alongEnd: number;
  crossStart: number;
  crossEnd: number;
}

interface CorridorSeg {
  fromAlong: number;
  toAlong: number;
  cross: number;
}

interface LabelBox {
  along: number;
  cross: number;
}

function clampVal(v: number, lo: number, hi: number): number {
  if (hi < lo) return (lo + hi) / 2;
  return Math.max(lo, Math.min(hi, v));
}

function intervalsOverlap(a0: number, a1: number, b0: number, b1: number): boolean {
  const loA = Math.min(a0, a1);
  const hiA = Math.max(a0, a1);
  const loB = Math.min(b0, b1);
  const hiB = Math.max(b0, b1);
  return loA < hiB && hiA > loB;
}

function crossInInterval(c: number, lo: number, hi: number): boolean {
  return c > lo && c < hi;
}

/**
 * Route worker/coordinator -> task execution edges cleanly across the open seam
 * while dodging intermediate worker, task, and gate cards and separating shared corridors
 * and edge labels in both horizontal (LR) and vertical (TB) layouts.
 */
export function computeExecRoutes(input: {
  dir: Dir;
  seam: number | undefined;
  pos: ReadonlyMap<string, Pt>;
  sizeOf: (id: string) => { w: number; h: number };
  obstacleIds: readonly string[];
  links: readonly { key: string; from: string; to: string; kind?: "exec" | "review" }[];
}): Map<string, ExecRoutePlan> {
  const plans = new Map<string, ExecRoutePlan>();
  const { dir, seam, pos, sizeOf, obstacleIds, links } = input;
  if (seam == null || links.length === 0) return plans;

  const rects: AlongRect[] = [];
  const rectById = new Map<string, AlongRect>();
  for (const id of obstacleIds) {
    const p = pos.get(id);
    if (!p || rectById.has(id)) continue;
    const s = sizeOf(id);
    const rect: AlongRect = dir === "LR"
      ? { id, alongStart: p.x, alongEnd: p.x + s.w, crossStart: p.y, crossEnd: p.y + s.h }
      : { id, alongStart: p.y, alongEnd: p.y + s.h, crossStart: p.x, crossEnd: p.x + s.w };
    rects.push(rect);
    rectById.set(id, rect);
  }

  let orchMaxAlong = -Infinity;
  let taskMinAlong = Infinity;
  for (const r of rects) {
    if (r.alongEnd <= seam) orchMaxAlong = Math.max(orchMaxAlong, r.alongEnd);
    if (r.alongStart >= seam) taskMinAlong = Math.min(taskMinAlong, r.alongStart);
  }
  const seamLo = Number.isFinite(orchMaxAlong) ? orchMaxAlong + 26 : seam - 60;
  const seamHi = Number.isFinite(taskMinAlong) ? taskMinAlong - 26 : seam + 60;
  const seamMid = Number.isFinite(orchMaxAlong) && Number.isFinite(taskMinAlong) ? (seamLo + seamHi) / 2 : seam;
  const availWidth = Math.max(0, seamHi - seamLo);
  const rawStep = links.length > 1 ? Math.max(6, Math.min(20, availWidth / (links.length - 1))) : 0;
  const seamStep = links.length > 1 && availWidth > 0 ? Math.min(rawStep, Math.max(4, availWidth / (links.length - 1))) : rawStep;

  interface EnrichedLink {
    key: string;
    from: string;
    to: string;
    kind: "exec" | "review";
    sAlong: number;
    sCross: number;
    tAlong: number;
    tCross: number;
  }

  const enriched: EnrichedLink[] = [];
  for (const link of links) {
    const fr = rectById.get(link.from);
    const tr = rectById.get(link.to);
    if (!fr || !tr) continue;
    const kind = link.kind ?? "exec";
    const sAlong = fr.alongEnd;
    const sCross = fr.crossStart + (fr.crossEnd - fr.crossStart) * (link.from === COORD_ID ? 0.68 : 0.5);
    const tAlong = tr.alongStart;
    const tCross = tr.crossStart + (tr.crossEnd - tr.crossStart) * (kind === "review" ? 0.5 : 0.33);
    enriched.push({ key: link.key, from: link.from, to: link.to, kind, sAlong, sCross, tAlong, tCross });
  }
  if (enriched.length === 0) return plans;

  enriched.sort(
    (a, b) =>
      a.sCross - b.sCross ||
      a.tAlong - b.tAlong ||
      a.tCross - b.tCross ||
      (a.kind === b.kind ? 0 : a.kind === "exec" ? -1 : 1) ||
      (a.key < b.key ? -1 : a.key > b.key ? 1 : 0),
  );

  const byTarget = new Map<string, EnrichedLink[]>();
  const byRevSource = new Map<string, EnrichedLink[]>();
  for (const link of enriched) {
    const tk = `${link.kind}\0${link.to}`;
    const group = byTarget.get(tk) ?? [];
    group.push(link);
    byTarget.set(tk, group);
    if (link.kind === "review") {
      const sg = byRevSource.get(link.from) ?? [];
      sg.push(link);
      byRevSource.set(link.from, sg);
    }
  }
  const targetSlot = new Map<string, { idx: number; count: number }>();
  for (const group of byTarget.values()) {
    group.forEach((item, idx) => targetSlot.set(item.key, { idx, count: group.length }));
  }
  const revSourceSlot = new Map<string, { idx: number; count: number }>();
  for (const group of byRevSource.values()) {
    group.forEach((item, idx) => revSourceSlot.set(item.key, { idx, count: group.length }));
  }

  const usedCorridors: CorridorSeg[] = [];
  const placedLabels: LabelBox[] = [];
  const hasReview = enriched.some((l) => l.kind === "review");
  const labelWAlong = hasReview ? (dir === "LR" ? 86 : 90) : dir === "LR" ? 66 : 72;
  const labelHCross = 20;
  const spansCross = dir === "LR" ? labelHCross : labelWAlong;
  const spansAlong = dir === "LR" ? labelWAlong : labelHCross;

  const collidesUsed = (fromAlong: number, toAlong: number, cross: number, margin: number) =>
    usedCorridors.some((u) => intervalsOverlap(fromAlong, toAlong, u.fromAlong, u.toAlong) && Math.abs(cross - u.cross) < margin);

  enriched.forEach((link, idx) => {
    const seamAlong = seamMid + (idx - (enriched.length - 1) / 2) * seamStep;
    if (seamAlong <= link.sAlong + 8 || seamAlong >= link.tAlong - 8) return;

    const srcSlot = revSourceSlot.get(link.key);
    const sourceOffset = srcSlot && srcSlot.count > 1 ? clampVal((srcSlot.idx - (srcSlot.count - 1) / 2) * 10, -14, 14) : 0;
    const sCrossEffective = link.sCross + sourceOffset;
    const slot = targetSlot.get(link.key) ?? { idx: 0, count: 1 };
    const entryOffset = slot.count > 1 ? clampVal((slot.idx - (slot.count - 1) / 2) * 10, -18, 18) : 0;
    const renderEntryOffset = clampVal(entryOffset, -14, 14);
    const dropStep = slot.idx * 12;
    const desiredCross = link.tCross + renderEntryOffset;

    // 1. Pre-seam worker avoidance (e.g. coordinator -> task passing worker column)
    let preSeamAlong: number | undefined;
    let preCorridorCross: number | undefined;
    const preBlocking = rects.filter(
      (r) => r.id !== link.from && r.id !== link.to && intervalsOverlap(link.sAlong + 8, seamAlong - 8, r.alongStart - 8, r.alongEnd + 8),
    );
    const preHit = preBlocking.some((r) => crossInInterval(sCrossEffective, r.crossStart - 10, r.crossEnd + 10));
    if (preHit && preBlocking.length > 0) {
      const minPreAlong = Math.min(...preBlocking.map((r) => r.alongStart));
      if (minPreAlong > link.sAlong + 20) {
        const candidatePreSeam = clampVal((link.sAlong + minPreAlong) / 2, link.sAlong + 10, minPreAlong - 10);
        const sortedPre = [...preBlocking].sort((a, b) => a.crossStart - b.crossStart);
        const preMerged: { start: number; end: number }[] = [];
        for (const r of sortedPre) {
          const tail = preMerged[preMerged.length - 1];
          if (tail && r.crossStart <= tail.end) tail.end = Math.max(tail.end, r.crossEnd);
          else preMerged.push({ start: r.crossStart, end: r.crossEnd });
        }
        const preCands: number[] = [];
        for (let i = 0; i < preMerged.length - 1; i++) {
          const gLo = preMerged[i].end;
          const gHi = preMerged[i + 1].start;
          const gap = gHi - gLo;
          if (gap >= 38) {
            preCands.push(gLo + 15, (gLo + gHi) / 2, gHi - 15);
          } else if (gap >= 26) {
            preCands.push((gLo + gHi) / 2);
          }
        }
        const minPreCross = Math.min(...preBlocking.map((r) => r.crossStart));
        const maxPreCross = Math.max(...preBlocking.map((r) => r.crossEnd));
        for (let k = 0; k <= enriched.length + 1; k++) {
          preCands.push(minPreCross - 24 - k * 14, maxPreCross + 24 + k * 14);
        }
        const clearPreCands = preCands.filter(
          (c) => !preBlocking.some((r) => crossInInterval(c, r.crossStart - 10, r.crossEnd + 10)),
        );
        const freePreCands = clearPreCands.filter((c) => !collidesUsed(candidatePreSeam, seamAlong, c, 9));
        const pool = freePreCands.length > 0 ? freePreCands : clearPreCands;
        if (pool.length > 0) {
          pool.sort(
            (a, b) =>
              Math.abs(a - sCrossEffective) +
              0.25 * Math.abs(a - desiredCross) -
              (Math.abs(b - sCrossEffective) + 0.25 * Math.abs(b - desiredCross)),
          );
          preSeamAlong = candidatePreSeam;
          preCorridorCross = pool[0];
          usedCorridors.push({ fromAlong: candidatePreSeam, toAlong: seamAlong, cross: preCorridorCross });
        }
      }
    }

    // 2. Post-seam intermediate task/gate avoidance & horizontal lane deduplication
    let corridorCross: number | undefined;
    let dropAlong: number | undefined;
    const directObs = rects.filter(
      (r) => r.id !== link.from && r.id !== link.to && intervalsOverlap(seamAlong + 6, link.tAlong - 12, r.alongStart - 8, r.alongEnd + 8),
    );
    const directBlockedByObs = directObs.some((r) => crossInInterval(desiredCross, r.crossStart - 12, r.crossEnd + 12));
    const directBlockedByUsed = collidesUsed(seamAlong, link.tAlong, desiredCross, 8.5);

    if (!directBlockedByObs && !directBlockedByUsed) {
      usedCorridors.push({ fromAlong: seamAlong, toAlong: link.tAlong, cross: desiredCross });
    } else {
      const beforeTarget = rects.filter(
        (r) => r.id !== link.from && r.id !== link.to && r.alongEnd <= link.tAlong - 16 && r.alongEnd > seamAlong + 6,
      );
      const maxBlockingAlong = beforeTarget.length > 0 ? Math.max(...beforeTarget.map((r) => r.alongEnd)) : seamAlong;
      const gapLo = maxBlockingAlong + 16 <= link.tAlong - 14 ? maxBlockingAlong + 16 : Math.min(link.tAlong - 8, maxBlockingAlong + 8);
      const gapHi = maxBlockingAlong + 16 <= link.tAlong - 14 ? link.tAlong - 14 : link.tAlong - 8;
      const candidateDrop = clampVal(link.tAlong - 26 - dropStep, gapLo, gapHi);

      const corridorObs = rects.filter(
        (r) => r.id !== link.from && r.id !== link.to && intervalsOverlap(seamAlong + 6, candidateDrop - 6, r.alongStart - 8, r.alongEnd + 8),
      );
      const cands: number[] = [];
      if (corridorObs.length === 0) {
        for (let k = 0; k <= enriched.length + 1; k++) {
          cands.push(desiredCross - 18 - k * 14, desiredCross + 18 + k * 14);
        }
      } else {
        const sortedObs = [...corridorObs].sort((a, b) => a.crossStart - b.crossStart);
        const merged: { start: number; end: number }[] = [];
        for (const r of sortedObs) {
          const tail = merged[merged.length - 1];
          if (tail && r.crossStart <= tail.end) tail.end = Math.max(tail.end, r.crossEnd);
          else merged.push({ start: r.crossStart, end: r.crossEnd });
        }
        for (let i = 0; i < merged.length - 1; i++) {
          const gLo = merged[i].end;
          const gHi = merged[i + 1].start;
          const gap = gHi - gLo;
          if (gap >= 52) {
            cands.push(gLo + 16, (gLo + gHi) / 2, gHi - 16);
          } else if (gap >= 28) {
            cands.push((gLo + gHi) / 2);
          }
        }
        const minObsCross = Math.min(...corridorObs.map((r) => r.crossStart));
        const maxObsCross = Math.max(...corridorObs.map((r) => r.crossEnd));
        for (let k = 0; k <= enriched.length + 2; k++) {
          cands.push(minObsCross - 26 - k * 16, maxObsCross + 26 + k * 16);
        }
      }

      const clearCands = cands.filter(
        (c) => !corridorObs.some((r) => crossInInterval(c, r.crossStart - 12, r.crossEnd + 12)),
      );
      const freeCands = clearCands.filter((c) => !collidesUsed(seamAlong, candidateDrop, c, 11));
      const pool = freeCands.length > 0 ? freeCands : clearCands;
      if (pool.length > 0 && candidateDrop > seamAlong + 6 && candidateDrop < link.tAlong - 4) {
        const refCross = preCorridorCross ?? sCrossEffective;
        pool.sort(
          (a, b) =>
            Math.abs(a - refCross) +
            0.55 * Math.abs(a - desiredCross) -
            (Math.abs(b - refCross) + 0.55 * Math.abs(b - desiredCross)),
        );
        corridorCross = pool[0];
        dropAlong = candidateDrop;
        usedCorridors.push({ fromAlong: seamAlong, toAlong: dropAlong, cross: corridorCross });
      } else {
        usedCorridors.push({ fromAlong: seamAlong, toAlong: link.tAlong, cross: desiredCross });
      }
    }

    // 3. Non-overlapping label placement on this link's seam segment
    const segStart = preCorridorCross ?? sCrossEffective;
    const segEnd = corridorCross ?? desiredCross;
    const segLo = Math.min(segStart, segEnd);
    const segHi = Math.max(segStart, segEnd);
    const segMid = (segLo + segHi) / 2;

    const labelCollides = (la: number, lc: number) =>
      placedLabels.some((p) => Math.abs(la - p.along) < spansAlong - 4 && Math.abs(lc - p.cross) < spansCross - 2);

    let labelAlong = seamAlong;
    let labelCross = segMid;
    if (labelCollides(labelAlong, labelCross)) {
      let resolved = false;
      const maxShift = Math.max(40, (segHi - segLo) / 2);
      for (let d = 10; d <= maxShift; d += 10) {
        for (const sign of [-1, 1]) {
          const candCross = clampVal(segMid + sign * d, segLo, segHi);
          if (Math.abs(candCross - segMid) < d - 5) continue;
          if (!labelCollides(seamAlong, candCross)) {
            labelCross = candCross;
            resolved = true;
            break;
          }
        }
        if (resolved) break;
      }
      if (!resolved && segHi - segLo < 24) {
        const exitMin = preSeamAlong != null ? preSeamAlong + 8 : link.sAlong + 12;
        const exitMax = (dropAlong ?? link.tAlong) - 12;
        for (const offset of [-36, 36, -68, 68]) {
          const candAlong = clampVal(seamAlong + offset, exitMin, exitMax);
          const candCross = offset < 0 ? segStart : segEnd;
          if (!labelCollides(candAlong, candCross)) {
            labelAlong = candAlong;
            labelCross = candCross;
            resolved = true;
            break;
          }
        }
      }
      if (!resolved) {
        for (let d = 22; d <= 160; d += 22) {
          for (const sign of [-1, 1]) {
            const candCross = segMid + sign * d;
            if (!labelCollides(seamAlong, candCross)) {
              labelCross = candCross;
              resolved = true;
              break;
            }
          }
          if (resolved) break;
        }
      }
    }
    placedLabels.push({ along: labelAlong, cross: labelCross });

    plans.set(link.key, {
      seamAlong,
      ...(preSeamAlong != null ? { preSeamAlong, preCorridorCross } : {}),
      ...(corridorCross != null ? { corridorCross, dropAlong } : {}),
      ...(sourceOffset !== 0 ? { sourceOffset } : {}),
      entryOffset,
      labelAlong,
      labelCross,
    });
  });

  return plans;
}

export function computeForkLinks(snap: RunSnapshot): ForkLink[] {
  const authors = authorWorkers(snap.workers);
  const nums = workerNumbers(authors);
  const workerById = new Map(snap.workers.map((w) => [w.id, w]));
  const out: ForkLink[] = [];

  for (const w of authors) {
    const raw = w.forkedFrom?.trim();
    if (!raw) continue;

    let sourceNodeId: string | null = null;
    if (raw === "coordinator" || raw === COORD_ID) {
      sourceNodeId = COORD_ID;
    } else {
      const parent = workerById.get(raw);
      if (parent) {
        sourceNodeId = isReviewer(parent) ? reviewerNodeId(parent.id) : workerNodeId(parent.id);
      }
    }
    if (!sourceNodeId) continue;

    const meta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
    const targetNodeId = workerNodeId(w.id);
    out.push({
      id: forkRouteKey(sourceNodeId, targetNodeId),
      sourceNodeId,
      targetNodeId,
      sourceWorkerId: raw,
      targetWorkerId: w.id,
      targetNum: nums.get(w.id),
      roleKind: meta.visualKind,
      colorVar: roleColorVar(meta.visualKind),
    });
  }

  return out;
}

function formatWhenVal(v: unknown): string {
  if (typeof v === "string") return v.length > 16 ? `${v.slice(0, 15)}…` : v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  if (v === null) return "null";
  try {
    const s = JSON.stringify(v);
    return s.length > 16 ? `${s.slice(0, 15)}…` : s;
  } catch {
    return String(v);
  }
}

export function dataWhenShortLabel(when: TaskWhen | undefined): string | null {
  if (!when || !isDataWhen(when)) return null;
  const field = when.path?.trim() ? when.path.trim() : "data";
  if ("equals" in when && when.equals !== undefined) return `${field}=${formatWhenVal(when.equals)}`;
  if (when.in && when.in.length > 0) return `${field}∈[${when.in.slice(0, 3).map(formatWhenVal).join(",")}${when.in.length > 3 ? ",…" : ""}]`;
  if (when.exists === false) return `!${field}`;
  return `has ${field}`;
}

export function computeDataBranchLinks(snap: RunSnapshot): DataBranchLink[] {
  const out: DataBranchLink[] = [];
  for (const task of snap.tasks) {
    if (!task.when || !isDataWhen(task.when)) continue;
    const source = dataBranchSource(task, snap.tasks);
    if (!source) continue;
    const label = dataWhenShortLabel(task.when);
    if (!label) continue;
    out.push({
      id: `data-when:${source.id}__${task.id}`,
      sourceTaskId: source.id,
      targetTaskId: task.id,
      sharesDepEdge: task.deps.includes(source.id),
      phase: branchPhase(task, snap.tasks) ?? "open",
      label,
    });
  }
  return out;
}

function taskMatchesRole(task: Task, role: LensRole, snap: RunSnapshot): boolean {
  if (role === "reviewer") {
    if (taskRequiresReview(task, snap.attempts)) return true;
    return snap.attempts.some((a) => a.taskId === task.id && a.kind === "review");
  }
  if (role === "coordinator") {
    return snap.attempts.some(
      (a) => a.taskId === task.id && a.kind !== "review" && (a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec"),
    );
  }
  if (task.assign?.role) {
    const meta = resolveRoleMeta(task.assign.role);
    if (meta.visualKind === role) return true;
  }
  for (const a of snap.attempts) {
    if (a.taskId !== task.id || a.kind === "review") continue;
    const w = snap.workers.find((worker) => worker.id === a.workerId);
    if (!w || isReviewer(w)) continue;
    if (resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir }).visualKind === role) return true;
  }
  return false;
}

export function computeLensSpotlight(snap: RunSnapshot, lens: DagLens): LensSpotlight | null {
  if (!lens.role && !lens.phase) return null;

  const nodes = new Set<string>();
  const edges = new Set<string>();
  const taskById = new Map(snap.tasks.map((t) => [t.id, t]));
  const authorAttempts = snap.attempts.filter((a) => a.kind !== "review");

  const matchedTasks = new Set<string>();
  for (const task of snap.tasks) {
    const phaseOk = !lens.phase || task.phase === lens.phase;
    const roleOk = !lens.role || taskMatchesRole(task, lens.role, snap);
    if (phaseOk && roleOk) {
      matchedTasks.add(task.id);
      nodes.add(task.id);
    }
  }

  if (lens.role === "coordinator") {
    nodes.add(COORD_ID);
    for (const a of authorAttempts) {
      if (a.workerId !== "coordinator" && a.workerId !== "exec" && a.kind !== "exec") continue;
      const t = taskById.get(a.taskId);
      if (!t || (lens.phase && t.phase !== lens.phase)) continue;
      nodes.add(t.id);
      edges.add(execRouteKey(a.workerId, t.id));
    }
    for (const w of authorWorkers(snap.workers)) {
      if (w.state === "busy" || w.state === "starting") {
        const wNode = workerNodeId(w.id);
        edges.add(dispatchRouteKey(wNode));
        nodes.add(wNode);
      }
      if (w.forkedFrom === "coordinator" || w.forkedFrom === COORD_ID) {
        const wNode = workerNodeId(w.id);
        nodes.add(wNode);
        edges.add(forkRouteKey(COORD_ID, wNode));
      }
    }
    for (const w of snap.workers) {
      if (!isReviewer(w)) continue;
      if (w.state === "busy" || w.state === "starting") {
        const rNode = reviewerNodeId(w.id);
        edges.add(dispatchRouteKey(rNode));
        nodes.add(rNode);
      }
    }
  } else if (lens.phase && !lens.role) {
    nodes.add(COORD_ID);
    for (const a of authorAttempts) {
      if (a.workerId !== "coordinator" && a.workerId !== "exec" && a.kind !== "exec") continue;
      const t = taskById.get(a.taskId);
      if (t && t.phase === lens.phase) {
        nodes.add(t.id);
        edges.add(execRouteKey(a.workerId, t.id));
      }
    }
  }

  for (const w of snap.workers) {
    const rev = isReviewer(w);
    const meta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
    const wNId = rev ? reviewerNodeId(w.id) : workerNodeId(w.id);

    if (rev) {
      const boardTasks = snap.tasks.filter((t) => {
        if (!matchedTasks.has(t.id)) return false;
        const board = reviewBoard(t, snap.attempts, snap.workers);
        return Boolean(board?.rounds.some((r) => r.worker?.id === w.id) || t.reviewState?.reviewerWorkerId === w.id);
      });
      const roleMatch = !lens.role || lens.role === "reviewer";
      const phaseMatch = !lens.phase || boardTasks.length > 0;
      if (roleMatch && phaseMatch) {
        nodes.add(wNId);
        nodes.add(COORD_ID);
        edges.add(dispatchRouteKey(wNId));
        for (const t of snap.tasks) {
          const board = reviewBoard(t, snap.attempts, snap.workers);
          const readsTask = Boolean(board?.rounds.some((r) => r.worker?.id === w.id) || t.reviewState?.reviewerWorkerId === w.id);
          if (readsTask && (!lens.phase || t.phase === lens.phase)) {
            nodes.add(t.id);
            edges.add(reviewRouteKey(t.id, wNId));
          }
        }
      }
      continue;
    }

    const wTasks = tasksInOrder(w, authorAttempts)
      .map((id) => taskById.get(id))
      .filter((t): t is Task => t != null);
    const roleMatch = !lens.role || meta.visualKind === lens.role;
    const phaseMatch = !lens.phase || wTasks.some((t) => t.phase === lens.phase);
    if (roleMatch && phaseMatch) {
      nodes.add(wNId);
      nodes.add(COORD_ID);
      edges.add(dispatchRouteKey(wNId));
      for (const t of wTasks) {
        if (!lens.phase || t.phase === lens.phase) {
          nodes.add(t.id);
          edges.add(execRouteKey(w.id, t.id));
        }
      }
      if (w.forkedFrom) {
        const parent = snap.workers.find((p) => p.id === w.forkedFrom);
        const src =
          w.forkedFrom === "coordinator" || w.forkedFrom === COORD_ID
            ? COORD_ID
            : parent
              ? (isReviewer(parent) ? reviewerNodeId(parent.id) : workerNodeId(parent.id))
              : null;
        if (src) {
          nodes.add(src);
          edges.add(forkRouteKey(src, wNId));
        }
      }
    }
  }

  for (const t of snap.tasks) {
    if (!nodes.has(t.id)) continue;
    for (const depId of t.deps) {
      if (nodes.has(depId)) edges.add(depRouteKey(depId, t.id));
    }
    if (t.when && isDataWhen(t.when)) {
      const src = dataBranchSource(t, snap.tasks);
      if (src && nodes.has(src.id)) {
        edges.add(`data-when:${src.id}__${t.id}`);
        edges.add(depRouteKey(src.id, t.id));
      }
    }
  }

  return { nodes, edges };
}
