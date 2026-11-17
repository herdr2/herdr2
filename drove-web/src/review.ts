import { workerNumbers } from "./format";
import type { Attempt, ReviewVerdict, RunSnapshot, Task, Worker } from "./types";
import { taskFailure } from "./failure";

/** A reviewer session is not an authoring worker, even when it is stored on the same table. */
export function isReviewer(worker: { role?: string; readOnlyDir?: string }): boolean {
  return worker.role === "reviewer" || Boolean(worker.readOnlyDir && worker.role !== "researcher");
}

export function authorWorkers<T extends { role?: string; readOnlyDir?: string }>(workers: readonly T[]): T[] {
  return workers.filter((worker) => !isReviewer(worker));
}

/** Canvas id of a reviewer node, distinct from the authoring worker node. */
export const reviewerNodeId = (id: string) => `reviewer:${id}`;

/** Worker id encoded by `reviewerNodeId`, or null when the selection is not a reviewer node. */
export const parseReviewerNodeId = (sel: string | null | undefined): string | null =>
  (sel?.startsWith("reviewer:") ? sel.slice("reviewer:".length) : null);

/**
 * Authoring sub-role such as "author" or "implementer".
 * Reviewer sessions have no custom authoring role.
 */
export function workerCustomRole(worker: { role?: string; readOnlyDir?: string }): string | null {
  if (isReviewer(worker)) return null;
  const role = worker.role?.trim() ?? "";
  return role.length > 0 ? role : null;
}

/** R1..Rn by creation time, independent of worker numbers. */
export function reviewerNumbers(workers: readonly { id: string; createdAt: string; role?: string; readOnlyDir?: string }[]): Map<string, number> {
  return new Map(
    workers
      .filter((worker) => isReviewer(worker))
      .slice()
      .sort((a, b) => (a.createdAt === b.createdAt ? (a.id < b.id ? -1 : 1) : a.createdAt < b.createdAt ? -1 : 1))
      .map((worker, index) => [worker.id, index + 1] as const),
  );
}

/** Where the review loop stands, independent of the task's own success or failure. */
export type ReviewStanding = "waiting" | "reviewing" | "rework" | "approved" | "rejected" | "broken" | "stopped";

export interface ReviewRoundView {
  attempt: Attempt;
  index: number;
  worker?: Worker;
  num?: number;
  round: number;
  verdict?: "approved" | "changes_requested";
  summary?: string;
  findings: NonNullable<ReviewVerdict["findings"]>;
  patchArtifactId?: string;
  deltaPatchArtifactId?: string;
  treeSha?: string;
  baseTreeSha?: string;
}

export interface ReviewBoard {
  standing: ReviewStanding;
  rounds: ReviewRoundView[];
  criteria?: string;
  maxRounds: number;
  round: number;
  summary?: string;
  findings: NonNullable<ReviewVerdict["findings"]>;
}

function byStarted(a: Attempt, b: Attempt): number {
  if (a.startedAt === b.startedAt) return a.id < b.id ? -1 : 1;
  return a.startedAt < b.startedAt ? -1 : 1;
}

function hasLaterRework(attempt: Attempt, attempts: readonly Attempt[]): boolean {
  return attempts.some((other) => other.taskId === attempt.taskId && other.kind !== "review" && byStarted(attempt, other) < 0);
}

function roundVerdict(attempt: Attempt, task: Task, attempts: readonly Attempt[], isLast: boolean): ReviewRoundView["verdict"] {
  if (attempt.reviewRound?.verdict) return attempt.reviewRound.verdict;
  if (hasLaterRework(attempt, attempts)) return "changes_requested";
  if (attempt.reviewRound || !isLast) return undefined;
  if (!attempt.endedAt && !attempt.outcome) return undefined;
  if (attempt.outcome === "failed" || attempt.failure) return undefined;
  const state = task.reviewState;
  if (state?.approvedTreeSha || task.status === "succeeded") return "approved";
  if (task.status === "failed" || task.execPhase === "rework" || state?.lastSummary || (state?.findings?.length ?? 0) > 0) {
    return "changes_requested";
  }
  return undefined;
}

function inferLegacyRounds(task: Task, attempts: readonly Attempt[], maxRounds: number): Map<string, number> {
  const sorted = attempts.filter((attempt) => attempt.taskId === task.id).sort(byStarted);
  const out = new Map<string, number>();
  let cycleRound = 0;
  let lastAuthor: Attempt | undefined;
  for (const attempt of sorted) {
    if (attempt.kind === "review") {
      cycleRound = attempt.reviewRound?.round ?? (cycleRound + 1);
      out.set(attempt.id, cycleRound);
      continue;
    }
    if (
      cycleRound > 0
      && (cycleRound >= maxRounds || Boolean(attempt.retryOf) || attempt.reused === false || lastAuthor?.failure?.kind === "review_rejected")
    ) {
      cycleRound = 0;
    }
    lastAuthor = attempt;
  }
  return out;
}

function toRoundView(
  attempt: Attempt,
  index: number,
  inferredRound: number,
  task: Task,
  attempts: readonly Attempt[],
  byId: Map<string, Worker>,
  numbers: Map<string, number>,
  isLast: boolean,
): ReviewRoundView {
  const recorded = attempt.reviewRound;
  const state = task.reviewState;
  const findings = recorded ? (recorded.findings ?? []) : (isLast ? (state?.findings ?? []) : []);
  return {
    attempt,
    index,
    worker: byId.get(attempt.workerId),
    num: numbers.get(attempt.workerId),
    round: recorded?.round ?? inferredRound,
    verdict: roundVerdict(attempt, task, attempts, isLast),
    summary: recorded ? recorded.summary : (isLast ? state?.lastSummary : undefined),
    findings,
    patchArtifactId: recorded ? recorded.patchArtifactId : (isLast ? state?.patchArtifactId : undefined),
    deltaPatchArtifactId: recorded?.deltaPatchArtifactId,
    treeSha: recorded ? recorded.treeSha : (isLast ? (state?.lastReviewedTreeSha ?? state?.currentTreeSha) : undefined),
    baseTreeSha: recorded ? recorded.baseTreeSha : (isLast ? state?.baseTreeSha : undefined),
  };
}

/** Include persisted review history even when an older snapshot lacks its review config. */
export function taskRequiresReview(task: Task, attempts: readonly Attempt[] = []): boolean {
  return Boolean(task.review || task.reviewState || attempts.some((attempt) => attempt.taskId === task.id && attempt.kind === "review"));
}

/** Review loop for one task. Absent when the task has no review configured and no review turns. */
export function reviewBoard(task: Task, attempts: readonly Attempt[], workers: readonly Worker[]): ReviewBoard | null {
  if (!taskRequiresReview(task, attempts)) return null;
  const rounds = attempts.filter((attempt) => attempt.taskId === task.id && attempt.kind === "review").sort(byStarted);
  const maxRounds = task.review?.maxRounds ?? rounds.at(-1)?.reviewRound?.maxRounds ?? (task.review ? 2 : 1);
  const legacyRounds = inferLegacyRounds(task, attempts, maxRounds);
  const numbers = reviewerNumbers(workers);
  const byId = new Map(workers.map((worker) => [worker.id, worker]));
  const failure = taskFailure(task, attempts);
  const lastRecordedVerdict = rounds.at(-1)?.reviewRound?.verdict;
  const standaloneRejected = !task.review && lastRecordedVerdict === "changes_requested";
  const approved = !standaloneRejected && (Boolean(task.reviewState?.approvedTreeSha) || (task.status === "succeeded" && Boolean(task.review || rounds.length > 0)));
  let standing: ReviewStanding = "waiting";
  if (failure?.bucket === "review" || standaloneRejected) standing = "rejected";
  else if (failure && rounds.some((attempt) => attempt.id === failure.attemptId)) standing = "broken";
  else if (task.status === "failed") standing = "stopped";
  else if (approved) standing = "approved";
  else if (task.execPhase === "review" || rounds.some((attempt) => !attempt.endedAt && !attempt.outcome)) standing = "reviewing";
  else if (task.execPhase === "rework") standing = "rework";
  const roundViews = rounds.map((attempt, index) =>
    toRoundView(attempt, index + 1, legacyRounds.get(attempt.id) ?? (index + 1), task, attempts, byId, numbers, index === rounds.length - 1),
  );
  return {
    standing,
    rounds: roundViews,
    criteria: task.review?.criteria,
    maxRounds,
    round: task.reviewState?.round ?? roundViews.at(-1)?.round ?? 0,
    summary: task.reviewState?.lastSummary ?? roundViews.at(-1)?.summary,
    findings: task.reviewState?.findings ?? roundViews.at(-1)?.findings ?? [],
  };
}

/** One step of a task's author → review → rework loop, in `startedAt` order. */
export type ReviewTimelineStep =
  | { kind: "author_submit"; attempt: Attempt; worker?: Worker; workerNum?: number }
  | { kind: "review_round"; attempt: Attempt; worker?: Worker; reviewerNum?: number; roundView: ReviewRoundView }
  | { kind: "author_rework"; attempt: Attempt; worker?: Worker; workerNum?: number; reworkIndex: number };

/**
 * Closed review loop for one task: the candidate submission, each read-only review round,
 * and each author rework that followed a review. Earlier retries before the first submission are omitted.
 */
export function buildReviewTimeline(task: Task, attempts: readonly Attempt[], workers: readonly Worker[]): ReviewTimelineStep[] {
  const relevant = attempts.filter((attempt) => attempt.taskId === task.id).sort(byStarted);
  const reviews = relevant.filter((attempt) => attempt.kind === "review");
  if (!task.review && !task.reviewState && reviews.length === 0) return [];
  const board = reviewBoard(task, attempts, workers);
  const roundById = new Map((board?.rounds ?? []).map((round) => [round.attempt.id, round]));
  const byId = new Map(workers.map((worker) => [worker.id, worker]));
  const authorNums = workerNumbers(authorWorkers(workers));
  const reviewNums = reviewerNumbers(workers);
  const exec = relevant.filter((attempt) => attempt.kind !== "review");
  const firstReview = reviews[0];
  const submit = (firstReview ? exec.filter((attempt) => byStarted(attempt, firstReview) < 0) : exec).at(-1);
  const steps: ReviewTimelineStep[] = [];
  let seenReview = false;
  let reworkIndex = 0;
  for (const attempt of relevant) {
    const worker = byId.get(attempt.workerId);
    if (attempt.kind === "review") {
      seenReview = true;
      const roundView = roundById.get(attempt.id);
      if (!roundView) continue;
      steps.push({ kind: "review_round", attempt, worker, reviewerNum: reviewNums.get(attempt.workerId), roundView });
      continue;
    }
    if (submit && attempt.id === submit.id) {
      steps.push({ kind: "author_submit", attempt, worker, workerNum: authorNums.get(attempt.workerId) });
      continue;
    }
    if (!seenReview) continue;
    reworkIndex += 1;
    steps.push({ kind: "author_rework", attempt, worker, workerNum: authorNums.get(attempt.workerId), reworkIndex });
  }
  return steps;
}

/** Tasks in this run that have a review configured or at least one review turn. */
export function reviewedTasks(snap: RunSnapshot): { task: Task; board: ReviewBoard }[] {
  const out: { task: Task; board: ReviewBoard }[] = [];
  for (const task of snap.tasks) {
    const board = reviewBoard(task, snap.attempts, snap.workers);
    if (board) out.push({ task, board });
  }
  return out;
}
