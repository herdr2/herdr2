import { agentModel, shortId } from "./format";
import { isReviewer, reviewerNodeId, reviewerNumbers } from "./review";
import type { Attempt, Message, MessageParty, Run, RunSnapshot, TalkHop, Task, Worker } from "./types";

export { reviewerNodeId };

export const COORD_ID = "coord:run";
export const workerNodeId = (id: string) => `worker:${id}`;

const HOST_SESSION_PREFIX_RE = /^(antigravity|claude|codex|cursor|kimi|dsh|zcode|devin|opencode|qoder)[-_]/i;

/** Resolve coordinator host label, falling back to transcript host or hostSession prefix when "unknown". */
export function coordinatorHostLabel(run: Pick<Run, "coordinator">, fallbackHost?: string): string {
  const host = run.coordinator.host?.trim();
  if (host && host !== "unknown") return host;
  const fallback = fallbackHost?.trim();
  if (fallback && fallback !== "unknown") return fallback;
  const match = HOST_SESSION_PREFIX_RE.exec(run.coordinator.hostSession);
  if (match?.[1]) return match[1].toLowerCase();
  return host || "unknown";
}

/** Visual mark for one attempt. A retry stays a retry even when it also failed or succeeded. */
export type AttemptMark = "ok" | "fail" | "retry" | "run";

export const MARK_LABEL: Record<AttemptMark, string> = {
  ok: "outcome.succeeded",
  fail: "outcome.failed",
  retry: "outcome.retry",
  run: "outcome.running",
};

export function attemptMark(a: Attempt): AttemptMark {
  if (a.retryOf) return "retry";
  if (a.outcome === "succeeded") return "ok";
  if (a.outcome === "failed") return "fail";
  return "run";
}

/** 1-based chronological ordinal for each attempt of a task. */
export function attemptOrdinals(attempts: readonly Attempt[]): Map<string, number> {
  const sorted = [...attempts].sort(byStarted);
  return new Map(sorted.map((a, i) => [a.id, i + 1]));
}

/** Human-readable reason explaining why this attempt was triggered. */
export function attemptExecReason(
  a: Attempt,
  taskAttemptsList: readonly Attempt[],
  t: (key: string, vars?: Record<string, string | number>) => string,
): string {
  const chron = [...taskAttemptsList].sort(byStarted);
  const idx = chron.findIndex((item) => item.id === a.id);
  const ord = idx >= 0 ? idx + 1 : 1;
  const prev = a.retryOf
    ? chron.find((item) => item.id === a.retryOf)
    : idx > 0
      ? chron[idx - 1]
      : undefined;
  const prevIdx = prev ? chron.findIndex((item) => item.id === prev.id) : -1;
  const prevOrd = prevIdx >= 0 ? prevIdx + 1 : undefined;

  if (a.kind === "review") {
    return t("attempt.reasonReview", { n: reviewOrdinal(a, chron) });
  }
  if (a.workerId === "coordinator") {
    if (prev?.failure && prevOrd) {
      return t("attempt.reasonTakeoverWithFail", { n: prevOrd, fail: t(`fail.${prev.failure.kind}`) });
    }
    return t("attempt.reasonTakeover");
  }
  const hasPriorReview = idx > 0 && chron.slice(0, idx).some((item) => item.kind === "review");
  if (hasPriorReview || prev?.failure?.kind === "review_rejected") {
    return prevOrd ? t("attempt.reasonReworkFrom", { n: prevOrd }) : t("attempt.reasonRework");
  }
  if (a.retryOf) {
    const targetOrd = prevOrd ?? Math.max(1, ord - 1);
    if (prev?.failure) {
      return t("attempt.reasonRetryWithFail", { n: targetOrd, fail: t(`fail.${prev.failure.kind}`) });
    }
    return t("attempt.reasonRetry", { n: targetOrd });
  }
  if (idx <= 0) return t("attempt.reasonFirst");
  if (prev?.failure && prevOrd) {
    return t("attempt.reasonRetryWithFail", { n: prevOrd, fail: t(`fail.${prev.failure.kind}`) });
  }
  return t("attempt.reasonFollowup", { n: ord });
}

export function byStarted(a: Attempt, b: Attempt): number {
  if (a.startedAt === b.startedAt) return a.id < b.id ? -1 : 1;
  return a.startedAt < b.startedAt ? -1 : 1;
}

export function workerAttempts(workerId: string, attempts: Attempt[]): Attempt[] {
  return attempts.filter((a) => a.workerId === workerId).sort(byStarted);
}

export function taskAttempts(taskId: string, attempts: Attempt[]): Attempt[] {
  return attempts.filter((a) => a.taskId === taskId).sort(byStarted);
}

/**
 * Tasks a worker executed, earliest attempt first.
 * Fingerprint task ids with no attempt are appended, so a reused worker still lists them.
 */
export function tasksInOrder(worker: Worker, attempts: Attempt[]): string[] {
  const times = new Map<string, string>();
  for (const a of workerAttempts(worker.id, attempts)) {
    if (!times.has(a.taskId)) times.set(a.taskId, a.startedAt);
  }
  for (const id of worker.fingerprint.taskIds) {
    if (!times.has(id)) times.set(id, "\uffff");
  }
  return [...times.entries()]
    .sort((a, b) => (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : a[0] < b[0] ? -1 : 1))
    .map(([id]) => id);
}

/** Tasks this reviewer has reviewed (or is currently assigned to review), earliest review attempt first. */
export function reviewerTasksInOrder(worker: Worker, attempts: Attempt[], tasks: readonly Task[] = []): string[] {
  const times = new Map<string, string>();
  for (const attempt of workerAttempts(worker.id, attempts)) {
    if (attempt.kind !== "review" || times.has(attempt.taskId)) continue;
    times.set(attempt.taskId, attempt.startedAt);
  }
  for (const task of tasks) {
    if (task.reviewState?.reviewerWorkerId === worker.id && !times.has(task.id)) {
      times.set(task.id, "\uffff");
    }
  }
  return [...times.entries()]
    .sort((a, b) => (a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : a[0] < b[0] ? -1 : 1))
    .map(([id]) => id);
}

function reviewOrdinal(attempt: Attempt, attempts: readonly Attempt[]): number {
  if (attempt.reviewRound?.round) return attempt.reviewRound.round;
  const reviews = attempts.filter((item) => item.taskId === attempt.taskId && item.kind === "review");
  const index = reviews.findIndex((item) => item.id === attempt.id);
  return index >= 0 ? index + 1 : 1;
}

/** Full message list of the Run, including coordinator → worker dispatches and nudges. */
export function allMessages(snap: RunSnapshot): Message[] {
  const raw = snap.messages ?? snap.pendingMessages;
  const out: Message[] = [];
  const taskById = new Map(snap.tasks.map((t) => [t.id, t]));
  const sortedAttempts = [...snap.attempts].sort(byStarted);

  sortedAttempts.forEach((a, idx) => {
    const task = taskById.get(a.taskId);
    const isExec = a.workerId === "exec" || a.kind === "exec";
    const priorReview = a.kind !== "review" && sortedAttempts.some((prev) => prev.taskId === a.taskId && prev.kind === "review" && byStarted(prev, a) < 0);
    const kindPrefix = isExec
      ? "确定性命令执行"
      : a.workerId === "coordinator"
        ? "主 Agent 接管收口"
        : priorReview ? "返工派发" : a.retryOf ? "重试派发" : a.reused ? "复用派发" : "派发任务";
    const title = task ? `${task.key}（${task.title}）` : a.taskId;
    const lines: string[] = [];
    if (a.kind === "review") {
      const reviews = sortedAttempts.filter((item) => item.taskId === a.taskId && item.kind === "review");
      const lastReview = reviews.length > 0 && reviews[reviews.length - 1]?.id === a.id;
      const treeSha = a.reviewRound?.treeSha ?? (lastReview ? (task?.reviewState?.currentTreeSha ?? task?.reviewState?.lastReviewedTreeSha) : undefined);
      const patchId = a.reviewRound?.patchArtifactId ?? (lastReview ? task?.reviewState?.patchArtifactId : undefined);
      if (a.routeReason) lines.push(`路由理由：${a.routeReason}`);
      if (task?.review?.criteria) lines.push(`审查标准：${task.review.criteria}`);
      if (treeSha) lines.push(`候选树：${treeSha}`);
      if (patchId) lines.push(`补丁制品：${patchId}`);
      if (a.reviewRound?.deltaPatchArtifactId) lines.push(`增量补丁：${a.reviewRound.deltaPatchArtifactId}`);
      out.push({
        id: `dispatch:${a.id}`,
        seq: -(sortedAttempts.length - idx),
        runId: a.runId,
        type: "dispatch",
        from: "coordinator",
        to: reviewerNodeId(a.workerId),
        taskId: a.taskId,
        attemptId: a.id,
        subject: `发起审查（第 ${reviewOrdinal(a, sortedAttempts)} 轮）：${title}`,
        body: lines.join("\n"),
        needsAction: false,
        createdAt: a.startedAt,
      });
      return;
    }
    if (a.routeReason) lines.push(`路由策略：${a.routeReason}`);
    if (a.hint) lines.push(`重试提示：${a.hint}`);
    if (task) {
      lines.push(`目标：${task.spec.target}`);
      lines.push(`改动：${task.spec.change}`);
      if (task.spec.constraints) lines.push(`约束：${task.spec.constraints}`);
      if (task.spec.acceptance) lines.push(`验收：${task.spec.acceptance}`);
      if (task.deps.length > 0) {
        const depNotes = task.deps
          .map((depId) => taskById.get(depId))
          .filter((d): d is NonNullable<typeof d> => Boolean(d))
          .map((d) => (d.result?.summary ? `${d.key}：${d.result.summary}` : d.key));
        if (depNotes.length > 0) lines.push(`上游依赖交接：${depNotes.join("；")}`);
      }
    }
    out.push({
      id: `dispatch:${a.id}`,
      seq: -(sortedAttempts.length - idx),
      runId: a.runId,
      type: "dispatch",
      from: "coordinator",
      to: isExec
        ? (a.taskId ? `task:${a.taskId}` : "coordinator")
        : a.workerId === "coordinator"
          ? "coordinator"
          : `worker:${a.workerId}`,
      taskId: a.taskId,
      attemptId: a.id,
      subject: `${kindPrefix}：${title}`,
      body: lines.join("\n"),
      needsAction: false,
      createdAt: a.startedAt,
    });
  });

  for (const m of raw) {
    out.push(m);
  }

  for (const a of sortedAttempts) {
    if (a.failure?.kind === "no_done") {
      const task = taskById.get(a.taskId);
      out.push({
        id: `nudge:${a.id}`,
        seq: 0,
        runId: a.runId,
        type: "note",
        from: "coordinator",
        to: `worker:${a.workerId}`,
        taskId: a.taskId,
        attemptId: a.id,
        subject: `收尾提醒：请执行 co worker done（${task?.key ?? a.taskId}）`,
        body: `Worker 回合结束但未提交报告，编排者已发送收尾催促（${a.failure.message}）。`,
        needsAction: false,
        createdAt: a.endedAt ?? a.startedAt,
      });
    }
  }

  return out;
}

/** Ids of messages that have received a reply in the same thread. */
export function answeredQuestionIds(messages: Message[]): Set<string> {
  return new Set(messages.filter((m) => m.threadId && m.threadId !== m.id).map((m) => m.threadId as string));
}

function addressedWorkerId(addr: string): string | null {
  if (addr.startsWith("reviewer:")) return addr.slice("reviewer:".length);
  if (addr.startsWith("worker:")) return addr.slice("worker:".length);
  return null;
}

/** Resolve which worker a message belongs to (via from, to, raisedByWorker, or attemptId). */
export function messageWorkerId(m: Message, attempts: readonly Attempt[]): string | null {
  const fromId = addressedWorkerId(m.from);
  if (fromId) return fromId;
  const toId = addressedWorkerId(m.to);
  if (toId) return toId;
  if (m.raisedByWorker) return m.raisedByWorker;
  if (m.attemptId) return attempts.find((a) => a.id === m.attemptId)?.workerId ?? null;
  return null;
}

/** True when the message is part of a read-only review channel rather than an authoring turn. */
export function isReviewMessage(m: Message, attempts: readonly Attempt[], workers?: readonly Worker[]): boolean {
  if (m.from.startsWith("reviewer:") || m.to.startsWith("reviewer:")) return true;
  if (m.attemptId && attempts.some((attempt) => attempt.id === m.attemptId && attempt.kind === "review")) return true;
  const workerId = messageWorkerId(m, attempts);
  return Boolean(workerId && workers?.some((worker) => worker.id === workerId && isReviewer(worker)));
}

/** Open worker questions still awaiting a coordinator reply. */
export function openQuestions(messages: Message[]): Message[] {
  const answered = answeredQuestionIds(messages);
  return messages.filter((m) => m.type === "question" && m.needsAction && !m.handledAt && !answered.has(m.id));
}

/** Unhandled escalations or failures needing coordinator action. */
export function openEscalations(messages: Message[]): Message[] {
  return messages.filter((m) => (m.type === "escalation" || m.type === "failed") && m.needsAction && !m.handledAt);
}

/** All messages involving `workerId`, including replies in the same thread. */
export function workerMessages(workerId: string, messages: Message[], attempts: Attempt[]): Message[] {
  const directIds = new Set<string>();
  const threadIds = new Set<string>();
  for (const m of messages) {
    if (messageWorkerId(m, attempts) === workerId) {
      directIds.add(m.id);
      if (m.threadId) threadIds.add(m.threadId);
    }
  }
  return messages.filter(
    (m) => directIds.has(m.id) || (m.threadId != null && directIds.has(m.threadId)) || threadIds.has(m.id),
  );
}

/** All messages associated with `taskId`, including replies in the same thread. */
export function taskMessages(taskId: string, messages: Message[]): Message[] {
  const directIds = new Set<string>();
  const threadIds = new Set<string>();
  for (const m of messages) {
    if (m.taskId === taskId) {
      directIds.add(m.id);
      if (m.threadId) threadIds.add(m.threadId);
    }
  }
  return messages.filter(
    (m) => directIds.has(m.id) || (m.threadId != null && directIds.has(m.threadId)) || threadIds.has(m.id),
  );
}

export function messageParty(
  addr: string | null | undefined,
  m: Message,
  snap: RunSnapshot,
  wNums: Map<string, number>,
): MessageParty | null {
  if (!addr) return null;
  const taskLabel = (id?: string) => (id ? (snap.tasks.find((t) => t.id === id)?.title ?? id) : null);
  if (addr === "coordinator") {
    const c = snap.run.coordinator;
    return {
      role: "coord",
      name: c.host ? `${c.host} · ${shortId(c.hostSession, 10)}` : null,
      task: null,
      select: COORD_ID,
      raw: addr,
    };
  }
  if (addr.startsWith("worker:") || addr.startsWith("reviewer:")) {
    const reviewerAddr = addr.startsWith("reviewer:");
    const wId = addr.slice(reviewerAddr ? "reviewer:".length : "worker:".length);
    const w = snap.workers.find((x) => x.id === wId);
    const reviewer = reviewerAddr || Boolean(w && isReviewer(w));
    if (reviewer) {
      return {
        role: "reviewer",
        name: w ? agentModel(w.agent, w.model, w.id) : shortId(wId),
        reviewer: true,
        reviewerNum: reviewerNumbers(snap.workers).get(wId),
        task: taskLabel(m.taskId),
        select: reviewerNodeId(w?.id ?? wId),
        raw: addr,
      };
    }
    return {
      role: "worker",
      name: w ? agentModel(w.agent, w.model, w.id) : shortId(wId),
      workerNum: wNums.get(wId),
      task: taskLabel(m.taskId),
      select: w ? workerNodeId(w.id) : null,
      raw: addr,
    };
  }
  if (addr.startsWith("task:")) {
    const tId = addr.slice("task:".length);
    const task = snap.tasks.find((x) => x.id === tId);
    return {
      role: "task",
      name: task ? `${task.title} (${task.key})` : shortId(tId),
      task: null,
      select: task ? task.id : null,
      raw: addr,
    };
  }
  if (addr === "system") {
    const wId = messageWorkerId(m, snap.attempts);
    const w = wId ? snap.workers.find((x) => x.id === wId) : undefined;
    const num = wId ? wNums.get(wId) : undefined;
    return {
      role: "system",
      name: w ? `系统监控 · ${num ? `W${num} ` : ""}${agentModel(w.agent, w.model, w.id)}` : "系统",
      workerNum: num,
      task: taskLabel(m.taskId),
      select: w ? workerNodeId(w.id) : m.taskId && snap.tasks.some((t) => t.id === m.taskId) ? m.taskId : COORD_ID,
      raw: addr,
    };
  }
  return { role: "system", name: addr, task: null, select: null, raw: addr };
}

export function messageHop(m: Message, snap: RunSnapshot, wNums: Map<string, number>): TalkHop {
  const fromParty = messageParty(m.from, m, snap, wNums);
  const toParty = messageParty(m.to, m, snap, wNums);
  return {
    id: m.id,
    from: fromParty?.select ?? null,
    to: toParty?.select ?? null,
    type: m.type,
    subject: m.subject,
    reply: m.type === "reply" || (!!m.threadId && m.threadId !== m.id),
    taskId: m.taskId && snap.tasks.some((t) => t.id === m.taskId) ? m.taskId : null,
  };
}
