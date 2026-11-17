import type { CSSProperties } from "react";
import { BaseEdge, EdgeLabelRenderer, getBezierPath, getSmoothStepPath, Position, type EdgeProps } from "@xyflow/react";
import type { ExecRoutePlan } from "../dag-lens";
import type { Dir, Pt, Route } from "../layout";

export type RoutedEdgeData = {
  /** Orthogonal route from the layout. Absent → plain smooth-step. */
  route?: Route;
  /** Open seam channel between leading zones and tasks. */
  seam?: number;
  reviewSeam?: number;
  review?: boolean;
  /** Precomputed obstacle-avoiding route plan from `computeExecRoutes`. */
  plan?: ExecRoutePlan;
  /** Plan `from` is left/top of seam while edge source is right/bottom (`Task → Reviewer`). */
  reversePlan?: boolean;
  /** Parallel spread for multiple review rounds between the same task and reviewer. */
  parallelOffset?: number;
  labelShift?: number;
  dir?: Dir;
  /** Small caption drawn at the middle of the route, e.g. a branch edge. */
  label?: string;
  labelClass?: string;
};

const RADIUS = 8;
/** How far a handle may drift past a lane before the route is considered stale. */
const SLACK = 2;

function cleanOrthogonalPoints(pts: Pt[]): Pt[] {
  const dedup = pts.filter((p, i) => i === 0 || Math.hypot(p.x - pts[i - 1].x, p.y - pts[i - 1].y) > 0.5);
  if (dedup.length <= 2) return dedup;
  const clean: Pt[] = [dedup[0]];
  for (let i = 1; i < dedup.length - 1; i++) {
    const a = clean[clean.length - 1];
    const b = dedup[i];
    const c = dedup[i + 1];
    const straight = (Math.abs(a.x - b.x) < 0.5 && Math.abs(b.x - c.x) < 0.5) || (Math.abs(a.y - b.y) < 0.5 && Math.abs(b.y - c.y) < 0.5);
    if (!straight) clean.push(b);
  }
  clean.push(dedup[dedup.length - 1]);
  return clean;
}

function clampBetween(v: number, a: number, b: number): number {
  const lo = Math.min(a, b);
  const hi = Math.max(a, b);
  return Math.max(lo, Math.min(hi, v));
}

/**
 * Join source → route → target into an orthogonal polyline (LR shown; TB is
 * the same with axes swapped):
 *
 *   S ─▶ (lane0, S.y) ─▶ (lane0, cross0) ─▶ (lane1, cross0) … (laneN, T.y) ─▶ T
 *
 * Returns null when the handles no longer fit the route — after a node drag,
 * say — so the caller can fall back instead of drawing a line that doubles
 * back through a card.
 */
function polyline(s: Pt, t: Pt, route: Route, dir: Dir): Pt[] | null {
  const A = (p: Pt) => (dir === "LR" ? p.x : p.y);
  const at = (alongV: number, acrossV: number): Pt => (dir === "LR" ? { x: alongV, y: acrossV } : { x: acrossV, y: alongV });
  const { lanes, cross } = route;
  if (lanes.length === 0 || cross.length !== lanes.length - 1) return null;
  let prev = A(s);
  for (const l of lanes) {
    if (l < prev - SLACK) return null;
    prev = l;
  }
  if (A(t) < prev - SLACK) return null;

  const sAcross = dir === "LR" ? s.y : s.x;
  const tAcross = dir === "LR" ? t.y : t.x;
  const pts: Pt[] = [s, at(lanes[0], sAcross)];
  for (let i = 0; i < cross.length; i++) {
    pts.push(at(lanes[i], cross[i]), at(lanes[i + 1], cross[i]));
  }
  pts.push(at(lanes[lanes.length - 1], tAcross), t);

  return cleanOrthogonalPoints(pts);
}

/** SVG path through `pts`, each corner rounded by up to RADIUS. */
function roundedPath(pts: Pt[]): string {
  let d = `M${pts[0].x},${pts[0].y}`;
  for (let i = 1; i < pts.length - 1; i++) {
    const p = pts[i - 1];
    const c = pts[i];
    const n = pts[i + 1];
    const r = Math.min(RADIUS, Math.hypot(c.x - p.x, c.y - p.y) / 2, Math.hypot(n.x - c.x, n.y - c.y) / 2);
    const ax = c.x + Math.sign(p.x - c.x) * r;
    const ay = c.y + Math.sign(p.y - c.y) * r;
    const bx = c.x + Math.sign(n.x - c.x) * r;
    const by = c.y + Math.sign(n.y - c.y) * r;
    d += ` L${ax},${ay} Q${c.x},${c.y} ${bx},${by}`;
  }
  const last = pts[pts.length - 1];
  return `${d} L${last.x},${last.y}`;
}

/**
 * Edge that follows the layout's orthogonal route through the gaps between
 * ranks. Without a usable route (a dragged node, an edge dagre couldn't rank)
 * it falls back to React Flow's smooth-step.
 */
export function RoutedEdge(props: EdgeProps) {
  const { sourceX, sourceY, targetX, targetY, sourcePosition, targetPosition, markerEnd, style, interactionWidth } =
    props;
  const data = (props.data ?? {}) as RoutedEdgeData;
  const dir = data.dir ?? "LR";
  const isTB = dir === "TB";
  const parallel = data.review && data.parallelOffset ? data.parallelOffset : 0;
  const sPt = { x: sourceX, y: sourceY };
  const tPt = { x: targetX, y: targetY };

  let pts: Pt[] | null = null;
  const planned = data.plan ? plannedExecPath(sPt, tPt, dir, data.plan, Boolean(data.reversePlan)) : null;
  if (!planned && data.route) {
    if (parallel !== 0) {
      const lanes = data.route.lanes.map((l) => l + parallel * 0.8);
      const along = (p: Pt) => (isTB ? p.y : p.x);
      const across = (p: Pt) => (isTB ? p.x : p.y);
      const at = (a: number, c: number): Pt => (isTB ? { x: c, y: a } : { x: a, y: c });
      const sAlong = along(sPt);
      const tAlong = along(tPt);
      const sCross = across(sPt);
      const tCross = across(tPt);
      const crossStep = Math.max(-12, Math.min(12, parallel * 0.45));
      const sClear = Math.min(14, Math.max(4, (lanes[0] - sAlong) / 3));
      const tClear = Math.min(14, Math.max(4, (tAlong - lanes[lanes.length - 1]) / 3));
      const sEff = at(sAlong + sClear, sCross + crossStep);
      const tEff = at(tAlong - tClear, tCross + crossStep);
      const inner = polyline(sEff, tEff, { lanes, cross: data.route.cross }, dir);
      if (inner) {
        pts = cleanOrthogonalPoints([sPt, at(sAlong + sClear, sCross), ...inner, at(tAlong - tClear, tCross), tPt]);
      }
    } else {
      pts = polyline(sPt, tPt, data.route, dir);
    }
  }

  let path: string;
  let labelAt: Pt | null = null;
  const seam = data.reviewSeam ?? data.seam;
  const via = !planned && seam != null ? throughSeam(sPt, tPt, seam + parallel * 0.8, dir, 0) : null;
  if (planned) {
    path = planned.d;
    labelAt = { x: planned.lx, y: planned.ly };
  } else if (pts) {
    path = roundedPath(pts);
    // Review routes have one lane: place the capsule in the open channel.
    const middle = Math.floor((pts.length - 1) / 2);
    labelAt = { x: (pts[middle].x + pts[middle + 1].x) / 2, y: (pts[middle].y + pts[middle + 1].y) / 2 };
  } else if (via) {
    path = via.d;
    labelAt = { x: via.lx, y: via.ly };
  } else {
    const stepped = getSmoothStepPath({
      sourceX,
      sourceY,
      targetX,
      targetY,
      sourcePosition: sourcePosition ?? Position.Right,
      targetPosition: targetPosition ?? Position.Left,
      borderRadius: RADIUS,
    });
    path = stepped[0];
    labelAt = { x: stepped[1], y: stepped[2] };
  }
  if (!planned && labelAt && (data.labelShift || parallel !== 0)) {
    const shift = data.labelShift ?? parallel;
    labelAt = isTB ? { x: labelAt.x + shift, y: labelAt.y } : { x: labelAt.x, y: labelAt.y + shift };
  }
  return (
    <>
      <BaseEdge path={path} markerEnd={markerEnd} style={style} interactionWidth={interactionWidth ?? 16} />
      {data.label && labelAt && (
        <EdgeLabelRenderer>
          <div
            className={`${data.review ? "review-label" : "branch-label"}${data.labelClass ? ` ${data.review ? "review-label" : "branch-label"}--${data.labelClass}` : ""} nopan`}
            style={{ position: "absolute", transform: `translate(-50%, -50%) translate(${labelAt.x}px, ${labelAt.y}px)` }}
          >
            {data.label}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

export type ExecEdgeData = {
  /** Parallel edges that share a pair are spread across this lane (0 = center). */
  lane?: number;
  label?: string;
  mark?: string;
  /** Always-on edge colored by the latest attempt. Success labels use green. */
  persist?: boolean;
  /** Actively executing live edge highlights in role color. */
  live?: boolean;
  /** CSS color variable/string for role accenting. */
  roleColor?: string;
  /** Open channel between the two groups. The edge jogs there instead of cutting cards. */
  seam?: number;
  dir?: Dir;
  /** Precomputed obstacle-avoiding route plan from `computeExecRoutes`. */
  plan?: ExecRoutePlan;
};

const LANE_SHIFT = 26;

/**
 * Worker → task edge across the gap between the two groups, with an optional label.
 * Shown for every pair while the toolbar switch is on, and only for the selection while it is off.
 */
function throughSeam(s: Pt, t: Pt, seam: number, dir: Dir, lane: number): { d: string; lx: number; ly: number } | null {
  const shift = lane * LANE_SHIFT;
  if (dir === "TB") {
    const y = seam + shift;
    if (y <= Math.min(s.y, t.y) + 8 || y >= Math.max(s.y, t.y) - 8) return null;
    return { d: roundedPath([s, { x: s.x, y }, { x: t.x, y }, t]), lx: (s.x + t.x) / 2, ly: y };
  }
  const x = seam + shift;
  if (x <= Math.min(s.x, t.x) + 8 || x >= Math.max(s.x, t.x) - 8) return null;
  return { d: roundedPath([s, { x, y: s.y }, { x, y: t.y }, t]), lx: x, ly: (s.y + t.y) / 2 };
}

function plannedExecPath(s: Pt, t: Pt, dir: Dir, plan: ExecRoutePlan, reverse = false): { d: string; lx: number; ly: number } | null {
  const isTB = dir === "TB";
  const along = (p: Pt) => (isTB ? p.y : p.x);
  const across = (p: Pt) => (isTB ? p.x : p.y);
  const at = (a: number, c: number): Pt => (isTB ? { x: c, y: a } : { x: a, y: c });

  const leftPt = reverse ? t : s;
  const rightPt = reverse ? s : t;
  const leftAlong = along(leftPt);
  const rightAlong = along(rightPt);
  if (leftAlong + 8 >= plan.seamAlong || plan.seamAlong >= rightAlong - 8) return null;

  const sourceShift = reverse ? Math.max(-14, Math.min(14, plan.sourceOffset ?? 0)) : 0;
  const entryShift = Math.max(-14, Math.min(14, plan.entryOffset));
  const leftBaseCross = across(leftPt);
  const rightBaseCross = across(rightPt);
  const leftCross = leftBaseCross + sourceShift;
  const rightCross = rightBaseCross + entryShift;

  const hasPre =
    plan.preSeamAlong != null &&
    plan.preCorridorCross != null &&
    plan.preSeamAlong > leftAlong + 4 &&
    plan.preSeamAlong < plan.seamAlong - 4;
  const hasPost =
    plan.corridorCross != null &&
    plan.dropAlong != null &&
    plan.dropAlong > plan.seamAlong + 4 &&
    plan.dropAlong < rightAlong - 4;

  const curLeftCross = hasPre ? plan.preCorridorCross! : leftCross;
  const postCross = hasPost ? plan.corridorCross! : rightCross;
  let pts: Pt[];
  if (reverse) {
    pts = [s];
    if (hasPost) {
      if (Math.abs(entryShift) > 0.5) {
        const stubAlong = Math.min(rightAlong - 4, Math.max(plan.dropAlong! + 4, rightAlong - 8));
        pts.push(at(stubAlong, rightBaseCross), at(stubAlong, rightCross));
      }
      pts.push(at(plan.dropAlong!, rightCross), at(plan.dropAlong!, plan.corridorCross!), at(plan.seamAlong, plan.corridorCross!));
    } else {
      if (Math.abs(entryShift) > 0.5) {
        const stubAlong = Math.max(plan.seamAlong + 6, rightAlong - 8);
        pts.push(at(stubAlong, rightBaseCross), at(stubAlong, rightCross));
      }
      pts.push(at(plan.seamAlong, rightCross));
    }
    pts.push(at(plan.seamAlong, curLeftCross));
    if (hasPre) {
      pts.push(at(plan.preSeamAlong!, curLeftCross), at(plan.preSeamAlong!, leftCross));
    }
    pts.push(at(leftAlong, leftCross));
  } else {
    pts = [s];
    if (hasPre) {
      pts.push(at(plan.preSeamAlong!, leftBaseCross), at(plan.preSeamAlong!, curLeftCross));
    }
    pts.push(at(plan.seamAlong, curLeftCross));
    if (hasPost) {
      pts.push(at(plan.seamAlong, plan.corridorCross!), at(plan.dropAlong!, plan.corridorCross!), at(plan.dropAlong!, rightCross));
    } else {
      pts.push(at(plan.seamAlong, rightCross));
    }
    pts.push(at(rightAlong, rightCross));
  }

  const d = roundedPath(cleanOrthogonalPoints(pts));
  let lAlong = plan.labelAlong;
  let lCross = plan.labelCross;
  if (Math.abs(plan.labelAlong - plan.seamAlong) <= 0.5) {
    lAlong = plan.seamAlong;
    lCross = clampBetween(plan.labelCross, curLeftCross, postCross);
  } else if (plan.labelAlong < plan.seamAlong) {
    lAlong = clampBetween(plan.labelAlong, hasPre ? plan.preSeamAlong! : leftAlong, plan.seamAlong);
    lCross = curLeftCross;
  } else {
    lAlong = clampBetween(plan.labelAlong, plan.seamAlong, hasPost ? plan.dropAlong! : rightAlong);
    lCross = postCross;
  }
  const lPt = at(lAlong, lCross);
  return { d, lx: lPt.x, ly: lPt.y };
}

export function ExecEdge(props: EdgeProps) {
  const data = (props.data ?? {}) as ExecEdgeData;
  const lane = data.lane ?? 0;
  const dir = data.dir ?? "LR";
  const s = { x: props.sourceX, y: props.sourceY };
  const t = { x: props.targetX, y: props.targetY };
  const planned = data.plan ? plannedExecPath(s, t, dir, data.plan) : null;
  const via = planned ? null : data.seam != null ? throughSeam(s, t, data.seam, dir, lane) : null;
  const horiz = props.sourcePosition !== Position.Top && props.sourcePosition !== Position.Bottom;
  let path: string;
  let lx: number;
  let ly: number;
  if (planned) {
    path = planned.d;
    lx = planned.lx;
    ly = planned.ly;
  } else if (via) {
    path = via.d;
    lx = via.lx;
    ly = via.ly;
  } else {
    const delta = lane * LANE_SHIFT;
    const stepped = getSmoothStepPath({
      sourceX: s.x + (horiz ? 0 : delta),
      sourceY: s.y + (horiz ? delta : 0),
      targetX: t.x + (horiz ? 0 : delta),
      targetY: t.y + (horiz ? delta : 0),
      sourcePosition: props.sourcePosition ?? (horiz ? Position.Right : Position.Bottom),
      targetPosition: props.targetPosition ?? (horiz ? Position.Left : Position.Top),
      borderRadius: RADIUS,
    });
    path = stepped[0];
    lx = stepped[1];
    ly = stepped[2];
  }
  const mark = data.mark ?? "order";
  const persistOk = data.persist && mark === "ok" ? " exec-label--persist" : "";
  const liveCls = data.live ? " exec-label--live" : "";
  const labelStyle: CSSProperties = {
    transform: `translate(-50%, -50%) translate(${lx}px, ${ly}px)`,
    whiteSpace: "nowrap",
    ...(data.roleColor ? ({ "--edge-role": data.roleColor } as CSSProperties) : {}),
  };
  return (
    <>
      <BaseEdge path={path} markerEnd={props.markerEnd} style={props.style} interactionWidth={props.interactionWidth ?? 16} />
      {data.label && (
        <EdgeLabelRenderer>
          <div className={`exec-label exec-label--${mark}${persistOk}${liveCls} nopan`} style={labelStyle}>
            {data.label}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

export type ForkEdgeData = {
  label?: string;
  roleColor?: string;
  dir?: Dir;
  bow?: number;
};

export function ForkEdge(props: EdgeProps) {
  const data = (props.data ?? {}) as ForkEdgeData;
  const { sourceX, sourceY, targetX, targetY } = props;
  const isTB = data.dir === "TB";
  const sameRank = isTB ? Math.abs(sourceY - targetY) < 48 : Math.abs(sourceX - targetX) < 48;
  let path: string;
  let lx: number;
  let ly: number;

  if (sameRank) {
    const bow = (data.bow ?? 42) + Math.min(48, Math.abs(isTB ? targetX - sourceX : targetY - sourceY) * 0.14);
    const cx = isTB ? (sourceX + targetX) / 2 : Math.min(sourceX, targetX) - bow;
    const cy = isTB ? Math.min(sourceY, targetY) - bow : (sourceY + targetY) / 2;
    path = `M ${sourceX} ${sourceY} Q ${cx} ${cy} ${targetX} ${targetY}`;
    lx = 0.25 * sourceX + 0.5 * cx + 0.25 * targetX;
    ly = 0.25 * sourceY + 0.5 * cy + 0.25 * targetY;
  } else {
    const bezier = getBezierPath({
      sourceX,
      sourceY,
      targetX,
      targetY,
      sourcePosition: props.sourcePosition ?? (isTB ? Position.Bottom : Position.Right),
      targetPosition: props.targetPosition ?? (isTB ? Position.Top : Position.Left),
    });
    path = bezier[0];
    lx = bezier[1];
    ly = bezier[2];
  }

  return (
    <>
      <BaseEdge path={path} markerEnd={props.markerEnd} style={props.style} interactionWidth={props.interactionWidth ?? 14} />
      {data.label && (
        <EdgeLabelRenderer>
          <div
            className="fork-label nopan"
            style={{
              transform: `translate(-50%, -50%) translate(${lx}px, ${ly}px)`,
              ...(data.roleColor ? ({ "--fork-color": data.roleColor } as CSSProperties) : {}),
            }}
          >
            {data.label}
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}
