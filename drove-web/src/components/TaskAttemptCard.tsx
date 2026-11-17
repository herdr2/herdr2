import { useState } from "react";
import { failureBucket } from "../failure";
import { agentModel, attemptTokens, fmtCost, fmtDuration, fmtTokens, jsonPreview } from "../format";
import { useT } from "../i18n";
import { attemptExecReason, attemptMark, MARK_LABEL, workerNodeId } from "../relate";
import { reviewerNodeId, type ReviewBoard } from "../review";
import { resolveRoleMeta, roleDisplayName } from "../roles";
import { clockTime, relTime } from "../time";
import type { Attempt, Item, ReviewVerdict, RunSnapshot, Task, TaskResult } from "../types";
import { Estimate } from "./Estimate";
import { TaskSummaryView } from "./TaskAiPanel";
import { VerifyLogViewer } from "./VerifyLogViewer";
import { WorkerTranscript } from "./WorkerTranscript";

function isReviewVerdictData(data: unknown): data is ReviewVerdict {
  return Boolean(
    data
      && typeof data === "object"
      && !Array.isArray(data)
      && "verdict" in data
      && ["approved", "changes_requested"].includes(String((data as { verdict?: unknown }).verdict)),
  );
}

function reviewVerdictText(
  verdict: ReviewVerdict["verdict"] | undefined,
  attempt: Attempt,
  t: (key: string) => string,
): { label: string; mark: "ok" | "fail" | "run" } {
  if (verdict === "approved") return { label: "通过", mark: "ok" };
  if (verdict === "changes_requested") return { label: "要求修改", mark: "fail" };
  if (attempt.failure) return { label: t(`fail.${attempt.failure.kind}`), mark: "fail" };
  if (!attempt.endedAt && !attempt.outcome) return { label: t("review.roundRunning"), mark: "run" };
  if (attempt.outcome === "succeeded") return { label: t("review.verdict.submitted"), mark: "ok" };
  return { label: t("outcome.running"), mark: "run" };
}

function resolveAuthorResult(attempt: Attempt, allAttempts: readonly Attempt[], task: Task): TaskResult | undefined {
  if (attempt.result) return attempt.result;
  const lastSucceededAuthor = [...allAttempts]
    .filter((item) => item.kind !== "review" && item.outcome === "succeeded")
    .at(-1);
  if (lastSucceededAuthor?.id !== attempt.id) return undefined;
  if (task.candidateResult) return task.candidateResult;
  if (task.result && !isReviewVerdictData(task.result.data)) return task.result;
  return undefined;
}

export function TaskAttemptCard({
  attempt,
  ordinal,
  allAttempts,
  task,
  snap,
  liveItems,
  streamEpoch,
  workerNums,
  reviewerNums,
  board,
  defaultExpanded,
  compact = false,
  onSelectNode,
  onOpenWorker,
  onOpenReviews,
}: {
  attempt: Attempt;
  ordinal: number;
  allAttempts: readonly Attempt[];
  task: Task;
  snap: RunSnapshot;
  liveItems: Item[];
  streamEpoch: number;
  workerNums: Map<string, number>;
  reviewerNums: Map<string, number>;
  board: ReviewBoard | null;
  defaultExpanded?: boolean;
  compact?: boolean;
  onSelectNode?: (id: string) => void;
  onOpenWorker?: (workerId: string) => void;
  onOpenReviews?: (taskId: string) => void;
}) {
  const { t } = useT();
  const isRunning = !attempt.endedAt && !attempt.outcome;
  const [expanded, setExpanded] = useState<boolean>(defaultExpanded ?? isRunning);

  const isReview = attempt.kind === "review";
  const isExec = attempt.kind === "exec" || attempt.workerId === "exec";
  const worker = snap.workers.find((item) => item.id === attempt.workerId);
  const wNum = workerNums.get(attempt.workerId);
  const rNum = reviewerNums.get(attempt.workerId);
  const roleMeta = worker ? resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir }) : null;

  const roundView = isReview
    ? board?.rounds.find((item) => item.attempt.id === attempt.id)
    : undefined;
  const roundSummary = roundView?.summary ?? attempt.reviewRound?.summary;
  const roundFindings = roundView?.findings ?? attempt.reviewRound?.findings ?? [];
  const roundNum = roundView?.round ?? attempt.reviewRound?.round;
  const maxRounds = board?.maxRounds ?? attempt.reviewRound?.maxRounds;
  const reviewVerdict = isReview
    ? reviewVerdictText(roundView?.verdict ?? attempt.reviewRound?.verdict, attempt, t)
    : null;

  const mark = isReview ? reviewVerdict!.mark : attemptMark(attempt);
  const markLabel = isReview ? reviewVerdict!.label : t(MARK_LABEL[attemptMark(attempt)]);
  const typeLabel = isExec
    ? t("panel.attemptExec")
    : isReview
      ? (roundNum != null && maxRounds != null
        ? t("attempt.kindReviewRound", { round: roundNum, max: maxRounds })
        : t("attempt.kindReview"))
      : t("attempt.kindAuthor");

  const execReason = attemptExecReason(attempt, allAttempts, t);
  const authorResult = !isReview ? resolveAuthorResult(attempt, allAttempts, task) : undefined;
  const canOpenWorker = Boolean(worker && attempt.workerId !== "exec" && attempt.workerId !== "coordinator");

  const openExecutor = () => {
    if (attempt.workerId === "coordinator") {
      if (onSelectNode) onSelectNode("coordinator");
      return;
    }
    if (!canOpenWorker) return;
    if (onSelectNode) {
      onSelectNode(isReview ? reviewerNodeId(attempt.workerId) : workerNodeId(attempt.workerId));
      return;
    }
    onOpenWorker?.(attempt.workerId);
  };

  return (
    <article className={`wb-subcard attempt-card${isReview ? " attempt-card--review" : ""}${isRunning ? " attempt-card--live" : ""}`}>
      <div className="wb-subcard__head attempt-card__head">
        <span className="attempt__no" title={`${attempt.id} · ${clockTime(attempt.startedAt)}`}>{`#${ordinal}`}</span>
        <span className={`micro-badge${isReview ? " micro-badge--review" : ""}`}>{typeLabel}</span>
        <span className={`attempt-mark attempt-mark--${mark}`}>{markLabel}</span>
        {!isExec && attempt.workerId !== "coordinator" && (
          <span className="micro-badge">{attempt.reused ? t("panel.reused") : t("panel.fresh")}</span>
        )}
        {attempt.effort && <span className="mono gnode__dim">{attempt.effort}</span>}
        <span className="attempt__id" title={clockTime(attempt.startedAt)}>
          {relTime(attempt.startedAt)} · {fmtDuration(attempt.startedAt, attempt.endedAt)}
        </span>
      </div>

      <div className="attempt-card__exec">
        {isExec ? (
          <span className="micro-badge">EXEC</span>
        ) : attempt.workerId === "coordinator" ? (
          <button type="button" className="link-btn attempt-card__worker" onClick={openExecutor}>
            <span className="micro-badge">COORDINATOR</span>
          </button>
        ) : (
          <button type="button" className="link-btn attempt-card__worker" disabled={!canOpenWorker} onClick={openExecutor}>
            {isReview ? (
              <span className="worker-num">{`R${rNum ?? "?"}`}</span>
            ) : (
              wNum != null && <span className="worker-num">{`W${wNum}`}</span>
            )}
            {roleMeta && !isReview && (
              <span className={`role-badge role-badge--compact ${roleMeta.badgeClass}`}>
                {roleDisplayName(roleMeta, t)}
              </span>
            )}
            <span className="mono">{agentModel(attempt.agent, attempt.model, attempt.workerId)}</span>
          </button>
        )}
        <span className="attempt-card__stats">
          {!compact && (
            <>
              <span className="mono hint">{attempt.id}</span>
              {" · "}
            </>
          )}
          <Estimate text={`${fmtTokens(attemptTokens(attempt))} tok`} estimated={attempt.estimated?.tokens} />
          {" · "}
          <Estimate text={fmtCost(attempt.cost)} estimated={attempt.estimated?.cost} />
        </span>
      </div>

      <div className="attempt-card__meta">
        <div><span className="attempt-card__meta-key">{t("attempt.reasonLabel")}：</span>{execReason}</div>
        <div className="hint"><span className="attempt-card__meta-key">{t("panel.route")}：</span>{attempt.routeReason}</div>
        {attempt.hint && <div className="hint"><span className="attempt-card__meta-key">Hint：</span>{attempt.hint}</div>}
      </div>

      {attempt.failure && (
        <div className="fail-banner fail-banner--other">
          <div className="fail-banner__detail">
            {t(`fail.bucket.${failureBucket(attempt.failure.kind, attempt.kind)}`)} · {t(`fail.${attempt.failure.kind}`)} — {attempt.failure.message}
          </div>
        </div>
      )}

      {attempt.verifyAiDigest && (
        <div className="ai-card ai-card--compact">
          <div className="ai-card__head">
            <span className="micro-badge micro-badge--ai">{t("ai.verifyDigestTitle")}</span>
          </div>
          <pre className="ai-card__pre">{attempt.verifyAiDigest}</pre>
        </div>
      )}

      {isReview && (roundSummary || roundFindings.length > 0 || onOpenReviews) && (
        <div className="attempt-card__section">
          <div className="attempt-card__section-head">
            <span className="panel-field__key">{t("attempt.resultLabel")}</span>
            {onOpenReviews && (
              <button type="button" className="link-btn sp-inline-link" onClick={() => onOpenReviews(task.id)}>
                {t("wb.openInReviews")}
              </button>
            )}
          </div>
          {roundSummary && <p className="review-round__summary">{roundSummary}</p>}
          {roundFindings.length > 0 && (
            <ul className="review-findings">
              {roundFindings.map((finding, index) => (
                <li key={index}>
                  {finding.severity && <span className="review-finding__sev">{finding.severity}</span>}
                  {finding.file && <span className="mono">{finding.file} </span>}
                  <span>{finding.message}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {!isReview && authorResult && (
        <div className="attempt-card__section">
          <span className="panel-field__key">{t("attempt.resultLabel")}</span>
          <TaskSummaryView summary={authorResult.summary} compactSummary={authorResult.compactSummary} />
          {authorResult.filesModified && authorResult.filesModified.length > 0 && (
            <ul className="sp-file-list">
              {authorResult.filesModified.map((file) => <li key={file} className="sp-file-chip mono">{file}</li>)}
            </ul>
          )}
          {authorResult.blockers && authorResult.blockers.length > 0 && (
            <ul className="sp-bullet-list">
              {authorResult.blockers.map((blocker, index) => <li key={index}>{blocker}</li>)}
            </ul>
          )}
          {authorResult.data != null && (
            <details>
              <summary className="hint" style={{ cursor: "pointer" }}>{t("panel.data")}</summary>
              <pre className="stream__pre">{jsonPreview(authorResult.data)}</pre>
            </details>
          )}
        </div>
      )}

      {attempt.verifyLogArtifactId && (
        <VerifyLogViewer runId={snap.run.id} artifactId={attempt.verifyLogArtifactId} />
      )}

      {isExec && task.exec && (
        <details>
          <summary className="hint" style={{ cursor: "pointer" }}>EXEC 配置</summary>
          <pre className="stream__pre">{jsonPreview(task.exec)}</pre>
        </details>
      )}

      {canOpenWorker && worker && (
        <div className="attempt-card__details">
          <button
            type="button"
            className="btn btn--ghost btn--sm attempt-card__toggle"
            aria-expanded={expanded}
            onClick={() => setExpanded((prev) => !prev)}
          >
            {expanded ? `▾ ${t("attempt.detailsHide")}` : `▸ ${t("attempt.detailsShow")}`}
          </button>
          {expanded && (
            <div className="attempt-card__stream">
              <WorkerTranscript
                worker={worker}
                liveItems={liveItems}
                streamEpoch={streamEpoch}
                attempts={snap.attempts}
                tasks={snap.tasks}
                filterAttemptId={attempt.id}
                lockAttempt
                variant={compact ? "panel" : "workbench"}
                bare
              />
            </div>
          )}
        </div>
      )}
    </article>
  );
}
