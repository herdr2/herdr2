import { memo, useMemo } from "react";
import { COORD_ID } from "../relate";
import { taskFailure, type FailureBucket } from "../failure";
import { useT } from "../i18n";
import { taskRequiresReview } from "../review";
import { ReviewRequirement } from "./ReviewRequirement";
import type { BriefingRef, RunSnapshot, Task } from "../types";

function resolveBriefingRefOpenId(ref: BriefingRef, snap: RunSnapshot): string {
  if (ref.kind === "task") {
    const found = snap.tasks.find((task) => task.id === ref.target || task.key === ref.target);
    if (found) return found.id;
  }
  if (ref.kind === "attempt") {
    const att = snap.attempts.find((a) => a.id === ref.target);
    if (att) return att.taskId;
  }
  if (ref.kind === "message") {
    const msg = (snap.messages ?? snap.pendingMessages).find((m) => m.id === ref.target);
    if (msg?.taskId) return msg.taskId;
  }
  return COORD_ID;
}

const RANK = { failed: 0, stall: 1, question: 2, budget: 3 } as const;
type TodoKind = keyof typeof RANK;

interface Todo {
  id: string;
  kind: TodoKind;
  bucket?: FailureBucket;
  label: string;
  title: string;
  open: string;
}

function settled(task: Task): boolean {
  return task.status === "succeeded" || task.status === "skipped";
}

function budgetHot(snap: RunSnapshot): boolean {
  const budget = snap.run.budget;
  if (!budget) return false;
  const tokenHot = budget.tokens != null && budget.tokens > 0 && snap.stats.tokens / budget.tokens >= 0.8;
  const costHot = budget.cost != null && budget.cost > 0 && snap.stats.cost / budget.cost >= 0.8;
  return tokenHot || costHot;
}

/** Phase progress, goal-gate badge, and a priority-ordered stream of actionable chips. */
export const OverviewBar = memo(function OverviewBar({
  snap,
  onOpen,
  activePhase,
  onTogglePhase,
}: {
  snap: RunSnapshot;
  onOpen: (id: string) => void;
  activePhase?: string | null;
  onTogglePhase?: (phase: string) => void;
}) {
  const { t } = useT();
  const reviewCount = snap.tasks.filter((task) => taskRequiresReview(task, snap.attempts)).length;
  const phases = useMemo(() => {
    const order: string[] = [];
    const map = new Map<string, { done: number; total: number }>();
    for (const task of snap.tasks) {
      const name = task.phase?.trim() || t("overview.unphased");
      if (!map.has(name)) {
        map.set(name, { done: 0, total: 0 });
        order.push(name);
      }
      const row = map.get(name)!;
      row.total += 1;
      if (settled(task)) row.done += 1;
    }
    return order.map((name) => ({ name, ...map.get(name)! }));
  }, [snap.tasks, t]);

  const todos = useMemo(() => {
    const items: Todo[] = [];
    for (const task of snap.tasks) {
      if (task.status !== "failed") continue;
      const cause = taskFailure(task, snap.attempts);
      items.push({
        id: `failed:${task.id}`,
        kind: "failed",
        bucket: cause?.bucket,
        label: task.title,
        title: cause ? t(cause.labelKey) : t("overview.failed"),
        open: task.id,
      });
    }
    for (const [index, stall] of (snap.stalls ?? []).entries()) {
      const taskId = stall.taskIds?.find((id) => snap.tasks.some((task) => task.id === id));
      items.push({
        id: `stall:${stall.kind}:${index}:${stall.since}`,
        kind: "stall",
        label: stall.summary,
        title: t(`stall.${stall.kind}`),
        open: taskId ?? COORD_ID,
      });
    }
    for (const message of snap.pendingMessages) {
      if (message.handledAt || !message.needsAction || message.type !== "question") continue;
      items.push({
        id: `question:${message.id}`,
        kind: "question",
        label: message.subject,
        title: t("overview.question"),
        open: COORD_ID,
      });
    }
    const budgetMsgs = snap.pendingMessages.filter((message) => message.type === "budget" && message.needsAction && !message.handledAt);
    if (snap.run.pauseReason === "budget" || budgetMsgs.length > 0 || budgetHot(snap)) {
      const label = budgetMsgs[0]?.subject
        || (snap.run.pauseReason === "budget" ? t("overview.budgetPaused") : t("overview.budgetHot"));
      items.push({
        id: `budget:${budgetMsgs[0]?.id ?? snap.run.pauseReason ?? "hot"}`,
        kind: "budget",
        label,
        title: t("overview.budget"),
        open: COORD_ID,
      });
    }
    items.sort((a, b) => RANK[a.kind] - RANK[b.kind] || a.label.localeCompare(b.label));
    return items;
  }, [snap, t]);

  const fin = snap.run.finalization;
  const showFinal = fin != null && fin.state !== "idle";
  const briefing = snap.run.briefing;
  const hasBriefing = Boolean(briefing?.headline?.trim());

  return (
    <section className={`overview${hasBriefing ? " overview--with-briefing" : ""}`} aria-label={t("overview.title")}>
      <div className="overview__phases">
        <ReviewRequirement required count={reviewCount} />
        <ReviewRequirement required={false} count={snap.tasks.length - reviewCount} />
        {phases.map((phase) => {
          const isDone = phase.total > 0 && phase.done === phase.total;
          const isActive = activePhase === phase.name;
          const cls = `overview__phase${isDone ? " overview__phase--done" : ""}${isActive ? " overview__phase--active" : ""}${onTogglePhase ? " overview__phase--btn" : ""}`;
          return onTogglePhase ? (
            <button
              key={phase.name}
              type="button"
              className={cls}
              aria-pressed={isActive}
              onClick={() => onTogglePhase(phase.name)}
              title={t("overview.phaseFilterTitle", { name: phase.name, done: phase.done, total: phase.total })}
            >
              <span className="overview__phase-name">{phase.name}</span>
              <span className="overview__phase-count">{phase.done}/{phase.total}</span>
            </button>
          ) : (
            <span
              key={phase.name}
              className={cls}
              title={t("overview.phaseTitle", { name: phase.name, done: phase.done, total: phase.total })}
            >
              <span className="overview__phase-name">{phase.name}</span>
              <span className="overview__phase-count">{phase.done}/{phase.total}</span>
            </span>
          );
        })}
        {showFinal && fin && (
          <span className={`overview__final overview__final--${fin.state}`} title={fin.lastFailure || t(`final.${fin.state}`)}>
            {t(`final.${fin.state}`)}
          </span>
        )}
      </div>
      <div className="overview__todos">
        {todos.length === 0 && <span className="overview__quiet">{t("overview.clear")}</span>}
        {todos.map((todo) => (
          <button
            key={todo.id}
            type="button"
            className={`overview__todo overview__todo--${todo.kind}${todo.bucket ? ` overview__todo--${todo.bucket}` : ""}`}
            title={`${todo.title} · ${todo.label}`}
            onClick={() => onOpen(todo.open)}
          >
            <span className="overview__todo-kind">{todo.title}</span>
            <span className="overview__todo-label">{todo.label}</span>
          </button>
        ))}
      </div>
      {hasBriefing && briefing && (
        <div className="overview__briefing">
          <span className="overview__briefing-badge">{t("overview.briefingLabel")}</span>
          <span className="overview__briefing-headline" title={briefing.runningSummary || briefing.headline}>
            {briefing.headline}
          </span>
          <div className="overview__briefing-items">
            {briefing.attention.slice(0, 2).map((alert) => (
              <span key={alert.id} className={`overview__briefing-chip overview__briefing-chip--${alert.severity}`} title={alert.why || alert.title}>
                <span>{alert.title}</span>
                {alert.refs.map((ref) => (
                  <button
                    key={`${ref.kind}:${ref.target}`}
                    type="button"
                    className="overview__briefing-ref"
                    title={t("overview.briefingJump", { label: ref.label })}
                    onClick={() => onOpen(resolveBriefingRefOpenId(ref, snap))}
                  >
                    [{ref.label}]
                  </button>
                ))}
              </span>
            ))}
            {briefing.threads.slice(0, 3).map((th) => (
              <span key={th.id} className={`overview__briefing-chip overview__briefing-chip--${th.state}`} title={th.now}>
                <span>{th.title}: {th.now}</span>
                {th.refs.slice(0, 2).map((ref) => (
                  <button
                    key={`${ref.kind}:${ref.target}`}
                    type="button"
                    className="overview__briefing-ref"
                    title={t("overview.briefingJump", { label: ref.label })}
                    onClick={() => onOpen(resolveBriefingRefOpenId(ref, snap))}
                  >
                    [{ref.label}]
                  </button>
                ))}
              </span>
            ))}
          </div>
        </div>
      )}
    </section>
  );
});
