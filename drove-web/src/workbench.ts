import { branchUnselected } from "./branch";
import { failureToken, taskFailure, type TaskFailure } from "./failure";
import { ROLE_RANK } from "./layout";
import { COORD_ID } from "./relate";
import { isReviewer, parseReviewerNodeId } from "./review";
import { resolveRoleMeta, type RoleMeta } from "./roles";
import type { Attempt, BuiltInWorkerRole, DaemonEvent, Item, MessageType, Profile, RunSnapshot, Task, Worker, WorkerState } from "./types";

export const WORKBENCH_PAGES = ["dag", "coordinator", "tasks", "workers", "reviews", "daemon", "agents"] as const;
export type Page = (typeof WORKBENCH_PAGES)[number];
export type CoordSection = "host" | "messages" | "stalls" | "goal";

export type TaskFilter = "all" | "running" | "queued" | "done" | "attention";
export type WorkerFilter = "all" | "active" | "ended" | "author" | "reviewer";
export type WorkerRoleFilter = "all" | BuiltInWorkerRole | "custom";

export interface WorkbenchLocation {
  page: Page;
  sel: string | null;
  taskId: string | null;
  reviewTaskId: string | null;
  workerId: string | null;
  coordTab: CoordSection;
}

export interface WorkbenchHrefPatch {
  page: Page;
  taskId?: string | null;
  reviewTaskId?: string | null;
  workerId?: string | null;
  coordTab?: CoordSection;
  sel?: string | null;
  runId?: string | null;
  clearRunScoped?: boolean;
}

export type DetailTarget =
  | { page: "coordinator"; coordTab: CoordSection }
  | { page: "tasks"; taskId: string }
  | { page: "workers"; workerId: string };

const PAGES = new Set<Page>(WORKBENCH_PAGES);
const COORD_SECTIONS = new Set<CoordSection>(["host", "messages", "stalls", "goal"]);
const ACTIVE_WORKER_STATES = new Set<WorkerState>(["starting", "idle", "busy"]);

export function parseCoordSection(raw: string | null | undefined): CoordSection {
  if (raw === "gates") return "stalls";
  return raw && COORD_SECTIONS.has(raw as CoordSection) ? (raw as CoordSection) : "host";
}

export function readWorkbenchLocation(search: string): WorkbenchLocation {
  const q = new URLSearchParams(search);
  const rawPage = q.get("page");
  const page: Page = rawPage && PAGES.has(rawPage as Page) ? (rawPage as Page) : "dag";
  const rawTask = q.get("task");
  const sel = q.get("sel");
  return {
    page,
    sel,
    taskId: page === "tasks" ? rawTask : null,
    reviewTaskId: page === "reviews" ? rawTask : null,
    workerId: page === "workers" ? q.get("worker") : null,
    coordTab: page === "coordinator" ? parseCoordSection(q.get("tab")) : "host",
  };
}

export function buildWorkbenchHref(currentHref: string, patch: WorkbenchHrefPatch): string {
  const url = new URL(currentHref, "http://localhost");
  if (patch.runId !== undefined) {
    if (patch.runId) url.searchParams.set("run", patch.runId);
    else url.searchParams.delete("run");
  }
  if (patch.clearRunScoped) {
    url.searchParams.delete("sel");
    url.searchParams.delete("task");
    url.searchParams.delete("worker");
    url.searchParams.delete("tab");
  }
  if (patch.sel !== undefined) {
    if (patch.sel) url.searchParams.set("sel", patch.sel);
    else url.searchParams.delete("sel");
  }

  if (patch.page === "dag") url.searchParams.delete("page");
  else url.searchParams.set("page", patch.page);

  url.searchParams.delete("task");
  url.searchParams.delete("worker");
  url.searchParams.delete("tab");

  if (patch.page === "tasks" && patch.taskId) {
    url.searchParams.set("task", patch.taskId);
  } else if (patch.page === "reviews" && patch.reviewTaskId) {
    url.searchParams.set("task", patch.reviewTaskId);
  } else if (patch.page === "workers" && patch.workerId) {
    url.searchParams.set("worker", patch.workerId);
  } else if (patch.page === "coordinator" && patch.coordTab && patch.coordTab !== "host") {
    url.searchParams.set("tab", patch.coordTab);
  }
  return `${url.pathname}${url.search}${url.hash}`;
}

export function selectionToDetailTarget(selectionId: string | null | undefined): DetailTarget | null {
  if (!selectionId) return null;
  if (selectionId === COORD_ID) {
    return { page: "coordinator", coordTab: "host" };
  }
  if (selectionId.startsWith("worker:")) {
    return { page: "workers", workerId: selectionId.slice("worker:".length) };
  }
  const reviewerId = parseReviewerNodeId(selectionId);
  if (reviewerId) {
    return { page: "workers", workerId: reviewerId };
  }
  return { page: "tasks", taskId: selectionId };
}

export function taskMatchesFilter(task: Task, filter: TaskFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "running":
      return task.status === "running";
    case "queued":
      return task.status === "pending" || task.status === "ready";
    case "done":
      return task.status === "succeeded" || task.status === "skipped" || task.status === "cancelled";
    case "attention":
      return task.status === "failed" || task.status === "blocked";
  }
}

export function countTasksByFilter(tasks: readonly Task[]): Record<TaskFilter, number> {
  const counts: Record<TaskFilter, number> = { all: tasks.length, running: 0, queued: 0, done: 0, attention: 0 };
  for (const task of tasks) {
    if (taskMatchesFilter(task, "running")) counts.running += 1;
    if (taskMatchesFilter(task, "queued")) counts.queued += 1;
    if (taskMatchesFilter(task, "done")) counts.done += 1;
    if (taskMatchesFilter(task, "attention")) counts.attention += 1;
  }
  return counts;
}

export interface TaskStatusBadgeInfo {
  token: string;
  label: string;
  pulse: boolean;
  cause: TaskFailure | null;
  unselected: boolean;
}

export function deriveTaskStatusBadge(
  task: Task,
  attempts: readonly Attempt[],
  allTasks: readonly Task[],
  t: (key: string, vars?: Record<string, string | number>) => string,
): TaskStatusBadgeInfo {
  const unselected = branchUnselected(task, allTasks);
  const cause = taskFailure(task, attempts);
  const awaiting = attempts.some((a) => a.awaitingLateDone && !a.endedAt);
  const token =
    task.status === "skipped" || unselected
      ? "skipped"
      : cause
        ? failureToken(cause.bucket)
        : task.status === "succeeded"
          ? "done"
          : task.status === "cancelled"
            ? "pending"
            : task.status;
  const label = awaiting
    ? t("task.awaiting")
    : task.status === "skipped"
      ? t("status.skipped")
      : unselected
        ? t("status.unselected")
        : cause
          ? t(cause.labelKey)
          : t(`status.${task.status}`);
  return { token, label, pulse: task.status === "running", cause, unselected };
}

/** Tasks directly waiting on `taskId` via deps, data branches, or foreach expansion. */
export function taskDependents(taskId: string, tasks: readonly Task[]): Task[] {
  return tasks.filter(
    (item) =>
      item.id !== taskId &&
      (item.deps.includes(taskId) ||
        Boolean(item.when && "task" in item.when && item.when.task === taskId) ||
        item.foreachFrom?.task === taskId),
  );
}

/** Expanded instance tasks generated from a template task (`expandedFrom === task.id`). */
export function templateChildren(task: Task, tasks: readonly Task[]): Task[] {
  return tasks.filter((item) => item.expandedFrom === task.id);
}

export function orderedWorkers(workers: readonly Worker[]): Worker[] {
  return workers
    .slice()
    .sort((a, b) => (a.createdAt === b.createdAt ? (a.id < b.id ? -1 : a.id > b.id ? 1 : 0) : a.createdAt < b.createdAt ? -1 : 1));
}

export function workerMatchesFilter(worker: Worker, filter: WorkerFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "active":
      return ACTIVE_WORKER_STATES.has(worker.state);
    case "ended":
      return !ACTIVE_WORKER_STATES.has(worker.state);
    case "author":
      return !isReviewer(worker);
    case "reviewer":
      return isReviewer(worker);
  }
}

export function countWorkersByFilter(workers: readonly Worker[]): Record<WorkerFilter, number> {
  const counts: Record<WorkerFilter, number> = { all: workers.length, active: 0, ended: 0, author: 0, reviewer: 0 };
  for (const worker of workers) {
    if (ACTIVE_WORKER_STATES.has(worker.state)) counts.active += 1;
    else counts.ended += 1;
    if (isReviewer(worker)) counts.reviewer += 1;
    else counts.author += 1;
  }
  return counts;
}

export function workerMatchesRoleFilter(worker: Worker, roleFilter: WorkerRoleFilter): boolean {
  if (roleFilter === "all") return true;
  const meta = resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir });
  return meta.visualKind === roleFilter;
}

export function countWorkersByRoleFilter(workers: readonly Worker[]): Record<WorkerRoleFilter, number> {
  const counts: Record<WorkerRoleFilter, number> = {
    all: workers.length,
    coder: 0,
    tester: 0,
    writer: 0,
    reviewer: 0,
    researcher: 0,
    custom: 0,
  };
  for (const worker of workers) {
    const meta = resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir });
    if (meta.visualKind === "custom") counts.custom += 1;
    else if (meta.visualKind !== "coordinator") counts[meta.visualKind] += 1;
  }
  return counts;
}

export interface WorkerCategorySection {
  key: string;
  roleMeta: RoleMeta;
  workers: Worker[];
}

export function groupWorkersByCategory(
  workers: readonly Worker[],
  profile?: Profile | null,
): WorkerCategorySection[] {
  const map = new Map<string, WorkerCategorySection>();
  for (const worker of workers) {
    const roleMeta = resolveRoleMeta(worker.role, { readOnlyDir: worker.readOnlyDir, profile });
    let section = map.get(roleMeta.id);
    if (!section) {
      section = { key: roleMeta.id, roleMeta, workers: [] };
      map.set(roleMeta.id, section);
    }
    section.workers.push(worker);
  }
  return [...map.values()].sort(
    (a, b) =>
      (ROLE_RANK[a.roleMeta.visualKind] ?? ROLE_RANK.custom) -
        (ROLE_RANK[b.roleMeta.visualKind] ?? ROLE_RANK.custom) ||
      (a.key < b.key ? -1 : a.key > b.key ? 1 : 0),
  );
}

const ATTENTION_RANK = { failed: 0, stall: 1, question: 2, budget: 3 } as const;
export type CoordinatorAttentionKind = keyof typeof ATTENTION_RANK;

export interface CoordinatorAttentionItem {
  id: string;
  kind: CoordinatorAttentionKind;
  severity: "danger" | "warn";
  label: string;
  title: string;
  section: CoordSection;
  taskId?: string;
  messageType?: MessageType;
}

function isBudgetHot(snap: RunSnapshot): boolean {
  const budget = snap.run.budget;
  if (!budget) return false;
  const tokenHot = budget.tokens != null && budget.tokens > 0 && snap.stats.tokens / budget.tokens >= 0.8;
  const costHot = budget.cost != null && budget.cost > 0 && snap.stats.cost / budget.cost >= 0.8;
  return tokenHot || costHot;
}

export function buildCoordinatorAttention(
  snap: RunSnapshot,
  t: (key: string, vars?: Record<string, string | number>) => string,
): CoordinatorAttentionItem[] {
  const items: CoordinatorAttentionItem[] = [];
  for (const task of snap.tasks) {
    if (task.status !== "failed") continue;
    const cause = taskFailure(task, snap.attempts);
    items.push({
      id: `failed:${task.id}`,
      kind: "failed",
      severity: "danger",
      label: task.title,
      title: cause ? t(cause.labelKey) : t("overview.failed"),
      section: "messages",
      taskId: task.id,
      messageType: "failed",
    });
  }
  for (const [index, stall] of (snap.stalls ?? []).entries()) {
    const taskId = stall.taskIds?.find((id) => snap.tasks.some((task) => task.id === id));
    items.push({
      id: `stall:${stall.kind}:${index}:${stall.since}`,
      kind: "stall",
      severity: "warn",
      label: stall.summary,
      title: t(`stall.${stall.kind}`),
      section: "stalls",
      taskId,
    });
  }
  for (const message of snap.pendingMessages) {
    if (message.handledAt || !message.needsAction || message.type !== "question") continue;
    items.push({
      id: `question:${message.id}`,
      kind: "question",
      severity: "warn",
      label: message.subject,
      title: t("overview.question"),
      section: "messages",
      taskId: message.taskId,
      messageType: "question",
    });
  }
  const budgetMsgs = snap.pendingMessages.filter(
    (message) => message.type === "budget" && message.needsAction && !message.handledAt,
  );
  if (snap.run.pauseReason === "budget" || budgetMsgs.length > 0 || isBudgetHot(snap)) {
    items.push({
      id: `budget:${budgetMsgs[0]?.id ?? snap.run.pauseReason ?? "hot"}`,
      kind: "budget",
      severity: snap.run.pauseReason === "budget" ? "danger" : "warn",
      label:
        budgetMsgs[0]?.subject ||
        (snap.run.pauseReason === "budget" ? t("overview.budgetPaused") : t("overview.budgetHot")),
      title: t("overview.budget"),
      section: "goal",
    });
  }
  items.sort((a, b) => ATTENTION_RANK[a.kind] - ATTENTION_RANK[b.kind] || a.label.localeCompare(b.label));
  return items;
}

export const DIALOGUE_ITEM_KINDS: ReadonlySet<Item["kind"]> = new Set<Item["kind"]>(["user", "assistant"]);

export function isDialogueItem(item: Pick<Item, "kind">): boolean {
  return DIALOGUE_ITEM_KINDS.has(item.kind);
}

export function filterStreamItems<T extends Pick<Item, "kind">>(
  items: readonly T[],
  showFullStream: boolean,
): { visible: T[]; hiddenCount: number } {
  if (showFullStream) {
    return { visible: [...items], hiddenCount: 0 };
  }
  const visible = items.filter(isDialogueItem);
  return { visible, hiddenCount: items.length - visible.length };
}

export function hostUserItemMeta(
  text: string | undefined,
  labels: { user: string; coNotify: string; contextSummary: string },
): { kindLabel: string; summary: string | null } {
  if (!text) return { kindLabel: labels.user, summary: null };
  const trimmed = text.trim();
  if (trimmed.startsWith("[co 通知]") || trimmed.startsWith("[drove 通知]")) {
    const firstSentence = trimmed.split(/[。\n]/)[0]?.trim() ?? "";
    const summary = firstSentence.length > 80 ? `${firstSentence.slice(0, 80)}…` : (firstSentence || labels.coNotify);
    return { kindLabel: labels.coNotify, summary };
  }
  if (trimmed.startsWith("<CONTEXT_SUMMARY>")) {
    return { kindLabel: labels.contextSummary, summary: `${labels.contextSummary} (${trimmed.length} chars)` };
  }
  return {
    kindLabel: labels.user,
    summary: trimmed.length > 220 ? `${trimmed.slice(0, 60)}…` : null,
  };
}

export function parseHostToolText(text: string): { title: string; detail: string | null } {
  const trimmed = text.trim();
  if (!trimmed) return { title: "tool", detail: null };
  const match = /^([a-zA-Z0-9_./:-]+):\s+([\s\S]+)$/.exec(trimmed);
  if (!match) return { title: trimmed, detail: null };
  const detail = match[2].trim();
  return { title: match[1], detail: detail || null };
}

/** Sort control-plane daemon events oldest-first (tie-broken by autoincrement id). */
export function sortDaemonEventsChronological(events: readonly DaemonEvent[]): DaemonEvent[] {
  return [...events].sort((a, b) => (a.ts === b.ts ? a.id - b.id : a.ts < b.ts ? -1 : 1));
}

/** Merge daemon event pages by id and return in oldest-first order. */
export function mergeDaemonEventsChronological(
  existing: readonly DaemonEvent[],
  incoming: readonly DaemonEvent[],
): DaemonEvent[] {
  const byId = new Map<number, DaemonEvent>();
  for (const item of existing) byId.set(item.id, item);
  for (const item of incoming) byId.set(item.id, item);
  return sortDaemonEventsChronological([...byId.values()]);
}
