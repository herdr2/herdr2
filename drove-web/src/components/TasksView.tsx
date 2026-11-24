import { useEffect, useMemo, useState } from "react";
import { branchCaption } from "../branch";
import {
  fmtCost,
  fmtTokens,
  jsonPreview,
  taskMicroBadges,
  taskUsage,
  workerNumbers,
} from "../format";
import { useT } from "../i18n";
import { taskAttempts } from "../relate";
import { authorWorkers, reviewBoard, reviewCircuitView, reviewerNumbers, taskRequiresReview } from "../review";
import type { Item, RunSnapshot, Task } from "../types";
import {
  countTasksByFilter,
  deriveTaskStatusBadge,
  taskDependents,
  taskMatchesFilter,
  templateChildren,
  type TaskFilter,
} from "../workbench";
import { ReviewRequirement } from "./ReviewRequirement";
import { TaskAiDiagnosisPanel, TaskSummaryView } from "./TaskAiPanel";
import { TaskAttemptCard } from "./TaskAttemptCard";
import {
  StatusBadge,
  Workbench,
  WorkbenchEmpty,
  WorkbenchFilterBar,
  WorkbenchHeader,
} from "./WorkbenchShell";

export function TasksView({
  snap,
  liveItems,
  streamEpoch,
  selectedTaskId,
  onSelectTask,
  onJumpToDag,
  onOpenWorker,
  onOpenReviews,
  onRefresh,
}: {
  snap: RunSnapshot | null;
  liveItems: Item[];
  streamEpoch: number;
  selectedTaskId: string | null;
  onSelectTask: (id: string | null) => void;
  onJumpToDag: (id: string) => void;
  onOpenWorker: (workerId: string) => void;
  onOpenReviews: (taskId: string) => void;
  onRefresh: () => void;
}) {
  const { t } = useT();
  const [filter, setFilter] = useState<TaskFilter>("all");

  const counts = useMemo(
    () => (snap ? countTasksByFilter(snap.tasks) : { all: 0, running: 0, queued: 0, done: 0, attention: 0 }),
    [snap],
  );
  const filtered = useMemo(
    () => (snap ? snap.tasks.filter((task) => taskMatchesFilter(task, filter)) : []),
    [snap, filter],
  );

  useEffect(() => {
    if (!snap) return;
    const valid = selectedTaskId != null && snap.tasks.some((task) => task.id === selectedTaskId);
    if (valid) return;
    const next = filtered[0]?.id ?? snap.tasks[0]?.id ?? null;
    if (next !== selectedTaskId) onSelectTask(next);
  }, [snap, selectedTaskId, filtered, onSelectTask]);

  const task: Task | null = useMemo(
    () => (snap && selectedTaskId ? snap.tasks.find((item) => item.id === selectedTaskId) ?? null : null),
    [snap, selectedTaskId],
  );

  const workerNums = useMemo(
    () => (snap ? workerNumbers(authorWorkers(snap.workers)) : new Map<string, number>()),
    [snap],
  );
  const reviewerNums = useMemo(
    () => (snap ? reviewerNumbers(snap.workers) : new Map<string, number>()),
    [snap],
  );

  const attempts = useMemo(
    () => (snap && task ? taskAttempts(task.id, snap.attempts) : []),
    [snap, task],
  );

  if (!snap) {
    return (
      <Workbench
        role="task"
        header={<WorkbenchHeader badge={<span className="role-badge role-badge--task">{t("role.task")}</span>} title={t("wb.tasksTitle")} />}
        master={<div className="wb-list-empty">{t("empty.pickTitle")}</div>}
        detail={<WorkbenchEmpty title={t("empty.pickTitle")} body={t("empty.pickBody")} />}
      />
    );
  }

  const filterOptions: { value: TaskFilter; label: string; count: number }[] = [
    { value: "all", label: t("review.filterAll"), count: counts.all },
    { value: "running", label: t("wb.filterRunning"), count: counts.running },
    { value: "queued", label: t("wb.filterQueued"), count: counts.queued },
    { value: "done", label: t("wb.filterDone"), count: counts.done },
    { value: "attention", label: t("wb.filterAttention"), count: counts.attention },
  ];

  const master = (
    <div className="wb-side-stack">
      <WorkbenchFilterBar value={filter} options={filterOptions} onChange={setFilter} ariaLabel={t("wb.tasksTitle")} />
      <div className="wb-list">
        {filtered.length === 0 ? (
          <div className="wb-list-empty">{t("wb.noTasksFiltered")}</div>
        ) : (
          filtered.map((item) => {
            const tAttempts = taskAttempts(item.id, snap.attempts);
            const badge = deriveTaskStatusBadge(item, tAttempts, snap.tasks, t);
            const latestAuthor = [...tAttempts].reverse().find((a) => a.kind !== "review");
            const wNum = latestAuthor ? workerNums.get(latestAuthor.workerId) : undefined;
            const usage = taskUsage(item.id, tAttempts);
            const micro = taskMicroBadges(item, t);
            const itemCircuit = reviewCircuitView(item, t);
            return (
              <button
                key={item.id}
                type="button"
                className={`wb-item${task?.id === item.id ? " wb-item--active" : ""}`}
                onClick={() => onSelectTask(item.id)}
              >
                <div className="wb-item__top">
                  <span className="wb-item__title">{item.title}</span>
                  <StatusBadge token={badge.token} pulse={badge.pulse}>{badge.label}</StatusBadge>
                </div>
                <div className="wb-item__sub mono">
                  {item.key}
                  {item.phase ? ` · ${item.phase}` : ""}
                </div>
                <div className="wb-item__meta">
                  {item.kind === "exec" ? (
                    <span className="micro-badge">EXEC</span>
                  ) : wNum != null ? (
                    <span className="worker-num">W{wNum}</span>
                  ) : null}
                  {taskRequiresReview(item, tAttempts) && (
                    <ReviewRequirement required />
                  )}
                  {itemCircuit && (
                    <span className={`circuit-pill circuit-pill--${itemCircuit.tone}`} title={itemCircuit.description}>
                      {itemCircuit.shortLabel}
                    </span>
                  )}
                  {micro.map((m) => (
                    <span key={m.kind} className={`micro-badge micro-badge--${m.kind}`} title={m.title}>{m.text}</span>
                  ))}
                  {usage.tokens > 0 && (
                    <span className="hint">{fmtTokens(usage.tokens)} · {fmtCost(usage.cost)}</span>
                  )}
                </div>
              </button>
            );
          })
        )}
      </div>
    </div>
  );

  if (!task) {
    return (
      <Workbench
        role="task"
        header={<WorkbenchHeader badge={<span className="role-badge role-badge--task">{t("role.task")}</span>} title={t("wb.tasksTitle")} />}
        master={master}
        detail={<WorkbenchEmpty title={t("wb.noTasksFiltered")} />}
      />
    );
  }

  const badge = deriveTaskStatusBadge(task, attempts, snap.tasks, t);
  const micro = taskMicroBadges(task, t);
  const dependents = taskDependents(task.id, snap.tasks);
  const children = templateChildren(task, snap.tasks);
  const parentTemplate = task.expandedFrom ? snap.tasks.find((item) => item.id === task.expandedFrom) : undefined;
  const branchText = branchCaption(task);
  const board = reviewBoard(task, snap.attempts, snap.workers);
  const circuit = reviewCircuitView(task, t);
  const needsReview = taskRequiresReview(task, attempts);

  const detail = (
    <div className="wb-detail-stack">
      <div className="wb-card">
        <div className="wb-card__head">
          <div>
            <h4 className="wb-card__title">{t("panel.spec")} & {t("panel.result")}</h4>
            <div className="wb-chips" style={{ marginTop: 6 }}>
              <span className="mono hint">{task.title} · {task.key} · {task.id}</span>
              <StatusBadge token={badge.token} pulse={badge.pulse}>{badge.label}</StatusBadge>
              <ReviewRequirement required={needsReview} />
              {circuit && (
                <span className={`circuit-pill circuit-pill--${circuit.tone}`} title={circuit.description}>
                  {circuit.shortLabel}
                </span>
              )}
              {task.kind === "exec" && <span className="micro-badge">EXEC</span>}
              {task.execPhase && <span className="micro-badge">{t(`phase.${task.execPhase}`)}</span>}
              {micro.map((m) => (
                <span key={m.kind} className={`micro-badge micro-badge--${m.kind}`} title={m.title}>{m.text}</span>
              ))}
            </div>
          </div>
          <div className="wb-actions">
            {needsReview && (
              <button type="button" className="btn btn--primary btn--sm" onClick={() => onOpenReviews(task.id)}>
                {t("wb.openInReviews")}
              </button>
            )}
            <button type="button" className="btn btn--ghost btn--sm" onClick={() => onJumpToDag(task.id)}>
              {t("review.jumpDag")}
            </button>
          </div>
        </div>
        <div className="wb-two-col">
          <div>
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.target")}</span>
              <div className="panel-field__val">{task.spec.target}</div>
            </div>
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.change")}</span>
              <div className="panel-field__val">{task.spec.change}</div>
            </div>
            {task.spec.constraints && (
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.constraints")}</span>
                <div className="panel-field__val">{task.spec.constraints}</div>
              </div>
            )}
            {task.spec.ownership && (
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.ownership")}</span>
                <div className="panel-field__val">{task.spec.ownership}</div>
              </div>
            )}
            <div className="panel-field">
              <span className="panel-field__key">{t("panel.acceptance")}</span>
              <div className="panel-field__val">{task.spec.acceptance}</div>
            </div>
            {task.spec.verify && (
              <div className="panel-field">
                <span className="panel-field__key">{t("wb.taskVerify")}</span>
                <pre className="stream__pre">
                  {typeof task.spec.verify === "string" ? task.spec.verify : jsonPreview(task.spec.verify)}
                </pre>
              </div>
            )}
            {task.outputSchema && (
              <div className="panel-field">
                <span className="panel-field__key">outputSchema</span>
                <pre className="stream__pre">{jsonPreview(task.outputSchema)}</pre>
              </div>
            )}
          </div>

          <div>
            {task.cachedFrom && (
              <div className="wb-subcard">
                <span className="micro-badge">{t("wb.cachedFrom")}</span>
                <p className="hint">{t("wb.cachedHitBody")} <span className="mono">{task.cachedFrom}</span></p>
              </div>
            )}
            {task.candidateResult && (
              <div className="wb-subcard">
                <span className="micro-badge micro-badge--review">{t("wb.candidateResult")}</span>
                <TaskSummaryView summary={task.candidateResult.summary} compactSummary={task.candidateResult.compactSummary} />
                {task.candidateResult.filesModified && task.candidateResult.filesModified.length > 0 && (
                  <ul className="agents__models">{task.candidateResult.filesModified.map((f) => <li key={f} className="mono">{f}</li>)}</ul>
                )}
              </div>
            )}
            {task.result ? (
              <div className="wb-subcard">
                <span className="micro-badge">{t("panel.result")}</span>
                <TaskSummaryView summary={task.result.summary} compactSummary={task.result.compactSummary} />
                {task.result.filesModified && task.result.filesModified.length > 0 && (
                  <ul className="agents__models">{task.result.filesModified.map((f) => <li key={f} className="mono">{f}</li>)}</ul>
                )}
                {task.result.blockers && task.result.blockers.length > 0 && (
                  <div className="panel-field">
                    <span className="panel-field__key">Blockers</span>
                    <ul className="agents__models">{task.result.blockers.map((b, i) => <li key={i}>{b}</li>)}</ul>
                  </div>
                )}
                {task.result.data != null && (
                  <details>
                    <summary>{t("panel.data")}</summary>
                    <pre className="stream__pre">{jsonPreview(task.result.data)}</pre>
                  </details>
                )}
              </div>
            ) : !task.candidateResult ? (
              <p className="hint">{t("wb.noResultYet")}</p>
            ) : null}
          </div>
        </div>
      </div>

      {badge.cause && (
        <div className={`fail-banner fail-banner--${badge.cause.bucket}`}>
          <div className="fail-banner__class">{t(`fail.bucket.${badge.cause.bucket}`)}</div>
          <div className="fail-banner__detail">
            {t(`fail.${badge.cause.kind}`)}
            {badge.cause.message ? ` — ${badge.cause.message}` : ""}
          </div>
        </div>
      )}

      {circuit && (circuit.tone === "handoff" || circuit.tone === "post_handoff" || circuit.tone === "awaiting") && (
        <div className={`circuit-banner circuit-banner--${circuit.tone}`}>
          <div className="circuit-banner__head">
            <span className="circuit-banner__title">{t("review.circuit.title")} · {circuit.label}</span>
            <span className="circuit-banner__meta mono">
              R{circuit.rounds}/{circuit.maxRounds}
              {circuit.overrides > 0 ? ` · ${t("review.circuit.overrides", { n: circuit.overrides })}` : ""}
            </span>
          </div>
          <p className="circuit-banner__desc">{circuit.description}</p>
          {circuit.history.length > 0 && (
            <ul className="circuit-banner__history">
              {circuit.history.map((rec) => (
                <li key={`${rec.round}:${rec.at}`}>
                  <strong className="mono">R{rec.round}{rec.postHandoff ? "*" : ""}</strong>
                  {" · "}
                  <span>{rec.summary}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      <TaskAiDiagnosisPanel task={task} attempts={attempts} onRefresh={onRefresh} />

      <div className="wb-card">
        <h4 className="wb-card__title">{t("wb.topologyBranchesRules")}</h4>
        <div className="wb-kv-grid">
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.isolation")}</span>
            <div className="panel-field__val mono">{task.isolation}</div>
          </div>
          {task.triggerRule && (
            <div className="panel-field">
              <span className="panel-field__key">{t("wb.triggerRule")}</span>
              <div className="panel-field__val mono">{task.triggerRule}</div>
            </div>
          )}
          {task.timeoutSec != null && (
            <div className="panel-field">
              <span className="panel-field__key">{t("wb.timeout")}</span>
              <div className="panel-field__val mono">{task.timeoutSec}s</div>
            </div>
          )}
          {task.cachedFrom && (
            <div className="panel-field">
              <span className="panel-field__key">{t("wb.cachedFrom")}</span>
              <div className="panel-field__val mono">{task.cachedFrom}</div>
            </div>
          )}
        </div>

        {(branchText || task.when) && (
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.whenCondition")}</span>
            <div className="panel-field__val">
              {branchText && <span className="branch-tag branch-tag--open">{branchText}</span>}
              {task.when && <pre className="stream__pre">{jsonPreview(task.when)}</pre>}
            </div>
          </div>
        )}

        {task.foreachFrom && (
          <div className="panel-field">
            <span className="panel-field__key">foreachFrom</span>
            <pre className="stream__pre">{jsonPreview(task.foreachFrom)}</pre>
          </div>
        )}

        {task.deps.length > 0 && (
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.upstreamDeps")} ({task.deps.length})</span>
            <div className="wb-chip-actions">
              {task.deps.map((depId) => {
                const dep = snap.tasks.find((item) => item.id === depId);
                return (
                  <button key={depId} type="button" className="btn btn--ghost btn--sm" onClick={() => onSelectTask(depId)}>
                    {dep?.title ?? depId}
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {dependents.length > 0 && (
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.downstreamDeps")} ({dependents.length})</span>
            <div className="wb-chip-actions">
              {dependents.map((dep) => (
                <button key={dep.id} type="button" className="btn btn--ghost btn--sm" onClick={() => onSelectTask(dep.id)}>
                  {dep.title}
                </button>
              ))}
            </div>
          </div>
        )}

        {parentTemplate && (
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.expandedFrom")}</span>
            <button type="button" className="link-btn" onClick={() => onSelectTask(parentTemplate.id)}>
              {parentTemplate.title}
            </button>
          </div>
        )}

        {children.length > 0 && (
          <div className="panel-field">
            <span className="panel-field__key">{t("wb.templateChildren")} ({children.length})</span>
            <div className="wb-chip-actions">
              {children.map((child) => (
                <button key={child.id} type="button" className="btn btn--ghost btn--sm" onClick={() => onSelectTask(child.id)}>
                  {child.title}
                </button>
              ))}
            </div>
          </div>
        )}

        {(board || task.review) && (
          <div className="panel-field">
            <span className="panel-field__key">{t("review.title")}</span>
            <div className="wb-actions">
              {board && (
                <>
                  <span className={`review-standing review-standing--${board.standing}`}>{t(`review.standing.${board.standing}`)}</span>
                  <span className="hint">{t("review.round", { round: board.round, max: board.maxRounds })}</span>
                </>
              )}
              {needsReview && (
                <button type="button" className="btn btn--ghost btn--sm" onClick={() => onOpenReviews(task.id)}>
                  {t("wb.openInReviews")}
                </button>
              )}
            </div>
            {task.review?.criteria && <div className="panel-field__val hint" style={{ marginTop: 6 }}>{task.review.criteria}</div>}
          </div>
        )}
      </div>

      <div className="wb-card">
        <h3 className="wb-card__title">{`${t("wb.attemptsDetail")} (${attempts.length})`}</h3>

        {attempts.length === 0 ? (
          <p className="hint">{t("panel.noAttempts")}</p>
        ) : (
          <div className="wb-list-grid">
            {attempts.map((attempt, index) => (
              <TaskAttemptCard
                key={attempt.id}
                attempt={attempt}
                ordinal={index + 1}
                allAttempts={attempts}
                task={task}
                snap={snap}
                liveItems={liveItems}
                streamEpoch={streamEpoch}
                workerNums={workerNums}
                reviewerNums={reviewerNums}
                board={board}
                onOpenWorker={onOpenWorker}
                onOpenReviews={onOpenReviews}
              />
            ))}
          </div>
        )}
      </div>
    </div>
  );

  return (
    <Workbench
      role="task"
      header={
        <WorkbenchHeader
          badge={<span className="role-badge role-badge--task">{t("role.task")}</span>}
          title={t("wb.tasksTitle")}
          subtitle={`${snap.tasks.length} ${t("tb.tasks")} · ${counts.running} ${t("wb.filterRunning")} · ${counts.attention} ${t("wb.filterAttention")}`}
        />
      }
      master={master}
      detail={detail}
    />
  );
}
