/** Hand-synced from src/core/types.ts. The web bundle cannot import the server. */

export type Effort = "low" | "medium" | "high" | "xhigh" | "max";
export type TaskStatus = "pending" | "ready" | "running" | "succeeded" | "failed" | "blocked" | "cancelled" | "skipped";
export type WorkerState = "starting" | "idle" | "busy" | "dead" | "closed";
export type Outcome = "succeeded" | "failed";
export type FailureKind = "agent_error" | "no_done" | "timeout" | "schema" | "cancelled" | "merge_conflict" | "process_exited" | "worker_reported" | "quota_exhausted" | "verification_failed" | "review_rejected";
export type MessageType = "dispatch" | "question" | "escalation" | "failed" | "done" | "budget" | "run_done" | "note" | "reply" | "system";
/** How a task with multiple dependencies is released when some of them end up skipped. */
export type TriggerRule = "all_success" | "any_success";
/** Persistent execution sub-phase of a running task; `Task.phase` stays the display/grouping phase. */
export type TaskPhase = "author" | "verifying" | "review" | "rework" | "integrating";

/** Deterministic gate command run on task completion and again on rework submissions. */
export interface VerifySpec {
  cmd: string;
  timeoutSec?: number;
  /** Total rejections allowed before pausing with needsAction; never skips verification. Default 8. */
  maxRejections?: number;
  /** Rejections with BOTH the same errorHash and an unchanged candidate tree. Default 3. */
  maxStagnantRejections?: number;
}
/** Terminal goal gate policy for a Run (`Run.verify` self-healing loop). */
export interface GoalPolicy {
  autoRepair?: boolean;
  /** Default 3. */
  maxRounds?: number;
}
/** Single-flight finalization state machine; any task mutation bumps `generation`. */
export interface RunFinalization {
  state: "idle" | "verifying" | "repairing" | "passed" | "failed";
  generation: number;
  treeSha?: string;
  verifiedTreeSha?: string;
  commandId?: string;
  repairsUsed: number;
  lastFailure?: string;
  logArtifactId?: string;
}
/** Deterministic command node executed without an agent; `cmd` runs through sh -c. */
export interface ExecSpec {
  cmd?: string;
  program?: string;
  args?: string[];
  env?: Record<string, string>;
  timeoutSec?: number;
  parseJson?: boolean;
}
/** Data-driven fan-out source; the template task becomes a virtual barrier over the expanded instances. */
export interface ForeachFrom {
  task: string;
  path?: string;
}
export interface ReviewSpec {
  agent?: string;
  model?: string;
  effort?: Effort;
  differentAgent?: boolean;
  criteria?: string;
  /** Default 2, allowed 1..5. */
  maxRounds?: number;
}
export interface ReviewVerdict {
  verdict: "approved" | "changes_requested";
  summary: string;
  findings?: Array<{ severity?: string; file?: string; message: string }>;
}
export type ReviewCircuitStage =
  | "normal"
  | "handoff_to_coordinator"
  | "post_handoff_review"
  | "awaiting_user_decision"
  | "cleared";
export interface ReviewRoundRecord {
  round: number;
  verdict: string;
  summary: string;
  attemptId?: string;
  keyFindings?: string[];
  at: string;
  postHandoff?: boolean;
}
export interface ReviewCircuit {
  stage: ReviewCircuitStage;
  reviewRounds: number;
  maxNormalRounds: number;
  handoffToCoordinator?: boolean;
  handoffAt?: string;
  postHandoffReviewUsed?: boolean;
  awaitingUserDecision?: boolean;
  trippedAt?: string;
  userOverrides?: number;
  history?: ReviewRoundRecord[];
}
/** Progress of the adversarial review loop; bound to immutable candidate tree snapshots. */
export interface ReviewState {
  round: number;
  reviewerWorkerId?: string;
  baseTreeSha?: string;
  lastReviewedTreeSha?: string;
  currentTreeSha?: string;
  approvedTreeSha?: string;
  patchArtifactId?: string;
  findings?: ReviewVerdict["findings"];
  lastSummary?: string;
}
/** One review round stored on the attempt that produced it. Historical runs may omit it. */
export interface AttemptReview {
  round: number;
  maxRounds?: number;
  baseTreeSha?: string;
  treeSha?: string;
  patchArtifactId?: string;
  deltaPatchArtifactId?: string;
  verdict?: ReviewVerdict["verdict"];
  summary?: string;
  findings?: ReviewVerdict["findings"];
}
/** One page of a stored review patch (`GET /api/tasks/:id/review-diff`). */
export interface ReviewDiffSlice {
  artifactId: string;
  content: string;
  totalBytes: number;
  truncated: boolean;
  offset: number;
}
/** Persisted claim on a completion report; guards concurrent `done` against races. */
export interface CompletionLease {
  completionId: string;
  payloadHash: string;
  generation: number;
  startedAt: string;
  pid?: number;
}
/** Explicit cache policy; `kind: "exec"` tasks opt in only when this is set. */
export type CacheSpec = boolean | { inputs?: string[]; outputs?: string[] };
/** Native daemon AI policy modes. */
export type AiConflictPolicy = "off" | "warn" | "auto_worktree";
export type AiQuestionMode = "off" | "draft" | "auto_when_offline";
export type AiLoopDetectMode = "off" | "warn" | "steer_once";
export interface AiConfig {
  enabled?: boolean;
  baseUrl?: string;
  model?: string;
  verifyDigest?: boolean;
  handoverCompact?: boolean;
  autoTitle?: boolean;
  taskEnrich?: boolean;
  conflictPolicy?: AiConflictPolicy;
  questionMode?: AiQuestionMode;
  loopDetect?: AiLoopDetectMode;
}
export interface AiUsage {
  calls: number;
  inputTokens: number;
  outputTokens: number;
  cacheHitTokens: number;
  cost: number;
}
export interface AttemptAiDiagnosis {
  at: string;
  basedOnUpdatedAt: string;
  rootCause: string;
  evidence: string[];
  recommendedAction: "retry_same" | "retry_fresh" | "switch_agent" | "coordinator_takeover" | "wait_quota";
  suggestedCommand?: string;
  hint?: string;
}
export interface MessageAiDraft {
  answer: string;
  confidence: number;
  category: "factual_in_context" | "needs_coordinator" | "needs_user_gate";
  sources: string[];
  autoReplied?: boolean;
  createdAt: string;
}
/** Stall signal surfaced to the Web overview bar. */
export interface StallSignal {
  kind: "unanswered_action" | "dead_end_deps" | "capacity_starved" | "spinning_worker";
  summary: string;
  since: string;
  taskIds?: string[];
  messageIds?: string[];
}
export type DaemonEventCategory = "ai" | "gate" | "schedule" | "review";
export type DaemonEventKind =
  | "auto_title"
  | "task_enrich"
  | "verify_digest"
  | "handover_compact"
  | "question_triage"
  | "loop_detect"
  | "deep_diagnose"
  | "ai_probe"
  | "verify_gate"
  | "review_round"
  | "review_handoff"
  | "post_handoff_review"
  | "review_circuit_open"
  | "review_override"
  | "finalization_gate"
  | "conflict_check"
  | "worktree_integrate";
export type DaemonEventStatus = "ok" | "warn" | "fallback" | "failed";
export interface DaemonEvent {
  id: number;
  ts: string;
  runId?: string;
  taskId?: string;
  attemptId?: string;
  workerId?: string;
  messageId?: string;
  category: DaemonEventCategory;
  kind: DaemonEventKind;
  status: DaemonEventStatus;
  title: string;
  detail?: string;
  durationMs?: number;
  tokens?: { input: number; output: number; cacheHit: number };
  cost?: number;
}
export interface DaemonAiStatus {
  enabled: boolean;
  configured: boolean;
  keySource: string;
  keyFilePath: string;
  baseUrl: string;
  protocol?: "openai" | "anthropic";
  model: string;
  effort?: string;
  contextWindow?: string;
  maxTokens?: number;
  language?: string;
  circuitOpen: boolean;
  cooldownUntil?: string;
  consecutiveFailures: number;
  lastError?: string;
  switches: {
    verifyDigest: boolean;
    handoverCompact: boolean;
    autoTitle: boolean;
    taskEnrich: boolean;
    conflictPolicy: AiConflictPolicy;
    questionMode: AiQuestionMode;
    loopDetect: AiLoopDetectMode;
  };
}
export interface DaemonHealth {
  ok: boolean;
  pid: number;
  startedAt: string;
  version?: string;
}
export interface DaemonAiProbeResult {
  ok: boolean;
  latencyMs: number;
  model: string;
  protocol?: "openai" | "anthropic";
  reply?: string;
  error?: string;
}
export interface DaemonAiConfigPatch {
  enabled?: boolean;
  baseUrl?: string;
  model?: string;
  effort?: string;
  contextWindow?: string;
  maxTokens?: number;
  language?: string;
  apiKey?: string;
  clearKey?: boolean;
  verifyDigest?: boolean;
  handoverCompact?: boolean;
  autoTitle?: boolean;
  taskEnrich?: boolean;
  conflictPolicy?: AiConflictPolicy;
  questionMode?: AiQuestionMode;
  loopDetect?: AiLoopDetectMode;
}

export interface Spec {
  target: string;
  change: string;
  constraints?: string;
  ownership?: string;
  acceptance: string;
  verify?: string | VerifySpec;
}
export interface Assign {
  agent?: string;
  model?: string;
  effort?: Effort;
  worker?: string;
  reason?: string;
  source?: "plan" | "user" | "retry";
  role?: string;
  fork?: string;
}
export interface Run {
  id: string;
  objective: string;
  cwd: string;
  coordinator: {
    host: string;
    hostSession: string;
    generation: number;
    agent?: string;
    model?: string;
    hostPid?: number;
    notification?: { id: string; threadId: string; codexHome?: string; lastSeq: number; error?: string };
  };
  maxConcurrency: number;
  budget?: { tokens?: number; cost?: number };
  status: "active" | "paused" | "done" | "archived";
  /** Why a paused run stopped; a "user" pause is never auto-resumed by budget recovery. */
  pauseReason?: "user" | "budget";
  /** Terminal goal gate applied once every task has settled. */
  verify?: string | VerifySpec;
  goal?: GoalPolicy;
  finalization?: RunFinalization;
  /** Set when `Run.verify` last passed on the current tree. */
  verifiedAt?: string;
  /** Display name. The objective is shown when this is absent. */
  name?: string;
  /** Manual list position. Runs without it sort after numbered ones, newest first. */
  order?: number;
  /** ISO time. Present while the run is in the trash. */
  trashedAt?: string;
  /** Status restored when the run leaves the trash. Trash itself pauses scheduling. */
  statusBeforeTrash?: "active" | "paused" | "done" | "archived";
  /** Cumulative daemon-side AI token & cost usage for this Run. */
  aiUsage?: AiUsage;
  /** Remote SSH machine name when federated onto local Drove Web. */
  machine?: string;
  createdAt: string;
  updatedAt: string;
}
export interface RunSummary extends Run { taskCounts: Record<TaskStatus, number>; }
/**
 * Task branch: waits for another task's result data at `path` (default the whole `data`).
 */
export type TaskWhen = { task: string; path?: string; equals?: unknown; in?: unknown[]; exists?: boolean };
export interface TaskResult {
  summary: string;
  compactSummary?: string;
  data?: unknown;
  filesModified?: string[];
  /** Structured handover reasons; never trimmed from coordinator notifications. */
  blockers?: string[];
  /** sha256 of declared output files at completion time; cache hits re-verify against these. */
  outputFileHashes?: Record<string, string>;
}
export interface Task {
  id: string; runId: string; key: string; title: string; phase?: string;
  spec: Spec; tags: string[]; files: string[]; deps: string[];
  outputSchema?: object; isolation: "none" | "worktree";
  timeoutSec?: number; stallWarnSec?: number; stallKillSec?: number;
  when?: TaskWhen;
  assign?: Assign; status: TaskStatus;
  result?: TaskResult;
  failures: number; createdAt: string; updatedAt: string;
  /** "exec" runs a deterministic command without an agent; default "agent". */
  kind?: "agent" | "exec";
  /** Runtime sub-phase; kept off `phase`, which names the display/grouping phase. */
  execPhase?: TaskPhase;
  /** Bumped on every async boundary crossing so stale callbacks cannot land. */
  executionVersion?: number;
  triggerRule?: TriggerRule;
  context?: Record<string, unknown>;
  exec?: ExecSpec;
  foreachFrom?: ForeachFrom;
  /** Virtual barrier node expanded at runtime from `foreachFrom`. */
  template?: boolean;
  expansionState?: "pending" | "expanded" | "done";
  expandedFrom?: string;
  review?: ReviewSpec;
  reviewState?: ReviewState;
  reviewCircuit?: ReviewCircuit;
  /** Author output held until verify + review pass; promoted to `result` on integration. */
  candidateResult?: TaskResult;
  cache?: CacheSpec;
  cachedFrom?: string;
}
/** True when the corresponding figure was estimated rather than reported by the adapter. */
export interface UsageEstimate { tokens?: boolean; cost?: boolean }
export interface Attempt {
  id: string; taskId: string; runId: string; workerId: string;
  agent: string; model?: string; effort?: Effort;
  routeReason: string; reused: boolean; retryOf?: string; hint?: string;
  startedAt: string; endedAt?: string;
  /** First recovery timestamp; retained across subsequent daemon restarts. */
  awaitingLateDone?: string;
  tokens: { input: number; output: number; cacheRead: number }; cost?: number;
  estimated?: UsageEstimate;
  outcome?: Outcome; failure?: { kind: FailureKind; message: string };
  /** Output reported or settled during this specific attempt. */
  result?: TaskResult;
  /** "exec" attempts use the literal workerId "exec"; "review" attempts are reviewer turns. */
  kind?: "author" | "review" | "exec";
  /** Verdict and patch ids for this review turn. Absent on older runs; fall back to `Task.reviewState`. */
  reviewRound?: AttemptReview;
  verifying?: CompletionLease;
  verifyRejections?: number;
  stagnantRejections?: number;
  lastVerifyHash?: string;
  lastTreeSha?: string;
  verifyLogArtifactId?: string;
  verifyAiDigest?: string;
  aiDiagnosis?: AttemptAiDiagnosis;
  spinningDetectedAt?: string;
}
export interface Worker {
  id: string; runId: string; agent: string; model?: string; effort?: Effort;
  sessionKey: string; h2Agent?: string; cwd: string; worktree?: string;
  state: WorkerState; currentAttemptId?: string;
  /** Host PID of the detached ACP supervisor (outside the sandbox PID namespace). */
  pid?: number;
  fingerprint: Fingerprint;
  context: { used: number; size: number };
  totals: { tokens: number; cost: number; attempts: number; estimated?: UsageEstimate };
  createdAt: string; lastActiveAt: string;
  role?: string;
  forkedFrom?: string;
  /** Preamble generation the live session last received; 0/gap means resend in full. */
  protocolGen?: number;
  /** Read-only candidate view bound for a reviewer worker. */
  readOnlyDir?: string;
}
export interface Fingerprint { files: string[]; tags: string[]; taskIds: string[]; }
export interface Message {
  id: string; seq: number; runId: string; type: MessageType; from: string; to: string;
  threadId?: string; taskId?: string; attemptId?: string;
  subject: string; body?: string; options?: string[]; recommend?: string; why?: string; raisedBy?: "plan" | "coordinator" | "worker"; raisedByWorker?: string;
  /** Free-form label for board notes (e.g. `status`, `risk`). */
  tag?: string;
  aiDraft?: MessageAiDraft;
  needsAction: boolean; handledAt?: string; createdAt: string;
}
export interface Item {
  id: number; workerId: string; attemptId?: string; ts: string;
  kind: "user" | "assistant" | "thought" | "tool" | "usage" | "plan" | "system";
  text?: string;
  tool?: { id: string; title?: string; kind?: string; status?: string; locations?: string[]; input?: unknown; output?: string };
  usage?: { used?: number; size?: number; cost?: number; input?: number; output?: number; cacheRead?: number };
}

export type ProfileTag = "planning" | "architecture" | "backend" | "frontend" | "ui" | "refactor" | "debugging" | "tests" | "docs" | "review" | "data" | "scripting" | "devops" | "research" | "long-context" | "quick-edits" | "bulk-mechanical" | "diagrams";
export const PROFILE_TAGS: ProfileTag[] = [
  "planning", "architecture", "backend", "frontend", "ui", "refactor", "debugging",
  "tests", "docs", "review", "data", "scripting", "devops", "research",
  "long-context", "quick-edits", "bulk-mechanical", "diagrams",
];
export interface AvoidWindow {
  start: string;
  end: string;
  tz?: string;
  reason?: string;
}
export interface ModelQuotaConfig {
  providers: string[];
  pool?: string;
  windows?: string[];
}
export interface ModelBlock {
  kind: "window" | "quota";
  reason: string;
  retryAtMs?: number;
}
export interface ModelAvailability {
  available: boolean;
  block?: ModelBlock;
  detail?: string;
}
export interface QuotaWindow {
  unlimited?: boolean;
  name: string;
  used?: number;
  remaining: number;
  resetsAt?: string;
  display?: string;
  text?: boolean;
  pool?: string;
}
export interface QuotaCard {
  provider: string;
  name?: string;
  kind?: string;
  plan?: string;
  user?: string;
  windows?: QuotaWindow[];
  balance?: string;
  error?: string;
  asOf?: string;
}
export interface ProfileModel {
  id: string; default?: boolean; summary?: string; bestFor?: string[];
  cost?: "low" | "medium" | "high"; speed?: "slow" | "medium" | "fast";
  contextWindow?: number; maxInstances?: number;
  supportedEfforts?: Effort[];
  price?: { input: number; output: number; cacheRead?: number };
  sessionConfig?: Record<string, string>;
  verified?: { ok: boolean; at: string; error?: string };
  avoidWindows?: AvoidWindow[];
  quota?: ModelQuotaConfig;
}
export interface ProfileAgent {
  id: string; launch: { acpx?: string; command?: string };
  modelSelection: "session" | "none"; modelEnv?: string | null; effortKey?: string | null;
  supportedEfforts?: Effort[];
  summary?: string; bestFor?: string[]; avoid?: string[]; maxInstances?: number;
  sessionConfig?: Record<string, string>;
  isolateSecrets?: boolean;
  models: ProfileModel[];
}
export type BuiltInWorkerRole = "coder" | "tester" | "writer" | "reviewer" | "researcher";
export type RoleAccessMode = "readonly" | "scoped_write" | "writable";
export interface RoleCandidateConfig {
  agent: string;
  model?: string | null;
  effort?: Effort;
  fast?: boolean;
  context?: string | null;
  strengths?: string[];
  note?: string | null;
}
export interface RoleProfileConfig {
  label?: string;
  summary?: string;
  tags?: string[];
  agents?: string[];
  candidates?: RoleCandidateConfig[];
  model?: string;
  effort?: Effort;
  access?: RoleAccessMode;
  allowedPaths?: string[];
  prompt?: string;
}
export interface Profile {
  version: number; updatedAt: string;
  note?: string | null;
  defaults?: {
    maxInstances?: number;
    idleTtlSec?: number;
    stallWarnSec?: number;
    stallKillSec?: number;
    attemptTimeoutSec?: number;
    lateDoneTtlSec?: number;
    nudgeTimes?: number;
    nudgeDelayMs?: number;
    ai?: AiConfig;
  };
  agents: ProfileAgent[];
  roles?: Record<string, RoleProfileConfig>;
}
export interface PlanTask {
  key: string; title: string; phase?: string; spec: Spec;
  tags?: string[]; files?: string[]; deps?: string[]; assign?: Assign;
  outputSchema?: object; isolation?: "none" | "worktree";
  timeoutSec?: number; stallWarnSec?: number; stallKillSec?: number;
  when?: TaskWhen;
  kind?: "agent" | "exec"; triggerRule?: TriggerRule;
  context?: Record<string, unknown>;
  exec?: ExecSpec; foreachFrom?: ForeachFrom;
  template?: boolean; expandedFrom?: string;
  review?: ReviewSpec; cache?: CacheSpec;
  foreach?: string[];
}
export interface PlanDoc {
  objective?: string; defaults?: Assign & { isolation?: "none" | "worktree" };
  phases?: string[]; tasks: Record<string, Omit<PlanTask, "key">>;
  verify?: string | VerifySpec;
  goal?: GoalPolicy;
}
export interface Candidate {
  kind: "reuse" | "new"; workerId?: string; agent: string; model?: string; effort?: Effort;
  score: number; parts: Record<string, number>; reason: string;
}
export interface RouteDecision { choice: Candidate; candidates: Candidate[]; waitFor?: { workerId: string; untilMs: number }; }
export interface RouterConfig { reuseThreshold: number; contextSoft: number; contextHard: number; waitBusyMs: number; }
export interface Diagnosis {
  task: Task; attempt?: Attempt; failure?: Attempt["failure"]; routeReason?: string;
  lastItems: Item[]; filesModified: string[]; gitStatus: string; suggestion: string;
  ai?: AttemptAiDiagnosis;
}
export interface StatsEntry {
  n: number; success: number; avgTokens: number; avgCost: number; avgDurationMs: number; retryRate: number; noDoneRate?: number;
  estimated?: UsageEstimate;
}
export type StatsTable = Record<string, StatsEntry>;
export interface RunSnapshot {
  run: Run; tasks: Task[]; workers: Worker[]; attempts: Attempt[];
  pendingMessages: Message[]; messages?: Message[]; stats: { tokens: number; cost: number; workerTokens?: number; workerCost?: number; aiTokens?: number; aiCost?: number };
  /** Stall signals computed by the daemon for the overview bar. */
  stalls?: StallSignal[];
}

/** One message as a hop on the canvas: which node sent it, which received it. */
export interface TalkHop {
  id: string;
  /** canvas selection ids (`coord:run` / `worker:<id>` / `<taskId>`); null = not on the canvas */
  from: string | null;
  to: string | null;
  type: MessageType;
  subject: string;
  /** a reply to an earlier message in the same thread */
  reply: boolean;
  /** the task the message is about (highlighted with its two ends) */
  taskId: string | null;
}

/** A thread being replayed on the canvas, hop by hop (oldest first). */
export interface TalkPlay {
  /** the thread's root message id */
  key: string;
  /** new on every click — replays from the first hop even for the same thread */
  nonce: number;
  hops: TalkHop[];
}

export interface MessageParty {
  role: "coord" | "worker" | "reviewer" | "task" | "system";
  name: string | null;
  workerNum?: number;
  /** Set when the address is a reviewer session rather than an authoring worker. */
  reviewer?: boolean;
  reviewerNum?: number;
  task: string | null;
  select: string | null;
  raw: string;
}
export interface ChangeEvent { table: string; id: string | number; runId?: string; }
export interface DaemonInfo { pid: number; port: number; token: string; startedAt: string; readToken?: string; }
export type TurnResult = { status: "completed" | "cancelled" | "failed"; error?: { message: string; code?: string } };

/** Canvas layout. Labels are i18n keys. */
export type LayoutKind = "layered-lr" | "layered-tb";
export const LAYOUTS: { kind: LayoutKind; label: string; title: string }[] = [
  { kind: "layered-lr", label: "layout.lr", title: "layout.lr.title" },
  { kind: "layered-tb", label: "layout.tb", title: "layout.tb.title" },
];

export const EFFORTS: Effort[] = ["low", "medium", "high", "xhigh", "max"];

/** PUT /api/tasks/:id/assign accepts only pending or ready, and only before any attempt exists. */
export const ASSIGNABLE = new Set<TaskStatus>(["pending", "ready"]);

export interface HostTokens {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
  total: number;
}

export interface HostTranscriptItem {
  index: number;
  kind: "user" | "assistant" | "thought" | "tool";
  text: string;
  title?: string;
  detail?: string;
  status?: string;
  ts?: string;
}

export interface CoordinatorTranscriptSlice {
  available: boolean;
  host: string;
  agent?: string;
  hostSession: string;
  match?: "session-id" | "run-id" | "cwd-and-time";
  sourceFile?: string;
  models: string[];
  tokens?: HostTokens;
  firstTs?: string;
  lastTs?: string;
  total: number;
  start: number;
  hasMore: boolean;
  items: HostTranscriptItem[];
}

export interface VerifyLogSlice {
  artifact: string;
  content: string;
  totalBytes: number;
  truncated: boolean;
  offset: number;
}
