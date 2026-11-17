import type { Attempt, FailureKind, Task } from "./types";

/** Why a failed task is failed: the work itself, a review rejection, or something else. */
export type FailureBucket = "task" | "review" | "other";

const TASK_KINDS = new Set<FailureKind>([
  "agent_error",
  "no_done",
  "timeout",
  "schema",
  "process_exited",
  "worker_reported",
]);

export interface TaskFailure {
  bucket: FailureBucket;
  kind: FailureKind;
  message: string;
  attemptId: string;
  /** Short status label, used on the task card and the overview chip. */
  labelKey: string;
}

export function failureBucket(kind: FailureKind, attemptKind?: Attempt["kind"]): FailureBucket {
  if (kind === "review_rejected") return "review";
  // A reviewer turn that crashed or returned a bad verdict is not a rejection of the work.
  if (attemptKind === "review") return "other";
  if (TASK_KINDS.has(kind)) return "task";
  return "other";
}

/** i18n key for the short label of one failure. */
export function failureLabelKey(kind: FailureKind, attemptKind?: Attempt["kind"]): string {
  if (kind === "review_rejected") return "fail.class.review";
  if (attemptKind === "review") return "fail.class.reviewBroken";
  switch (kind) {
    case "verification_failed":
      return "fail.class.verify";
    case "merge_conflict":
      return "fail.class.merge";
    case "quota_exhausted":
      return "fail.class.quota";
    case "cancelled":
      return "fail.class.cancelled";
    case "agent_error":
    case "no_done":
    case "timeout":
    case "schema":
    case "process_exited":
    case "worker_reported":
      return "fail.class.task";
    default:
      return "fail.class.other";
  }
}

/** Status color token (`--st-*`) for a failure bucket. */
export function failureToken(bucket: FailureBucket): "failed" | "review" | "other" {
  if (bucket === "review") return "review";
  if (bucket === "other") return "other";
  return "failed";
}

function settledAt(attempt: Attempt): string {
  return attempt.endedAt ?? attempt.startedAt;
}

/**
 * The failure that explains a task's current `failed` status.
 * The latest attempt that actually carries a failure wins. A later review turn
 * with no failure does not hide `review_rejected` stamped on the author attempt.
 */
export function taskFailure(task: Task, attempts: readonly Attempt[]): TaskFailure | null {
  if (task.status !== "failed") return null;
  const failed = attempts.filter((attempt) => attempt.taskId === task.id && attempt.failure);
  failed.sort((a, b) => {
    const left = settledAt(a);
    const right = settledAt(b);
    if (left === right) return a.id < b.id ? -1 : 1;
    return left < right ? -1 : 1;
  });
  const latest = failed.at(-1);
  if (!latest?.failure) {
    return { bucket: "other", kind: "agent_error", message: "", attemptId: "", labelKey: "fail.class.other" };
  }
  const { kind, message } = latest.failure;
  return {
    bucket: failureBucket(kind, latest.kind),
    kind,
    message,
    attemptId: latest.id,
    labelKey: failureLabelKey(kind, latest.kind),
  };
}
