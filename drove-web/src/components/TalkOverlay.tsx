import { ViewportPortal, type Node } from "@xyflow/react";
import { NODE_SIZE, type Dir } from "../layout";
import type { TalkHop } from "../types";

/**
 * A replayed message thread, drawn over the canvas (in flow coordinates, via
 * ViewportPortal).
 *
 * It draws its OWN paths, not the dispatch edges: those carry an arrowhead
 * pointing coordinator → worker, so a worker's question travelling the other
 * way along them read as going the wrong direction. Each hop gets a curved path
 * with an arrowhead pointing the way the message went, bowed to the left of its
 * direction of travel — a question and its answer between the same two cards
 * therefore sit on opposite sides instead of on top of each other. Hops already
 * played stay drawn (fainter, numbered) so the exchange can be read as a whole
 * once it has finished.
 */

const BOW = 34;
const ARROW = 9;

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

function rectOf(n: Node | undefined): Rect | null {
  if (!n) return null;
  const size = NODE_SIZE[n.type as keyof typeof NODE_SIZE];
  const w = n.measured?.width ?? n.width ?? size?.w;
  const h = n.measured?.height ?? n.height ?? size?.h;
  return w && h ? { x: n.position.x, y: n.position.y, w, h } : null;
}

type Pt = { x: number; y: number };

/** Path from the side of `a` facing `b` to the side of `b` facing `a`. */
function geometry(a: Rect, b: Rect, dir: Dir, fanIndex = 0) {
  const ca = { x: a.x + a.w / 2, y: a.y + a.h / 2 };
  const cb = { x: b.x + b.w / 2, y: b.y + b.h / 2 };
  let s: Pt;
  let e: Pt;
  if (dir === "LR") {
    const right = cb.x >= ca.x;
    s = { x: right ? a.x + a.w : a.x, y: ca.y };
    e = { x: right ? b.x : b.x + b.w, y: cb.y };
  } else {
    const down = cb.y >= ca.y;
    s = { x: ca.x, y: down ? a.y + a.h : a.y };
    e = { x: cb.x, y: down ? b.y : b.y + b.h };
  }
  const dx = e.x - s.x;
  const dy = e.y - s.y;
  const len = Math.hypot(dx, dy) || 1;
  const nx = dy / len;
  const ny = -dx / len;
  const bow = BOW + fanIndex * 24;
  const c = { x: (s.x + e.x) / 2 + nx * bow, y: (s.y + e.y) / 2 + ny * bow };
  const d = `M${s.x},${s.y} Q${c.x},${c.y} ${e.x},${e.y}`;
  const tx = e.x - c.x;
  const ty = e.y - c.y;
  const tl = Math.hypot(tx, ty) || 1;
  const ux = tx / tl;
  const uy = ty / tl;
  const base = { x: e.x - ux * ARROW, y: e.y - uy * ARROW };
  const arrow = `${e.x},${e.y} ${base.x - uy * ARROW * 0.6},${base.y + ux * ARROW * 0.6} ${base.x + uy * ARROW * 0.6},${base.y - ux * ARROW * 0.6}`;
  const mid = { x: 0.25 * s.x + 0.5 * c.x + 0.25 * e.x, y: 0.25 * s.y + 0.5 * c.y + 0.25 * e.y };
  return { d, arrow, mid, end: e };
}

const reducedMotion = (() => {
  try {
    return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  } catch {
    return false;
  }
})();

export function TalkOverlay({
  hops,
  step,
  runKey,
  nodes,
  dir,
  labelOf,
}: {
  hops: TalkHop[];
  /** the hop playing now; `hops.length` = finished */
  step: number;
  /** changes on every click, so the dot restarts */
  runKey: string;
  nodes: Node[];
  dir: Dir;
  labelOf: (hop: TalkHop) => string;
}) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const pairSeen = new Map<string, number>();
  const drawn = hops
    .map((hop, i) => {
      const a = rectOf(hop.from ? byId.get(hop.from) : undefined);
      const b = rectOf(hop.to ? byId.get(hop.to) : undefined);
      if (!a || !b || hop.from === hop.to) return null;
      const pk = `${hop.from}\0${hop.to}`;
      const fanIndex = pairSeen.get(pk) ?? 0;
      pairSeen.set(pk, fanIndex + 1);
      return { hop, i, g: geometry(a, b, dir, fanIndex) };
    })
    .filter((x): x is NonNullable<typeof x> => x !== null && x.i <= step);
  if (!drawn.length) return null;
  const done = step >= hops.length;
  return (
    <ViewportPortal>
      <svg className="talk-overlay" style={{ position: "absolute", left: 0, top: 0, overflow: "visible", pointerEvents: "none" }} width="1" height="1">
        {drawn.map(({ hop, i, g }) => {
          const current = i === step;
          const kind = hop.reply ? "reply" : hop.type === "done" ? "worker_done" : hop.type;
          return (
            <g key={`${runKey}:${i}`} className={`talk talk--${kind}${current ? " talk--current" : done ? " talk--finished" : " talk--past"}`}>
              <path d={g.d} className="talk__path" fill="none" pathLength={1} />
              <polygon points={g.arrow} className="talk__arrow" />
              <g transform={`translate(${g.mid.x},${g.mid.y})`} className="talk__badge">
                <circle r={9} />
                <text textAnchor="middle" dy="0.35em">
                  {i + 1}
                </text>
              </g>
              {current && (
                <g className="talk__dot" transform={reducedMotion ? `translate(${g.end.x},${g.end.y})` : undefined}>
                  {!reducedMotion && <animateMotion dur="1.3s" fill="freeze" path={g.d} calcMode="linear" />}
                  <circle r={6} />
                  <text y={-11} textAnchor="middle">
                    {labelOf(hop)}
                  </text>
                </g>
              )}
            </g>
          );
        })}
      </svg>
    </ViewportPortal>
  );
}
