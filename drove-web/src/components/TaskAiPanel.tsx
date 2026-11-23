import { useEffect, useState } from "react";
import { inspectTask } from "../api";
import { useT } from "../i18n";
import { clockTime } from "../time";
import type { Attempt, AttemptAiDiagnosis, Task } from "../types";

/** Toggle between AI-compacted summary and full worker summary when compactSummary is present. */
export function TaskSummaryView({ summary, compactSummary }: { summary: string; compactSummary?: string }) {
  const { t } = useT();
  const hasCompact = Boolean(compactSummary && compactSummary.trim() && compactSummary.trim() !== summary.trim());
  const [mode, setMode] = useState<"compact" | "full">("compact");
  const showingCompact = hasCompact && mode === "compact";

  return (
    <div className="ai-summary">
      {hasCompact && (
        <div className="ai-summary__bar">
          <span className="micro-badge micro-badge--ai">{t("ai.summaryBadge")}</span>
          <div className="seg seg--xs" role="group" aria-label={t("panel.summary")}>
            <button
              type="button"
              className={mode === "compact" ? "active" : ""}
              aria-pressed={mode === "compact"}
              onClick={() => setMode("compact")}
            >
              {t("ai.summaryCompact")}
            </button>
            <button
              type="button"
              className={mode === "full" ? "active" : ""}
              aria-pressed={mode === "full"}
              onClick={() => setMode("full")}
            >
              {t("ai.summaryFull")}
            </button>
          </div>
        </div>
      )}
      <div className="ai-summary__text">{showingCompact ? compactSummary : summary}</div>
    </div>
  );
}

/** Shared failed-task AI diagnosis box + on-demand "AI 深度诊断 / 重新诊断" trigger and verify digest display. */
export function TaskAiDiagnosisPanel({
  task,
  attempts,
  onRefresh,
}: {
  task: Task;
  attempts: Attempt[];
  onRefresh: () => void;
}) {
  const { t, lang } = useT();
  const [fetchedAi, setFetchedAi] = useState<AttemptAiDiagnosis | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    setFetchedAi(null);
    setErr(null);
    setCopied(false);
  }, [task.id]);

  const ordered = [...attempts].sort((a, b) => (a.startedAt < b.startedAt ? 1 : -1));
  const cachedAi = ordered.find((a) => a.aiDiagnosis)?.aiDiagnosis;
  const ai = fetchedAi ?? cachedAi;
  const verifyDigestAttempt = ordered.find((a) => a.verifyAiDigest);
  const failedAttempt = ordered.find((a) => a.outcome === "failed" || Boolean(a.failure));
  const canDiagnose = task.status !== "succeeded" && (task.status === "failed" || Boolean(failedAttempt));

  const failDetail = failedAttempt?.failure
    ? `${t(`fail.${failedAttempt.failure.kind}`)}${failedAttempt.failure.message ? ` — ${failedAttempt.failure.message}` : ""}`
    : null;
  const triggerReason = task.status === "failed"
    ? (failDetail && failedAttempt ? `${t("ai.triggerTaskFailed")}（${failedAttempt.id} · ${failDetail}）` : t("ai.triggerTaskFailed"))
    : failedAttempt
      ? (failDetail ? `${t("ai.triggerAttemptFailed", { id: failedAttempt.id })} · ${failDetail}` : t("ai.triggerAttemptFailed", { id: failedAttempt.id }))
      : ai
        ? t("ai.triggerCached")
        : null;

  if (!canDiagnose && !ai && !verifyDigestAttempt?.verifyAiDigest) return null;

  async function runDiagnose() {
    if (busy) return;
    setBusy(true);
    setErr(null);
    try {
      const diag = await inspectTask(task.id, { ai: true, refresh: Boolean(ai), lang });
      if (diag.ai) setFetchedAi(diag.ai);
      onRefresh();
    } catch (e) {
      setErr((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function copyCommand(cmd: string) {
    try {
      await navigator.clipboard?.writeText(cmd);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* ignore clipboard unavailability */
    }
  }

  return (
    <div className="ai-diag-stack">
      {verifyDigestAttempt?.verifyAiDigest && (
        <div className="ai-diag ai-diag--digest">
          <div className="ai-diag__head">
            <span className="micro-badge micro-badge--ai">{t("ai.verifyDigestTitle")}</span>
            <span className="mono hint">{verifyDigestAttempt.id}</span>
          </div>
          <pre className="stream__pre ai-diag__pre">{verifyDigestAttempt.verifyAiDigest}</pre>
        </div>
      )}

      {(canDiagnose || ai) && (
        <div className="ai-diag">
          <div className="ai-diag__head">
            <div className="wb-actions">
              <span className="micro-badge micro-badge--ai">{t("ai.diagnosisTitle")}</span>
              {ai && <span className="msg__tag">{ai.recommendedAction}</span>}
              {ai && <span className="hint" title={ai.at}>{clockTime(ai.at)}</span>}
            </div>
            {canDiagnose && (
              <button type="button" className="btn btn--ghost btn--xs" disabled={busy} onClick={() => { void runDiagnose(); }}>
                {busy ? t("ai.diagnosing") : ai ? t("ai.rediagnoseBtn") : t("ai.diagnoseBtn")}
              </button>
            )}
          </div>
          {triggerReason && (
            <p className="hint ai-diag__trigger">
              <strong>{t("ai.triggerReasonLabel")}：</strong>
              {triggerReason}
            </p>
          )}
          {err && <p className="err-banner">{err}</p>}
          {ai && (
            <div className="ai-diag__body">
              <p className="ai-diag__cause">{ai.rootCause}</p>
              {ai.evidence.length > 0 && (
                <div className="panel-field">
                  <span className="panel-field__key">{t("ai.evidenceLabel")}</span>
                  <ul className="ai-diag__evidence">
                    {ai.evidence.map((ev, idx) => <li key={idx}>{ev}</li>)}
                  </ul>
                </div>
              )}
              {ai.hint && (
                <div className="panel-field">
                  <span className="panel-field__key">{t("ai.hintLabel")}</span>
                  <div className="panel-field__val">{ai.hint}</div>
                </div>
              )}
              {ai.suggestedCommand && (
                <div className="ai-diag__cmd">
                  <span className="panel-field__key">{t("ai.suggestedCmd")}</span>
                  <div className="ai-diag__cmdrow">
                    <code className="mono">{ai.suggestedCommand}</code>
                    <button type="button" className="btn btn--ghost btn--xs" onClick={() => { void copyCommand(ai.suggestedCommand!); }}>
                      {copied ? t("ai.copied") : t("ai.copy")}
                    </button>
                  </div>
                </div>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
