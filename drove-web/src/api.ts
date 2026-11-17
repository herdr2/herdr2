import { isMock, mockAgents, mockAiStatus, mockAssign, mockCoordinatorTranscript, mockDaemonEvents, mockDaemonHealth, mockDeleteRun, mockInspectTask, mockItems, mockListRuns, mockPauseRun, mockRenameRun, mockReorderRuns, mockRestoreRun, mockResumeRun, mockReviewDiff, mockSaveAiConfig, mockSaveProfile, mockSnapshot, mockStop, mockTestAiConnection, mockTrashRun, mockVerifyLog } from "./mock";
import type { CoordinatorTranscriptSlice, DaemonAiConfigPatch, DaemonAiProbeResult, DaemonAiStatus, DaemonEvent, DaemonEventCategory, DaemonEventStatus, DaemonHealth, Diagnosis, Effort, Item, ModelAvailability, Profile, QuotaCard, ReviewDiffSlice, Run, RunSnapshot, RunSummary, StatsTable, VerifyLogSlice } from "./types";

export interface AgentsResponse {
  profile: Profile | null;
  stats: StatsTable;
  availability?: Record<string, ModelAvailability>;
  magpieEnabled?: boolean;
  quotas?: QuotaCard[];
  quotaError?: string;
}

export interface ItemPage {
  items: Item[];
  hasMore: boolean;
}

export class ApiError extends Error {
  readonly status: number;
  constructor(message: string, status: number) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

async function parse(res: Response): Promise<unknown> {
  const text = await res.text();
  if (!text) return {};
  try {
    return JSON.parse(text) as unknown;
  } catch {
    return { message: text };
  }
}

function fail(res: Response, body: unknown): never {
  const b = (body ?? {}) as { error?: string; message?: string };
  throw new ApiError(b.message || b.error || `HTTP ${res.status}`, res.status);
}

// The injected token permits reads plus the Web management actions (assign, stop, rename, reorder, trash, restore, delete).
function webHeaders(): Record<string, string> {
  const headers: Record<string, string> = { "Content-Type": "application/json" };
  const token = window.__DROVE_TOKEN__ ?? window.__CO_TOKEN__;
  if (token) {
    headers["x-drove-token"] = token;
    headers["x-co-token"] = token;
  }
  return headers;
}

async function get<T>(url: string): Promise<T> {
  const res = await fetch(url, { headers: webHeaders() });
  const body = await parse(res);
  if (!res.ok) fail(res, body);
  return body as T;
}

async function send(method: "POST" | "PUT" | "PATCH" | "DELETE", url: string, body?: unknown, extra?: Record<string, string>): Promise<unknown> {
  const res = await fetch(url, {
    method,
    headers: { ...webHeaders(), ...extra },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await parse(res);
  if (!res.ok) fail(res, json);
  return json;
}

export function eventsUrl(runId: string): string {
  return `/api/events?run=${encodeURIComponent(runId)}`;
}

export async function fetchRuns(trash = false): Promise<RunSummary[]> {
  if (isMock()) return mockListRuns(trash);
  const body = await get<unknown>(trash ? "/api/runs?trash=1" : "/api/runs");
  if (Array.isArray(body)) return body as RunSummary[];
  if (body && typeof body === "object" && Array.isArray((body as { runs?: unknown }).runs)) {
    return (body as { runs: RunSummary[] }).runs;
  }
  return [];
}

export async function fetchSnapshot(runId: string): Promise<RunSnapshot> {
  if (isMock()) return mockSnapshot(runId);
  return get<RunSnapshot>(`/api/runs/${encodeURIComponent(runId)}`);
}

function asItemPage(body: unknown, limit: number): ItemPage {
  if (Array.isArray(body)) {
    const items = body as Item[];
    return { items, hasMore: items.length >= limit };
  }
  if (body && typeof body === "object") {
    const rec = body as { items?: unknown; hasMore?: unknown };
    const items = Array.isArray(rec.items) ? (rec.items as Item[]) : [];
    return { items, hasMore: typeof rec.hasMore === "boolean" ? rec.hasMore : items.length >= limit };
  }
  return { items: [], hasMore: false };
}

export async function fetchReviewDiff(taskId: string, artifactId: string, opts?: { offset?: number; limit?: number }): Promise<ReviewDiffSlice> {
  if (isMock()) return mockReviewDiff(taskId, artifactId, opts);
  const q = new URLSearchParams({ artifact: artifactId });
  if (opts?.offset != null) q.set("offset", String(opts.offset));
  if (opts?.limit != null) q.set("limit", String(opts.limit));
  return get<ReviewDiffSlice>(`/api/tasks/${encodeURIComponent(taskId)}/review-diff?${q}`);
}

export async function fetchCoordinatorTranscript(runId: string, opts?: { before?: number; limit?: number }): Promise<CoordinatorTranscriptSlice> {
  if (isMock()) return mockCoordinatorTranscript(runId, opts);
  const q = new URLSearchParams();
  if (opts?.limit != null) q.set("limit", String(opts.limit));
  if (opts?.before != null) q.set("before", String(opts.before));
  const suffix = q.toString() ? `?${q}` : "";
  return get<CoordinatorTranscriptSlice>(`/api/runs/${encodeURIComponent(runId)}/coordinator-transcript${suffix}`);
}

export async function fetchVerifyLog(runId: string, artifactId: string, opts?: { offset?: number; limit?: number }): Promise<VerifyLogSlice> {
  if (isMock()) return mockVerifyLog(runId, artifactId, opts);
  const q = new URLSearchParams({ artifact: artifactId });
  if (opts?.offset != null) q.set("offset", String(opts.offset));
  if (opts?.limit != null) q.set("limit", String(opts.limit));
  return get<VerifyLogSlice>(`/api/runs/${encodeURIComponent(runId)}/verify-log?${q}`);
}

export async function fetchItems(workerId: string, before?: number, limit = 200, attemptId?: string): Promise<ItemPage> {
  if (isMock()) return mockItems(workerId, before, limit, attemptId);
  const q = new URLSearchParams({ limit: String(limit) });
  if (before != null) q.set("before", String(before));
  if (attemptId) q.set("attempt", attemptId);
  const body = await get<unknown>(`/api/workers/${encodeURIComponent(workerId)}/items?${q}`);
  return asItemPage(body, limit);
}

export async function assignTask(taskId: string, patch: { agent?: string; model?: string; effort?: Effort; role?: string; worker?: string; fork?: string }): Promise<void> {
  if (isMock()) {
    mockAssign(taskId, patch);
    return;
  }
  await send("PUT", `/api/tasks/${encodeURIComponent(taskId)}/assign`, patch);
}

export async function pauseRun(id: string): Promise<Run> {
  if (isMock()) return mockPauseRun(id);
  return (await send("POST", `/api/runs/${encodeURIComponent(id)}/pause`)) as Run;
}

export async function resumeRun(id: string): Promise<Run> {
  if (isMock()) return mockResumeRun(id);
  return (await send("POST", `/api/runs/${encodeURIComponent(id)}/resume`)) as Run;
}

export async function renameRun(id: string, name: string): Promise<Run> {
  if (isMock()) return mockRenameRun(id, name);
  return (await send("PATCH", `/api/runs/${encodeURIComponent(id)}`, { name })) as Run;
}

export async function reorderRuns(ids: string[]): Promise<void> {
  if (isMock()) {
    mockReorderRuns(ids);
    return;
  }
  await send("POST", "/api/runs/reorder", { ids });
}

export async function trashRun(id: string): Promise<Run> {
  if (isMock()) return mockTrashRun(id);
  return (await send("POST", `/api/runs/${encodeURIComponent(id)}/trash`)) as Run;
}

export async function restoreRun(id: string): Promise<Run> {
  if (isMock()) return mockRestoreRun(id);
  return (await send("POST", `/api/runs/${encodeURIComponent(id)}/restore`)) as Run;
}

export async function deleteRun(id: string): Promise<void> {
  if (isMock()) {
    mockDeleteRun(id);
    return;
  }
  await send("DELETE", `/api/runs/${encodeURIComponent(id)}`, undefined, { "x-drove-confirm": id, "x-co-confirm": id });
}

export async function stopWorker(workerId: string): Promise<void> {
  if (isMock()) {
    mockStop(workerId);
    return;
  }
  await send("POST", `/api/workers/${encodeURIComponent(workerId)}/stop`);
}

let detectedMachine: string | null | undefined;
let resolvingMachine: Promise<string | null> | null = null;

function explicitMachineParam(): string | null {
  if (typeof window === "undefined") return null;
  const sp = new URLSearchParams(window.location.search);
  const on = sp.get("on")?.trim();
  if (on) return on;
  const run = sp.get("run")?.trim();
  if (run && run.includes("/")) return run.split("/")[0] || null;
  return null;
}

function withMachine(path: string, machine: string | null): string {
  if (!machine) return path;
  const sep = path.includes("?") ? "&" : "?";
  return `${path}${sep}on=${encodeURIComponent(machine)}`;
}

async function resolveDefaultMachine(): Promise<string | null> {
  if (detectedMachine !== undefined) return detectedMachine;
  if (!resolvingMachine) {
    resolvingMachine = (async () => {
      try {
        const [localAi, localAgents] = await Promise.all([
          get<Partial<DaemonAiStatus>>("/api/ai/status"),
          get<Partial<AgentsResponse>>("/api/agents"),
        ]);
        if (localAi.configured || Boolean(localAi.baseUrl?.trim()) || Boolean(localAgents.magpieEnabled)) {
          detectedMachine = null;
          return null;
        }
        const [wslAi, wslAgents] = await Promise.all([
          get<Partial<DaemonAiStatus>>("/api/ai/status?on=wsl").catch(() => null),
          get<Partial<AgentsResponse>>("/api/agents?on=wsl").catch(() => null),
        ]);
        if (wslAi && (wslAi.configured || Boolean(wslAi.baseUrl?.trim()) || Boolean(wslAgents?.magpieEnabled))) {
          detectedMachine = "wsl";
          return "wsl";
        }
      } catch {}
      detectedMachine = null;
      return null;
    })().finally(() => {
      resolvingMachine = null;
    });
  }
  return resolvingMachine;
}

async function targetMachine(): Promise<string | null> {
  return explicitMachineParam() ?? await resolveDefaultMachine();
}

export async function fetchAgents(opts?: { refresh?: boolean }): Promise<AgentsResponse> {
  if (isMock()) return mockAgents();
  const machine = await targetMachine();
  const path = withMachine(opts?.refresh ? "/api/agents?refresh=1" : "/api/agents", machine);
  const body = await get<Partial<AgentsResponse>>(path);
  return {
    profile: body.profile ?? null,
    stats: body.stats ?? {},
    ...(typeof body.magpieEnabled === "boolean" ? { magpieEnabled: body.magpieEnabled } : {}),
    ...(body.availability ? { availability: body.availability } : {}),
    ...(body.quotas ? { quotas: body.quotas } : {}),
    ...(body.quotaError ? { quotaError: body.quotaError } : {}),
  };
}

export interface DaemonEventsPage {
  events: DaemonEvent[];
  hasMore: boolean;
}

export async function fetchDaemonHealth(): Promise<DaemonHealth> {
  if (isMock()) return mockDaemonHealth();
  return get<DaemonHealth>("/api/health");
}

export async function fetchAiStatus(): Promise<DaemonAiStatus> {
  if (isMock()) return mockAiStatus();
  const machine = await targetMachine();
  return get<DaemonAiStatus>(withMachine("/api/ai/status", machine));
}

export async function testAiConnection(): Promise<DaemonAiProbeResult> {
  if (isMock()) return mockTestAiConnection();
  const machine = await targetMachine();
  return (await send("POST", withMachine("/api/ai/test", machine))) as DaemonAiProbeResult;
}

export async function saveAiConfig(patch: DaemonAiConfigPatch): Promise<DaemonAiStatus> {
  if (isMock()) return mockSaveAiConfig(patch);
  return (await send("PUT", "/api/ai/config", patch)) as DaemonAiStatus;
}

export async function fetchDaemonEvents(opts?: {
  runId?: string;
  category?: DaemonEventCategory;
  status?: DaemonEventStatus;
  before?: number;
  limit?: number;
}): Promise<DaemonEventsPage> {
  if (isMock()) return mockDaemonEvents(opts);
  const q = new URLSearchParams();
  if (opts?.runId) q.set("runId", opts.runId);
  if (opts?.category) q.set("category", opts.category);
  if (opts?.status) q.set("status", opts.status);
  if (opts?.before != null) q.set("before", String(opts.before));
  if (opts?.limit != null) q.set("limit", String(opts.limit));
  if (!opts?.runId?.includes("/")) {
    const machine = await targetMachine();
    if (machine) q.set("on", machine);
  }
  const suffix = q.toString() ? `?${q}` : "";
  return get<DaemonEventsPage>(`/api/daemon/events${suffix}`);
}

export async function inspectTask(id: string, opts?: { ai?: boolean; refresh?: boolean }): Promise<Diagnosis> {
  if (isMock()) return mockInspectTask(id, opts);
  const q = new URLSearchParams();
  if (opts?.ai === true) q.set("ai", "1");
  else if (opts?.ai === false) q.set("ai", "0");
  if (opts?.refresh) q.set("refresh", "1");
  const suffix = q.toString() ? `?${q}` : "";
  return get<Diagnosis>(`/api/tasks/${encodeURIComponent(id)}/inspect${suffix}`);
}

export async function saveAgentsProfile(profile: Profile): Promise<AgentsResponse> {
  if (isMock()) return mockSaveProfile(profile);
  const machine = await targetMachine();
  const body = (await send("PUT", withMachine("/api/agents", machine), { profile })) as Partial<AgentsResponse>;
  return {
    profile: body.profile ?? null,
    stats: body.stats ?? {},
    ...(typeof body.magpieEnabled === "boolean" ? { magpieEnabled: body.magpieEnabled } : {}),
    ...(body.availability ? { availability: body.availability } : {}),
    ...(body.quotas ? { quotas: body.quotas } : {}),
    ...(body.quotaError ? { quotaError: body.quotaError } : {}),
  };
}
