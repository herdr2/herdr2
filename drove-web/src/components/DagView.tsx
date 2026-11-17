import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  Handle,
  MarkerType,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useEdgesState,
  useNodesState,
  useReactFlow,
  useUpdateNodeInternals,
  type Edge,
  type FitViewOptions,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { branchCaption, branchPhase, branchUnselected, dataBranchSource, type BranchPhase } from "../branch";
import {
  computeDataBranchLinks,
  computeExecRoutes,
  computeForkLinks,
  computeLensSpotlight,
  computeRunProgressSummary,
  computeSmartExecLinks,
  computeVisibleReviewLinks,
  roleColorVar,
  summarizeCanvasRoles,
  workerCurrentTask,
  type DagLens,
  type LensRole,
  type RunProgressSummary,
  type SmartExecLink,
} from "../dag-lens";
import { agentModel, circNum, contextPct, fmtCost, fmtTokens, runToken, shortId, taskMicroBadges, taskToken, taskUsage, workerNumbers, workerToken, type MicroBadge } from "../format";
import { failureToken, taskFailure } from "../failure";
import { authorWorkers, isReviewer, parseReviewerNodeId, reviewBoard, reviewerNodeId, reviewerNumbers, type ReviewStanding } from "../review";
import { resolveRoleMeta, roleDisplayName, type RoleVisualKind } from "../roles";
import { Estimate } from "./Estimate";
import { ReviewRequirement } from "./ReviewRequirement";
import { useT } from "../i18n";
import { depRouteKey, dispatchRouteKey, layoutBoard, NODE_SIZE, ROLE_RANK, type Dir } from "../layout";
import {
  COORD_ID,
  MARK_LABEL,
  coordinatorHostLabel,
  allMessages,
  attemptMark,
  messageWorkerId,
  openEscalations,
  openQuestions,
  reviewerTasksInOrder,
  taskAttempts,
  tasksInOrder,
  workerMessages,
  workerNodeId,
  type AttemptMark,
} from "../relate";
import type { LayoutKind, Message, MessageType, Run, RunSnapshot, TalkHop, TalkPlay, Task, TaskStatus, Worker } from "../types";
import { DagRoleLensBar, DagZoneBackdrop, type WorkerCategoryGroup, type ZoneInput } from "./DagZoneBackdrop";
import { ExecEdge, ForkEdge, RoutedEdge, type ExecEdgeData, type ForkEdgeData, type RoutedEdgeData } from "./RoutedEdge";
import { TalkOverlay } from "./TalkOverlay";

export { COORD_ID, workerNodeId };

const HOP_MS = 1800;
const HOP_TYPE_KEY: Record<MessageType, string> = {
  dispatch: "hop.dispatch",
  question: "hop.question",
  escalation: "hop.escalation",
  failed: "hop.failed",
  done: "hop.done",
  budget: "hop.budget",
  run_done: "hop.run_done",
  note: "hop.note",
  reply: "hop.reply",
  system: "hop.system",
};

/** Flow coordinates captured when a node is selected, restored by Esc / browser back. */
export type CanvasViewport = { x: number; y: number; zoom: number };

const FIT = (duration: number): FitViewOptions => ({
  padding: 0.18,
  maxZoom: 1,
  duration,
});

type RoleKey = "role.coordinator" | "role.worker" | "role.task";
type CrossMark = AttemptMark | "order";

interface ExecLink {
  key: string;
  routeKey: string;
  workerId: string;
  from: string;
  to: string;
  label: string;
  mark: CrossMark;
  /** Always-on edge colored by attempt outcome. Success labels use green. */
  persist?: boolean;
  live?: boolean;
  roleColor?: string;
  emphasis?: "" | " edge--focus" | " edge--dim";
}

interface MarkChip {
  key: string;
  kind: AttemptMark;
  label: string;
}

const EMPTY_LENS: DagLens = { role: null, phase: null };

function compactElapsed(start: string | null | undefined, end?: string | null, now = Date.now()): string | null {
  if (!start) return null;
  const a = Date.parse(start);
  const b = end ? Date.parse(end) : now;
  if (!Number.isFinite(a) || !Number.isFinite(b)) return null;
  const s = Math.max(0, Math.round((b - a) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

function useLiveElapsed(start: string | null | undefined, live: boolean, end?: string | null): string | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!live || !start) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [live, start]);
  return compactElapsed(start, end, now);
}

function RoleBadge({ role, kind }: { role: RoleKey; kind: "coord" | "worker" | "task" }) {
  const { t } = useT();
  return <span className={`role-badge role-badge--${kind}`}>{t(role)}</span>;
}

function tone(token: string): CSSProperties {
  return { "--st": `var(--st-${token})`, "--st-bg": `var(--st-${token}-bg)` } as CSSProperties;
}

function ContextBar({ used, size, compact = false }: { used: number; size: number; compact?: boolean }) {
  const pct = contextPct(used, size);
  if (pct == null) return compact ? <span className="gnode__dim">ctx —</span> : <span className="gnode__dim">上下文 —</span>;
  const hot = pct >= 90 ? " ctx__fill--full" : pct >= 70 ? " ctx__fill--hot" : "";
  return (
    <div className={`ctx${compact ? " ctx--compact" : ""}`} title={`上下文 ${used} / ${size}`}>
      <span className="ctx__bar">
        <span className={`ctx__fill${hot}`} style={{ width: `${pct}%` }} />
      </span>
      <span className="ctx__pct">{pct}%</span>
    </div>
  );
}

type TaskNodeData = {
  label: string;
  statusKey: TaskStatus;
  subphaseText: string | null;
  awaitingLateDone: boolean;
  token: string;
  line: string;
  targetRole: { kind: RoleVisualKind; label: string; colorVar: string } | null;
  /** Worker numbers that attempted this task, one chip per worker (first-attempt order, latest attempt's mark). */
  wnums: { key: string; num: number; count: number; mark: AttemptMark; line: string; roleColor: string }[];
  coordTakeover: boolean;
  tokens: string;
  cost: string;
  tokensEstimated: boolean;
  costEstimated: boolean;
  startedAt: string | null;
  endedAt: string | null;
  failures: number;
  failLabel: string | null;
  failTitle: string | null;
  selected: boolean;
  dir: Dir;
  hasBranch: boolean;
  branchText: string | null;
  branchPhase: BranchPhase | null;
  unselected: boolean;
  badges: MicroBadge[];
  exec: boolean;
  hasReviewEdge: boolean;
  reviewRequired: boolean;
  review: { text: string; standing: ReviewStanding } | null;
  rel: "up" | "down" | "ran" | "dim" | null;
  lensDim: boolean;
  order: number | null;
};

const REL_LABEL = { up: "task.relUp", down: "task.relDown", ran: "task.relRan" } as const;

function TaskNode({ id, data }: NodeProps<Node<TaskNodeData>>) {
  const { t } = useT();
  const isTB = data.dir === "TB";
  const inputs = [data.exec && "exec", data.hasReviewEdge && "review", data.hasBranch && "branch", "dep"].filter(Boolean) as string[];
  const at = (k: string) => {
    const i = inputs.indexOf(k);
    return { [isTB ? "left" : "top"]: `${i < 0 ? 50 : ((i + 1) * 100) / (inputs.length + 1)}%` };
  };
  const update = useUpdateNodeInternals();
  const handleKey = `${data.dir}:${inputs.join(",")}`;
  useEffect(() => update(id), [handleKey, id, update]);
  const live = data.statusKey === "running";
  const elapsed = useLiveElapsed(data.startedAt, live, data.endedAt);
  const statusText = data.awaitingLateDone
    ? t("task.awaiting")
    : data.failLabel
      ? data.failLabel
      : data.statusKey === "skipped"
        ? t("status.skipped")
        : data.unselected
          ? t("status.unselected")
          : live && data.subphaseText
            ? data.subphaseText
            : t(`status.${data.statusKey}`);
  return (
    <div
      className={`gnode gnode--task${data.selected ? " gnode--selected" : ""}${data.rel ? ` gnode--rel-${data.rel}` : ""}${data.lensDim ? " gnode--lens-dim" : ""}${data.unselected ? " gnode--branch-miss" : ""}${data.branchPhase === "hit" ? " gnode--branch-hit" : ""}`}
      data-status={data.token}
      data-branch={data.branchPhase ?? undefined}
      style={tone(data.token)}
    >
      {data.exec && (
        <Handle id="exec-in" type="target" position={isTB ? Position.Top : Position.Left} className="gnode__handle--exec" style={at("exec")} />
      )}
      {data.hasReviewEdge && (
        <Handle id="review-out" type="source" position={isTB ? Position.Top : Position.Left} className="gnode__handle--review" style={at("review")} />
      )}
      {data.hasBranch && (
        <Handle id="branch-in" type="target" position={isTB ? Position.Top : Position.Left} className="gnode__handle--branch" style={at("branch")} />
      )}
      <Handle id="dep-in" type="target" position={isTB ? Position.Top : Position.Left} style={at("dep")} />
      <div className="gnode__head">
        <RoleBadge role="role.task" kind="task" />
        {data.targetRole && (
          <span
            className="task-role-pill"
            data-role={data.targetRole.kind}
            style={{ "--role-accent": data.targetRole.colorVar } as CSSProperties}
            title={data.targetRole.label}
          >
            {data.targetRole.label}
          </span>
        )}
        {data.order != null && <span className="order-badge" title={t("task.relRan")}>{circNum(data.order)}</span>}
        {data.rel && data.rel !== "dim" && <span className={`rel-tag rel-tag--${data.rel}`}>{t(REL_LABEL[data.rel])}</span>}
        <span className="gnode__status" title={data.failTitle ?? undefined}>
          <span className={`sdot${live ? " sdot--pulse" : ""}`} />
          {statusText}
        </span>
      </div>
      <div className="gnode__chips">
        <ReviewRequirement required={data.reviewRequired} />
        {data.badges.map((badge) => (
          <span key={badge.kind} className={`micro-badge micro-badge--${badge.kind}`} title={badge.title}>{badge.text}</span>
        ))}
        {data.branchText && <div className={`branch-tag branch-tag--${data.branchPhase ?? "open"}`} title={data.branchText}>{data.branchText}</div>}
      </div>
      <div className="gnode__title" title={data.label}>{data.label}</div>
      <div className="gnode__meta" title={data.line}>
        {data.wnums.map((wn, i) => (
          <span
            key={wn.key}
            className={`task-wnum task-wnum--colored${i === data.wnums.length - 1 ? "" : " task-wnum--dim"}${wn.mark === "fail" ? " task-wnum--fail" : ""}`}
            style={{ "--wnum-color": wn.roleColor } as CSSProperties}
            title={`W${wn.num} · ${wn.line}${wn.count > 1 ? ` · ×${wn.count}` : ""} · ${t(MARK_LABEL[wn.mark])}`}
          >{wn.count > 1 ? `W${wn.num}×${wn.count}` : `W${wn.num}`}</span>
        ))}
        {data.coordTakeover && (
          <span className="task-wnum task-wnum--coord" title={t("task.coordTakeover")}>{t("task.coordShort")}</span>
        )}
        <span className="mono gnode__model">{data.line}</span>
        {data.review && <span className={`gnode__review-pill gnode__review-pill--${data.review.standing}`} style={{ flexShrink: 0 }}>{data.review.text}</span>}
      </div>
      <div className="gnode__meta gnode__meta--dim">
        <span><Estimate text={`${data.tokens} tok`} estimated={data.tokensEstimated} /></span>
        {elapsed && <span className="gnode__elapsed" title={t("task.elapsed")}>{elapsed}</span>}
        <span className="gnode__sep" />
        <span><Estimate text={data.cost} estimated={data.costEstimated} /></span>
        <span className={data.failures > 0 ? "gnode__flag" : "gnode__nowrap"}>{t("task.failures", { n: data.failures })}</span>
      </div>
      <Handle id="dep-out" type="source" position={isTB ? Position.Bottom : Position.Right} />
    </div>
  );
}

type WorkerNodeData = {
  worker: Worker;
  line: string;
  num: number;
  roleKind: LensRole;
  taskCount: number;
  messageCount: number;
  openQuestions: number;
  marks: MarkChip[];
  currentTask: { id: string; title: string; startedAt: string } | null;
  selected: boolean;
  dim: boolean;
  lensDim: boolean;
  hot: boolean;
  exec: boolean;
  dir: Dir;
  forkLabel: string | null;
};

function WorkerNode({ id, data }: NodeProps<Node<WorkerNodeData>>) {
  const { t } = useT();
  const w = data.worker;
  const token = workerToken(w.state);
  const isTB = data.dir === "TB";
  const live = w.state === "busy" || w.state === "starting";
  const roleMeta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
  const roleLabel = roleDisplayName(roleMeta, t);
  const roleColor = roleColorVar(data.roleKind);
  const elapsed = useLiveElapsed(data.currentTask?.startedAt, live);
  const update = useUpdateNodeInternals();
  useEffect(() => update(id), [id, update, data.dir, data.exec]);
  return (
    <div
      className={`gnode gnode--worker${data.selected ? " gnode--selected" : ""}${data.hot ? " gnode--rel-ran" : ""}${data.dim ? " gnode--rel-dim" : ""}${data.lensDim ? " gnode--lens-dim" : ""}`}
      data-status={token}
      data-role={data.roleKind}
      style={{ ...tone(token), "--worker-role": roleColor } as CSSProperties}
    >
      <Handle id="dispatch-in" type="target" position={isTB ? Position.Top : Position.Left} />
      <Handle id="fork-in" type="target" position={isTB ? Position.Top : Position.Left} style={{ [isTB ? "left" : "top"]: "26%", opacity: 0, pointerEvents: "none" }} />
      <div className="gnode__head">
        <span className={`role-badge ${roleMeta.badgeClass}`} title={roleMeta.id}>{roleLabel}</span>
        <span className="worker-num worker-num--role" title={`${roleLabel} ${data.num}`}>{`W${data.num}`}</span>
        {roleMeta.access === "readonly" && <span className="gnode__readonly micro-badge">{t("role.access.readonly")}</span>}
        {data.forkLabel && <span className="micro-badge micro-badge--fork" title={t("worker.forkedFrom")}>{t("worker.forkChip", { label: data.forkLabel })}</span>}
        {data.openQuestions > 0 ? (
          <span className="msg-chip msg-chip--ask" title={t("msg.openQ", { n: data.openQuestions })}>
            ? {data.openQuestions}
          </span>
        ) : data.messageCount > 0 ? (
          <span className="msg-chip" title={t("msg.count", { n: data.messageCount })}>
            ✉ {data.messageCount}
          </span>
        ) : null}
        <span className="gnode__status gnode__status--fit" title={w.state}>
          <span className={`sdot${live ? " sdot--pulse" : ""}`} />
          <span className="gnode__status-text">{t(`worker.${w.state}`)}</span>
        </span>
      </div>
      {live && data.currentTask ? (
        <>
          <div className="gnode__title gnode__title--sm gnode__active-task" title={`${data.currentTask.title} · ${data.line}`}>
            ▶ {data.currentTask.title}
          </div>
          <div className="gnode__meta">
            <span className="mono gnode__model" title={data.line}>{data.line}</span>
            {elapsed && <span className="gnode__elapsed">{elapsed}</span>}
          </div>
          <div className="gnode__meta">
            <ContextBar used={w.context.used} size={w.context.size} compact />
            <span className="gnode__sep" />
            <span className="gnode__nowrap">{t("worker.taskCount", { n: data.taskCount })}</span>
            {data.marks.map((m) => (
              <span key={m.key} className={`attempt-mark attempt-mark--${m.kind}`} title={m.label}>{m.label}</span>
            ))}
          </div>
        </>
      ) : (
        <>
          <div className="gnode__title gnode__title--sm" title={data.line}>{data.line}</div>
          <div className="gnode__meta">
            <ContextBar used={w.context.used} size={w.context.size} />
          </div>
          <div className="gnode__meta">
            <span className="gnode__nowrap">{t("worker.taskCount", { n: data.taskCount })}</span>
            {data.marks.map((m) => (
              <span key={m.key} className={`attempt-mark attempt-mark--${m.kind}`} title={m.label}>{m.label}</span>
            ))}
          </div>
        </>
      )}
      {data.exec && <Handle id="exec-out" type="source" position={isTB ? Position.Bottom : Position.Right} className="gnode__handle--exec" />}
      <Handle id="fork-out" type="source" position={isTB ? Position.Top : Position.Left} style={{ [isTB ? "left" : "top"]: "74%", opacity: 0, pointerEvents: "none" }} />
    </div>
  );
}

type ReviewerNodeData = {
  worker: Worker;
  num: number;
  taskCount: number;
  marks: MarkChip[];
  selected: boolean;
  hot: boolean;
  dim: boolean;
  lensDim: boolean;
  hasReviewEdge: boolean;
  dir: Dir;
};

function ReviewerNode({ id, data }: NodeProps<Node<ReviewerNodeData>>) {
  const { t } = useT();
  const w = data.worker;
  const live = w.state === "busy" || w.state === "starting";
  const token = workerToken(w.state);
  const update = useUpdateNodeInternals();
  useEffect(() => update(id), [id, update, data.dir, data.hasReviewEdge]);
  return (
    <div className={`gnode gnode--reviewer${data.selected ? " gnode--selected" : ""}${data.hot ? " gnode--rel-ran" : ""}${data.dim ? " gnode--rel-dim" : ""}${data.lensDim ? " gnode--lens-dim" : ""}`} data-status={token} style={{ ...tone(token), width: NODE_SIZE.reviewer.w, height: NODE_SIZE.reviewer.h, boxSizing: "border-box" }}>
      <Handle id="dispatch-in" type="target" position={data.dir === "TB" ? Position.Top : Position.Left} />
      <Handle id="review-in" type="target" position={data.dir === "TB" ? Position.Bottom : Position.Right} className="gnode__handle--review" style={data.hasReviewEdge ? undefined : { opacity: 0, pointerEvents: "none" }} />
      <div className="gnode__head">
        <span className="worker-num">R{data.num}</span>
        <span className="role-badge role-badge--reviewer" title="Reviewer">{t("role.reviewer")} / Reviewer</span>
        <span className="gnode__readonly micro-badge">只读</span>
        <span className="gnode__status" title={t(`worker.${w.state}`)}><span className={`sdot${live ? " sdot--pulse" : ""}`} /></span>
      </div>
      <div className="gnode__title gnode__title--sm" title={agentModel(w.agent, w.model, w.agent)}>{agentModel(w.agent, w.model, w.agent)}</div>
      <div className="gnode__meta"><ContextBar used={w.context.used} size={w.context.size} /></div>
      <div className="gnode__meta gnode__meta--dim">
        <span>{data.taskCount} 个审查任务</span>
        <span className="gnode__marks" style={{ display: "inline-flex", gap: 4 }}>{data.marks.map((mark) => <span key={mark.key} className={`sdot mark-dot mark-dot--${mark.kind}`} style={{ color: mark.kind === "ok" ? "var(--st-done)" : mark.kind === "fail" ? "var(--st-failed)" : "var(--st-review)" }} title={mark.label} aria-label={mark.label} />)}</span>
      </div>
    </div>
  );
}

type CoordNodeData = {
  run: Run;
  token: string;
  busy: number;
  workers: number;
  pending: number;
  openQuestions: number;
  messageCount: number;
  progress: RunProgressSummary;
  selected: boolean;
  hot: boolean;
  dim: boolean;
  lensDim: boolean;
  exec: boolean;
  dir: Dir;
};

function CoordinatorNode({ id, data }: NodeProps<Node<CoordNodeData>>) {
  const { t } = useT();
  const isTB = data.dir === "TB";
  const driving = data.busy > 0 && data.run.status === "active";
  const update = useUpdateNodeInternals();
  useEffect(() => update(id), [id, update, data.dir, data.exec]);
  const p = data.progress;
  const total = Math.max(1, p.total);
  const donePct = (p.done / total) * 100;
  const runPct = (p.running / total) * 100;
  const failPct = (p.failed / total) * 100;
  return (
    <div className={`gnode gnode--coord${data.selected ? " gnode--selected" : ""}${data.hot ? " gnode--rel-ran" : ""}${data.dim ? " gnode--rel-dim" : ""}${data.lensDim ? " gnode--lens-dim" : ""}`} data-status={data.token} style={tone(data.token)}>
      <div className="gnode__head">
        <RoleBadge role="role.coordinator" kind="coord" />
        {data.openQuestions > 0 ? (
          <span className="msg-chip msg-chip--ask" title={t("msg.openQ", { n: data.openQuestions })}>
            ? {data.openQuestions}
          </span>
        ) : data.pending > 0 ? (
          <span className="msg-chip msg-chip--ask">{t("coord.pending", { n: data.pending })}</span>
        ) : data.messageCount > 0 ? (
          <span className="msg-chip" title={t("msg.totalCount", { n: data.messageCount })}>
            ✉ {data.messageCount}
          </span>
        ) : null}
        <span className="gnode__status">
          <span className={`sdot${driving ? " sdot--pulse" : ""}`} />
          {t(`run.${data.run.status}`)}
        </span>
      </div>
      <div className="gnode__title gnode__title--sm" title={`${coordinatorHostLabel(data.run)} · ${data.run.coordinator.hostSession}`}>
        {`${coordinatorHostLabel(data.run)} · ${shortId(data.run.coordinator.hostSession, 10)}`}
      </div>
      <div className="coord-progress" title={`${p.done}/${p.total} (${p.pct}%)`}>
        <div className="coord-progress__track">
          {p.done > 0 && <span className="coord-progress__seg coord-progress__seg--done" style={{ width: `${donePct}%` }} />}
          {p.running > 0 && <span className="coord-progress__seg coord-progress__seg--run" style={{ width: `${runPct}%` }} />}
          {p.failed > 0 && <span className="coord-progress__seg coord-progress__seg--fail" style={{ width: `${failPct}%` }} />}
        </div>
        <span className="coord-progress__pct">{`${p.done}/${p.total} (${p.pct}%)`}</span>
      </div>
      <div className="gnode__meta gnode__meta--dim">
        <span>{data.workers} {t("tb.workers")}{data.busy ? ` · ${data.busy} ${t("worker.busy")}` : ""}</span>
        {p.pendingActions > 0 && (
          <span className="gnode__flag">{t("coord.actions", { n: p.pendingActions })}</span>
        )}
      </div>
      <Handle type="source" position={isTB ? Position.Bottom : Position.Right} />
      <Handle
        id="exec-out"
        type="source"
        position={isTB ? Position.Bottom : Position.Right}
        className="gnode__handle--exec"
        style={{ [isTB ? "left" : "top"]: "68%", ...(data.exec ? {} : { opacity: 0, pointerEvents: "none" }) }}
      />
      <Handle id="fork-out" type="source" position={isTB ? Position.Bottom : Position.Right} style={{ [isTB ? "left" : "top"]: "72%", opacity: 0, pointerEvents: "none" }} />
    </div>
  );
}

const nodeTypes = { task: TaskNode, worker: WorkerNode, reviewer: ReviewerNode, coordinator: CoordinatorNode };
const edgeTypes = { routed: RoutedEdge, exec: ExecEdge, fork: ForkEdge };

function edgeColors(): Record<"depends" | "satisfied" | "dispatch" | "branch", string> {
  const cs = typeof window !== "undefined" ? getComputedStyle(document.documentElement) : null;
  const v = (name: string, fallback: string) => cs?.getPropertyValue(name).trim() || fallback;
  return {
    depends: v("--edge-depends", "#7a8494"),
    satisfied: v("--edge-satisfied", "#1f8a4c"),
    dispatch: v("--edge-dispatch", "#0f766e"),
    branch: v("--edge-branch", "#b45309"),
  };
}

const MARK_VAR: Record<CrossMark, string> = {
  ok: "--edge-executes",
  order: "--edge-executes",
  fail: "--st-failed",
  retry: "--st-blocked",
  run: "--st-running",
};

function markColor(mark: CrossMark): string {
  const fallback: Record<CrossMark, string> = {
    ok: "#4f46e5",
    order: "#4f46e5",
    fail: "#c93a32",
    retry: "#7a52c7",
    run: "#c98a0c",
  };
  if (typeof window === "undefined") return fallback[mark];
  return getComputedStyle(document.documentElement).getPropertyValue(MARK_VAR[mark]).trim() || fallback[mark];
}

/** Always-on success edges are green. Other marks keep the selection palette. */
function persistColor(mark: CrossMark): string {
  if (mark !== "ok") return markColor(mark);
  if (typeof window === "undefined") return "#1f8a4c";
  return getComputedStyle(document.documentElement).getPropertyValue("--st-done").trim() || "#1f8a4c";
}

function depsOf(task: Task, byId: Map<string, Task>): { id: string; source: string; target: string }[] {
  const out: { id: string; source: string; target: string }[] = [];
  for (const dep of task.deps) {
    const source = byId.has(dep) ? dep : [...byId.values()].find((t) => t.key === dep)?.id;
    if (source && byId.has(source)) out.push({ id: `${source}__${task.id}`, source, target: task.id });
  }
  return out;
}

function mergePositions(raw: Node[], cur: Node[], dragged: Map<string, { x: number; y: number }>, draggingId: string | null, dirChanged: boolean): Node[] {
  const currentById = new Map(cur.map((n) => [n.id, n]));
  return raw.map((n) => {
    const current = currentById.get(n.id);
    const keep = dragged.get(n.id) ?? (draggingId === n.id ? current?.position : undefined);
    return { ...n, position: keep ?? n.position, measured: dirChanged ? undefined : current?.measured };
  });
}

function lanesOf(links: ExecLink[]): Map<string, number> {
  const groups = new Map<string, string[]>();
  for (const link of links) {
    const k = link.persist ? "persist" : "sel";
    const list = groups.get(k) ?? [];
    list.push(link.key);
    groups.set(k, list);
  }
  const lanes = new Map<string, number>();
  for (const keys of groups.values()) {
    keys.forEach((key, i) => lanes.set(key, i - (keys.length - 1) / 2));
  }
  return lanes;
}

/**
 * Move `id` to the middle of the React Flow pane. The pane is the narrowed
 * canvas (the detail panel is a sibling column, not an overlay), so the
 * pane's center is the visible center.
 */
function centerNode(rf: ReturnType<typeof useReactFlow>, id: string): boolean {
  const node = rf.getNode(id);
  const w = node?.measured?.width;
  const h = node?.measured?.height;
  if (!node || !w || !h) return false;
  const zoom = Math.min(1, Math.max(rf.getZoom(), 0.85));
  void rf.setCenter(node.position.x + w / 2, node.position.y + h / 2, { zoom, duration: 300 });
  return true;
}

function frameNodes(rf: ReturnType<typeof useReactFlow>, ids: string[]): boolean {
  const unique = [...new Set(ids)];
  for (const id of unique) {
    const node = rf.getNode(id);
    if (!node?.measured?.width || !node.measured.height) return false;
  }
  if (unique.length === 0) return false;
  if (unique.length === 1) return centerNode(rf, unique[0]);
  void rf.fitView({ nodes: unique.map((id) => ({ id })), duration: 300, padding: 0.22, maxZoom: 1 });
  return true;
}

function Board({
  snap,
  selectedId,
  onSelect,
  layout,
  reorgNonce,
  showWorkers,
  showReviewers,
  showExec,
  showReviewEdges,
  lens,
  onLensChange,
  camera,
  bindViewport,
  restoreViewport,
  talk,
  onStopTalk,
}: {
  snap: RunSnapshot;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  layout: LayoutKind;
  reorgNonce: number;
  showWorkers: boolean;
  showReviewers: boolean;
  showExec: boolean;
  showReviewEdges: boolean;
  lens?: DagLens;
  onLensChange?: (next: DagLens) => void;
  camera: "frame" | "center" | "restore";
  bindViewport?: (get: () => CanvasViewport) => void;
  restoreViewport?: { vp: CanvasViewport; token: number } | null;
  talk?: TalkPlay | null;
  onStopTalk?: () => void;
}) {
  const rf = useReactFlow();
  const { t } = useT();
  const [fallbackLens, setFallbackLens] = useState<DagLens>(EMPTY_LENS);
  const activeLens = lens ?? fallbackLens;
  const updateLens = useCallback(
    (next: DagLens) => {
      if (onLensChange) onLensChange(next);
      else setFallbackLens(next);
    },
    [onLensChange],
  );
  const prevDir = useRef<Dir | null>(null);
  const dragged = useRef<Map<string, { x: number; y: number }>>(new Map());
  const draggingId = useRef<string | null>(null);
  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const byId = useMemo(() => new Map(snap.tasks.map((task) => [task.id, task])), [snap.tasks]);
  const progress = useMemo(() => computeRunProgressSummary(snap), [snap]);
  const roleChips = useMemo(() => summarizeCanvasRoles(snap), [snap]);
  const spotlight = useMemo(() => computeLensSpotlight(snap, activeLens), [snap, activeLens]);

  const boardModel = useMemo(() => {
    const dir: Dir = layout === "layered-tb" ? "TB" : "LR";
    const depEdges = snap.tasks.flatMap((task) => depsOf(task, byId));
    const authors = authorWorkers(snap.workers);
    const authorIdSet = new Set(authors.map((w) => w.id));
    const authorAttempts = snap.attempts.filter((a) => a.kind !== "review");
    const reviewerWorkers = showReviewers ? snap.workers.filter(isReviewer) : [];
    const showCoord = showWorkers || reviewerWorkers.length > 0;
    const laid = layoutBoard(layout, {
      taskIds: snap.tasks.map((task) => task.id),
      deps: depEdges,
      workers: showWorkers
        ? authors.map((w) => {
            const roleMeta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
            const rawFork = w.forkedFrom?.trim();
            const forkedFrom = rawFork
              ? rawFork === "coordinator" || rawFork === COORD_ID
                ? COORD_ID
                : authorIdSet.has(rawFork)
                  ? workerNodeId(rawFork)
                  : undefined
              : undefined;
            return {
              id: workerNodeId(w.id),
              taskIds: tasksInOrder(w, authorAttempts).filter((id) => byId.has(id)),
              roleRank: ROLE_RANK[roleMeta.visualKind] ?? ROLE_RANK.custom,
              roleGroup: roleMeta.id,
              forkedFrom,
            };
          })
        : [],
      reviewers: reviewerWorkers.map((w) => ({ id: reviewerNodeId(w.id), taskIds: reviewerTasksInOrder(w, snap.attempts, snap.tasks) })),
      coordId: showCoord ? COORD_ID : null,
    });
    const workerGroupsMap = new Map<string, WorkerCategoryGroup>();
    if (showWorkers) {
      for (const w of authors) {
        const roleMeta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
        let group = workerGroupsMap.get(roleMeta.id);
        if (!group) {
          group = {
            key: roleMeta.id,
            builtIn: roleMeta.builtIn,
            roleKind: roleMeta.visualKind,
            labelKey: roleMeta.labelKey,
            fallbackLabel: roleMeta.fallbackLabel,
            colorVar: roleColorVar(roleMeta.visualKind),
            ids: [],
            busyCount: 0,
          };
          workerGroupsMap.set(roleMeta.id, group);
        }
        group.ids.push(workerNodeId(w.id));
        if (w.state === "busy" || w.state === "starting") group.busyCount += 1;
      }
    }
    const workerGroups = [...workerGroupsMap.values()].sort(
      (a, b) =>
        (ROLE_RANK[a.roleKind] ?? ROLE_RANK.custom) - (ROLE_RANK[b.roleKind] ?? ROLE_RANK.custom) ||
        (a.key < b.key ? -1 : a.key > b.key ? 1 : 0),
    );
    const zones: ZoneInput[] = [
      { kind: "orch", ids: showCoord ? [COORD_ID, ...(showWorkers ? authors.map((w) => workerNodeId(w.id)) : [])] : [] },
      { kind: "tasks", ids: snap.tasks.map((task) => task.id) },
      { kind: "review", ids: reviewerWorkers.map((w) => reviewerNodeId(w.id)) },
    ];
    return {
      dir,
      laid,
      zones,
      workerGroups,
      depEdges,
      authors,
      reviewerWorkers,
      showCoord,
      authorAttempts,
    };
  }, [snap, layout, showWorkers, showReviewers, byId]);

  const zoneSizeOf = useCallback((id: string) => {
    if (id === COORD_ID) return NODE_SIZE.coordinator;
    if (id.startsWith("reviewer:")) return NODE_SIZE.reviewer;
    if (id.startsWith("worker:")) return NODE_SIZE.worker;
    return NODE_SIZE.task;
  }, []);

  const focusIds = useMemo(() => {
    if (!selectedId) return [] as string[];
    if (camera === "center") return [selectedId];
    if (selectedId === COORD_ID) {
      const busyWs = authorWorkers(snap.workers)
        .filter((w) => w.state === "busy" || w.state === "starting")
        .map((w) => workerNodeId(w.id));
      const busyRevs = showReviewers
        ? snap.workers
            .filter((w) => isReviewer(w) && (w.state === "busy" || w.state === "starting"))
            .map((w) => reviewerNodeId(w.id))
        : [];
      const directTasks = snap.attempts
        .filter((a) => a.kind !== "review" && (a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec") && byId.has(a.taskId))
        .map((a) => a.taskId);
      return [COORD_ID, ...busyWs, ...busyRevs, ...directTasks];
    }
    const reviewerId = parseReviewerNodeId(selectedId);
    if (reviewerId) {
      const reviewer = snap.workers.find((w) => w.id === reviewerId && isReviewer(w));
      const tasks = reviewer ? reviewerTasksInOrder(reviewer, snap.attempts, snap.tasks).filter((id) => byId.has(id)) : [];
      return [...(showReviewers ? [selectedId] : []), ...tasks];
    }
    if (selectedId.startsWith("worker:")) {
      const worker = snap.workers.find((w) => workerNodeId(w.id) === selectedId);
      const tasks = worker ? tasksInOrder(worker, snap.attempts.filter((a) => a.kind !== "review")).filter((id) => byId.has(id)) : [];
      if (worker && isReviewer(worker)) return [...(showReviewers ? [reviewerNodeId(worker.id)] : []), ...reviewerTasksInOrder(worker, snap.attempts, snap.tasks).filter((id) => byId.has(id))];
      return [selectedId, ...tasks];
    }
    if (byId.has(selectedId)) {
      const depEdges = snap.tasks.flatMap((task) => depsOf(task, byId));
      const ups = depEdges.filter((e) => e.target === selectedId).map((e) => e.source);
      const downs = depEdges.filter((e) => e.source === selectedId).map((e) => e.target);
      const authors = authorWorkers(snap.workers);
      const workers = showWorkers
        ? taskAttempts(selectedId, snap.attempts)
            .filter((a) => a.kind !== "review")
            .map((a) =>
              a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec"
                ? COORD_ID
                : authors.some((w) => w.id === a.workerId)
                  ? workerNodeId(a.workerId)
                  : null,
            )
            .filter((id): id is string => id != null)
        : [];
      const reviewers = showReviewers ? snap.workers.filter((w) => isReviewer(w) && reviewerTasksInOrder(w, snap.attempts, snap.tasks).includes(selectedId)).map((w) => reviewerNodeId(w.id)) : [];
      const task = byId.get(selectedId);
      const whenSource = task ? dataBranchSource(task, snap.tasks) : undefined;
      return [selectedId, ...workers, ...reviewers, ...ups, ...downs, ...(whenSource ? [whenSource.id] : [])];
    }
    return [selectedId];
  }, [snap, selectedId, camera, showWorkers, showReviewers, byId]);
  const focusKey = focusIds.join("|");
  const focusIdsRef = useRef(focusIds);
  focusIdsRef.current = focusIds;

  useLayoutEffect(() => {
    bindViewport?.(() => rf.getViewport());
  }, [rf, bindViewport]);

  useEffect(() => {
    dragged.current.clear();
  }, [layout, reorgNonce, snap.run.id]);

  useEffect(() => {
    if (nodes.length === 0 || selectedId || camera === "restore") return;
    const timer = window.setTimeout(() => void rf.fitView(FIT(220)), 80);
    return () => window.clearTimeout(timer);
  }, [rf, layout, reorgNonce, snap.run.id, nodes.length, showWorkers, showReviewers, selectedId, camera]);

  // Put the viewport back after the detail column has finished resizing the pane.
  useEffect(() => {
    if (camera !== "restore" || !restoreViewport) return;
    const vp = restoreViewport.vp;
    const board = document.querySelector<HTMLElement>(".dag-board");
    let dead = false;
    let tries = 0;
    let lastW = -1;
    let stable = 0;
    const tick = () => {
      if (dead) return;
      const w = board?.clientWidth ?? 0;
      if (w > 40 && w === lastW) stable += 1;
      else stable = 0;
      lastW = w;
      tries += 1;
      if (stable >= 1 && tries >= 2) {
        void rf.setViewport(vp, { duration: 280 });
        return;
      }
      if (tries < 30) window.setTimeout(tick, 40);
    };
    const timer = window.setTimeout(tick, 40);
    return () => {
      dead = true;
      window.clearTimeout(timer);
    };
  }, [rf, camera, restoreViewport]);

  // Wait until the docked panel has finished narrowing the canvas, then
  // center (a detail jump) or frame the highlighted nodes (a canvas click).
  useEffect(() => {
    if (!selectedId || nodes.length === 0 || camera === "restore") return;
    const board = document.querySelector<HTMLElement>(".dag-board");
    let dead = false;
    let tries = 0;
    let lastW = -1;
    let stable = 0;
    const tick = () => {
      if (dead) return;
      const w = board?.clientWidth ?? 0;
      if (w > 40 && w === lastW) stable += 1;
      else stable = 0;
      lastW = w;
      tries += 1;
      const settled = stable >= 1 && tries >= 2;
      const ids = focusIdsRef.current;
      const ok = settled && (camera === "center" ? centerNode(rf, selectedId) : frameNodes(rf, ids));
      if (!ok && tries < 30) window.setTimeout(tick, 40);
    };
    const timer = window.setTimeout(tick, 40);
    return () => {
      dead = true;
      window.clearTimeout(timer);
    };
  }, [rf, selectedId, camera, focusKey, layout, reorgNonce, nodes.length]);

  useEffect(() => {
    const { dir, laid, depEdges, authors, reviewerWorkers, showCoord, authorAttempts } = boardModel;
    const dirChanged = prevDir.current !== null && prevDir.current !== dir;
    prevDir.current = dir;
    const reviewNums = reviewerNumbers(snap.workers);
    const at = (id: string) => laid.pos.get(id) ?? { x: 0, y: 0 };
    const color = edgeColors();
    const routed = (key: string): RoutedEdgeData => ({ route: laid.routes.get(key), dir });
    const isDimmedByLens = (id: string) => spotlight != null && !spotlight.nodes.has(id);
    const edgeLensClass = (id: string) => (spotlight != null && !spotlight.edges.has(id) ? " edge--lens-dim" : "");

    const focus = selectedId != null && byId.has(selectedId) ? selectedId : null;
    const focusCoord = showCoord && selectedId === COORD_ID;
    const selectedWorker = snap.workers.find((w) => workerNodeId(w.id) === selectedId) ?? null;
    const focusReviewer = snap.workers.find((w) => isReviewer(w) && reviewerNodeId(w.id) === selectedId) ?? (selectedWorker && isReviewer(selectedWorker) ? selectedWorker : null);
    const focusWorker = showWorkers && selectedWorker && !focusReviewer ? selectedWorker : null;
    const ordered = (focusReviewer ? reviewerTasksInOrder(focusReviewer, snap.attempts, snap.tasks) : focusWorker ? tasksInOrder(focusWorker, authorAttempts) : []).filter((id) => byId.has(id));
    const orderOf = new Map(ordered.map((id, i) => [id, i + 1]));
    const ran = new Set(ordered);
    const ups = new Set(focus ? depEdges.filter((e) => e.target === focus).map((e) => e.source) : []);
    const downs = new Set(focus ? depEdges.filter((e) => e.source === focus).map((e) => e.target) : []);

    const messages = allMessages(snap);
    const openQ = openQuestions(messages);
    const openEsc = openEscalations(messages);
    const coordActionWorkerIds = new Set(
      [...openQ, ...openEsc]
        .map((m) => messageWorkerId(m, snap.attempts))
        .filter((id): id is string => id != null),
    );
    const coordActionTaskIds = new Set<string>(
      [...openQ, ...openEsc]
        .map((m) => m.taskId)
        .filter((id): id is string => id != null),
    );
    for (const w of authors) {
      if (w.state === "busy" || w.state === "starting") {
        const cur = workerCurrentTask(w, snap);
        if (cur) coordActionTaskIds.add(cur.task.id);
      }
    }
    for (const a of authorAttempts) {
      if ((a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec") && byId.has(a.taskId)) {
        coordActionTaskIds.add(a.taskId);
      }
    }

    const dimGraph = Boolean(focus || focusWorker || focusReviewer || focusCoord);
    const relOf = (id: string): TaskNodeData["rel"] => {
      if (focusCoord) return coordActionTaskIds.has(id) ? "ran" : "dim";
      if (focusWorker || focusReviewer) return ran.has(id) ? "ran" : "dim";
      if (!focus || id === focus) return null;
      if (ups.has(id)) return "up";
      if (downs.has(id)) return "down";
      return "dim";
    };
    const depEdgeFocus = (source: string, target: string): "" | " edge--focus" | " edge--dim" => {
      if (focusCoord) return coordActionTaskIds.has(source) && coordActionTaskIds.has(target) ? "" : " edge--dim";
      if (focusWorker || focusReviewer) return ran.has(source) && ran.has(target) ? "" : " edge--dim";
      if (!focus) return "";
      return source === focus || target === focus ? " edge--focus" : " edge--dim";
    };

    const allSmart = showWorkers ? computeSmartExecLinks(snap, true, selectedId, activeLens) : [];
    const smartExecs = showWorkers ? computeSmartExecLinks(snap, showExec, selectedId, activeLens) : [];
    const execTargetSet = new Set(smartExecs.map((link) => link.taskId));
    if (showWorkers) {
      for (const taskId of orderOf.keys()) {
        if (!focusReviewer) execTargetSet.add(taskId);
      }
      if (focus) {
        for (const a of taskAttempts(focus, snap.attempts)) {
          if (
            a.kind !== "review" &&
            (a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec" || authors.some((w) => w.id === a.workerId))
          ) {
            execTargetSet.add(focus);
          }
        }
      }
    }
    const taskHasExec = (id: string) => execTargetSet.has(id);

    const rawNodes: Node[] = [];
    const rawEdges: Edge[] = [];
    // Author numbers remain stable even when their canvas group is hidden.
    const workerNum = workerNumbers(authors);
    const authorById = new Map(authors.map((w) => [w.id, w]));
    const wnumsOf = (taskId: string): { list: TaskNodeData["wnums"]; coordTakeover: boolean } => {
      const byNum = new Map<number, TaskNodeData["wnums"][number]>();
      let coordTakeover = false;
      for (const a of taskAttempts(taskId, snap.attempts)) {
        if (a.kind === "review") continue;
        if (a.workerId === "coordinator") {
          coordTakeover = true;
          continue;
        }
        const num = workerNum.get(a.workerId);
        if (num == null) continue;
        const w = authorById.get(a.workerId);
        const roleMeta = resolveRoleMeta(w?.role, { readOnlyDir: w?.readOnlyDir });
        const prev = byNum.get(num);
        byNum.set(num, {
          key: `${a.id}:${a.workerId}`,
          num,
          count: (prev?.count ?? 0) + 1,
          mark: attemptMark(a),
          line: agentModel(a.agent, a.model, ""),
          roleColor: roleColorVar(roleMeta.visualKind),
        });
      }
      return { list: [...byNum.values()], coordTakeover };
    };

    if (showCoord) {
      const busyAuthors = authors.filter((w) => w.state === "busy" || w.state === "starting").length;
      const busyReviewers = reviewerWorkers.filter((w) => w.state === "busy" || w.state === "starting").length;
      const pending = messages.filter((m) => m.needsAction && !m.handledAt && m.type !== "run_done").length;
      const coordHasExec = smartExecs.some((link) => link.workerNodeId === COORD_ID);
      const coordHasFocusTask =
        focus != null &&
        snap.attempts.some(
          (a) => a.taskId === focus && a.kind !== "review" && (a.workerId === "coordinator" || a.workerId === "exec" || a.kind === "exec"),
        );
      rawNodes.push({
        id: COORD_ID,
        type: "coordinator",
        position: at(COORD_ID),
        zIndex: selectedId === COORD_ID || focusCoord || coordHasFocusTask ? 5 : 0,
        data: {
          run: snap.run,
          token: runToken(snap.run.status, busyAuthors > 0 || busyReviewers > 0),
          busy: busyAuthors,
          workers: authors.length,
          pending,
          openQuestions: openQ.length,
          messageCount: messages.length,
          progress,
          selected: selectedId === COORD_ID,
          hot: focusCoord || coordHasFocusTask,
          dim: dimGraph && !focusCoord && !coordHasFocusTask,
          lensDim: isDimmedByLens(COORD_ID),
          exec: coordHasExec,
          dir,
        } satisfies CoordNodeData,
      });
    }

    if (showWorkers) {
      for (const w of authors) {
        const id = workerNodeId(w.id);
        const live = w.state === "busy" || w.state === "starting";
        const ranFocus = focus != null && snap.attempts.some((a) => a.taskId === focus && a.workerId === w.id && a.kind !== "review");
        const coordHot = focusCoord && (live || coordActionWorkerIds.has(w.id));
        const dim = focusCoord
          ? !coordHot
          : focusReviewer
            ? true
            : focusWorker
              ? id !== workerNodeId(focusWorker.id)
              : focus
                ? !ranFocus
                : false;
        const marks: MarkChip[] = focus
          ? taskAttempts(focus, snap.attempts)
              .filter((a) => a.workerId === w.id && a.kind !== "review")
              .map((a) => {
                const kind = attemptMark(a);
                return { key: a.id, kind, label: t(MARK_LABEL[kind]) };
              })
          : [];
        const hot = marks.length > 0 || coordHot;
        const hasExec = smartExecs.some((link) => link.workerId === w.id) || selectedId === id || hot;
        const wMsgs = workerMessages(w.id, messages, snap.attempts);
        const wOpenQ = openQ.filter((m) => messageWorkerId(m, snap.attempts) === w.id);
        const roleMeta = resolveRoleMeta(w.role, { readOnlyDir: w.readOnlyDir });
        const cur = workerCurrentTask(w, snap);
        const rawFork = w.forkedFrom?.trim();
        const forkLabel = rawFork
          ? rawFork === "coordinator" || rawFork === COORD_ID
            ? t("role.coordinator")
            : workerNum.get(rawFork) != null
              ? `W${workerNum.get(rawFork)}`
              : shortId(rawFork)
          : null;
        rawNodes.push({
          id,
          type: "worker",
          position: at(id),
          zIndex: selectedId === id || hot ? 5 : 0,
          data: {
            worker: w,
            line: agentModel(w.agent, w.model, t("task.unassigned")),
            num: workerNum.get(w.id) ?? 0,
            roleKind: roleMeta.visualKind,
            taskCount: tasksInOrder(w, authorAttempts).filter((taskId) => byId.has(taskId)).length,
            messageCount: wMsgs.length,
            openQuestions: wOpenQ.length,
            marks,
            currentTask: cur ? { id: cur.task.id, title: cur.task.title, startedAt: cur.startedAt } : null,
            selected: selectedId === id,
            dim,
            lensDim: isDimmedByLens(id),
            hot,
            exec: hasExec,
            dir,
            forkLabel,
          } satisfies WorkerNodeData,
        });
        const edgeFocus = dim ? " edge--dim" : dimGraph ? " edge--focus" : "";
        const dispatchId = dispatchRouteKey(id);
        rawEdges.push({
          id: dispatchId,
          source: COORD_ID,
          target: id,
          targetHandle: "dispatch-in",
          type: "routed",
          animated: live,
          className: `edge edge--dispatch${live ? " edge--active" : ""}${edgeFocus}${edgeLensClass(dispatchId)}`,
          markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: color.dispatch },
          data: routed(dispatchRouteKey(id)),
        });
      }

      for (const f of computeForkLinks(snap)) {
        const edgeId = f.id;
        const related = selectedId === f.sourceNodeId || selectedId === f.targetNodeId || focusCoord;
        const edgeFocus = dimGraph && !related ? " edge--dim" : "";
        rawEdges.push({
          id: edgeId,
          source: f.sourceNodeId,
          target: f.targetNodeId,
          sourceHandle: "fork-out",
          targetHandle: "fork-in",
          type: "fork",
          selectable: false,
          zIndex: 4,
          style: { "--fork-color": f.colorVar } as CSSProperties,
          className: `edge edge--fork${edgeFocus}${edgeLensClass(edgeId)}`,
          markerEnd: { type: MarkerType.ArrowClosed, width: 13, height: 13, color: f.colorVar },
          data: {
            label: t("edge.fork"),
            roleColor: f.colorVar,
            dir,
          } satisfies ForkEdgeData,
        });
      }
    }

    const visibleReviewLinks = showReviewers ? computeVisibleReviewLinks(snap, showReviewEdges, selectedId, spotlight) : [];
    const tasksWithReviewEdge = new Set(visibleReviewLinks.map((r) => r.taskId));
    const reviewersWithEdge = new Set(visibleReviewLinks.map((r) => r.reviewerNodeId));
    const pendingReviewEdges = visibleReviewLinks.map((r) => ({
      ...r,
      edgeLabel:
        r.mark === "waiting"
          ? `${t("review.title")} · ${t("review.standing.waiting")}`
          : `${t("review.roundShort", { round: r.round, max: r.maxRounds })} · ${t(`review.edge.${r.mark}`)}`,
    }));

    for (const w of reviewerWorkers) {
      const id = reviewerNodeId(w.id);
      const revLive = w.state === "busy" || w.state === "starting";
      const taskIds = reviewerTasksInOrder(w, snap.attempts, snap.tasks).filter((taskId) => byId.has(taskId));
      const hot = focus != null && taskIds.includes(focus);
      const coordHot = focusCoord && (revLive || coordActionWorkerIds.has(w.id));
      const dim = focusCoord
        ? !coordHot
        : focusReviewer
          ? focusReviewer.id !== w.id
          : focus
            ? !hot
            : Boolean(focusWorker);
      const rounds = snap.tasks.flatMap((task) => reviewBoard(task, snap.attempts, snap.workers)?.rounds.filter((round) => round.attempt.workerId === w.id) ?? []);
      rawNodes.push({
        id,
        type: "reviewer",
        position: at(id),
        width: NODE_SIZE.reviewer.w,
        height: NODE_SIZE.reviewer.h,
        zIndex: selectedId === id || hot ? 5 : 0,
        data: {
          worker: w,
          num: reviewNums.get(w.id) ?? 0,
          taskCount: taskIds.length,
          marks: rounds.map((round) => ({
            key: round.attempt.id,
            kind: round.verdict === "approved" ? "ok" : round.verdict === "changes_requested" || round.attempt.outcome === "failed" ? "fail" : "run",
            label: `Round ${round.round} · ${round.verdict === "approved" ? "通过" : round.verdict === "changes_requested" ? "要求修改" : round.attempt.outcome === "failed" ? "审查异常" : "审查中"}`,
          })),
          selected: selectedId === id,
          hot: hot || coordHot,
          dim,
          lensDim: isDimmedByLens(id),
          hasReviewEdge: reviewersWithEdge.has(id),
          dir,
        } satisfies ReviewerNodeData,
      });
      if (showCoord) {
        const edgeFocus = dim ? " edge--dim" : dimGraph ? " edge--focus" : "";
        const dispatchId = dispatchRouteKey(id);
        rawEdges.push({
          id: dispatchId,
          source: COORD_ID,
          target: id,
          targetHandle: "dispatch-in",
          type: "routed",
          animated: revLive,
          className: `edge edge--dispatch${revLive ? " edge--active" : ""}${edgeFocus}${edgeLensClass(dispatchId)}`,
          markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: color.dispatch },
          data: routed(dispatchId),
        });
      }
    }

    for (const task of snap.tasks) {
      const board = reviewBoard(task, snap.attempts, snap.workers);
      const reviewText = board
        ? board.standing === "approved"
          ? t("review.edge.approved")
          : board.standing === "rejected"
            ? t("review.edge.changes")
            : `${t("review.roundShort", { round: board.round, max: board.maxRounds })} · ${t(`review.standing.${board.standing}`)}`
        : null;
      const nonReview = taskAttempts(task.id, snap.attempts).filter((attempt) => attempt.kind !== "review");
      const latest = nonReview.at(-1);
      const firstStartedAt = nonReview[0]?.startedAt ?? null;
      const usage = taskUsage(task.id, snap.attempts);
      const rel = relOf(task.id);
      const cause = taskFailure(task, snap.attempts);
      const assignedRole = task.assign?.role?.trim();
      const latestWorker = latest ? authorById.get(latest.workerId) : undefined;
      let targetRole: TaskNodeData["targetRole"] = null;
      if (task.kind !== "exec" && (assignedRole || latestWorker?.role)) {
        const meta = resolveRoleMeta(assignedRole || latestWorker?.role, { readOnlyDir: latestWorker?.readOnlyDir });
        targetRole = { kind: meta.visualKind, label: roleDisplayName(meta, t), colorVar: roleColorVar(meta.visualKind) };
      }
      const subphaseText = task.status === "running"
        ? task.execPhase === "verifying" || Boolean(latest?.verifying)
          ? t("task.subphase.verifying")
          : task.execPhase === "integrating"
            ? t("task.subphase.integrating")
            : task.execPhase === "review" || board?.standing === "reviewing"
              ? t("task.subphase.reviewing", { round: Math.max(1, board?.round || 1), max: board?.maxRounds ?? 2 })
              : task.execPhase === "rework" || board?.standing === "rework"
                ? t("task.subphase.rework")
                : t("task.subphase.coding")
        : null;
      const wInfo = wnumsOf(task.id);
      rawNodes.push({
        id: task.id,
        type: "task",
        position: at(task.id),
        zIndex: task.id === selectedId || rel === "ran" || rel === "up" || rel === "down" ? 5 : 0,
        data: {
          label: task.title,
          statusKey: task.status,
          subphaseText,
          awaitingLateDone: Boolean(latest?.awaitingLateDone && !latest.endedAt),
          token: branchUnselected(task, snap.tasks) ? "skipped" : cause ? failureToken(cause.bucket) : taskToken(task.status),
          line: agentModel(latest?.agent ?? task.assign?.agent, latest?.model ?? task.assign?.model, t("task.unassigned")),
          targetRole,
          wnums: wInfo.list,
          coordTakeover: wInfo.coordTakeover,
          reviewRequired: board !== null,
          review: board && reviewText ? { text: reviewText, standing: board.standing } : null,
          tokens: fmtTokens(usage.tokens),
          cost: fmtCost(usage.cost),
          tokensEstimated: usage.estimatedTokens,
          costEstimated: usage.estimatedCost,
          startedAt: task.status === "running" ? (latest?.startedAt ?? firstStartedAt) : firstStartedAt,
          endedAt: task.status === "running" ? null : (latest?.endedAt ?? null),
          failures: task.failures,
          failLabel: cause ? t(cause.labelKey) : null,
          failTitle: cause ? `${t(`fail.bucket.${cause.bucket}`)} · ${t(`fail.${cause.kind}`)}${cause.message ? ` — ${cause.message}` : ""}` : null,
          selected: task.id === selectedId,
          dir,
          hasBranch: Boolean(task.when),
          branchText: branchCaption(task),
          branchPhase: branchPhase(task, snap.tasks),
          unselected: branchUnselected(task, snap.tasks),
          badges: taskMicroBadges(task, t).filter((badge) => badge.kind !== "review" && badge.kind !== "role"),
          exec: taskHasExec(task.id),
          hasReviewEdge: tasksWithReviewEdge.has(task.id),
          rel,
          lensDim: isDimmedByLens(task.id),
          order: orderOf.get(task.id) ?? null,
        } satisfies TaskNodeData,
      });
    }

    const dataBranches = computeDataBranchLinks(snap);
    const sharedDataBranch = new Map(
      dataBranches
        .filter((d) => d.sharesDepEdge)
        .map((d) => [`${d.sourceTaskId}\0${d.targetTaskId}`, d] as const),
    );

    for (const e of depEdges) {
      const satisfied = byId.get(e.source)?.status === "succeeded";
      const dataBranch = sharedDataBranch.get(`${e.source}\0${e.target}`);
      const depId = depRouteKey(e.source, e.target);
      rawEdges.push({
        id: `dep:${e.id}`,
        source: e.source,
        target: e.target,
        sourceHandle: "dep-out",
        targetHandle: "dep-in",
        type: "routed",
        className: `edge edge--depends${satisfied ? " edge--satisfied" : ""}${dataBranch ? ` edge--data-branch edge--branch-${dataBranch.phase}` : ""}${depEdgeFocus(e.source, e.target)}${edgeLensClass(depId)}`,
        markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16, color: dataBranch ? (dataBranch.phase === "hit" ? color.satisfied : dataBranch.phase === "miss" ? color.depends : color.branch) : satisfied ? color.satisfied : color.depends },
        data: dataBranch
          ? { ...routed(depId), label: dataBranch.label, labelClass: dataBranch.phase }
          : routed(depId),
      });
    }

    for (const db of dataBranches) {
      if (db.sharesDepEdge) continue;
      const related = db.sourceTaskId === focus || db.targetTaskId === focus || (focusWorker != null && ran.has(db.targetTaskId));
      const dim = dimGraph && !related;
      const colorOf = db.phase === "hit" ? color.satisfied : db.phase === "miss" ? color.depends : color.branch;
      rawEdges.push({
        id: db.id,
        source: db.sourceTaskId,
        target: db.targetTaskId,
        sourceHandle: "dep-out",
        targetHandle: "branch-in",
        type: "routed",
        className: `edge edge--branch edge--branch-${db.phase}${dim ? " edge--dim" : ""}${related && dimGraph ? " edge--focus" : ""}${edgeLensClass(db.id)}`,
        markerEnd: { type: MarkerType.ArrowClosed, width: 14, height: 14, color: colorOf },
        data: { ...routed(depRouteKey(db.sourceTaskId, db.targetTaskId)), label: db.label, labelClass: db.phase },
      });
    }

    const links: ExecLink[] = [];
    const takenAttemptIds = new Set<string>();
    const actorTag = (s: SmartExecLink) =>
      s.execKind === "exec"
        ? t("badge.exec")
        : s.execKind === "coordinator"
          ? t("task.coordShort")
          : `W${s.wNum ?? "?"}`;
    const execLabel = (s: SmartExecLink, ordNum: number, includeOutcome: boolean) => {
      const actor = actorTag(s);
      const translatedRole = t(s.roleLabelKey);
      const roleName = translatedRole === s.roleLabelKey ? s.roleFallback : translatedRole;
      const prefix = `${circNum(ordNum)} ${actor}`;
      if (s.live) return s.execKind === "worker" ? `${prefix} · ${roleName}` : `${prefix} · ${t(MARK_LABEL.run)}`;
      if (s.rework) return `${prefix} · ${t("outcome.rework")}${s.mark === "fail" ? ` (${t(MARK_LABEL.fail)})` : ""}`;
      if (s.retry) return `${prefix} · ${t("outcome.retry")}${s.mark === "fail" ? ` (${t(MARK_LABEL.fail)})` : ""}`;
      if (s.mark !== "ok" || includeOutcome) return `${prefix} · ${t(MARK_LABEL[s.mark])}`;
      return prefix;
    };

    if (showWorkers && focusWorker) {
      allSmart
        .filter((s) => s.workerId === focusWorker.id)
        .forEach((s, idx) => {
          takenAttemptIds.add(s.attemptId);
          links.push({
            key: `sel:${s.attemptId}`,
            routeKey: s.routeKey,
            workerId: s.workerId,
            from: s.workerNodeId,
            to: s.taskId,
            label: execLabel(s, idx + 1, s.attemptTotal > 1 || s.mark !== "ok"),
            mark: s.mark,
            live: s.live,
            roleColor: s.colorVar,
            emphasis: " edge--focus",
          });
        });
    } else if (showWorkers && focus) {
      allSmart
        .filter((s) => s.taskId === focus)
        .forEach((s, idx) => {
          takenAttemptIds.add(s.attemptId);
          links.push({
            key: `sel:${s.attemptId}`,
            routeKey: s.routeKey,
            workerId: s.workerId,
            from: s.workerNodeId,
            to: s.taskId,
            label: execLabel(s, idx + 1, true),
            mark: s.mark,
            live: s.live,
            roleColor: s.colorVar,
            emphasis: " edge--focus",
          });
        });
    }

    if (showWorkers) {
      for (const smart of smartExecs) {
        if (takenAttemptIds.has(smart.attemptId)) continue;
        takenAttemptIds.add(smart.attemptId);
        const dimThis =
          !smart.live &&
          dimGraph &&
          !(focusCoord && (smart.workerNodeId === COORD_ID || coordActionWorkerIds.has(smart.workerId)));
        const coordHotThis =
          focusCoord && (smart.live || smart.workerNodeId === COORD_ID || coordActionWorkerIds.has(smart.workerId));
        links.push({
          key: smart.id,
          routeKey: smart.routeKey,
          workerId: smart.workerId,
          from: smart.workerNodeId,
          to: smart.taskId,
          label: execLabel(smart, smart.step, false),
          mark: smart.mark,
          persist: !smart.live,
          live: smart.live,
          roleColor: smart.colorVar,
          emphasis: coordHotThis ? " edge--focus" : dimThis ? " edge--dim" : "",
        });
      }
    }

    const currentPosMap = new Map<string, { x: number; y: number }>();
    for (const [id, p] of laid.pos) currentPosMap.set(id, dragged.current.get(id) ?? p);
    const obstacleIds: string[] = [
      ...(showCoord ? [COORD_ID] : []),
      ...(showWorkers ? authors.map((w) => workerNodeId(w.id)) : []),
      ...reviewerWorkers.map((w) => reviewerNodeId(w.id)),
      ...snap.tasks.map((task) => task.id),
    ];
    const seamPlans = computeExecRoutes({
      dir,
      seam: laid.seam,
      pos: currentPosMap,
      sizeOf: zoneSizeOf,
      obstacleIds,
      links: [
        ...links.map((link) => ({ key: link.key, from: link.from, to: link.to, kind: "exec" as const })),
        ...pendingReviewEdges.map((r) => ({ key: r.edgeId, from: r.reviewerNodeId, to: r.taskId, kind: "review" as const })),
      ],
    });
    const lanes = lanesOf(links);
    for (const link of links) {
      const markerColor = link.live && link.roleColor
        ? link.roleColor
        : link.persist
          ? persistColor(link.mark)
          : markColor(link.mark);
      rawEdges.push({
        id: link.key,
        source: link.from,
        target: link.to,
        sourceHandle: "exec-out",
        targetHandle: "exec-in",
        type: "exec",
        selectable: false,
        animated: Boolean(link.live),
        zIndex: link.emphasis === " edge--focus" ? 9 : link.live ? 8 : link.emphasis === " edge--dim" ? 4 : 7,
        style: link.roleColor ? ({ "--edge-role": link.roleColor } as CSSProperties) : undefined,
        className: `edge edge--executes edge--mark-${link.mark}${link.persist ? " edge--persist" : ""}${link.mark === "retry" ? " edge--retry" : ""}${link.live ? " edge--active edge--exec-live" : ""}${link.emphasis ?? ""}${edgeLensClass(link.routeKey)}`,
        markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16, color: markerColor },
        data: {
          lane: lanes.get(link.key) ?? 0,
          label: link.label,
          mark: link.mark,
          persist: link.persist,
          live: link.live,
          roleColor: link.roleColor,
          seam: laid.seam,
          dir,
          plan: seamPlans.get(link.key),
        } satisfies ExecEdgeData,
      });
    }
    for (const r of pendingReviewEdges) {
      rawEdges.push({
        id: r.edgeId,
        source: r.taskId,
        target: r.reviewerNodeId,
        sourceHandle: "review-out",
        targetHandle: "review-in",
        type: "routed",
        selectable: false,
        animated: r.live,
        zIndex: r.emphasis === " edge--focus" ? 9 : 6,
        className: `edge edge--review edge--review-${r.mark}${r.emphasis}${edgeLensClass(r.baseEdgeId)}`,
        markerEnd: { type: MarkerType.ArrowClosed, width: 16, height: 16, color: "var(--role-reviewer)" },
        data: {
          ...routed(r.baseEdgeId),
          review: true,
          seam: laid.seam,
          plan: seamPlans.get(r.edgeId),
          reversePlan: true,
          parallelOffset: r.parallelOffset,
          labelShift: r.labelShift,
          label: r.edgeLabel,
          labelClass: r.mark,
        } satisfies RoutedEdgeData,
      });
    }

    setNodes((cur) => mergePositions(rawNodes, cur, dragged.current, draggingId.current, dirChanged));
    setEdges(rawEdges);
  }, [boardModel, snap, selectedId, showWorkers, showReviewers, showExec, showReviewEdges, activeLens, spotlight, progress, byId, setNodes, setEdges, t]);

  const onNodeDragStart = useCallback((_e: unknown, node: Node) => {
    draggingId.current = node.id;
  }, []);
  const onNodeDragStop = useCallback((_e: unknown, node: Node) => {
    dragged.current.set(node.id, node.position);
    draggingId.current = null;
  }, []);

  // --- replaying a message thread on the canvas ---
  const [step, setStep] = useState(0);
  useEffect(() => setStep(0), [talk]);
  useEffect(() => {
    if (!talk || step >= talk.hops.length) return;
    const timer = window.setTimeout(() => setStep((s) => s + 1), HOP_MS);
    return () => window.clearTimeout(timer);
  }, [talk, step]);
  const hop = talk && step < talk.hops.length ? talk.hops[step] : null;
  const related = useMemo(() => {
    const ids = new Set<string>();
    for (const h of talk?.hops ?? []) for (const id of [h.from, h.to, h.taskId]) if (id) ids.add(id);
    return ids;
  }, [talk]);
  const onCanvas = useMemo(() => {
    if (!talk) return true;
    const ids = new Set(nodes.map((n) => n.id));
    return talk.hops.some((h) => h.from && h.to && ids.has(h.from) && ids.has(h.to) && h.from !== h.to);
  }, [talk, nodes]);
  const shownNodes = useMemo(() => {
    if (!talk) return nodes;
    return nodes.map((n) => {
      const cls =
        hop && n.id === hop.from
          ? "talk-node talk-from"
          : hop && n.id === hop.to
            ? "talk-node talk-to"
            : related.has(n.id)
              ? "talk-node"
              : "talk-dim";
      return { ...n, className: cls };
    });
  }, [nodes, talk, hop, related]);
  const shownEdges = useMemo(
    () => (talk ? edges.map((e) => ({ ...e, animated: false, className: `${e.className ?? ""} edge--talk-dim` })) : edges),
    [edges, talk],
  );
  const wNums = useMemo(() => workerNumbers(authorWorkers(snap.workers)), [snap.workers]);
  const rNums = useMemo(() => reviewerNumbers(snap.workers), [snap.workers]);
  const roleOf = (id: string | null) => {
    if (!id) return "—";
    if (id === COORD_ID) return t("role.coordinator");
    const reviewerId = parseReviewerNodeId(id);
    if (reviewerId) return `R${rNums.get(reviewerId) ?? "?"}`;
    if (id.startsWith("worker:")) {
      const wId = id.slice("worker:".length);
      const reviewer = rNums.get(wId);
      if (reviewer) return `R${reviewer}`;
      const n = wNums.get(wId);
      return n ? `W${n}` : t("role.worker");
    }
    return byId.get(id)?.title ?? id;
  };
  const hopLabel = (h: TalkHop) => (h.reply ? t("msg.reply") : t(HOP_TYPE_KEY[h.type] ?? h.type));

  // Attention bar: open worker questions and unhandled escalations/failures.
  const attn = useMemo(() => {
    const msgs = allMessages(snap);
    const questions = openQuestions(msgs);
    const escalations = openEscalations(msgs);
    return { questions, escalations };
  }, [snap]);

  const msgTarget = (m: Message): string => {
    const wId = messageWorkerId(m, snap.attempts);
    if (showReviewers && wId && snap.workers.some((w) => w.id === wId && isReviewer(w))) return reviewerNodeId(wId);
    if (showWorkers && wId && authorWorkers(snap.workers).some((w) => w.id === wId)) return workerNodeId(wId);
    if (m.taskId && byId.has(m.taskId)) return m.taskId;
    return boardModel.showCoord ? COORD_ID : (m.taskId ?? COORD_ID);
  };
  const msgChipLabel = (m: Message): string => {
    const wId = messageWorkerId(m, snap.attempts);
    const wNum = wId ? wNums.get(wId) : undefined;
    const taskTitle = m.taskId ? byId.get(m.taskId)?.title : undefined;
    const rNum = wId ? rNums.get(wId) : undefined;
    const prefix = [rNum ? `R${rNum}` : wNum ? `W${wNum}` : null, taskTitle].filter(Boolean).join(" · ");
    return prefix ? `${prefix}：${m.subject}` : m.subject;
  };

  if (!showWorkers && (!showReviewers || !snap.workers.some(isReviewer)) && snap.tasks.length === 0) {
    return (
      <div className="dag-empty">
        <div className="dag-empty__title">{t("empty.title")}</div>
        <div className="dag-empty__hint">{t("empty.body")}</div>
      </div>
    );
  }

  return (
    <>
      {talk ? (
        <div className={`talk-caption${onCanvas ? "" : " talk-caption--off"}`} role="status">
          {!onCanvas ? (
            <span>{t("msg.offCanvas")}</span>
          ) : hop ? (
            <>
              <span className="talk-caption__step">
                {step + 1}/{talk.hops.length}
              </span>
              <span className={`msg__type msg__type--${hop.reply ? "reply" : hop.type}`}>{hopLabel(hop)}</span>
              <span>
                {roleOf(hop.from)} → {roleOf(hop.to)}
              </span>
              <span className="talk-caption__subject">{hop.subject}</span>
            </>
          ) : (
            <span>{t("msg.playDone", { n: talk.hops.length })}</span>
          )}
          <button type="button" className="talk-caption__close" onClick={onStopTalk} aria-label={t("msg.stop")} title={t("msg.stop")}>
            ✕
          </button>
        </div>
      ) : (
        attn.questions.length + attn.escalations.length > 0 && (
          <div className="gates" role="status">
            {attn.questions.length > 0 && <span className="gates__label gates__label--ask">? {t("attn.questions")}</span>}
            {attn.questions.map((m) => (
              <button
                key={m.id}
                className="gates__chip gates__chip--ask"
                onClick={() => onSelect(msgTarget(m))}
                title={m.body || m.subject}
              >
                {msgChipLabel(m)}
              </button>
            ))}
            {attn.escalations.length > 0 && <span className="gates__label gates__label--esc">! {t("attn.escalations")}</span>}
            {attn.escalations.map((m) => (
              <button
                key={m.id}
                className="gates__chip gates__chip--esc"
                onClick={() => onSelect(msgTarget(m))}
                title={m.body || m.subject}
              >
                {msgChipLabel(m)}
              </button>
            ))}
          </div>
        )
      )}
      <ReactFlow
        nodes={shownNodes}
        edges={shownEdges}
        onNodesChange={onNodesChange}
        onEdgesChange={onEdgesChange}
        onNodeDragStart={onNodeDragStart}
        onNodeDragStop={onNodeDragStop}
        nodeTypes={nodeTypes}
        edgeTypes={edgeTypes}
        nodesDraggable
        nodesConnectable={false}
        elementsSelectable
        minZoom={0.15}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_, n) => onSelect(n.id === selectedId ? null : n.id)}
        onPaneClick={() => onSelect(null)}
      >
        <Background variant={BackgroundVariant.Dots} gap={20} size={1} color="var(--grid)" />
        <Controls showInteractive={false} position="top-left" />
        <DagZoneBackdrop
          dir={boardModel.dir}
          zones={boardModel.zones}
          workerGroups={boardModel.workerGroups}
          lensRole={activeLens.role}
          pos={boardModel.laid.pos}
          sizeOf={zoneSizeOf}
          seam={boardModel.laid.seam}
          reviewSeam={boardModel.laid.reviewSeam}
          progress={progress}
          authorCount={boardModel.authors.length}
          reviewerCount={boardModel.reviewerWorkers.length}
        />
        {talk && onCanvas && (
          <TalkOverlay
            hops={talk.hops}
            step={step}
            runKey={`${talk.key}:${talk.nonce}`}
            nodes={nodes}
            dir={layout === "layered-tb" ? "TB" : "LR"}
            labelOf={hopLabel}
          />
        )}
      </ReactFlow>
      {!talk && (
        <DagRoleLensBar
          roles={roleChips}
          lens={activeLens}
          onToggleRole={(role: LensRole) => updateLens({ ...activeLens, role: activeLens.role === role ? null : role })}
          onClearPhase={() => updateLens({ ...activeLens, phase: null })}
          onClearAll={() => updateLens(EMPTY_LENS)}
        />
      )}
    </>
  );
}

export const DagView = memo(function DagView({
  snap,
  selectedId,
  onSelect,
  layout,
  reorgNonce,
  showWorkers,
  showReviewers = true,
  showExec,
  showReviewEdges = false,
  lens,
  onLensChange,
  camera,
  bindViewport,
  restoreViewport,
  talk,
  onStopTalk,
}: {
  snap: RunSnapshot;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  layout: LayoutKind;
  reorgNonce: number;
  showWorkers: boolean;
  showReviewers?: boolean;
  showExec: boolean;
  showReviewEdges?: boolean;
  lens?: DagLens;
  onLensChange?: (next: DagLens) => void;
  camera: "frame" | "center" | "restore";
  bindViewport?: (get: () => CanvasViewport) => void;
  restoreViewport?: { vp: CanvasViewport; token: number } | null;
  talk?: TalkPlay | null;
  onStopTalk?: () => void;
}) {
  const { t } = useT();
  return (
    <div className="dag-board" data-layout={layout} aria-label={t("canvas.label")}>
      <ReactFlowProvider>
        <Board
          snap={snap}
          selectedId={selectedId}
          onSelect={onSelect}
          layout={layout}
          reorgNonce={reorgNonce}
          showWorkers={showWorkers}
          showReviewers={showReviewers}
          showExec={showExec}
          showReviewEdges={showReviewEdges}
          lens={lens}
          onLensChange={onLensChange}
          camera={camera}
          bindViewport={bindViewport}
          restoreViewport={restoreViewport}
          talk={talk}
          onStopTalk={onStopTalk}
        />
      </ReactFlowProvider>
    </div>
  );
});
