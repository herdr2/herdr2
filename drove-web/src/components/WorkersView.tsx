import { useEffect, useMemo, useState } from "react";
import { stopWorker } from "../api";
import {
  agentModel,
  attemptTokens,
  circNum,
  contextPct,
  fmtCost,
  fmtDuration,
  fmtTokens,
  shortId,
  workerNumbers,
} from "../format";
import { useT } from "../i18n";
import {
  COORD_ID,
  MARK_LABEL,
  allMessages,
  attemptMark,
  reviewerTasksInOrder,
  tasksInOrder,
  workerAttempts,
  workerMessages,
} from "../relate";
import {
  authorWorkers,
  isReviewer,
  parseReviewerNodeId,
  reviewBoard,
  reviewerNodeId,
  reviewerNumbers,
} from "../review";
import { resolveRoleMeta, roleDisplayName } from "../roles";
import { workerNodeId } from "./DagView";
import { clockTime, relTime } from "../time";
import type { Item, Profile, RunSnapshot, Worker, WorkerState } from "../types";
import {
  countWorkersByFilter,
  countWorkersByRoleFilter,
  groupWorkersByCategory,
  orderedWorkers,
  workerMatchesFilter,
  workerMatchesRoleFilter,
  type WorkerFilter,
  type WorkerRoleFilter,
} from "../workbench";
import { Estimate } from "./Estimate";
import { MessageThread } from "./MessageThread";
import { VerifyLogViewer } from "./VerifyLogViewer";
import {
  StatusBadge,
  Workbench,
  WorkbenchEmpty,
  WorkbenchFilterBar,
  WorkbenchHeader,
  WorkbenchSubtabs,
} from "./WorkbenchShell";
import { WorkerTranscript, type StreamRound } from "./WorkerTranscript";

type WorkerSubtab = "stream" | "attempts" | "messages";
const STOPPABLE = new Set<WorkerState>(["starting", "idle", "busy"]);

function workerStateToken(state: WorkerState, reviewer: boolean): string {
  if (state === "busy" || state === "starting") return reviewer ? "review" : "running";
  if (state === "dead") return "failed";
  if (state === "idle") return "ready";
  return "pending";
}

export function WorkersView({
  snap,
  profile = null,
  selectedWorkerId,
  onSelectWorker,
  onJumpToDag,
  onOpenTask,
  onOpenCoordinator,
  onOpenReviews,
  onRefresh,
  liveItems,
  streamEpoch,
}: {
  snap: RunSnapshot | null;
  profile?: Profile | null;
  selectedWorkerId: string | null;
  onSelectWorker: (id: string | null) => void;
  onJumpToDag: (id: string) => void;
  onOpenTask: (taskId: string) => void;
  onOpenCoordinator: () => void;
  onOpenReviews: (taskId: string) => void;
  onRefresh: () => void;
  liveItems: Item[];
  streamEpoch: number;
}) {
  const { t } = useT();
  const [filter, setFilter] = useState<WorkerFilter>("all");
  const [roleFilter, setRoleFilter] = useState<WorkerRoleFilter>("all");
  const [subtab, setSubtab] = useState<WorkerSubtab>("messages");
  const [stopping, setStopping] = useState(false);
  const [stopErr, setStopErr] = useState<string | null>(null);

  const ordered = useMemo(() => (snap ? orderedWorkers(snap.workers) : []), [snap]);
  const counts = useMemo(() => countWorkersByFilter(ordered), [ordered]);
  const roleCounts = useMemo(() => countWorkersByRoleFilter(ordered), [ordered]);
  const filtered = useMemo(
    () => ordered.filter((w) => workerMatchesFilter(w, filter) && workerMatchesRoleFilter(w, roleFilter)),
    [ordered, filter, roleFilter],
  );
  const groupedFiltered = useMemo(() => groupWorkersByCategory(filtered, profile), [filtered, profile]);

  const authorNums = useMemo(() => workerNumbers(authorWorkers(ordered)), [ordered]);
  const revNums = useMemo(() => reviewerNumbers(ordered), [ordered]);

  useEffect(() => {
    if (!snap) return;
    const valid = selectedWorkerId != null && snap.workers.some((w) => w.id === selectedWorkerId);
    if (valid) return;
    const next = groupedFiltered[0]?.workers[0]?.id ?? filtered[0]?.id ?? ordered[0]?.id ?? null;
    if (next !== selectedWorkerId) onSelectWorker(next);
  }, [snap, selectedWorkerId, groupedFiltered, filtered, ordered, onSelectWorker]);

  useEffect(() => {
    setStopErr(null);
  }, [selectedWorkerId]);

  const worker: Worker | null = useMemo(
    () => (snap && selectedWorkerId ? snap.workers.find((w) => w.id === selectedWorkerId) ?? null : null),
    [snap, selectedWorkerId],
  );

  if (!snap) {
    return (
      <Workbench
        role="worker"
        header={<WorkbenchHeader badge={<span className="role-badge role-badge--worker">{t("role.worker")}</span>} title={t("wb.workersTitle")} />}
        master={<div className="wb-list-empty">{t("empty.pickTitle")}</div>}
        detail={<WorkbenchEmpty title={t("empty.pickTitle")} body={t("empty.pickBody")} />}
      />
    );
  }

  function routeEntity(targetId: string) {
    if (targetId === COORD_ID) {
      onOpenCoordinator();
      return;
    }
    if (targetId.startsWith("worker:")) {
      onSelectWorker(targetId.slice("worker:".length));
      return;
    }
    const revId = parseReviewerNodeId(targetId);
    if (revId) {
      onSelectWorker(revId);
      return;
    }
    onOpenTask(targetId);
  }

  async function handleStop(id: string) {
    setStopping(true);
    setStopErr(null);
    try {
      await stopWorker(id);
      onRefresh();
    } catch (e) {
      setStopErr((e as Error).message);
    } finally {
      setStopping(false);
    }
  }

  const filterOptions: { value: WorkerFilter; label: string; count: number }[] = [
    { value: "all", label: t("review.filterAll"), count: counts.all },
    { value: "active", label: t("wb.workerActive"), count: counts.active },
    { value: "ended", label: t("wb.workerEnded"), count: counts.ended },
    { value: "author", label: t("wb.workerAuthors"), count: counts.author },
    { value: "reviewer", label: t("wb.workerReviewers"), count: counts.reviewer },
  ];

  const roleFilterOptions: { value: WorkerRoleFilter; label: string; count: number }[] = [
    { value: "all", label: t("wb.workerRoleFilterAll"), count: roleCounts.all },
    { value: "coder", label: t("role.coder"), count: roleCounts.coder },
    { value: "tester", label: t("role.tester"), count: roleCounts.tester },
    { value: "writer", label: t("role.writer"), count: roleCounts.writer },
    { value: "reviewer", label: t("role.reviewer"), count: roleCounts.reviewer },
    { value: "researcher", label: t("role.researcher"), count: roleCounts.researcher },
    ...(roleCounts.custom > 0 ? [{ value: "custom" as const, label: t("role.custom"), count: roleCounts.custom }] : []),
  ];

  const renderWorkerItem = (item: Worker) => {
    const reviewer = isReviewer(item);
    const num = reviewer ? revNums.get(item.id) : authorNums.get(item.id);
    const code = `${reviewer ? "R" : "W"}${num ?? "?"}`;
    const roleMeta = resolveRoleMeta(item.role, { readOnlyDir: item.readOnlyDir, profile });
    const roleLabel = roleDisplayName(roleMeta, t);
    const pct = contextPct(item.context.used, item.context.size);
    const taskCount = reviewer
      ? reviewerTasksInOrder(item, snap.attempts, snap.tasks).length
      : tasksInOrder(item, workerAttempts(item.id, snap.attempts).filter((a) => a.kind !== "review")).length;
    return (
      <button
        key={item.id}
        type="button"
        className={`wb-item${worker?.id === item.id ? " wb-item--active" : ""}`}
        onClick={() => onSelectWorker(item.id)}
      >
        <div className="wb-item__top">
          <span className="wb-item__title">
            <span className="worker-num">{code}</span>{" "}
            {agentModel(item.agent, item.model, item.agent)}
          </span>
          <StatusBadge
            token={workerStateToken(item.state, reviewer)}
            pulse={item.state === "busy" || item.state === "starting"}
          >
            {t(`worker.${item.state}`)}
          </StatusBadge>
        </div>
        <div className="wb-item__meta">
          <span className={`role-badge ${roleMeta.badgeClass}`}>{roleLabel}</span>
          <span className={`access-pill access-pill--${roleMeta.access}`}>{t(roleMeta.accessKey)}</span>
          <span className="hint">{taskCount} {t("tb.tasks")}</span>
          <span className="hint">{fmtTokens(item.totals.tokens)} · {fmtCost(item.totals.cost)}</span>
        </div>
        {pct != null && (
          <div className="ctx" title={`${item.context.used} / ${item.context.size}`}>
            <span className="ctx__bar">
              <span
                className={`ctx__fill${pct >= 90 ? " ctx__fill--full" : pct >= 70 ? " ctx__fill--hot" : ""}`}
                style={{ width: `${pct}%` }}
              />
            </span>
            <span className="ctx__pct">{pct}%</span>
          </div>
        )}
      </button>
    );
  };

  const master = (
    <div className="wb-side-stack">
      <WorkbenchFilterBar value={filter} options={filterOptions} onChange={setFilter} ariaLabel={t("wb.workersTitle")} />
      <WorkbenchFilterBar value={roleFilter} options={roleFilterOptions} onChange={setRoleFilter} ariaLabel={t("assign.role")} />
      <div className="wb-list">
        {filtered.length === 0 ? (
          <div className="wb-list-empty">{t("wb.noWorkersFiltered")}</div>
        ) : roleFilter === "all" ? (
          groupedFiltered.map((section) => (
            <div key={section.key} className="wb-worker-group">
              <div className="wb-worker-group__head">
                <span className={`role-badge ${section.roleMeta.badgeClass}`}>{roleDisplayName(section.roleMeta, t)}</span>
                <span className="wb-worker-group__count">{section.workers.length}</span>
              </div>
              {section.workers.map(renderWorkerItem)}
            </div>
          ))
        ) : (
          filtered.map(renderWorkerItem)
        )}
      </div>
    </div>
  );

  if (!worker) {
    return (
      <Workbench
        role="worker"
        header={<WorkbenchHeader badge={<span className="role-badge role-badge--worker">{t("role.worker")}</span>} title={t("wb.workersTitle")} />}
        master={master}
        detail={<WorkbenchEmpty title={t("wb.noWorkersFiltered")} />}
      />
    );
  }

  const reviewer = isReviewer(worker);
  const num = reviewer ? revNums.get(worker.id) : authorNums.get(worker.id);
  const code = `${reviewer ? "R" : "W"}${num ?? "?"}`;
  const workerRoleMeta = resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir, profile });
  const workerRoleLabel = roleDisplayName(workerRoleMeta, t);
  const pct = contextPct(worker.context.used, worker.context.size);
  const attempts = workerAttempts(worker.id, snap.attempts);
  const forkNum = worker.forkedFrom ? authorNums.get(worker.forkedFrom) : undefined;
  const forkTarget = worker.forkedFrom ? snap.workers.find((w) => w.id === worker.forkedFrom) : undefined;
  const allMsgs = allMessages(snap);
  const wMsgs = workerMessages(worker.id, allMsgs, snap.attempts);
  const orderedTaskIds = reviewer ? reviewerTasksInOrder(worker, snap.attempts, snap.tasks) : tasksInOrder(worker, attempts.filter((a) => a.kind !== "review"));
  const reviewerRounds: StreamRound[] | undefined = reviewer
    ? orderedTaskIds.flatMap((tid) => {
        const task = snap.tasks.find((item) => item.id === tid);
        const board = task && reviewBoard(task, snap.attempts, snap.workers);
        if (!task || !board) return [];
        return board.rounds.filter((round) => round.attempt.workerId === worker.id).map((round) => ({ ...round, taskTitle: task.title }));
      })
    : undefined;

  const detail = (
    <div className="wb-detail-stack">
      <div className="wb-card wb-detail-head">
        <div>
          <div className="wb-chips">
            <span className={`role-badge ${workerRoleMeta.badgeClass}`}>
              {workerRoleLabel} · {code}
            </span>
            <span className={`access-pill access-pill--${workerRoleMeta.access}`}>{t(workerRoleMeta.accessKey)}</span>
            {workerRoleMeta.access === "readonly" && !reviewer && <span className="micro-badge micro-badge--review">{t("review.readOnly")}</span>}
            {reviewer && <span className="micro-badge micro-badge--review">{t("review.readOnly")}</span>}
            <StatusBadge
              token={workerStateToken(worker.state, reviewer)}
              pulse={worker.state === "busy" || worker.state === "starting"}
            >
              {t(`worker.${worker.state}`)}
            </StatusBadge>
          </div>
          <h3 className="wb-detail-head__title">
            {code} · {agentModel(worker.agent, worker.model, worker.agent)}
          </h3>
          <div className="mono hint">{worker.id}</div>
        </div>
        <div className="wb-actions">
          {STOPPABLE.has(worker.state) && (
            <button type="button" className="btn btn--danger btn--sm" disabled={stopping} onClick={() => void handleStop(worker.id)}>
              {stopping ? t("panel.stopping") : t("panel.stop")}
            </button>
          )}
          <button
            type="button"
            className="btn btn--ghost btn--sm"
            onClick={() => onJumpToDag(reviewer ? reviewerNodeId(worker.id) : workerNodeId(worker.id))}
          >
            {t("review.jumpDag")}
          </button>
        </div>
      </div>

      {stopErr && <p className="hint" style={{ color: "var(--danger)" }}>{stopErr}</p>}

      <div className="wb-two-col">
        <div className="wb-card">
          <h4 className="wb-card__title">{t("wb.workerEnv")}</h4>
          <div className="wb-kv-grid">
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.agent")}</span>
              <div className="panel-field__val">
                {agentModel(worker.agent, worker.model, t("task.unassigned"))}
                {worker.effort ? ` · ${worker.effort}` : ""}
              </div>
            </div>
            {worker.pid != null && (
              <div className="panel-field">
                <span className="panel-field__key">PID</span>
                <div className="panel-field__val mono">{worker.pid}</div>
              </div>
            )}
            {worker.protocolGen != null && (
              <div className="panel-field">
                <span className="panel-field__key">Protocol Gen</span>
                <div className="panel-field__val mono">{worker.protocolGen}</div>
              </div>
            )}
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.tokens")}</span>
              <div className="panel-field__val">
                <Estimate text={`${fmtTokens(worker.totals.tokens)} tok`} estimated={worker.totals.estimated?.tokens} />
                {" · "}
                <Estimate text={fmtCost(worker.totals.cost)} estimated={worker.totals.estimated?.cost} />
                {` · ${attempts.length} ${t("panel.attempts")}`}
              </div>
            </div>
          </div>

          {worker.forkedFrom && (
            <div className="panel-field">
              <span className="panel-field__key">{t("worker.forkedFrom")}</span>
              <div className="panel-field__val">
                {forkTarget ? (
                  <button type="button" className="link-btn" onClick={() => onSelectWorker(forkTarget.id)}>
                    {forkNum != null ? `W${forkNum} · ` : ""}{forkTarget.id}
                  </button>
                ) : (
                  <span className="mono">{shortId(worker.forkedFrom)}</span>
                )}
              </div>
            </div>
          )}

          <div className="panel-field">
            <span className="panel-field__key">{t("panel.context")}</span>
            <div className="panel-field__val">
              {pct == null ? (
                t("panel.contextEmpty")
              ) : (
                <div className="ctx ctx--lg" title={`${worker.context.used} / ${worker.context.size}`}>
                  <span className="ctx__bar">
                    <span className={`ctx__fill${pct >= 90 ? " ctx__fill--full" : pct >= 70 ? " ctx__fill--hot" : ""}`} style={{ width: `${pct}%` }} />
                  </span>
                  <span className="ctx__pct">{pct}%</span>
                </div>
              )}
            </div>
          </div>

          <div className="panel-field">
            <span className="panel-field__key">{t("panel.session")}</span>
            <div className="panel-field__val mono">{worker.sessionKey}</div>
          </div>
          <div className="panel-field">
            <span className="panel-field__key">{reviewer || worker.readOnlyDir ? t("wb.readOnlyPath") : t("panel.cwd")}</span>
            <div className="panel-field__val mono">{worker.readOnlyDir ? worker.readOnlyDir : (worker.worktree || worker.cwd)}</div>
          </div>
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.workerRoleScope")}</span>
            <div className="panel-field__val">
              {workerRoleMeta.access === "readonly"
                ? t("wb.workerRoleReadOnly")
                : workerRoleMeta.access === "scoped_write"
                  ? <span className="mono">{workerRoleMeta.allowedPaths.join(", ") || "—"}</span>
                  : t("wb.workerRoleUnrestricted")}
            </div>
          </div>
        </div>

        <div className="wb-card">
          <h4 className="wb-card__title">{t("panel.relatedTasks")} ({orderedTaskIds.length})</h4>
          {orderedTaskIds.length === 0 ? (
            <p className="hint">{t("panel.noRelated")}</p>
          ) : (
            <div className="wb-list-grid">
              {orderedTaskIds.map((tid, idx) => {
                const task = snap.tasks.find((item) => item.id === tid);
                const tAttempts = attempts.filter((a) => a.taskId === tid);
                const last = tAttempts[tAttempts.length - 1];
                const mark = last ? attemptMark(last) : null;
                return (
                  <div key={tid} className="wb-rel-row">
                    <button type="button" className="attempt rel-link" onClick={() => onOpenTask(tid)}>
                      <span className="order-badge">{circNum(idx + 1)}</span>
                      <span>{task?.title || task?.key || tid}</span>
                      {mark && <span className={`attempt-mark attempt-mark--${mark}`}>{t(MARK_LABEL[mark])}</span>}
                      {last && <span className="attempt__id">{fmtDuration(last.startedAt, last.endedAt)}</span>}
                    </button>
                    {reviewer && (
                      <button type="button" className="btn btn--ghost btn--sm" onClick={() => onOpenReviews(tid)}>
                        {t("review.title")}
                      </button>
                    )}
                  </div>
                );
              })}
            </div>
          )}

          {(worker.fingerprint.tags.length > 0 || worker.fingerprint.files.length > 0) && (
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.fingerprint")}</span>
              {worker.fingerprint.tags.length > 0 && (
                <div className="wb-chips">
                  {worker.fingerprint.tags.map((tag) => (
                    <span key={tag} className="micro-badge">{tag}</span>
                  ))}
                </div>
              )}
              {worker.fingerprint.files.length > 0 && (
                <pre className="stream__pre">{worker.fingerprint.files.join("\n")}</pre>
              )}
            </div>
          )}
        </div>
      </div>

      <div className="wb-card">
        <WorkbenchSubtabs
          value={subtab}
          onChange={setSubtab}
          ariaLabel={code}
          options={[
            { value: "messages", label: t("msg.title"), count: wMsgs.length },
            { value: "attempts", label: t("panel.attempts"), count: attempts.length },
            { value: "stream", label: t("panel.stream") },
          ]}
        />

        {subtab === "stream" && (
          <WorkerTranscript
            worker={worker}
            liveItems={liveItems}
            streamEpoch={streamEpoch}
            rounds={reviewerRounds}
            attempts={snap.attempts}
            tasks={snap.tasks}
            variant="workbench"
            bare
          />
        )}

        {subtab === "attempts" && (
          attempts.length === 0 ? (
            <p className="hint">{t("panel.noAttempts")}</p>
          ) : (
            <div className="wb-list-grid">
              {attempts.map((a) => {
                  const task = snap.tasks.find((item) => item.id === a.taskId);
                  const kind = attemptMark(a);
                  return (
                    <article key={a.id} className="wb-subcard">
                      <div className="wb-subcard__head">
                        <span className={`attempt-mark attempt-mark--${kind}`}>{t(MARK_LABEL[kind])}</span>
                        <button type="button" className="link-btn" onClick={() => onOpenTask(a.taskId)}>
                          {task?.title ?? a.taskId}
                        </button>
                        <span className="mono hint">{a.id}</span>
                        <span className="hint" title={clockTime(a.startedAt)}>
                          {relTime(a.startedAt)} · {fmtDuration(a.startedAt, a.endedAt)}
                        </span>
                      </div>
                      <p className="hint">{a.routeReason}</p>
                      <div className="wb-actions">
                        <Estimate text={`${fmtTokens(attemptTokens(a))} tok`} estimated={a.estimated?.tokens} />
                        <span>·</span>
                        <Estimate text={fmtCost(a.cost)} estimated={a.estimated?.cost} />
                      </div>
                      {a.failure && (
                        <div className="fail-banner fail-banner--other">
                          <div className="fail-banner__detail">{t(`fail.${a.failure.kind}`)} — {a.failure.message}</div>
                        </div>
                      )}
                      {a.verifyLogArtifactId && (
                        <VerifyLogViewer runId={snap.run.id} artifactId={a.verifyLogArtifactId} />
                      )}
                    </article>
                  );
                })}
            </div>
          )
        )}

        {subtab === "messages" && (
          <MessageThread
            messages={wMsgs}
            all={allMsgs}
            snap={snap}
            filters
            bare
            onSelect={routeEntity}
          />
        )}
      </div>
    </div>
  );

  return (
    <Workbench
      role="worker"
      header={
        <WorkbenchHeader
          badge={<span className="role-badge role-badge--worker">{t("role.worker")}</span>}
          title={t("wb.workersTitle")}
          subtitle={`${ordered.length} ${t("tb.workers")} · ${counts.active} ${t("wb.workerActive")} · ${counts.reviewer} ${t("tb.reviewers")}`}
        />
      }
      master={master}
      detail={detail}
    />
  );
}
