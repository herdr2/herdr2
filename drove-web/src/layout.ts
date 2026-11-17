import dagre from "@dagrejs/dagre";
import type { LayoutKind } from "./types";

/**
 * Card sizes per node type. The cards render at exactly these sizes
 * (styles.css `.gnode--*`), so layout spacing matches the screen and edges
 * meet the handles without a measuring round-trip. Keep the two in sync.
 */
export const NODE_SIZE = {
  task: { w: 260, h: 122 },
  worker: { w: 240, h: 108 },
  reviewer: { w: 240, h: 108 },
  coordinator: { w: 240, h: 112 },
} as const;

export const ROLE_RANK: Record<string, number> = {
  coordinator: 0,
  coder: 1,
  tester: 2,
  writer: 3,
  researcher: 4,
  custom: 5,
  reviewer: 6,
};

export type Dir = "LR" | "TB";
export interface Pt {
  x: number;
  y: number;
}

export interface OrchInput {
  taskIds: string[];
  deps: { id: string; source: string; target: string }[];
  /** worker node id → the tasks it executed (a worker is one agent terminal, which can run several) */
  workers: { id: string; taskIds: string[]; roleRank?: number; roleGroup?: string; forkedFrom?: string }[];
  /** coordinator node id, or null when the worker overlay is off */
  coordId: string | null;
  /** Reviewer node id → the tasks that reviewer read. Drawn in their own group ahead of tasks alongside workers. */
  reviewers?: { id: string; taskIds: string[] }[];
}

/**
 * An orthogonal route, independent of exact handle coordinates.
 *
 * `lanes[i]` is where the edge runs *across* the layout in the i-th channel it
 * crosses (an x in LR, a y in TB); `cross[i]` is where it travels *along* the
 * layout through the i-th intermediate rank (a y in LR, an x in TB). The edge
 * component joins source → lanes/cross → target at render time, so a route
 * survives sub-pixel handle drift between polls.
 */
export interface Route {
  lanes: number[];
  cross: number[];
}

export interface OrchLayout {
  dir: Dir;
  /** top-left position of every node */
  pos: Map<string, Pt>;
  /** Keyed by depRouteKey / dispatchRouteKey / execRouteKey / reviewRouteKey. A missing key → smooth-step fallback. */
  routes: Map<string, Route>;
  /**
   * Layout-axis coordinate of the open channel between the leading groups
   * (orchestration fleet + reviewers) and the task group (an x in LR, a y in TB).
   * Absent when no leading group or no tasks exist.
   */
  seam?: number;
  /**
   * Deprecated right-side review channel; unused when reviewers sit on the
   * leading side ahead of `seam`.
   */
  reviewSeam?: number;
}

export const depRouteKey = (source: string, target: string) => `dep:${source}__${target}`;
export const dispatchRouteKey = (workerId: string) => `dispatch:${workerId}`;
export const forkRouteKey = (sourceId: string, targetId: string) => `fork:${sourceId}__${targetId}`;
export const execRouteKey = (workerId: string, taskId: string) => `exec:${workerId}__${taskId}`;
export const reviewRouteKey = (taskId: string, reviewerId: string) => `review:${taskId}__${reviewerId}`;

/**
 * Lay out the whole orchestration as one layered graph that reads in a single
 * direction (left → right, or top → bottom):
 *
 *   COORDINATOR ⇢ WORKER → TASK → TASK …
 *
 * Workers are ordinary dagre nodes. The worker → task edge carries a heavy
 * weight, so dagre puts each worker in the rank right before the (first) task
 * it ran — directly to its left in LR, directly above in TB — and a worker
 * that ran several tasks fans out to all of them.
 *
 * The coordinator gets an edge to every worker, plus a layout-only edge
 * (minlen 2) to every root task nobody has run yet, so undispatched roots line
 * up with dispatched ones instead of sharing the coordinator's rank.
 *
 * Long edges are the reason to keep dagre's routes: an edge spanning several
 * ranks gets dummy nodes in the empty slots of the ranks it crosses, and the
 * returned `points` thread through those slots — between cards, not through
 * them. React Flow alone would draw a straight shortcut across whatever sits
 * in the way.
 */
export function layoutOrchestration(kind: LayoutKind, input: OrchInput): OrchLayout {
  const dir: Dir = kind === "layered-tb" ? "TB" : "LR";
  const { task: T, worker: W, coordinator: C, reviewer: R } = NODE_SIZE;
  const workerIds = new Set(input.workers.map((w) => w.id));
  const reviewerIds = new Set((input.reviewers ?? []).map((reviewer) => reviewer.id));
  const sizeOf = (id: string) =>
    id === input.coordId ? C : reviewerIds.has(id) ? R : workerIds.has(id) ? W : T;

  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: dir, nodesep: 40, ranksep: 88, edgesep: 24, marginx: 24, marginy: 24 });
  g.setDefaultEdgeLabel(() => ({}));

  const known = new Set(input.taskIds);
  for (const id of input.taskIds) g.setNode(id, { width: T.w, height: T.h });
  const hasIncoming = new Set<string>();
  for (const e of input.deps) {
    if (!known.has(e.source) || !known.has(e.target)) continue;
    g.setEdge(e.source, e.target, { weight: 2 });
    hasIncoming.add(e.target);
  }
  const executed = new Set<string>();
  for (const w of input.workers) {
    g.setNode(w.id, { width: W.w, height: W.h });
    for (const t of w.taskIds) {
      if (!known.has(t)) continue;
      // heavy: keep a worker right next to its task
      g.setEdge(w.id, t, { weight: 8 });
      executed.add(t);
    }
  }
  for (const reviewer of input.reviewers ?? []) {
    g.setNode(reviewer.id, { width: R.w, height: R.h });
    for (const taskId of reviewer.taskIds) {
      if (!known.has(taskId)) continue;
      g.setEdge(taskId, reviewer.id, { weight: 3 });
    }
  }
  if (input.coordId) {
    g.setNode(input.coordId, { width: C.w, height: C.h });
    for (const w of input.workers) g.setEdge(input.coordId, w.id, { weight: 1 });
    for (const id of input.taskIds) {
      if (!hasIncoming.has(id) && !executed.has(id)) g.setEdge(input.coordId, id, { weight: 1, minlen: 2 });
    }
  }
  dagre.layout(g);

  const centers = new Map<string, Pt>();
  for (const id of g.nodes()) {
    const n = g.node(id);
    if (n) centers.set(id, { x: n.x, y: n.y });
  }

  const drawn: RouteEdge[] = [];
  for (const e of input.deps) {
    if (g.hasEdge(e.source, e.target)) {
      drawn.push({ key: depRouteKey(e.source, e.target), v: e.source, w: e.target, kind: "dep" });
    }
  }
  for (const w of input.workers) {
    for (const t of w.taskIds) {
      if (g.hasEdge(w.id, t)) drawn.push({ key: execRouteKey(w.id, t), v: w.id, w: t, kind: "exec" });
    }
    if (input.coordId && g.hasEdge(input.coordId, w.id)) {
      drawn.push({ key: dispatchRouteKey(w.id), v: input.coordId, w: w.id, kind: "dispatch" });
    }
  }
  for (const reviewer of input.reviewers ?? []) {
    for (const taskId of reviewer.taskIds) {
      if (g.hasEdge(taskId, reviewer.id)) {
        drawn.push({ key: reviewRouteKey(taskId, reviewer.id), v: taskId, w: reviewer.id, kind: "review" });
      }
    }
  }
  const extent = (id: string) => {
    const s = sizeOf(id);
    return dir === "LR" ? s.w / 2 : s.h / 2;
  };
  const routes = routeEdges(g, dir, drawn, centers, extent);

  const pos = new Map<string, Pt>();
  for (const [id, c] of centers) {
    const s = sizeOf(id);
    pos.set(id, { x: c.x - s.w / 2, y: c.y - s.h / 2 });
  }
  return { dir, pos, routes };
}

/** Clear gap between the orchestration column and the task DAG, in px. Wider than ranksep so the two groups stay readable as one canvas. */
export const GROUP_GAP = 240;

function groupBounds(pos: Map<string, Pt>, sizeOf: (id: string) => { w: number; h: number }) {
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const [id, p] of pos) {
    const s = sizeOf(id);
    minX = Math.min(minX, p.x);
    minY = Math.min(minY, p.y);
    maxX = Math.max(maxX, p.x + s.w);
    maxY = Math.max(maxY, p.y + s.h);
  }
  return { minX, minY, maxX, maxY };
}

function shiftLayout(layout: OrchLayout, dx: number, dy: number): void {
  if (dx === 0 && dy === 0) return;
  for (const [id, p] of layout.pos) layout.pos.set(id, { x: p.x + dx, y: p.y + dy });
  for (const [key, route] of layout.routes) {
    layout.routes.set(
      key,
      layout.dir === "LR"
        ? { lanes: route.lanes.map((v) => v + dx), cross: route.cross.map((v) => v + dy) }
        : { lanes: route.lanes.map((v) => v + dy), cross: route.cross.map((v) => v + dx) },
    );
  }
}

/** dagre `nodesep`. Reviewer cards keep at least this much air between them. */
const REVIEWER_GAP = 40;

function boardSizeOf(input: OrchInput, id: string): { w: number; h: number } {
  if (id === input.coordId) return NODE_SIZE.coordinator;
  if (input.reviewers?.some((reviewer) => reviewer.id === id)) return NODE_SIZE.reviewer;
  if (input.workers.some((worker) => worker.id === id)) return NODE_SIZE.worker;
  return NODE_SIZE.task;
}

function crossCenter(dir: Dir, id: string, pos: Map<string, Pt>, sizeOf: (id: string) => { w: number; h: number }): number | null {
  const p = pos.get(id);
  if (!p) return null;
  const size = sizeOf(id);
  return dir === "LR" ? p.y + size.h / 2 : p.x + size.w / 2;
}

/**
 * Order reviewers by the mean cross-axis center of the tasks they read, then
 * pack any that would overlap. A tight cluster stays centered on that mean so
 * task → reviewer edges cross as little as possible.
 */
function reviewerCrossCenters(
  dir: Dir,
  reviewers: { id: string; taskIds: string[] }[],
  taskPos: Map<string, Pt>,
  fallback: number,
  sizeOf: (id: string) => { w: number; h: number },
): { id: string; cross: number }[] {
  const card = dir === "LR" ? NODE_SIZE.reviewer.h : NODE_SIZE.reviewer.w;
  const minStep = card + REVIEWER_GAP;
  const ordered = reviewers
    .map((reviewer) => {
      const centers = reviewer.taskIds
        .map((taskId) => crossCenter(dir, taskId, taskPos, sizeOf))
        .filter((center): center is number => center != null);
      const ideal = centers.length > 0 ? centers.reduce((sum, center) => sum + center, 0) / centers.length : fallback;
      return { id: reviewer.id, ideal };
    })
    .sort((a, b) => a.ideal - b.ideal || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return packCrossCenters(ordered, minStep);
}

function placeReviewers(
  dir: Dir,
  reviewers: { id: string; taskIds: string[] }[],
  along: number,
  taskPos: Map<string, Pt> | null,
  fallback: number,
  sizeOf: (id: string) => { w: number; h: number },
): Map<string, Pt> {
  const pos = new Map<string, Pt>();
  if (reviewers.length === 0) return pos;
  const R = NODE_SIZE.reviewer;
  const placed = reviewerCrossCenters(dir, reviewers, taskPos ?? new Map(), fallback, sizeOf);
  for (const reviewer of placed) {
    pos.set(
      reviewer.id,
      dir === "LR"
        ? { x: along, y: reviewer.cross - R.h / 2 }
        : { x: reviewer.cross - R.w / 2, y: along },
    );
  }
  return pos;
}

/** Gap between adjacent worker cards inside the same category cluster, px. */
export const WORKER_INNER_GAP = 24;
/** Cross-axis gap between different worker category clusters (room for category frame headers & borders), px. */
export const WORKER_GROUP_GAP = 56;
/** Cross-axis gap between the orchestration fleet zone and the review zone, px (> zone backdrop top+bottom pad). */
const ZONE_CROSS_GAP = 100;
/** Gap between coordinator and worker/reviewer column/row, px (matches dagre ranksep). */
const ORCH_RANK_GAP = 88;

function packCrossCenters(items: { id: string; ideal: number }[], minStep: number): { id: string; cross: number }[] {
  const cross = new Array<number>(items.length);
  let index = 0;
  while (index < items.length) {
    let end = index;
    let sum = 0;
    for (;;) {
      sum += items[end]?.ideal ?? 0;
      const count = end - index + 1;
      const center = sum / count;
      const start = center - ((count - 1) * minStep) / 2;
      const tail = start + (count - 1) * minStep;
      const next = items[end + 1];
      if (next && next.ideal < tail + minStep) {
        end += 1;
        continue;
      }
      for (let k = 0; k < count; k++) cross[index + k] = start + k * minStep;
      index = end + 1;
      break;
    }
  }
  for (let i = 1; i < cross.length; i++) {
    const min = (cross[i - 1] ?? 0) + minStep;
    if ((cross[i] ?? 0) < min) cross[i] = min;
  }
  return items.map((item, i) => ({ id: item.id, cross: cross[i] ?? item.ideal }));
}

function workerIdealCross(
  dir: Dir,
  workers: OrchInput["workers"],
  taskPos: Map<string, Pt> | null,
  fallback: number,
  sizeOf: (id: string) => { w: number; h: number },
): Map<string, number> {
  const direct = new Map<string, number>();
  const workerById = new Map(workers.map((w) => [w.id, w]));
  for (const w of workers) {
    if (!taskPos) continue;
    const centers = w.taskIds
      .map((taskId) => crossCenter(dir, taskId, taskPos, sizeOf))
      .filter((center): center is number => center != null);
    if (centers.length > 0) direct.set(w.id, centers.reduce((sum, c) => sum + c, 0) / centers.length);
  }
  const resolved = new Map<string, number>();
  const resolveOne = (id: string, trail: Set<string>): number => {
    const cached = resolved.get(id);
    if (cached != null) return cached;
    const own = direct.get(id);
    if (own != null) {
      resolved.set(id, own);
      return own;
    }
    const w = workerById.get(id);
    if (w?.forkedFrom && workerById.has(w.forkedFrom) && !trail.has(w.forkedFrom)) {
      trail.add(id);
      const val = resolveOne(w.forkedFrom, trail) + 0.01;
      resolved.set(id, val);
      return val;
    }
    resolved.set(id, fallback);
    return fallback;
  };
  for (const w of workers) resolveOne(w.id, new Set<string>());
  return resolved;
}

function packWorkerGroups(
  workers: OrchInput["workers"],
  ideals: Map<string, number>,
  fallback: number,
  wCross: number,
): { id: string; cross: number }[] {
  if (workers.length === 0) return [];
  interface WorkerMember {
    id: string;
    ideal: number;
    roleRank: number;
  }
  interface WorkerBucket {
    key: string;
    members: WorkerMember[];
    groupRoleRank: number;
    groupIdeal: number;
  }
  const byKey = new Map<string, WorkerMember[]>();
  for (const w of workers) {
    const key = w.roleGroup ?? `rank:${w.roleRank ?? ROLE_RANK.custom}`;
    const list = byKey.get(key) ?? [];
    list.push({
      id: w.id,
      ideal: ideals.get(w.id) ?? fallback,
      roleRank: w.roleRank ?? ROLE_RANK.custom,
    });
    byKey.set(key, list);
  }
  const buckets: WorkerBucket[] = [];
  for (const [key, members] of byKey) {
    members.sort((a, b) => a.ideal - b.ideal || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
    const groupRoleRank = Math.min(...members.map((m) => m.roleRank));
    const groupIdeal = members.reduce((sum, m) => sum + m.ideal, 0) / members.length;
    buckets.push({ key, members, groupRoleRank, groupIdeal });
  }
  buckets.sort(
    (a, b) =>
      a.groupRoleRank - b.groupRoleRank ||
      a.groupIdeal - b.groupIdeal ||
      (a.key < b.key ? -1 : a.key > b.key ? 1 : 0),
  );

  const innerStep = wCross + WORKER_INNER_GAP;
  const outerStep = wCross + WORKER_GROUP_GAP;
  const halfSpan = buckets.map((b) => ((b.members.length - 1) * innerStep) / 2);
  const prefix = new Array<number>(buckets.length);
  prefix[0] = 0;
  for (let g = 1; g < buckets.length; g++) {
    const minSep = (halfSpan[g - 1] ?? 0) + (halfSpan[g] ?? 0) + outerStep;
    prefix[g] = (prefix[g - 1] ?? 0) + minSep;
  }

  interface PavBlock {
    start: number;
    end: number;
    sumWU: number;
    sumW: number;
  }
  const stack: PavBlock[] = [];
  for (let g = 0; g < buckets.length; g++) {
    const bucket = buckets[g]!;
    const weight = bucket.members.length;
    const u = bucket.groupIdeal - (prefix[g] ?? 0);
    stack.push({ start: g, end: g, sumWU: weight * u, sumW: weight });
    while (stack.length >= 2) {
      const prev = stack[stack.length - 2]!;
      const curr = stack[stack.length - 1]!;
      if (prev.sumWU / prev.sumW <= curr.sumWU / curr.sumW) break;
      stack.pop();
      prev.end = curr.end;
      prev.sumWU += curr.sumWU;
      prev.sumW += curr.sumW;
    }
  }
  const center = new Array<number>(buckets.length);
  for (const block of stack) {
    const uVal = block.sumWU / block.sumW;
    for (let g = block.start; g <= block.end; g++) {
      center[g] = uVal + (prefix[g] ?? 0);
    }
  }

  const out: { id: string; cross: number }[] = [];
  for (let g = 0; g < buckets.length; g++) {
    const members = buckets[g]!.members;
    const startCross = (center[g] ?? fallback) - (halfSpan[g] ?? 0);
    for (let k = 0; k < members.length; k++) {
      out.push({ id: members[k]!.id, cross: startCross + k * innerStep });
    }
  }
  return out;
}

/**
 * Place coordinator and workers aligned with the cross-axis centroids of each
 * worker's tasks (with `forkedFrom` inheritance and category clustering via `roleGroup`/`roleRank`),
 * while guaranteeing zero overlap and fanned dispatch lanes.
 */
function placeOrchestration(
  dir: Dir,
  input: OrchInput,
  taskPos: Map<string, Pt> | null,
  sizeOf: (id: string) => { w: number; h: number },
): OrchLayout {
  const C = NODE_SIZE.coordinator;
  const W = NODE_SIZE.worker;
  const pos = new Map<string, Pt>();
  const routes = new Map<string, Route>();

  const tb = taskPos && taskPos.size > 0 ? groupBounds(taskPos, sizeOf) : null;
  const wCross = dir === "LR" ? W.h : W.w;
  const cCross = dir === "LR" ? C.h : C.w;
  const cAlong = dir === "LR" ? C.w : C.h;
  const fallback = tb ? (dir === "LR" ? (tb.minY + tb.maxY) / 2 : (tb.minX + tb.maxX) / 2) : 24 + Math.max(cCross, wCross) / 2;

  const ideals = workerIdealCross(dir, input.workers, taskPos, fallback, sizeOf);
  const placedWorkers = packWorkerGroups(input.workers, ideals, fallback, wCross);

  const coordAlong = 24;
  const workerAlong = input.coordId ? coordAlong + cAlong + ORCH_RANK_GAP : 24;

  for (const w of placedWorkers) {
    pos.set(
      w.id,
      dir === "LR"
        ? { x: workerAlong, y: w.cross - W.h / 2 }
        : { x: w.cross - W.w / 2, y: workerAlong },
    );
  }

  if (input.coordId) {
    const coordCross =
      placedWorkers.length > 0
        ? ((placedWorkers[0]?.cross ?? fallback) + (placedWorkers[placedWorkers.length - 1]?.cross ?? fallback)) / 2
        : fallback;
    pos.set(
      input.coordId,
      dir === "LR"
        ? { x: coordAlong, y: coordCross - C.h / 2 }
        : { x: coordCross - C.w / 2, y: coordAlong },
    );
    if (placedWorkers.length > 0) {
      const coordEnd = coordAlong + cAlong;
      const gap = workerAlong - coordEnd - 24;
      const mid = (coordEnd + workerAlong) / 2;
      const step = placedWorkers.length > 1 ? Math.max(4, Math.min(LANE_STEP, gap / (placedWorkers.length - 1))) : 0;
      placedWorkers.forEach((w, index) => {
        const lane = mid + (index - (placedWorkers.length - 1) / 2) * step;
        routes.set(dispatchRouteKey(w.id), { lanes: [lane], cross: [] });
      });
    }
  }

  return { dir, pos, routes };
}

/**
 * One coordinate space, two leading zones + task pipeline:
 *   LR: orchestration fleet (coordinator + workers) and review zone on the left,
 *       task DAG on the right of `seam`.
 *   TB: orchestration fleet and review zone on top, task DAG below `seam`.
 * Worker/reviewer ↔ task edges are not dagre layout edges; callers route them
 * dynamically across `seam` so cards keep a clear readable channel.
 */
export function layoutBoard(kind: LayoutKind, input: OrchInput): OrchLayout {
  const dir: Dir = kind === "layered-tb" ? "TB" : "LR";
  const reviewers = input.reviewers ?? [];
  const sizeOf = (id: string) => boardSizeOf(input, id);
  const orchOn = Boolean(input.coordId) || input.workers.length > 0;
  const tasks = input.taskIds.length > 0
    ? layoutOrchestration(kind, {
        taskIds: input.taskIds,
        deps: input.deps,
        workers: [],
        coordId: null,
      })
    : null;
  const orch = orchOn
    ? placeOrchestration(dir, input, tasks && tasks.pos.size > 0 ? tasks.pos : null, sizeOf)
    : null;

  const coordAlong = 24;
  const cAlong = dir === "LR" ? NODE_SIZE.coordinator.w : NODE_SIZE.coordinator.h;
  const leadingAlong = input.coordId ? coordAlong + cAlong + ORCH_RANK_GAP : 24;

  let reviewPos = new Map<string, Pt>();
  if (reviewers.length > 0) {
    const tb = tasks && tasks.pos.size > 0 ? groupBounds(tasks.pos, sizeOf) : null;
    const ob = orch && orch.pos.size > 0 ? groupBounds(orch.pos, sizeOf) : null;
    const baseFallback = tb
      ? (dir === "LR" ? (tb.minY + tb.maxY) / 2 : (tb.minX + tb.maxX) / 2)
      : ob
        ? (dir === "LR" ? ob.maxY + ZONE_CROSS_GAP : ob.maxX + ZONE_CROSS_GAP)
        : 24 + (dir === "LR" ? NODE_SIZE.reviewer.h : NODE_SIZE.reviewer.w) / 2;
    reviewPos = placeReviewers(dir, reviewers, leadingAlong, tasks && tasks.pos.size > 0 ? tasks.pos : null, baseFallback, sizeOf);

    if (ob && reviewPos.size > 0) {
      const rb = groupBounds(reviewPos, sizeOf);
      if (dir === "LR" && rb.minY < ob.maxY + ZONE_CROSS_GAP) {
        const shiftY = ob.maxY + ZONE_CROSS_GAP - rb.minY;
        for (const [id, p] of reviewPos) reviewPos.set(id, { x: p.x, y: p.y + shiftY });
      } else if (dir === "TB" && rb.minX < ob.maxX + ZONE_CROSS_GAP) {
        const shiftX = ob.maxX + ZONE_CROSS_GAP - rb.minX;
        for (const [id, p] of reviewPos) reviewPos.set(id, { x: p.x + shiftX, y: p.y });
      }
    }
  }

  const leadingPos = new Map<string, Pt>();
  if (orch) for (const [id, p] of orch.pos) leadingPos.set(id, p);
  for (const [id, p] of reviewPos) leadingPos.set(id, p);

  if (leadingPos.size > 0) {
    const lb = groupBounds(leadingPos, sizeOf);
    const minCross = dir === "LR" ? lb.minY : lb.minX;
    if (minCross < 24) {
      const shiftCross = 24 - minCross;
      if (dir === "LR") {
        if (orch) shiftLayout(orch, 0, shiftCross);
        for (const [id, p] of reviewPos) reviewPos.set(id, { x: p.x, y: p.y + shiftCross });
        if (tasks) shiftLayout(tasks, 0, shiftCross);
      } else {
        if (orch) shiftLayout(orch, shiftCross, 0);
        for (const [id, p] of reviewPos) reviewPos.set(id, { x: p.x + shiftCross, y: p.y });
        if (tasks) shiftLayout(tasks, shiftCross, 0);
      }
      leadingPos.clear();
      if (orch) for (const [id, p] of orch.pos) leadingPos.set(id, p);
      for (const [id, p] of reviewPos) leadingPos.set(id, p);
    }
  }

  if (leadingPos.size > 0 && tasks && tasks.pos.size > 0) {
    const lb = groupBounds(leadingPos, sizeOf);
    const tb = groupBounds(tasks.pos, sizeOf);
    const dx = dir === "LR" ? lb.maxX + GROUP_GAP - tb.minX : 0;
    const dy = dir === "LR" ? 0 : lb.maxY + GROUP_GAP - tb.minY;
    shiftLayout(tasks, dx, dy);
  }

  const pos = new Map<string, Pt>();
  const routes = new Map<string, Route>();
  for (const part of [orch, tasks]) {
    if (!part) continue;
    for (const [id, p] of part.pos) pos.set(id, p);
    for (const [key, route] of part.routes) routes.set(key, route);
  }
  for (const [id, p] of reviewPos) pos.set(id, p);

  // Fan all Coordinator -> Worker / Reviewer dispatch lanes without crossing.
  const coordPt = input.coordId ? pos.get(input.coordId) : undefined;
  if (input.coordId && coordPt) {
    const coordStartAlong = dir === "LR" ? coordPt.x : coordPt.y;
    const coordCross =
      (dir === "LR" ? coordPt.y : coordPt.x) +
      (dir === "LR" ? NODE_SIZE.coordinator.h : NODE_SIZE.coordinator.w) / 2;
    const dispatchTargets: { id: string; along: number; cross: number }[] = [];
    for (const w of input.workers) {
      const wp = pos.get(w.id);
      if (!wp) continue;
      const ws = sizeOf(w.id);
      dispatchTargets.push({
        id: w.id,
        along: dir === "LR" ? wp.x : wp.y,
        cross: (dir === "LR" ? wp.y : wp.x) + (dir === "LR" ? ws.h : ws.w) / 2,
      });
    }
    for (const r of reviewers) {
      const rp = pos.get(r.id);
      if (!rp) continue;
      const rs = sizeOf(r.id);
      dispatchTargets.push({
        id: r.id,
        along: dir === "LR" ? rp.x : rp.y,
        cross: (dir === "LR" ? rp.y : rp.x) + (dir === "LR" ? rs.h : rs.w) / 2,
      });
    }
    if (dispatchTargets.length > 0) {
      const coordEnd = coordStartAlong + cAlong;
      const minTargetAlong = Math.min(...dispatchTargets.map((t) => t.along));
      const gap = Math.max(8, minTargetAlong - coordEnd - 24);
      const mid = (coordEnd + minTargetAlong) / 2;
      const step =
        dispatchTargets.length > 1
          ? Math.max(4, Math.min(LANE_STEP, gap / (dispatchTargets.length - 1)))
          : 0;
      dispatchTargets.sort(
        (a, b) =>
          Math.abs(b.cross - coordCross) - Math.abs(a.cross - coordCross) ||
          a.cross - b.cross ||
          (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
      );
      dispatchTargets.forEach((target, index) => {
        const lane = mid + (index - (dispatchTargets.length - 1) / 2) * step;
        routes.set(dispatchRouteKey(target.id), { lanes: [lane], cross: [] });
      });
    }
  }

  let seam: number | undefined;
  if (leadingPos.size > 0 && tasks && tasks.pos.size > 0) {
    const lb = groupBounds(leadingPos, sizeOf);
    const tb = groupBounds(tasks.pos, sizeOf);
    seam = dir === "LR" ? (lb.maxX + tb.minX) / 2 : (lb.maxY + tb.minY) / 2;

    if (reviewPos.size > 0) {
      const gap = (dir === "LR" ? tb.minX - lb.maxX : tb.minY - lb.maxY) - 48;
      const links: { taskId: string; reviewerId: string }[] = [];
      for (const reviewer of reviewers) {
        for (const taskId of reviewer.taskIds) {
          if (tasks.pos.has(taskId) && reviewPos.has(reviewer.id)) links.push({ taskId, reviewerId: reviewer.id });
        }
      }
      const step = links.length > 1 ? Math.max(4, Math.min(16, gap / (links.length - 1))) : 0;
      links.forEach((link, index) => {
        const lane = seam! + (index - (links.length - 1) / 2) * step;
        routes.set(reviewRouteKey(link.taskId, link.reviewerId), { lanes: [lane], cross: [] });
      });
    }
  }

  return { dir, pos, routes, seam };
}

interface RouteEdge {
  key: string;
  v: string;
  w: string;
  kind: "dep" | "dispatch" | "exec" | "review";
}

/** Space between parallel lanes in one channel, px. */
const LANE_STEP = 16;

/**
 * Orthogonal routes for every drawn edge.
 *
 * dagre gives each rank a shared center on the layout axis; the open band
 * between two consecutive ranks is a *channel*. An edge from rank a to rank b
 * crosses channels a … b-1 (running across the layout inside each) and travels
 * along the layout through ranks a+1 … b-1 at the position of the dummy node
 * dagre reserved for it there — an empty slot, so it never cuts a card.
 *
 * Lanes keep different edges from sharing one line. Within a channel, edges
 * that fan out of the same source share a lane (a bus), as do edges that fan
 * into the same target; everything else gets its own. Lanes are spread around
 * the channel's middle and ordered by where they lead, which keeps crossings
 * down on small graphs.
 */
function routeEdges(
  g: dagre.graphlib.Graph,
  dir: Dir,
  edges: RouteEdge[],
  centers: Map<string, Pt>,
  extent: (id: string) => number,
): Map<string, Route> {
  const along = (p: Pt) => (dir === "LR" ? p.x : p.y);
  const across = (p: Pt) => (dir === "LR" ? p.y : p.x);

  // ranks: nodes sharing a center on the layout axis
  const ids = g.nodes().filter((id) => centers.has(id));
  const rankCenters: number[] = [];
  for (const c of ids.map((id) => along(centers.get(id)!)).sort((a, b) => a - b)) {
    if (!rankCenters.length || c - rankCenters[rankCenters.length - 1] > 1) rankCenters.push(c);
  }
  const rankOf = (id: string) => {
    const c = along(centers.get(id)!);
    return rankCenters.findIndex((r) => Math.abs(r - c) <= 1);
  };
  const lo = rankCenters.map(() => Infinity);
  const hi = rankCenters.map(() => -Infinity);
  for (const id of ids) {
    const r = rankOf(id);
    const c = along(centers.get(id)!);
    lo[r] = Math.min(lo[r], c - extent(id));
    hi[r] = Math.max(hi[r], c + extent(id));
  }

  const outCount = new Map<string, number>();
  for (const e of edges) outCount.set(`${e.v}:${e.kind}`, (outCount.get(`${e.v}:${e.kind}`) ?? 0) + 1);

  type Plan = { e: RouteEdge; ra: number; rb: number; cross: number[]; keys: string[] };
  const plans: Plan[] = [];
  // channel index → lane key → sort hint (cross-axis position it leads to)
  const channelKeys = new Map<number, Map<string, number>>();

  for (const e of edges) {
    if (!centers.has(e.v) || !centers.has(e.w)) continue;
    const ra = rankOf(e.v);
    const rb = rankOf(e.w);
    if (ra < 0 || rb <= ra) continue;
    const pts = ((g.edge(e.v, e.w)?.points ?? []) as Pt[]).filter((p) => Number.isFinite(p.x) && Number.isFinite(p.y));
    const cross: number[] = [];
    for (let r = ra + 1; r < rb; r++) {
      // the dummy dagre placed for this edge in rank r sits at that rank's center
      let best: Pt | null = null;
      for (const p of pts) if (!best || Math.abs(along(p) - rankCenters[r]) < Math.abs(along(best) - rankCenters[r])) best = p;
      cross.push(best ? across(best) : across(centers.get(e.w)!));
    }
    const keys: string[] = [];
    for (let c = ra; c < rb; c++) {
      let key: string;
      let hint: number;
      const first = c === ra;
      const last = c === rb - 1;
      if (first && (!last || (outCount.get(`${e.v}:${e.kind}`) ?? 0) > 1)) {
        key = `s:${e.v}:${e.kind}`;
        hint = across(centers.get(e.v)!);
      } else if (last) {
        key = `t:${e.w}:${e.kind}`;
        hint = across(centers.get(e.w)!);
      } else {
        key = `e:${e.key}`;
        hint = cross[c - ra - 1] ?? 0;
      }
      keys.push(key);
      const m = channelKeys.get(c) ?? new Map<string, number>();
      if (!m.has(key)) m.set(key, hint);
      channelKeys.set(c, m);
    }
    plans.push({ e, ra, rb, cross, keys });
  }

  // lane positions per channel
  const lanePos = new Map<string, number>();
  for (const [c, keys] of channelKeys) {
    if (!Number.isFinite(hi[c]) || !Number.isFinite(lo[c + 1])) continue;
    const mid = (hi[c] + lo[c + 1]) / 2;
    const width = lo[c + 1] - hi[c];
    const sorted = [...keys.entries()].sort((a, b) => a[1] - b[1]).map(([k]) => k);
    const n = sorted.length;
    const step = n > 1 ? Math.max(4, Math.min(LANE_STEP, (width - 24) / (n - 1))) : 0;
    sorted.forEach((k, i) => lanePos.set(`${c}|${k}`, mid + (i - (n - 1) / 2) * step));
  }

  const out = new Map<string, Route>();
  for (const p of plans) {
    const lanes = p.keys.map((k, i) => lanePos.get(`${p.ra + i}|${k}`));
    if (lanes.some((l) => l === undefined)) continue;
    out.set(p.e.key, { lanes: lanes as number[], cross: p.cross });
  }
  return out;
}
