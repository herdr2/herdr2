import { useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from "react";
import { stopWorker } from "../api";
import { COORD_ID } from "./DagView";
import { branchCaption, branchPhase } from "../branch";
import { agentModel, attemptTokens, circNum, contextPct, fmtCost, fmtDuration, fmtTokens, jsonPreview, shortId, snapshotEstimated, taskMicroBadges, workerNumbers } from "../format";
import { authorWorkers, isReviewer, parseReviewerNodeId, reviewBoard, reviewerNumbers, taskRequiresReview, type ReviewBoard as ReviewBoardModel, type ReviewRoundView } from "../review";
import { resolveRoleMeta, roleDisplayName } from "../roles";
import {
  MARK_LABEL,
  allMessages,
  attemptMark,
  coordinatorHostLabel,
  isReviewMessage,
  reviewerTasksInOrder,
  tasksInOrder,
  workerAttempts,
  workerMessages,
} from "../relate";
import { deriveTaskStatusBadge } from "../workbench";
import { Estimate } from "./Estimate";
import { HostTranscript } from "./HostTranscript";
import { MessageThread } from "./MessageThread";
import { ReviewRequirement } from "./ReviewRequirement";
import { TaskAiDiagnosisPanel, TaskSummaryView } from "./TaskAiPanel";
import { TaskAttemptCard } from "./TaskAttemptCard";
import { WorkerTranscript } from "./WorkerTranscript";
import { StatusBadge } from "./WorkbenchShell";
import { useT } from "../i18n";
import {
  type Item,
  type RunSnapshot,
  type TalkPlay,
  type Task,
  type Worker,
  type WorkerState,
} from "../types";

type DetailLinks = {
  onOpenReviews?: (taskId: string) => void;
};

type TalkProps = { playing?: string | null; onPlay?: (play: TalkPlay | null) => void };

const PANEL_WIDTH_KEY = "co-web:panel-width";
const PANEL_DEFAULT = 400;
const PANEL_MIN = 300;
const CANVAS_KEEP = 160;

const STOPPABLE = new Set<WorkerState>(["starting", "idle", "busy"]);

function readPanelWidth(): number {
  try {
    const n = Number(localStorage.getItem(PANEL_WIDTH_KEY));
    return Number.isFinite(n) && n >= PANEL_MIN ? n : PANEL_DEFAULT;
  } catch {
    return PANEL_DEFAULT;
  }
}

let panelWidth = readPanelWidth();

function storePanelWidth(w: number): void {
  panelWidth = w;
  try {
    localStorage.setItem(PANEL_WIDTH_KEY, String(Math.round(w)));
  } catch {
    /* keep it for this page only */
  }
}

function usePanelWidth() {
  const [width, setWidth] = useState(panelWidth);
  const [resizing, setResizing] = useState(false);
  const ref = useRef<HTMLElement>(null);
  const start = useRef<{ x: number; w: number; id: number } | null>(null);

  function clamp(w: number): number {
    const room = (ref.current?.parentElement?.clientWidth ?? window.innerWidth) - CANVAS_KEEP;
    return Math.round(Math.max(PANEL_MIN, Math.min(Math.max(PANEL_MIN, room), w)));
  }
  function apply(w: number) {
    const next = clamp(w);
    setWidth(next);
    storePanelWidth(next);
  }
  const handle = {
    onPointerDown(e: PointerEvent<HTMLDivElement>) {
      if (e.button !== 0) return;
      e.preventDefault();
      start.current = { x: e.clientX, w: ref.current?.offsetWidth ?? width, id: e.pointerId };
      e.currentTarget.setPointerCapture(e.pointerId);
      setResizing(true);
    },
    onPointerMove(e: PointerEvent<HTMLDivElement>) {
      const s = start.current;
      if (!s || e.pointerId !== s.id) return;
      apply(s.w + (s.x - e.clientX));
    },
    onPointerUp(e: PointerEvent<HTMLDivElement>) {
      if (start.current?.id !== e.pointerId) return;
      start.current = null;
      setResizing(false);
    },
    onLostPointerCapture() {
      start.current = null;
      setResizing(false);
    },
    onDoubleClick() {
      apply(PANEL_DEFAULT);
    },
    onKeyDown(e: KeyboardEvent<HTMLDivElement>) {
      const step = e.shiftKey ? 64 : 16;
      if (e.key === "ArrowLeft") apply(width + step);
      else if (e.key === "ArrowRight") apply(width - step);
      else if (e.key === "Home") apply(PANEL_DEFAULT);
      else return;
      e.preventDefault();
    },
  };
  return { ref, width, resizing, handle };
}

function Shell({
  role,
  roleKind,
  roleBadgeClass,
  badges,
  actions,
  title,
  subtitle,
  onClose,
  children,
}: {
  role: string;
  roleKind: "task" | "worker" | "coord" | "reviewer";
  roleBadgeClass?: string;
  badges?: ReactNode;
  actions?: ReactNode;
  title: string;
  subtitle?: ReactNode;
  onClose: () => void;
  children: ReactNode;
}) {
  const { t } = useT();
  const { ref, width, resizing, handle } = usePanelWidth();
  return (
    <aside
      ref={ref}
      data-role={roleKind}
      className={`side-panel${resizing ? " side-panel--resizing" : ""}`}
      style={{ "--panel-w": `${width}px` } as CSSProperties}
    >
      <div
        className="side-panel__resize"
        role="separator"
        aria-orientation="vertical"
        aria-label={t("panel.resize")}
        aria-valuenow={width}
        aria-valuemin={PANEL_MIN}
        title={t("panel.resize")}
        tabIndex={0}
        {...handle}
      />
      <header className="side-panel__header">
        <div className="side-panel__top">
          <div className="side-panel__badges">
            <span className={`role-badge ${roleBadgeClass ?? `role-badge--${roleKind}`}`}>{role}</span>
            {badges}
          </div>
          <div className="side-panel__actions">
            {actions}
            <button type="button" className="icon-btn" onClick={onClose} aria-label={t("panel.close")}>✕</button>
          </div>
        </div>
        <h3 className="side-panel__title">{title}</h3>
        {subtitle && <div className="side-panel__sub">{subtitle}</div>}
      </header>
      <div className="side-panel__body">{children}</div>
    </aside>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="panel-field">
      <span className="panel-field__key">{label}</span>
      <div className="panel-field__val">{children}</div>
    </div>
  );
}

function Group({
  title,
  count,
  action,
  hint,
  children,
}: {
  title: string;
  count?: number;
  action?: ReactNode;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <section className="panel-group">
      <div className="panel-group__head">
        <div className="sp-group-title-wrap">
          <span className="panel-group__title">{title}</span>
          {count != null && <span className="sp-count">{count}</span>}
        </div>
        {action}
      </div>
      {hint && <p className="hint sp-group-hint">{hint}</p>}
      <div className="sp-group-body">{children}</div>
    </section>
  );
}

function StatItem({ label, full = false, children }: { label: string; full?: boolean; children: ReactNode }) {
  return (
    <div className={`sp-kv${full ? " sp-kv--full" : ""}`}>
      <span className="panel-field__key">{label}</span>
      <div className="sp-kv__val">{children}</div>
    </div>
  );
}

function roundVerdict(round: ReviewRoundView, board: ReviewBoardModel, t: (key: string) => string): string {
  if (round.verdict === "approved") return "通过";
  if (round.verdict === "changes_requested") return "要求修改";
  const attempt = round.attempt;
  const last = round.index === board.rounds.length;
  if (!attempt.endedAt && !attempt.outcome) return t("review.roundRunning");
  if (attempt.failure) return t(`fail.${attempt.failure.kind}`);
  if (attempt.outcome === "succeeded" && last && board.standing === "rejected") return t("review.verdict.rejected");
  if (attempt.outcome === "succeeded" && last && board.standing === "approved") return t("review.verdict.approved");
  if (attempt.outcome === "succeeded" && last && board.standing === "rework") return t("review.verdict.changes");
  if (attempt.outcome === "succeeded") return t("review.verdict.submitted");
  return t("outcome.running");
}

function TaskPanel({
  task,
  snap,
  liveItems,
  streamEpoch,
  onSelect,
  onClose,
  onRefresh,
  onOpenReviews,
}: {
  task: Task;
  snap: RunSnapshot;
  liveItems: Item[];
  streamEpoch: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  onRefresh: () => void;
} & DetailLinks) {
  const { t } = useT();
  const attempts = snap.attempts
    .filter((a) => a.taskId === task.id)
    .sort((a, b) => (a.startedAt === b.startedAt ? (a.id < b.id ? -1 : 1) : a.startedAt < b.startedAt ? -1 : 1));
  const phase = branchPhase(task, snap.tasks);
  const workerNums = workerNumbers(authorWorkers(snap.workers));
  const reviewerNums = reviewerNumbers(snap.workers);
  const board = reviewBoard(task, snap.attempts, snap.workers);
  const badges = taskMicroBadges(task, t);
  const statusBadge = deriveTaskStatusBadge(task, attempts, snap.tasks, t);
  const cause = statusBadge.cause;
  const branchText = branchCaption(task);
  const hasReviewAttempts = attempts.some((a) => a.kind === "review");
  const idLabel = task.key && task.key !== task.id ? `${task.key} · ${task.id}` : task.id;
  const assignedRoleMeta = task.assign?.role && task.kind !== "exec"
    ? resolveRoleMeta(task.assign.role)
    : null;
  const visibleBadges = badges.filter((b) => !(b.kind === "review" && b.text === "REVIEW"));

  return (
    <Shell
      role={t("role.task")}
      roleKind="task"
      badges={
        <>
          <StatusBadge token={statusBadge.token} pulse={statusBadge.pulse}>
            {statusBadge.label}
          </StatusBadge>
          <ReviewRequirement required={taskRequiresReview(task, attempts)} />
          {visibleBadges.map((badge) =>
            badge.kind === "role" && assignedRoleMeta ? (
              <span key={badge.kind} className={`role-badge ${assignedRoleMeta.badgeClass}`} title={badge.title}>
                {badge.text}
              </span>
            ) : (
              <span key={badge.kind} className={`micro-badge micro-badge--${badge.kind}`} title={badge.title}>
                {badge.text}
              </span>
            ),
          )}
        </>
      }
      title={task.title}
      subtitle={
        <>
          <code className="side-panel__id">{idLabel}</code>
          {branchText && <span className={`branch-tag branch-tag--${phase ?? "open"}`}>{branchText}</span>}
        </>
      }
      onClose={onClose}
    >
      {cause && (
        <div className={`fail-banner fail-banner--${cause.bucket}`}>
          <div className="fail-banner__class">{t(`fail.bucket.${cause.bucket}`)}</div>
          <div className="fail-banner__detail">
            {t(`fail.${cause.kind}`)}
            {cause.message ? ` — ${cause.message}` : ""}
          </div>
        </div>
      )}

      <TaskAiDiagnosisPanel task={task} attempts={attempts} onRefresh={onRefresh} />

      <Group title={t("panel.spec")}>
        <Field label={t("panel.target")}>{task.spec.target}</Field>
        <Field label={t("panel.change")}>{task.spec.change}</Field>
        {task.spec.constraints && <Field label={t("panel.constraints")}>{task.spec.constraints}</Field>}
        {task.spec.ownership && <Field label={t("panel.ownership")}>{task.spec.ownership}</Field>}
        <Field label={t("panel.acceptance")}>{task.spec.acceptance}</Field>
        {task.spec.verify && (
          <Field label={t("wb.taskVerify")}>
            <pre className="stream__pre">
              {typeof task.spec.verify === "string" ? task.spec.verify : jsonPreview(task.spec.verify)}
            </pre>
          </Field>
        )}
      </Group>

      {attempts.length === 0 && task.result && (
        <Group title={t("panel.result")}>
          <Field label={t("panel.summary")}>
            <TaskSummaryView summary={task.result.summary} compactSummary={task.result.compactSummary} />
          </Field>
          {task.result.filesModified && task.result.filesModified.length > 0 && (
            <Field label={t("panel.files")}>
              <ul className="sp-file-list">
                {task.result.filesModified.map((f) => <li key={f} className="sp-file-chip mono">{f}</li>)}
              </ul>
            </Field>
          )}
          {task.result.data != null && (
            <Field label={t("panel.data")}>
              <pre className="stream__pre">{jsonPreview(task.result.data)}</pre>
            </Field>
          )}
        </Group>
      )}

      <Group title={t("panel.attemptsDetail")} count={attempts.length}>
        {attempts.length === 0 && <span className="hint">{t("panel.noAttempts")}</span>}
        {attempts.length > 0 && (
          <div className="wb-list-grid">
            {attempts.map((a, i) => (
              <TaskAttemptCard
                key={a.id}
                attempt={a}
                ordinal={i + 1}
                allAttempts={attempts}
                task={task}
                snap={snap}
                liveItems={liveItems}
                streamEpoch={streamEpoch}
                workerNums={workerNums}
                reviewerNums={reviewerNums}
                board={board}
                compact
                onSelectNode={onSelect}
                onOpenReviews={onOpenReviews}
              />
            ))}
          </div>
        )}
        {task.review?.criteria && !hasReviewAttempts && (
          <Field label={t("attempt.reviewAwaiting")}>{task.review.criteria}</Field>
        )}
      </Group>
    </Shell>
  );
}

function WorkerPanel({
  worker,
  snap,
  liveItems,
  streamEpoch,
  onSelect,
  onClose,
  onRefresh,
  talk,
}: {
  worker: Worker;
  snap: RunSnapshot;
  liveItems: Item[];
  streamEpoch: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  onRefresh: () => void;
  talk: TalkProps;
}) {
  const { t } = useT();
  const [stopping, setStopping] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function stop() {
    setStopping(true);
    setErr(null);
    try {
      await stopWorker(worker.id);
      onRefresh();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setStopping(false);
    }
  }

  const pct = contextPct(worker.context.used, worker.context.size);
  const roleMeta = resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir });
  const attempts = workerAttempts(worker.id, snap.attempts).filter((a) => a.kind !== "review");
  const authorNums = workerNumbers(authorWorkers(snap.workers));
  const workerNum = authorNums.get(worker.id);
  const forkNum = worker.forkedFrom ? authorNums.get(worker.forkedFrom) : undefined;
  const forkChip = worker.forkedFrom ? (forkNum != null ? `W${forkNum}` : shortId(worker.forkedFrom)) : null;
  const forkText = worker.forkedFrom ? (forkNum != null ? `W${forkNum} · ${worker.forkedFrom}` : shortId(worker.forkedFrom)) : null;
  const stateToken = worker.state === "busy" || worker.state === "starting"
    ? "running"
    : worker.state === "dead"
      ? "failed"
      : worker.state === "idle"
        ? "ready"
        : "pending";

  return (
    <Shell
      role={`${roleDisplayName(roleMeta, t)} · W${workerNum ?? "?"}`}
      roleKind="worker"
      roleBadgeClass={roleMeta.badgeClass}
      badges={
        <>
          <StatusBadge token={stateToken} pulse={worker.state === "busy" || worker.state === "starting"}>
            {t(`worker.${worker.state}`)}
          </StatusBadge>
          <span className={`access-pill access-pill--${roleMeta.access}`}>{t(roleMeta.accessKey)}</span>
          {forkChip && (
            <span className="micro-badge micro-badge--fork" title={t("worker.forkedFrom")}>
              {t("worker.forkChip", { label: forkChip })}
            </span>
          )}
        </>
      }
      actions={
        STOPPABLE.has(worker.state) ? (
          <button type="button" className="btn btn--danger btn--xs" disabled={stopping} onClick={stop}>
            {stopping ? t("panel.stopping") : t("panel.stop")}
          </button>
        ) : undefined
      }
      title={agentModel(worker.agent, worker.model, worker.id)}
      subtitle={
        <code className="side-panel__id">
          {worker.id}
          {worker.effort ? ` · ${worker.effort}` : ""}
        </code>
      }
      onClose={onClose}
    >
      {err && (
        <div className="fail-banner fail-banner--other">
          <div className="fail-banner__detail">{err}</div>
        </div>
      )}

      <Group title={t("wb.workerEnv")}>
        <div className="sp-kv-grid">
          <StatItem label={t("panel.context")}>
            {pct == null ? (
              <span className="hint">{t("panel.contextEmpty")}</span>
            ) : (
              <div className="ctx ctx--lg" title={`${worker.context.used} / ${worker.context.size}`}>
                <span className="ctx__bar">
                  <span
                    className={`ctx__fill${pct >= 90 ? " ctx__fill--full" : pct >= 70 ? " ctx__fill--hot" : ""}`}
                    style={{ width: `${pct}%` }}
                  />
                </span>
                <span className="ctx__pct">{pct}%</span>
              </div>
            )}
          </StatItem>

          <StatItem label={`${t("panel.tokens")} / ${t("panel.cost")}`}>
            <Estimate text={fmtTokens(worker.totals.tokens)} estimated={worker.totals.estimated?.tokens} />
            {" · "}
            <Estimate text={fmtCost(worker.totals.cost)} estimated={worker.totals.estimated?.cost} />
            {" · "}
            {`${attempts.length} 次执行`}
          </StatItem>

          {forkText && (
            <StatItem full label={t("worker.forkedFrom")}>
              <span className="mono">{forkText}</span>
            </StatItem>
          )}

          <StatItem full label={t("panel.session")}>
            <div className="sp-code mono">{worker.sessionKey}</div>
          </StatItem>

          <StatItem full label={t("panel.cwd")}>
            <div className="sp-code mono">{worker.worktree || worker.cwd}</div>
          </StatItem>

          {(worker.fingerprint.files.length > 0 || worker.fingerprint.tags.length > 0) && (
            <StatItem full label={t("panel.fingerprint")}>
              {worker.fingerprint.tags.length > 0 && (
                <div className="micro-row" style={{ marginBottom: worker.fingerprint.files.length > 0 ? 6 : 0 }}>
                  {worker.fingerprint.tags.map((tag) => (
                    <span key={tag} className="micro-badge">{tag}</span>
                  ))}
                </div>
              )}
              {worker.fingerprint.files.length > 0 && (
                <ul className="sp-file-list">
                  {worker.fingerprint.files.map((f) => <li key={f} className="sp-file-chip mono">{f}</li>)}
                </ul>
              )}
            </StatItem>
          )}
        </div>
      </Group>

      <Group title={t("panel.relatedTasks")} count={attempts.length}>
        {attempts.length === 0 ? (
          <span className="hint">{t("panel.noRelated")}</span>
        ) : (
          <div className="sp-rel-list">
            {attempts.map((a) => {
              const task = snap.tasks.find((item) => item.id === a.taskId);
              const kind = attemptMark(a);
              const n = tasksInOrder(worker, attempts).indexOf(a.taskId);
              return (
                <button key={a.id} type="button" className="attempt rel-link" data-target={a.taskId} onClick={() => onSelect(a.taskId)}>
                  {n >= 0 && <span className="order-badge">{circNum(n + 1)}</span>}
                  <span>{task?.title || task?.key || a.taskId}</span>
                  {task?.key && task.title && <span className="mono gnode__dim">{task.key}</span>}
                  <span className={`attempt-mark attempt-mark--${kind}`}>{t(MARK_LABEL[kind])}</span>
                  <span className="attempt__id">
                    <Estimate text={`${fmtTokens(attemptTokens(a))} tok`} estimated={a.estimated?.tokens} />
                    {" · "}
                    <Estimate text={fmtCost(a.cost)} estimated={a.estimated?.cost} />
                    {" · "}
                    {fmtDuration(a.startedAt, a.endedAt)}
                  </span>
                </button>
              );
            })}
          </div>
        )}
      </Group>

      <MessageThread
        messages={workerMessages(worker.id, allMessages(snap), snap.attempts).filter((m) => !isReviewMessage(m, snap.attempts, snap.workers))}
        all={allMessages(snap)}
        snap={snap}
        onSelect={onSelect}
        {...talk}
      />

      <WorkerTranscript worker={worker} liveItems={liveItems} streamEpoch={streamEpoch} />
    </Shell>
  );
}

function ReviewerPanel({
  worker,
  snap,
  liveItems,
  streamEpoch,
  onSelect,
  onClose,
  onOpenReviews,
  talk,
}: {
  worker: Worker;
  snap: RunSnapshot;
  liveItems: Item[];
  streamEpoch: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  talk: TalkProps;
} & DetailLinks) {
  const { t } = useT();
  const num = reviewerNumbers(snap.workers).get(worker.id);
  const pct = contextPct(worker.context.used, worker.context.size);
  const roleMeta = resolveRoleMeta("reviewer", { readOnlyDir: worker.readOnlyDir });
  const tasks = reviewerTasksInOrder(worker, snap.attempts, snap.tasks).flatMap((id) => {
    const task = snap.tasks.find((task) => task.id === id);
    const board = task && reviewBoard(task, snap.attempts, snap.workers);
    return task && board ? [{ task, board }] : [];
  });
  const rounds = tasks.flatMap(({ task, board }) =>
    board.rounds
      .filter((round) => round.attempt.workerId === worker.id)
      .map((round) => ({ ...round, taskTitle: task.title })),
  );
  const messages = workerMessages(worker.id, allMessages(snap), snap.attempts).filter((m) =>
    isReviewMessage(m, snap.attempts, snap.workers),
  );
  const stateToken = worker.state === "busy" || worker.state === "starting"
    ? "review"
    : worker.state === "dead"
      ? "failed"
      : "pending";

  return (
    <Shell
      role={`${t("role.reviewer")} · R${num ?? "?"}`}
      roleKind="reviewer"
      badges={
        <>
          <StatusBadge token={stateToken} pulse={worker.state === "busy"}>
            {t(`worker.${worker.state}`)}
          </StatusBadge>
          <span className={`access-pill access-pill--${roleMeta.access}`}>{t(roleMeta.accessKey)}</span>
        </>
      }
      title={agentModel(worker.agent, worker.model, worker.agent)}
      subtitle={
        <code className="side-panel__id">
          {worker.id}
          {worker.effort ? ` · ${worker.effort}` : ""}
        </code>
      }
      onClose={onClose}
    >
      <Group title={t("wb.workerEnv")}>
        <div className="sp-kv-grid">
          <StatItem label={t("panel.context")}>
            {pct == null ? (
              <span className="hint">{t("panel.contextEmpty")}</span>
            ) : (
              <div className="ctx ctx--lg" title={`${worker.context.used} / ${worker.context.size}`}>
                <span className="ctx__bar">
                  <span
                    className={`ctx__fill${pct >= 90 ? " ctx__fill--full" : pct >= 70 ? " ctx__fill--hot" : ""}`}
                    style={{ width: `${pct}%` }}
                  />
                </span>
                <span className="ctx__pct">{pct}%</span>
              </div>
            )}
          </StatItem>

          <StatItem label={t("panel.reviewRounds")}>
            {rounds.length} 轮 · <Estimate text={`${fmtTokens(worker.totals.tokens)} tok`} estimated={worker.totals.estimated?.tokens} />
            {" · "}
            <Estimate text={fmtCost(worker.totals.cost)} estimated={worker.totals.estimated?.cost} />
          </StatItem>

          <StatItem full label={t("panel.session")}>
            <div className="sp-code mono">{worker.sessionKey}</div>
          </StatItem>

          <StatItem full label={t("wb.readOnlyPath")}>
            <div className="sp-code mono">{worker.readOnlyDir ?? t("panel.noSnapshotYet")}</div>
          </StatItem>
        </div>
      </Group>

      <Group title={t("panel.reviewerTasks")} count={tasks.length}>
        {tasks.length === 0 ? (
          <span className="hint">{t("panel.noReviewTasks")}</span>
        ) : (
          <div className="sp-rel-list">
            {tasks.map(({ task, board }) => (
              <article key={task.id} className="wb-subcard sp-review-task">
                <div className="sp-review-task__head">
                  <button type="button" className="attempt rel-link" data-target={task.id} onClick={() => onSelect(task.id)}>
                    <span>{task.title}</span>
                    <span className={`review-standing review-standing--${board.standing}`}>
                      {t(`review.standing.${board.standing}`)}
                    </span>
                  </button>
                  {onOpenReviews && (
                    <button type="button" className="link-btn sp-inline-link" onClick={() => onOpenReviews(task.id)}>
                      {t("wb.openInReviews")}
                    </button>
                  )}
                </div>
                {board.rounds
                  .filter((round) => round.attempt.workerId === worker.id)
                  .map((round) => (
                    <div key={round.attempt.id} className="sp-review-round">
                      <span className="micro-badge micro-badge--review">
                        Round {round.round}/{board.maxRounds} · {roundVerdict(round, board, t)}
                      </span>
                      {round.summary && <p className="review-round__summary">{round.summary}</p>}
                    </div>
                  ))}
              </article>
            ))}
          </div>
        )}
      </Group>

      <MessageThread
        title={t("panel.reviewMessages")}
        messages={messages}
        all={allMessages(snap)}
        snap={snap}
        onSelect={onSelect}
        {...talk}
      />

      <WorkerTranscript worker={worker} liveItems={liveItems} streamEpoch={streamEpoch} rounds={rounds} />
    </Shell>
  );
}

function CoordPanel({
  snap,
  onSelect,
  onClose,
  talk,
}: {
  snap: RunSnapshot;
  onSelect: (id: string) => void;
  onClose: () => void;
  talk: TalkProps;
}) {
  const { t } = useT();
  const run = snap.run;
  const est = snapshotEstimated(snap.attempts);
  const msgs = allMessages(snap);
  const budget = [
    run.budget?.tokens != null
      ? { kind: "tokens" as const, used: fmtTokens(snap.stats.tokens), limit: `${fmtTokens(run.budget.tokens)} tok`, estimated: est.tokens }
      : null,
    run.budget?.cost != null
      ? { kind: "cost" as const, used: fmtCost(snap.stats.cost), limit: fmtCost(run.budget.cost), estimated: est.cost }
      : null,
  ].filter((item) => item != null);
  const statusToken = run.status === "active"
    ? "running"
    : run.status === "done"
      ? "done"
      : run.status === "paused"
        ? "blocked"
        : "pending";

  return (
    <Shell
      role={t("role.coordinator")}
      roleKind="coord"
      badges={
        <>
          <StatusBadge token={statusToken} pulse={statusToken === "running"}>
            {t(`run.${run.status}`)}
          </StatusBadge>
          <span className="micro-badge">{coordinatorHostLabel(run)}</span>
          <span className="micro-badge">{`${t("panel.generation")} ${run.coordinator.generation}`}</span>
        </>
      }
      title={run.name || run.objective}
      subtitle={<code className="side-panel__id">{run.id}</code>}
      onClose={onClose}
    >
      <Group title={t("panel.coordOverview")} hint={t("panel.noReply")}>
        <Field label={t("panel.objective")}>{run.objective}</Field>
        <div className="sp-kv-grid">
          <StatItem label={t("panel.host")}>{coordinatorHostLabel(run)}</StatItem>
          <StatItem label={t("panel.concurrency")}>{run.maxConcurrency}</StatItem>
          <StatItem label={t("panel.spent")}>
            <Estimate text={`${fmtTokens(snap.stats.tokens)} tok`} estimated={est.tokens} />
            {" · "}
            <Estimate text={fmtCost(snap.stats.cost)} estimated={est.cost} />
          </StatItem>
          {budget.length > 0 && (
            <StatItem label={t("panel.budget")}>
              {budget.map((item, index) => (
                <span key={item.kind}>
                  {index > 0 && " · "}
                  <Estimate text={item.used} estimated={item.estimated} />
                  {` / ${item.limit}`}
                </span>
              ))}
            </StatItem>
          )}
          <StatItem full label={t("panel.sessionId")}>
            <div className="sp-code mono">{run.coordinator.hostSession}</div>
          </StatItem>
          <StatItem full label={t("panel.cwd")}>
            <div className="sp-code mono">{run.cwd}</div>
          </StatItem>
        </div>
      </Group>

      <MessageThread messages={msgs} all={msgs} snap={snap} onSelect={onSelect} {...talk} filters />

      <HostTranscript run={run} variant="panel" />
    </Shell>
  );
}

export function NodePanel({
  selectedId,
  snap,
  liveItems,
  streamEpoch,
  onSelect,
  onClose,
  onRefresh,
  playing,
  onPlay,
  onOpenReviews,
}: {
  selectedId: string;
  snap: RunSnapshot;
  liveItems: Item[];
  streamEpoch: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  onRefresh: () => void;
} & TalkProps & DetailLinks) {
  const talk = { playing, onPlay };
  if (selectedId === COORD_ID) {
    return <CoordPanel snap={snap} onSelect={onSelect} onClose={onClose} talk={talk} />;
  }
  const reviewerId = parseReviewerNodeId(selectedId);
  const legacyReviewerId = selectedId.startsWith("worker:") ? selectedId.slice("worker:".length) : null;
  const reviewer = snap.workers.find((w) => isReviewer(w) && (w.id === reviewerId || w.id === legacyReviewerId));
  if (reviewer) {
    return (
      <ReviewerPanel
        key={reviewer.id}
        worker={reviewer}
        snap={snap}
        liveItems={liveItems}
        streamEpoch={streamEpoch}
        onSelect={onSelect}
        onClose={onClose}
        onOpenReviews={onOpenReviews}
        talk={talk}
      />
    );
  }
  if (selectedId.startsWith("worker:")) {
    const id = selectedId.slice("worker:".length);
    const worker = snap.workers.find((w) => w.id === id);
    if (!worker) return null;
    return (
      <WorkerPanel
        key={worker.id}
        worker={worker}
        snap={snap}
        liveItems={liveItems}
        streamEpoch={streamEpoch}
        onSelect={onSelect}
        onClose={onClose}
        onRefresh={onRefresh}
        talk={talk}
      />
    );
  }
  const task = snap.tasks.find((item) => item.id === selectedId);
  if (!task) return null;
  return (
    <TaskPanel
      task={task}
      snap={snap}
      liveItems={liveItems}
      streamEpoch={streamEpoch}
      onSelect={onSelect}
      onClose={onClose}
      onRefresh={onRefresh}
      onOpenReviews={onOpenReviews}
    />
  );
}
