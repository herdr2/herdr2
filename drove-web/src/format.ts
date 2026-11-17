import type { Attempt, Run, RunSummary, StatsEntry, Task, TaskPhase, TaskStatus, WorkerState } from "./types";

const BUILTIN_ROLE_SET = new Set(["coder", "tester", "writer", "reviewer", "researcher"]);

/** W1..Wn by creation time, then id. The canvas and the detail list share this order. */
export function workerNumbers(workers: readonly { id: string; createdAt: string }[]): Map<string, number> {
  return new Map(
    workers
      .slice()
      .sort((a, b) => (a.createdAt === b.createdAt ? (a.id < b.id ? -1 : 1) : a.createdAt < b.createdAt ? -1 : 1))
      .map((worker, index) => [worker.id, index + 1] as const),
  );
}

/** ① through ⑳, then a plain decimal. */
export function circNum(n: number): string {
  if (n >= 1 && n <= 20) return String.fromCodePoint(0x245f + n);
  return String(n);
}

export function shortId(id: string | null | undefined, keep = 8): string {
  if (!id) return "—";
  const m = /^([a-z]+_)(.*)$/.exec(id);
  if (!m) return id.length > keep + 1 ? `${id.slice(0, keep)}…` : id;
  return m[2].length > keep ? `${m[1]}${m[2].slice(0, keep)}…` : id;
}

export function fmtTokens(n: number): string {
  if (!Number.isFinite(n)) return "—";
  const sign = n < 0 ? "-" : "";
  const v = Math.abs(n);
  if (v >= 1_000_000) return `${sign}${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1000) return `${sign}${(v / 1000).toFixed(1)}k`;
  return `${sign}${Math.round(v)}`;
}

export const USD_TO_CNY = 7.2;
export const DEEPSEEK_USD_TO_CNY = USD_TO_CNY;

/** Format a USD-stored token cost in RMB (¥) using the 7.2 RMB/USD rate. */
export function fmtCost(usd: number | undefined | null): string {
  if (usd == null || !Number.isFinite(usd)) return "—";
  const cny = usd * USD_TO_CNY;
  if (cny === 0) return "¥0";
  const sign = cny < 0 ? "-" : "";
  const abs = Math.abs(cny);
  if (abs < 0.0001) return `${sign}¥${abs.toFixed(6)}`;
  if (abs < 1) return `${sign}¥${abs.toFixed(4)}`;
  return `${sign}¥${abs.toFixed(2)}`;
}

/** Format USD-stored DeepSeek control-plane cost back into native RMB (¥) using the 7.2 RMB/USD rate in src/ai/deepseek.ts. */
export function fmtDeepSeekCny(usd: number | undefined | null): string {
  return fmtCost(usd);
}

export function fmtDuration(start: string, end?: string, now = Date.now()): string {
  const a = Date.parse(start);
  const b = end ? Date.parse(end) : now;
  if (!Number.isFinite(a) || !Number.isFinite(b)) return "—";
  const s = Math.max(0, Math.round((b - a) / 1000));
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分 ${s % 60} 秒`;
  return `${Math.floor(m / 60)} 小时 ${m % 60} 分`;
}

export function fmtMs(ms: number): string {
  if (!Number.isFinite(ms)) return "—";
  return fmtDuration(new Date(0).toISOString(), new Date(ms).toISOString());
}

/** Daemon counts are integers. A fractional value in (0, 1) is a rate (INTEGRATION_NOTES). */
export function statsSuccess(row: StatsEntry): { count: number; pct: number } {
  const count = row.success > 0 && row.success < 1 ? row.success * row.n : row.success;
  const pct = row.n > 0 ? Math.round((count / row.n) * 100) : 0;
  return { count, pct };
}

export function attemptTokens(a: Attempt): number {
  return a.tokens.input + a.tokens.output + a.tokens.cacheRead;
}

/** Prefix an estimated measurement. Reported values stay unchanged. */
export function withEstimate(text: string, estimated?: boolean): string {
  return estimated ? `≈${text}` : text;
}

export function taskUsage(taskId: string, attempts: Attempt[]): { tokens: number; cost: number; estimatedTokens: boolean; estimatedCost: boolean } {
  let tokens = 0;
  let cost = 0;
  let estimatedTokens = false;
  let estimatedCost = false;
  for (const a of attempts) {
    if (a.taskId !== taskId) continue;
    tokens += attemptTokens(a);
    cost += a.cost ?? 0;
    if (a.estimated?.tokens) estimatedTokens = true;
    if (a.estimated?.cost) estimatedCost = true;
  }
  return { tokens, cost, estimatedTokens, estimatedCost };
}

export function snapshotEstimated(attempts: Attempt[]): { tokens: boolean; cost: boolean } {
  return {
    tokens: attempts.some((a) => a.estimated?.tokens === true),
    cost: attempts.some((a) => a.estimated?.cost === true),
  };
}

export function latestAttempt(taskId: string, attempts: Attempt[]): Attempt | undefined {
  let best: Attempt | undefined;
  for (const a of attempts) {
    if (a.taskId !== taskId) continue;
    if (!best || a.startedAt > best.startedAt) best = a;
  }
  return best;
}

export function agentModel(agent?: string, model?: string, fallback = "未分配"): string {
  const line = [agent, model].filter(Boolean).join(" · ");
  return line || fallback;
}

/** Map a task status onto a :root status token (`--st-*`). */
export function taskToken(s: TaskStatus): string {
  if (s === "succeeded") return "done";
  if (s === "cancelled") return "pending";
  if (s === "skipped") return "skipped";
  return s;
}

export interface MicroBadge {
  kind: "exec" | "template" | "cached" | "review" | "role";
  text: string;
  title: string;
}

/** Compact EXEC / TEMPLATE / CACHED / REVIEW / ROLE chips for a task card and its detail panel. */
export function taskMicroBadges(task: Task, t: (key: string, vars?: Record<string, string | number>) => string): MicroBadge[] {
  const out: MicroBadge[] = [];
  if (task.kind === "exec") out.push({ kind: "exec", text: "EXEC", title: t("badge.execTitle") });
  if (task.assign?.role && task.kind !== "exec") {
    const rawRole = task.assign.role.trim();
    if (rawRole) {
      const label = BUILTIN_ROLE_SET.has(rawRole) ? t(`role.${rawRole}`) : rawRole;
      out.push({ kind: "role", text: label, title: `${t("assign.role")}: ${rawRole}` });
    }
  }
  if (task.template) out.push({ kind: "template", text: "TEMPLATE", title: t("badge.templateTitle") });
  if (task.cachedFrom) out.push({ kind: "cached", text: "CACHED", title: t("badge.cachedTitle", { id: task.cachedFrom }) });
  if (task.review || task.reviewState) {
    const max = task.review?.maxRounds ?? 2;
    const round = task.reviewState?.round ?? 0;
    const phase: TaskPhase | undefined = task.execPhase;
    const text = phase ? t("phase.reviewRound", { phase: t(`phase.${phase}`), round, max }) : "REVIEW";
    out.push({ kind: "review", text, title: t("badge.reviewTitle", { round, max }) });
  }
  return out;
}

export function workerToken(s: WorkerState): string {
  switch (s) {
    case "starting":
    case "busy":
      return "running";
    case "idle":
      return "ready";
    case "dead":
      return "failed";
    case "closed":
      return "pending";
  }
}

export function runToken(status: Run["status"], busy: boolean): string {
  if (status === "paused") return "blocked";
  if (status === "done") return "done";
  if (status === "archived") return "pending";
  return busy ? "running" : "ready";
}

/** Execution result for the list, including failures hidden by lifecycle status "done". */
export function runExecutionStatus(run: RunSummary): "running" | "pending" | "done" | "failed" | "blocked" | "paused" | "archived" | "cancelled" {
  if (run.status === "archived" || run.status === "paused") return run.status;
  const counts = run.taskCounts;
  // Older daemons only return the scheduling status.
  if (!counts) return run.status === "done" ? "done" : "running";
  if (counts.running > 0) return "running";
  if (counts.failed > 0) return "failed";
  if (counts.blocked > 0) return "blocked";
  const total = Object.values(counts).reduce((sum, count) => sum + count, 0);
  const skipped = counts.skipped ?? 0;
  if (total > 0 && counts.cancelled === total) return "cancelled";
  if (run.status === "done" || (total > 0 && counts.succeeded + counts.cancelled + skipped === total)) return "done";
  return "pending";
}

export function contextPct(used: number, size: number): number | null {
  if (!size) return null;
  return Math.max(0, Math.min(100, Math.round((used / size) * 100)));
}

export function isRejection(resolution: string | null | undefined): boolean {
  return /reject|deny|declin|^no$|拒绝|驳回/i.test(resolution ?? "");
}

export function jsonPreview(v: unknown, limit = 800): string {
  if (v == null) return "";
  const s = typeof v === "string" ? v : JSON.stringify(v, null, 2);
  return s.length > limit ? `${s.slice(0, limit)}…` : s;
}
