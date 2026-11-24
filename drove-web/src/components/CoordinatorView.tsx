import { useMemo, useState, type CSSProperties } from "react";
import { fmtCost, fmtTokens, jsonPreview, snapshotEstimated } from "../format";
import { useT } from "../i18n";
import { COORD_ID, allMessages, coordinatorAgentModelLine, coordinatorHostLabel } from "../relate";
import { parseReviewerNodeId } from "../review";
import { clockTime, relTime } from "../time";
import type { Run, RunSnapshot } from "../types";
import { buildCoordinatorAttention, type CoordSection } from "../workbench";
import { Estimate } from "./Estimate";
import { HostTranscript } from "./HostTranscript";
import { MessageThread, type MessageFocusRequest } from "./MessageThread";
import { VerifyLogViewer } from "./VerifyLogViewer";
import { StatusBadge, Workbench, WorkbenchEmpty, WorkbenchHeader } from "./WorkbenchShell";

function runStatusToken(status: Run["status"]): string {
  switch (status) {
    case "active":
      return "running";
    case "paused":
      return "blocked";
    case "done":
      return "done";
    default:
      return "pending";
  }
}

function BudgetMeter({
  label,
  usedText,
  limitText,
  estimated,
  ratio,
}: {
  label: string;
  usedText: string;
  limitText?: string;
  estimated?: boolean;
  ratio: number | null;
}) {
  const pct = ratio == null ? null : Math.min(100, Math.round(ratio * 100));
  const hot = ratio != null && ratio >= 0.8;
  const full = ratio != null && ratio >= 1;
  return (
    <div className="wb-meter-row">
      <div className="wb-meter-row__head">
        <span className="wb-meter-row__label">{label}</span>
        <span className="wb-meter-row__val">
          <Estimate text={usedText} estimated={estimated} />
          {limitText ? ` / ${limitText}` : ""}
        </span>
      </div>
      {pct != null && (
        <div className="wb-meter" aria-label={`${label} ${pct}%`}>
          <div
            className={`wb-meter__fill${full ? " wb-meter__fill--full" : hot ? " wb-meter__fill--hot" : ""}`}
            style={{ width: `${pct}%` } as CSSProperties}
          />
        </div>
      )}
    </div>
  );
}

export function CoordinatorView({
  snap,
  section,
  onSelectSection,
  onJumpToDag,
  onOpenTask,
  onOpenWorker,
}: {
  snap: RunSnapshot | null;
  section: CoordSection;
  onSelectSection: (next: CoordSection) => void;
  onJumpToDag: (id: string) => void;
  onOpenTask: (taskId: string) => void;
  onOpenWorker: (workerId: string) => void;
}) {
  const { t } = useT();
  const [msgFocus, setMsgFocus] = useState<MessageFocusRequest | null>(null);

  const msgs = useMemo(() => (snap ? allMessages(snap) : []), [snap]);
  const attentions = useMemo(() => (snap ? buildCoordinatorAttention(snap, t) : []), [snap, t]);
  const unhandledMsgs = useMemo(
    () => snap?.pendingMessages.filter((m) => m.needsAction && !m.handledAt && m.type !== "run_done") ?? [],
    [snap?.pendingMessages],
  );

  if (!snap) {
    return (
      <Workbench
        role="coord"
        header={<WorkbenchHeader badge={<span className="role-badge role-badge--coord">{t("role.coordinator")}</span>} title={t("wb.coordTitle")} />}
        master={<div className="wb-list-empty">{t("empty.pickTitle")}</div>}
        detail={<WorkbenchEmpty title={t("empty.pickTitle")} body={t("empty.pickBody")} />}
      />
    );
  }

  const run = snap.run;
  const coordModel = coordinatorAgentModelLine(run);
  const est = snapshotEstimated(snap.attempts);
  const tokenRatio = run.budget?.tokens ? snap.stats.tokens / run.budget.tokens : null;
  const costRatio = run.budget?.cost ? snap.stats.cost / run.budget.cost : null;
  const stalls = snap.stalls ?? [];

  function routeEntity(targetId: string) {
    if (targetId === COORD_ID) {
      onSelectSection("host");
      return;
    }
    if (targetId.startsWith("worker:")) {
      onOpenWorker(targetId.slice("worker:".length));
      return;
    }
    const revId = parseReviewerNodeId(targetId);
    if (revId) {
      onOpenWorker(revId);
      return;
    }
    onOpenTask(targetId);
  }

  const sections: { key: CoordSection; label: string; count?: string | number; hint: string }[] = [
    { key: "host", label: t("wb.coordHost"), hint: `${coordModel ? `${coordModel} · ` : ""}${coordinatorHostLabel(run)} · ${run.coordinator.hostSession}` },
    { key: "messages", label: t("wb.coordMessages"), count: msgs.length, hint: t("wb.coordMessagesHint") },
    { key: "stalls", label: t("wb.coordStalls"), count: stalls.length + unhandledMsgs.length, hint: t("wb.coordStallsHint") },
    { key: "goal", label: t("wb.coordGoal"), hint: run.finalization ? t(`final.${run.finalization.state}`) : run.status },
  ];

  const master = (
    <div className="wb-side-stack">
      <div className="wb-side-card">
        <div className="wb-side-card__title">{t("panel.budget")}</div>
        <BudgetMeter
          label={t("panel.tokens")}
          usedText={`${fmtTokens(snap.stats.tokens)} tok`}
          limitText={run.budget?.tokens != null ? `${fmtTokens(run.budget.tokens)} tok` : undefined}
          estimated={est.tokens}
          ratio={tokenRatio}
        />
        <BudgetMeter
          label={t("wb.cost")}
          usedText={fmtCost(snap.stats.cost)}
          limitText={run.budget?.cost != null ? fmtCost(run.budget.cost) : undefined}
          estimated={est.cost}
          ratio={costRatio}
        />
        {((snap.stats.workerTokens ?? 0) > 0 || (snap.stats.aiTokens ?? 0) > 0) && (
          <div className="hint" style={{ fontSize: "12px", marginTop: "6px", display: "flex", flexDirection: "column", gap: "2px" }}>
            {(snap.stats.workerTokens ?? 0) > 0 && (
              <div>
                {t("wb.workerUsage")}: {fmtTokens(snap.stats.workerTokens ?? 0)} tok
                {(snap.stats.workerCost ?? 0) > 0 ? ` · ${fmtCost(snap.stats.workerCost ?? 0)}` : ""}
              </div>
            )}
            {(snap.stats.aiTokens ?? 0) > 0 && (
              <div>
                {t("wb.aiUsage")}: {fmtTokens(snap.stats.aiTokens ?? 0)} tok · {fmtCost(snap.stats.aiCost ?? 0)}
              </div>
            )}
          </div>
        )}
      </div>

      <div className="wb-nav-cards" role="tablist" aria-label={t("wb.coordTitle")}>
        {sections.map((item) => (
          <button
            key={item.key}
            type="button"
            role="tab"
            aria-selected={section === item.key}
            className={`wb-item wb-nav-card${section === item.key ? " wb-item--active" : ""}`}
            onClick={() => onSelectSection(item.key)}
          >
            <div className="wb-item__top">
              <span className="wb-item__title">{item.label}</span>
              {item.count != null && <span className="wb-filter__count">{item.count}</span>}
            </div>
            <div className="wb-item__sub">{item.hint}</div>
          </button>
        ))}
      </div>

      <div className="wb-side-card">
        <div className="wb-side-card__title">{t("wb.attentionQueue")} ({attentions.length})</div>
        {attentions.length === 0 ? (
          <p className="hint">{t("wb.noAttention")}</p>
        ) : (
          <ul className="wb-attention-list">
            {attentions.map((att) => (
              <li key={att.id}>
                <button
                  type="button"
                  className={`wb-attention wb-attention--${att.severity}`}
                  onClick={() => {
                    onSelectSection(att.section);
                    if (att.section === "messages") {
                      setMsgFocus({ nonce: Date.now(), type: att.messageType, taskId: att.taskId });
                    }
                  }}
                >
                  <span className="wb-attention__kind">{att.title}</span>
                  <span className="wb-attention__label">{att.label}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );

  const detail = (
    <div className="wb-detail-stack">
      <div className="wb-card wb-run-strip">
        <div className="wb-run-strip__main">
          <div className="wb-run-strip__top">
            <StatusBadge token={runStatusToken(run.status)} pulse={run.status === "active"}>
              {t(`run.${run.status}`)}
            </StatusBadge>
            <span className="mono hint">{run.id}</span>
            {run.pauseReason && (
              <span className="micro-badge">
                {run.pauseReason === "budget" ? t("run.pauseBudget") : t("run.pauseUser")}
              </span>
            )}
          </div>
          <h3 className="wb-run-strip__title">{run.name || run.objective}</h3>
          <div className="mono hint">{run.cwd}</div>
        </div>
      </div>

      {section === "host" && <HostTranscript run={run} variant="workbench" />}

      {section === "messages" && (
        <div className="wb-card">
          <MessageThread
            messages={msgs}
            all={msgs}
            snap={snap}
            filters
            bare
            focusRequest={msgFocus}
            onSelect={routeEntity}
          />
        </div>
      )}

      {section === "stalls" && (
        <div className="wb-detail-stack">
          <div className="wb-card">
            <h3 className="wb-card__title">{t("wb.stalls")} ({stalls.length})</h3>
            {stalls.length === 0 ? (
              <p className="hint">{t("panel.none")}</p>
            ) : (
              <div className="wb-list-grid">
                {stalls.map((stall, idx) => (
                  <article key={`${stall.kind}-${idx}-${stall.since}`} className="wb-subcard">
                    <div className="wb-subcard__head">
                      <StatusBadge token="blocked" pulse>{t(`stall.${stall.kind}`)}</StatusBadge>
                      <span className="hint" title={clockTime(stall.since)}>{relTime(stall.since)}</span>
                    </div>
                    <p>{stall.summary}</p>
                    <div className="wb-actions">
                      {stall.taskIds?.map((tid) => (
                        <button key={tid} type="button" className="link-btn" onClick={() => onOpenTask(tid)}>
                          {snap.tasks.find((item) => item.id === tid)?.title ?? tid}
                        </button>
                      ))}
                      {stall.messageIds && stall.messageIds.length > 0 && (
                        <button type="button" className="btn btn--ghost btn--sm" onClick={() => onSelectSection("messages")}>
                          {t("wb.coordMessages")} ({stall.messageIds.length})
                        </button>
                      )}
                    </div>
                  </article>
                ))}
              </div>
            )}
          </div>

          <div className="wb-card">
            <h3 className="wb-card__title">{t("wb.unhandledMessages")} ({unhandledMsgs.length})</h3>
            {unhandledMsgs.length === 0 ? (
              <p className="hint">{t("panel.none")}</p>
            ) : (
              <MessageThread messages={unhandledMsgs} all={msgs} snap={snap} bare onSelect={routeEntity} />
            )}
          </div>
        </div>
      )}

      {section === "goal" && (
        <div className="wb-detail-stack">
          <div className="wb-card">
            <h3 className="wb-card__title">{t("panel.objective")}</h3>
            <p className="wb-prose">{run.objective}</p>
            <div className="wb-kv-grid">
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.agent")}</span>
                <div className="panel-field__val mono">{run.coordinator.agent || "—"}</div>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.model")}</span>
                <div className="panel-field__val mono">{run.coordinator.model || "—"}</div>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.host")}</span>
                <div className="panel-field__val">{coordinatorHostLabel(run)}</div>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.sessionId")}</span>
                <div className="panel-field__val mono">{run.coordinator.hostSession}</div>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.generation")}</span>
                <div className="panel-field__val">{run.coordinator.generation}</div>
              </div>
              <div className="panel-field">
                <span className="panel-field__key">{t("panel.concurrency")}</span>
                <div className="panel-field__val">{run.maxConcurrency}</div>
              </div>
              {run.coordinator.hostPid != null && (
                <div className="panel-field">
                  <span className="panel-field__key">Host PID</span>
                  <div className="panel-field__val mono">{run.coordinator.hostPid}</div>
                </div>
              )}
              {run.verifiedAt && (
                <div className="panel-field">
                  <span className="panel-field__key">{t("wb.verifiedAt")}</span>
                  <div className="panel-field__val" title={clockTime(run.verifiedAt)}>{relTime(run.verifiedAt)}</div>
                </div>
              )}
            </div>
            {run.coordinator.notification && (
              <div className="wb-subcard">
                <div className="wb-subcard__head">
                  <strong>Codex Notification</strong>
                  <span className="mono hint">seq {run.coordinator.notification.lastSeq}</span>
                </div>
                <div className="mono hint">threadId: {run.coordinator.notification.threadId}</div>
                {run.coordinator.notification.error && (
                  <p className="hint" style={{ color: "var(--danger)" }}>{run.coordinator.notification.error}</p>
                )}
              </div>
            )}
          </div>

          <div className="wb-card">
            <h3 className="wb-card__title">{t("wb.finalizationTitle")}</h3>
            {run.finalization ? (
              <>
                <div className="wb-chips">
                  <StatusBadge
                    token={
                      run.finalization.state === "passed"
                        ? "done"
                        : run.finalization.state === "failed"
                          ? "failed"
                          : run.finalization.state === "verifying" || run.finalization.state === "repairing"
                            ? "running"
                            : "pending"
                    }
                    pulse={run.finalization.state === "verifying" || run.finalization.state === "repairing"}
                  >
                    {t(`final.${run.finalization.state}`)}
                  </StatusBadge>
                  <span className="micro-badge">gen {run.finalization.generation}</span>
                  <span className="micro-badge">{t("wb.repairsUsed", { n: run.finalization.repairsUsed })}</span>
                </div>
                {run.finalization.lastFailure && (
                  <div className="fail-banner fail-banner--other">
                    <div className="fail-banner__detail">{run.finalization.lastFailure}</div>
                  </div>
                )}
                {run.finalization.logArtifactId && (
                  <VerifyLogViewer runId={run.id} artifactId={run.finalization.logArtifactId} />
                )}
              </>
            ) : (
              <p className="hint">{t("panel.none")}</p>
            )}

            {run.verify && (
              <div className="panel-field">
                <span className="panel-field__key">{t("wb.runVerify")}</span>
                <pre className="stream__pre">
                  {typeof run.verify === "string" ? run.verify : jsonPreview(run.verify)}
                </pre>
              </div>
            )}
            {run.goal && (
              <div className="panel-field">
                <span className="panel-field__key">{t("wb.runGoalPolicy")}</span>
                <pre className="stream__pre">{jsonPreview(run.goal)}</pre>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );

  return (
    <Workbench
      role="coord"
      header={
        <WorkbenchHeader
          badge={<span className="role-badge role-badge--coord">{t("role.coordinator")}</span>}
          title={t("wb.coordTitle")}
          subtitle={`${coordModel ? `${coordModel} · ` : ""}${coordinatorHostLabel(run)} · gen ${run.coordinator.generation} · ${t("panel.concurrency")} ${run.maxConcurrency}${attentions.length > 0 ? ` · ${t("coord.pending", { n: attentions.length })}` : ""}`}
          actions={
            <button type="button" className="btn btn--ghost btn--sm" onClick={() => onJumpToDag(COORD_ID)}>
              {t("review.jumpDag")}
            </button>
          }
        />
      }
      master={master}
      detail={detail}
    />
  );
}
