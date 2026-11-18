import type { Task, TaskWhen } from "./types";

export type DataWhen = Extract<TaskWhen, { task: string }>;

/** Data branch: another task's result, optionally at `path`. */
export function isDataWhen(when: TaskWhen): when is DataWhen {
  return "task" in when;
}

export function hasDataWhen(tasks: readonly Task[]): boolean {
  return tasks.some((task) => task.when != null && isDataWhen(task.when));
}

export type BranchPhase = "open" | "hit" | "miss";

const DANGEROUS_KEYS = new Set(["__proto__", "constructor", "prototype"]);

/** Own-property path lookup. A present null/false/0 is not a missing path. */
function valueAt(data: unknown, path?: string): { exists: boolean; value: unknown } {
  if (path === undefined || path === "" || path === ".") return { exists: true, value: data };
  let current: unknown = data;
  for (const raw of path.split(".")) {
    const segment = raw.trim();
    if (!segment || DANGEROUS_KEYS.has(segment)) return { exists: false, value: undefined };
    if (Array.isArray(current)) {
      if (!/^\d+$/.test(segment)) return { exists: false, value: undefined };
      const index = Number(segment);
      if (index >= current.length) return { exists: false, value: undefined };
      current = current[index];
      continue;
    }
    if (current !== null && typeof current === "object") {
      if (!Object.hasOwn(current, segment)) return { exists: false, value: undefined };
      current = (current as Record<string, unknown>)[segment];
      continue;
    }
    return { exists: false, value: undefined };
  }
  return { exists: true, value: current };
}

function jsonEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (typeof a !== "object" || typeof b !== "object" || a == null || b == null) return false;
  try {
    return JSON.stringify(a) === JSON.stringify(b);
  } catch {
    return false;
  }
}

function dataHit(when: DataWhen, data: unknown): boolean {
  const found = valueAt(data, when.path);
  if (typeof when.exists === "boolean" && found.exists !== when.exists) return false;
  if (when.equals !== undefined && !jsonEqual(found.value, when.equals)) return false;
  if (when.in && when.in.length > 0 && !when.in.some((item) => jsonEqual(item, found.value))) return false;
  if (when.equals === undefined && !(when.in && when.in.length > 0) && typeof when.exists !== "boolean") return found.exists;
  return true;
}

function stripMachinePrefix(ref: string): string {
  const idx = ref.indexOf("/");
  return idx >= 0 ? ref.slice(idx + 1) : ref;
}

function sourceTask(when: DataWhen, tasks: readonly Task[]): Task | undefined {
  const stripped = stripMachinePrefix(when.task);
  return tasks.find((task) => task.id === when.task || task.key === when.task || task.key === stripped);
}

/**
 * `open` until the source task settles.
 * Data hit matches `path` / `equals` / `in` / `exists`.
 */
export function branchPhase(task: Task, tasks: readonly Task[]): BranchPhase | null {
  const when = task.when;
  if (!when) return null;
  if (isDataWhen(when)) {
    const source = sourceTask(when, tasks);
    if (!source || source.status === "pending" || source.status === "ready" || source.status === "running" || source.status === "blocked") return "open";
    if (source.status !== "succeeded") return "miss";
    return dataHit(when, source.result?.data) ? "hit" : "miss";
  }
  return null;
}

/** Skipped/cancelled because the branch condition was not met. */
export function branchUnselected(task: Task, tasks: readonly Task[]): boolean {
  if ((task.status !== "cancelled" && task.status !== "skipped") || !task.when) return false;
  if (task.result?.summary?.startsWith("分支未选择")) return true;
  return branchPhase(task, tasks) === "miss";
}

/** Compact branch caption. Data branches read `task: <key> · <path>`. */
export function branchCaption(task: Task): string | null {
  const when = task.when;
  if (!when) return null;
  if (isDataWhen(when)) {
    return when.path ? `task: ${when.task} · ${when.path}` : `task: ${when.task}`;
  }
  return null;
}

/** The other task a data branch waits on, when it is on this canvas. */
export function dataBranchSource(task: Task, tasks: readonly Task[]): Task | undefined {
  if (!task.when || !isDataWhen(task.when)) return undefined;
  const source = sourceTask(task.when, tasks);
  return source && source.id !== task.id ? source : undefined;
}
