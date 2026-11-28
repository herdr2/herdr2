import type {
  Attempt,
  CoordinatorTranscriptSlice,
  DaemonAiProbeResult,
  DaemonAiStatus,
  DaemonEvent,
  DaemonEventCategory,
  DaemonEventStatus,
  DaemonHealth,
  Diagnosis,
  Effort,
  Item,
  Message,
  ModelAvailability,
  Profile,
  QuotaCard,
  Run,
  RunSummary,
  ReviewDiffSlice,
  RunSnapshot,
  StallSignal,
  StatsTable,
  Task,
  VerifyLogSlice,
  Worker,
} from "./types";

/** `?mock=1` renders this fixture so the UI can be developed without a daemon. */
export function isMock(): boolean {
  if (typeof location === "undefined") return false;
  return new URLSearchParams(location.search).get("mock") === "1";
}

const T0 = "2026-11-06T04:00:00.000Z";
const T1 = "2026-11-06T04:40:00.000Z";
const T2 = "2026-11-06T05:10:00.000Z";
const T3 = "2026-11-06T05:40:00.000Z";
const T4 = "2026-11-06T06:05:00.000Z";
const T5 = "2026-11-06T06:25:00.000Z";

function spec(target: string, change: string, acceptance: string, extra?: Partial<Task["spec"]>): Task["spec"] {
  return { target, change, acceptance, ...extra };
}

const demoRun: Run = {
  id: "run_demo0001",
  name: "可观察编排 MVP",
  objective: "给 co 补上可观察的多 agent 编排",
  cwd: "/home/zzh0838/Work/KnowBase/AI2D/co-agent",
  coordinator: { host: "claude-code", hostSession: "sess_demo0001", generation: 2 },
  maxConcurrency: 4,
  budget: { tokens: 2_000_000, cost: 20 },
  status: "active",
  verify: { cmd: "npm test", timeoutSec: 600 },
  goal: { autoRepair: true, maxRounds: 3 },
  finalization: { state: "repairing", generation: 2, repairsUsed: 1, lastFailure: "web build failed", logArtifactId: "fin_demo_verify_1" },
  aiUsage: { calls: 4, inputTokens: 18_400, outputTokens: 2_150, cacheHitTokens: 9_600, cost: 0.0086 },
  createdAt: T0,
  updatedAt: T5,
};

const doneRun: Run = {
  id: "run_done0002",
  objective: "整理仓库说明",
  cwd: "/home/zzh0838/Work/KnowBase/AI2D/co-agent",
  coordinator: { host: "codex", hostSession: "sess_done0002", generation: 1 },
  maxConcurrency: 2,
  status: "done",
  createdAt: "2026-11-05T02:00:00.000Z",
  updatedAt: "2026-11-05T03:12:00.000Z",
};

function task(partial: Task): Task {
  return partial;
}

const tasks: Task[] = [
  task({
    id: "t_spec0001",
    runId: demoRun.id,
    key: "spec",
    title: "写 TECH_SPEC",
    phase: "design",
    spec: spec("docs/TECH_SPEC.md", "把类型、HTTP 和调度契约写清楚", "文中类型与实现对得上", {
      constraints: "不改已发布字段名",
      ownership: "docs/TECH_SPEC.md",
    }),
    tags: ["architecture", "docs"],
    files: ["docs/TECH_SPEC.md"],
    deps: [],
    isolation: "none",
    assign: { agent: "antigravity", model: "gemini-4-argon", effort: "high", reason: "定契约与文档", source: "plan" },
    status: "succeeded",
    result: {
      summary: "规格已定稿，完整覆盖核心数据类型、SQLite 8 张表结构、HTTP REST 与 SSE 事件协议，以及调度器生命周期契约。",
      compactSummary: "规格已定稿，覆盖类型、SQLite 表与 HTTP/SSE 契约。",
      filesModified: ["docs/TECH_SPEC.md"],
    },
    failures: 0,
    createdAt: T0,
    updatedAt: T1,
  }),
  task({
    id: "t_store001",
    runId: demoRun.id,
    key: "store",
    title: "落地 SQLite",
    phase: "core",
    spec: spec("src/core/store.ts", "按规格建表并提供 CRUD", "单测能写入并读回 Run 与 Task"),
    tags: ["backend", "data"],
    files: ["src/core/store.ts"],
    deps: ["t_spec0001"],
    isolation: "none",
    assign: { agent: "codex", model: "gpt-6.1-sol", effort: "high", source: "plan" },
    status: "succeeded",
    result: { summary: "八张表和事件总线已接上。", filesModified: ["src/core/store.ts"], data: { backend: "sqlite", modules: ["daemon", "cli"] } },
    failures: 0,
    createdAt: T0,
    updatedAt: T2,
  }),
  task({
    id: "t_daemon01",
    runId: demoRun.id,
    key: "daemon",
    title: "实现 daemon HTTP",
    phase: "core",
    spec: spec("src/daemon", "提供 /api 与 SSE", "健康检查、快照和事件流可用", { ownership: "src/daemon/**" }),
    tags: ["backend"],
    files: ["src/daemon/server.ts"],
    deps: ["t_store001"],
    isolation: "none",
    assign: { agent: "antigravity", model: "gemini-4-hydrogen", effort: "high", role: "coder", source: "plan" },
    status: "running",
    failures: 0,
    createdAt: T0,
    updatedAt: T5,
  }),
  task({
    id: "t_cli00001",
    runId: demoRun.id,
    key: "cli",
    title: "实现 co CLI",
    phase: "core",
    spec: spec("src/cli", "注册 run、plan、task、wait 等命令", "co run create 能打到 daemon"),
    tags: ["backend"],
    files: ["src/cli/index.ts"],
    deps: ["t_store001"],
    isolation: "none",
    assign: { agent: "claude", model: "zcode/GLM-5.3[1m]", effort: "high", reason: "第一编码主力", source: "plan" },
    status: "ready",
    failures: 0,
    createdAt: T0,
    updatedAt: T2,
  }),
  task({
    id: "t_web00001",
    runId: demoRun.id,
    key: "web",
    title: "移植编排前端",
    phase: "ui",
    spec: spec("web/**", "DAG、四种面板和 Agents 页", "mock=1 能看到各类节点", {
      constraints: "只读监控看板，不在网页内直接回复问题",
      ownership: "web/**",
    }),
    tags: ["frontend", "ui"],
    files: ["web/src/App.tsx"],
    deps: ["t_daemon01", "t_cli00001"],
    isolation: "none",
    assign: { agent: "antigravity", model: "gemini-4-hydrogen", effort: "high", role: "coder", source: "plan" },
    status: "running",
    execPhase: "author",
    failures: 0,
    createdAt: T0,
    updatedAt: T5,
  }),
  task({
    id: "t_test0001",
    runId: demoRun.id,
    key: "test",
    title: "补集成测试",
    phase: "verify",
    spec: spec("test/", "用 mock ACP agent 跑通一次派发", "node --test 通过"),
    tags: ["tests"],
    files: ["test/daemon.test.mjs"],
    deps: ["t_daemon01"],
    isolation: "none",
    assign: { agent: "antigravity", model: "gemini-4-hydrogen", effort: "medium", role: "tester", source: "plan" },
    status: "failed",
    failures: 2,
    createdAt: T0,
    updatedAt: T4,
  }),
  task({
    id: "t_docs0001",
    runId: demoRun.id,
    key: "docs",
    title: "写使用说明",
    phase: "docs",
    spec: spec("README.md", "说明如何启动 daemon 和打开 web", "新人能按文档跑起来"),
    tags: ["docs"],
    files: ["README.md"],
    deps: ["t_web00001"],
    assign: { agent: "antigravity", model: "gemini-4-carbon", effort: "medium", role: "writer", source: "plan" },
    isolation: "none",
    status: "pending",
    failures: 0,
    createdAt: T0,
    updatedAt: T0,
  }),
  task({
    id: "t_clean001",
    runId: demoRun.id,
    key: "cleanup",
    title: "清理旧入口",
    phase: "cleanup",
    spec: spec("src/cli.ts", "删除旧的单文件入口", "新代码不再引用旧文件"),
    tags: ["refactor"],
    files: ["src/cli.ts"],
    deps: ["t_docs0001"],
    isolation: "none",
    status: "cancelled",
    failures: 0,
    createdAt: T0,
    updatedAt: T3,
  }),
  task({
    id: "t_db000001",
    runId: demoRun.id,
    key: "store-db",
    title: "落库到数据库",
    phase: "core",
    spec: spec("src/core/store.ts", "按数据库方案落地", "表结构可读写"),
    tags: ["backend", "data"],
    files: ["src/core/store.ts"],
    deps: ["t_store001"],
    isolation: "none",
    when: { task: "t_store001", path: "backend", equals: "sqlite" },
    status: "ready",
    failures: 0,
    createdAt: T2,
    updatedAt: T3,
  }),
  task({
    id: "t_file0001",
    runId: demoRun.id,
    key: "store-file",
    title: "落库到文件",
    phase: "core",
    spec: spec("src/core/store.ts", "按文件方案落地", "文件可读写"),
    tags: ["backend"],
    files: ["src/core/store.ts"],
    deps: ["t_store001"],
    isolation: "none",
    when: { task: "t_store001", path: "backend", equals: "file" },
    status: "skipped",
    result: { summary: "数据条件未命中" },
    failures: 0,
    createdAt: T2,
    updatedAt: T3,
  }),
  task({
    id: "t_exec0001",
    runId: demoRun.id,
    key: "lint",
    title: "跑 lint",
    phase: "verify",
    spec: spec("package.json", "执行确定性 lint，命中缓存则不跑 agent", "退出码为 0"),
    tags: ["tests"],
    files: ["package.json"],
    deps: ["t_store001"],
    isolation: "none",
    kind: "exec",
    exec: { cmd: "npm test", timeoutSec: 120 },
    status: "succeeded",
    cachedFrom: "cache:lint:9f3a",
    result: { summary: "缓存命中，未再执行。", data: { ok: true } },
    failures: 0,
    createdAt: T2,
    updatedAt: T3,
  }),
  task({
    id: "t_tpl0001",
    runId: demoRun.id,
    key: "fanout",
    title: "按模块展开",
    phase: "core",
    spec: spec("src", "把 store 结果里的 modules 展开成子任务屏障", "子实例全部收口后屏障才成功"),
    tags: ["backend"],
    files: [],
    deps: ["t_store001"],
    isolation: "none",
    template: true,
    foreachFrom: { task: "store", path: "modules" },
    expansionState: "expanded",
    status: "running",
    failures: 0,
    createdAt: T3,
    updatedAt: T5,
  }),
  task({
    id: "t_rev0001",
    runId: demoRun.id,
    key: "review-spec",
    title: "审查规格改动",
    phase: "verify",
    spec: spec("docs/TECH_SPEC.md", "异构审查作者提交", "审查通过或打回"),
    tags: ["review"],
    files: ["docs/TECH_SPEC.md"],
    deps: ["t_spec0001"],
    isolation: "none",
    status: "running",
    execPhase: "review",
    review: { agent: "codex", differentAgent: true, maxRounds: 2, criteria: "契约与实现对得上" },
    reviewState: {
      round: 1,
      reviewerWorkerId: "w_review01",
      baseTreeSha: "base0001aaa",
      currentTreeSha: "tree_rev0001aaa",
      patchArtifactId: "art_rev_r1",
    },
    failures: 0,
    createdAt: T3,
    updatedAt: T5,
  }),
  task({
    id: "t_revfail1",
    runId: demoRun.id,
    key: "review-fail",
    title: "审查未通过的改动",
    phase: "verify",
    spec: spec("src/daemon/server.ts", "补上暂停按钮文案", "审查通过后才算完成"),
    tags: ["backend"],
    files: ["src/daemon/server.ts"],
    deps: ["t_daemon01"],
    isolation: "worktree",
    assign: { agent: "antigravity", model: "gemini-4-hydrogen", effort: "high", role: "coder", source: "plan" },
    status: "failed",
    failures: 1,
    review: { agent: "codex", differentAgent: true, maxRounds: 2, criteria: "暂停语义和文案一致，不能把预算暂停写成手动暂停" },
    reviewState: {
      round: 2,
      reviewerWorkerId: "w_review01",
      baseTreeSha: "tree_revfail_r1",
      lastReviewedTreeSha: "tree_revfail_r2",
      currentTreeSha: "tree_revfail_r2",
      patchArtifactId: "art_revfail_r2",
      lastSummary: "两轮都没把预算暂停和手动暂停区分开",
      findings: [
        { severity: "error", file: "web/src/App.tsx", message: "暂停按钮仍只有一种文案" },
        { severity: "warning", file: "web/src/i18n.ts", message: "缺少预算暂停的说明" },
      ],
    },
    createdAt: T3,
    updatedAt: T5,
  }),
  task({
    id: "t_revok01",
    runId: demoRun.id,
    key: "review-ok",
    title: "两轮审查已通过",
    phase: "verify",
    spec: spec("web/src/i18n.ts", "把预算暂停和手动暂停分成两套文案后再交给审查", "审查通过后才算完成"),
    tags: ["frontend"],
    files: ["web/src/i18n.ts"],
    deps: ["t_spec0001"],
    isolation: "none",
    assign: { agent: "cursor", model: "grok-4.7", effort: "high", source: "plan", role: "author" },
    status: "succeeded",
    result: { summary: "两轮审查通过，预算暂停和手动暂停已经分开。", filesModified: ["web/src/i18n.ts"] },
    failures: 0,
    review: { agent: "codex", differentAgent: true, maxRounds: 2, criteria: "预算暂停与手动暂停的文案必须分开" },
    reviewState: {
      round: 2,
      reviewerWorkerId: "w_review01",
      baseTreeSha: "tree_ok_r1",
      lastReviewedTreeSha: "tree_ok_r2",
      currentTreeSha: "tree_ok_r2",
      approvedTreeSha: "tree_ok_r2",
      patchArtifactId: "art_revok_r2",
      lastSummary: "文案已分开，审查通过",
      findings: [],
    },
    createdAt: T4,
    updatedAt: T5,
  }),
  task({
    id: "t_vfail001",
    runId: demoRun.id,
    key: "typecheck",
    title: "类型检查没过",
    phase: "verify",
    spec: spec("src", "跑 tsc", "类型检查通过", { verify: "npx tsc -p . --noEmit" }),
    tags: ["tests"],
    files: [],
    deps: ["t_daemon01"],
    isolation: "none",
    assign: { agent: "codex", model: "gpt-6.1-sol", effort: "medium", source: "plan" },
    status: "failed",
    failures: 1,
    createdAt: T4,
    updatedAt: T5,
  }),
  task({
    id: "t_skip0001",
    runId: demoRun.id,
    key: "skip-file",
    title: "文件方案跳过",
    phase: "core",
    spec: spec("src/core/store.ts", "仅当存储后端是 file 时执行", "未命中则 skipped"),
    tags: ["backend"],
    files: ["src/core/store.ts"],
    deps: ["t_store001"],
    isolation: "none",
    when: { task: "store", path: "backend", equals: "file" },
    status: "skipped",
    result: { summary: "分支未选择：数据未命中" },
    failures: 0,
    createdAt: T3,
    updatedAt: T4,
  }),
  task({
    id: "t_join0001",
    runId: demoRun.id,
    key: "join-db",
    title: "数据库方案汇合",
    phase: "core",
    spec: spec("src/core/store.ts", "存储后端为 sqlite 时继续", "数据分支命中"),
    tags: ["backend", "data"],
    files: ["src/core/store.ts"],
    deps: ["t_store001"],
    isolation: "none",
    triggerRule: "any_success",
    when: { task: "store", path: "backend", equals: "sqlite" },
    status: "ready",
    failures: 0,
    createdAt: T3,
    updatedAt: T4,
  }),
  task({
    id: "t_readme01",
    runId: doneRun.id,
    key: "readme",
    title: "改 README 标题",
    spec: spec("README.md", "把标题改成 co", "标题是 co"),
    tags: ["docs"],
    files: ["README.md"],
    deps: [],
    isolation: "none",
    assign: { agent: "antigravity", model: "gemini-4-argon", effort: "high", source: "plan" },
    status: "succeeded",
    result: { summary: "标题已改。", filesModified: ["README.md"] },
    failures: 0,
    createdAt: doneRun.createdAt,
    updatedAt: doneRun.updatedAt,
  }),
];

const workers: Worker[] = [
  {
    id: "w_idle0001",
    runId: demoRun.id,
    agent: "antigravity",
    model: "gemini-4-carbon",
    effort: "high",
    sessionKey: "co-w_idle0001",
    cwd: "/tmp/co-review-w_idle0001",
    state: "idle",
    fingerprint: { files: ["docs/TECH_SPEC.md"], tags: ["research", "architecture"], taskIds: ["t_spec0001"] },
    context: { used: 42_000, size: 1_048_576 },
    totals: { tokens: 88_000, cost: 0.62, attempts: 1 },
    createdAt: T0,
    lastActiveAt: T1,
    role: "researcher",
    readOnlyDir: "/tmp/co-review-w_idle0001",
  },
  {
    id: "w_closed01",
    runId: demoRun.id,
    agent: "antigravity",
    model: "gemini-4-hydrogen",
    effort: "high",
    sessionKey: "co-w_closed01",
    cwd: demoRun.cwd,
    state: "closed",
    fingerprint: { files: ["src/core/store.ts"], tags: ["backend", "data"], taskIds: ["t_store001"] },
    context: { used: 70_000, size: 1_048_576 },
    totals: { tokens: 120_000, cost: 0.48, attempts: 1, estimated: { tokens: true, cost: true } },
    createdAt: T1,
    lastActiveAt: T2,
    role: "coder",
  },
  {
    id: "w_busy0001",
    runId: demoRun.id,
    agent: "antigravity",
    model: "gemini-4-hydrogen",
    effort: "high",
    sessionKey: "co-w_busy0001",
    cwd: demoRun.cwd,
    state: "busy",
    currentAttemptId: "a_daemon01",
    fingerprint: {
      files: ["test/daemon.test.mjs", "src/daemon/server.ts"],
      tags: ["tests", "backend"],
      taskIds: ["t_test0001", "t_daemon01"],
    },
    context: { used: 155_000, size: 1_048_576 },
    totals: { tokens: 82_000, cost: 0.26, attempts: 2 },
    createdAt: T1,
    lastActiveAt: T5,
    role: "tester",
    forkedFrom: "w_idle0001",
  },
  {
    id: "w_dead0001",
    runId: demoRun.id,
    agent: "antigravity",
    model: "gemini-4-carbon",
    effort: "medium",
    sessionKey: "co-w_dead0001",
    cwd: demoRun.cwd,
    state: "dead",
    fingerprint: { files: ["docs/GUIDE.md"], tags: ["docs"], taskIds: ["t_test0001"] },
    context: { used: 10_000, size: 1_048_576 },
    totals: { tokens: 22_000, cost: 0.06, attempts: 1 },
    createdAt: T3,
    lastActiveAt: T4,
    role: "writer",
  },
  {
    id: "w_start001",
    runId: demoRun.id,
    agent: "antigravity",
    model: "gemini-4-hydrogen",
    effort: "high",
    sessionKey: "co-w_start001",
    cwd: demoRun.cwd,
    state: "busy",
    currentAttemptId: "a_web00001",
    fingerprint: { files: ["web/src/App.tsx"], tags: ["frontend", "ui"], taskIds: ["t_web00001"] },
    context: { used: 62_000, size: 1_000_000 },
    totals: { tokens: 19_000, cost: 0.08, attempts: 1 },
    createdAt: T5,
    lastActiveAt: T5,
    role: "coder",
    forkedFrom: "coordinator",
  },
  {
    id: "w_review01",
    runId: demoRun.id,
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    sessionKey: "co-w_review01",
    cwd: "/tmp/co-review-w_review01",
    state: "busy",
    currentAttemptId: "a_rev_r1",
    fingerprint: { files: ["docs/TECH_SPEC.md"], tags: ["review"], taskIds: ["t_rev0001", "t_revfail1", "t_revok01"] },
    context: { used: 48_000, size: 500_000 },
    totals: { tokens: 36_000, cost: 0.18, attempts: 3 },
    createdAt: T3,
    lastActiveAt: T5,
    role: "reviewer",
    readOnlyDir: "/tmp/co-review-w_review01",
  },
  {
    id: "w_readme01",
    runId: doneRun.id,
    agent: "antigravity",
    model: "gemini-4-argon",
    effort: "high",
    sessionKey: "co-w_readme01",
    cwd: doneRun.cwd,
    state: "closed",
    fingerprint: { files: ["README.md"], tags: ["docs"], taskIds: ["t_readme01"] },
    context: { used: 4_000, size: 1_048_576 },
    totals: { tokens: 6_000, cost: 0.01, attempts: 1 },
    createdAt: doneRun.createdAt,
    lastActiveAt: doneRun.updatedAt,
    role: "author",
  },
];

const attempts: Attempt[] = [
  {
    id: "a_spec0001",
    taskId: "t_spec0001",
    runId: demoRun.id,
    workerId: "w_idle0001",
    agent: "antigravity",
    model: "gemini-4-argon",
    effort: "high",
    routeReason: "docs + architecture 标签唯一匹配 antigravity/gemini-4-argon",
    reused: false,
    startedAt: T0,
    endedAt: T1,
    tokens: { input: 40_000, output: 18_000, cacheRead: 30_000 },
    cost: 0.62,
    outcome: "succeeded",
  },
  {
    id: "a_store001",
    taskId: "t_store001",
    runId: demoRun.id,
    workerId: "w_closed01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "backend + data，codex 的成本档更合适",
    reused: false,
    startedAt: T1,
    endedAt: T2,
    tokens: { input: 55_000, output: 22_000, cacheRead: 43_000 },
    cost: 0.48,
    estimated: { tokens: true, cost: true },
    outcome: "succeeded",
  },
  {
    id: "a_daemon01",
    taskId: "t_daemon01",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "同一 cursor worker 刚跑过测试任务，复用它继续做 daemon",
    reused: true,
    startedAt: T2,
    tokens: { input: 28_000, output: 12_000, cacheRead: 24_000 },
    cost: 0.21,
  },
  {
    id: "a_web00001",
    taskId: "t_web00001",
    runId: demoRun.id,
    workerId: "w_start001",
    agent: "antigravity",
    model: "gemini-4-hydrogen",
    effort: "high",
    routeReason: "主控按 coder 首选路由 antigravity/gemini-4-hydrogen 并继承架构师上下文 (--fork coordinator)",
    reused: false,
    startedAt: T5,
    tokens: { input: 9_500, output: 3_500, cacheRead: 6_000 },
    cost: 0.08,
  },
  {
    id: "a_test0001",
    taskId: "t_test0001",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "tests 标签交给编码主力 cursor/grok-4.7；做完后复用去做 daemon",
    reused: false,
    startedAt: T1,
    endedAt: T2,
    tokens: { input: 12_000, output: 4_000, cacheRead: 2_000 },
    cost: 0.05,
    outcome: "failed",
    failure: { kind: "agent_error", message: "mock agent 在断言处退出" },
  },
  {
    id: "a_test0002",
    taskId: "t_test0001",
    runId: demoRun.id,
    workerId: "w_dead0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "上一次失败后换一个 worker 重试",
    reused: false,
    retryOf: "a_test0001",
    hint: "先看 daemon 是否监听随机端口",
    startedAt: "2026-11-06T05:56:00.000Z",
    endedAt: T4,
    tokens: { input: 14_000, output: 5_000, cacheRead: 3_000 },
    cost: 0.06,
    outcome: "failed",
    failure: { kind: "timeout", message: "等待 worker_done 超时" },
    verifyAiDigest: "根因：daemon 监听随机端口前测试进程已开始请求，导致等待 worker_done 超时。\n建议：改为等待 ready 信号后再执行探测。",
    aiDiagnosis: {
      at: T4,
      basedOnUpdatedAt: T4,
      rootCause: "测试子进程在 server.listen(0) 尚未完成端口绑定前发起连接，请求挂起直至超时。",
      evidence: ["v_test_fail_2: not ok 1 - 等待 worker_done 超时 (15000ms)", "hint: 先确认 server.listen(0) 是否已完成绑定"],
      recommendedAction: "retry_fresh",
      suggestedCommand: "co retry t_test0001 --fresh --hint \"先 await server.listen(0) 完成再发请求\"",
      hint: "先 await listening 事件后再执行测试请求",
    },
    verifyLogArtifactId: "v_test_fail_2",
  },
  {
    id: "a_readme01",
    taskId: "t_readme01",
    runId: doneRun.id,
    workerId: "w_readme01",
    agent: "antigravity",
    model: "gemini-4-argon",
    effort: "high",
    routeReason: "docs 标签由 antigravity/gemini-4-argon 唯一负责",
    reused: false,
    startedAt: doneRun.createdAt,
    endedAt: doneRun.updatedAt,
    tokens: { input: 3_000, output: 800, cacheRead: 2_200 },
    cost: 0.01,
    outcome: "succeeded",
  },
  {
    id: "a_rev_author",
    taskId: "t_rev0001",
    runId: demoRun.id,
    workerId: "w_idle0001",
    agent: "antigravity",
    model: "gemini-4-argon",
    effort: "high",
    routeReason: "复用写规格的会话，等审查员看候选快照",
    reused: true,
    kind: "author",
    startedAt: T3,
    endedAt: T4,
    tokens: { input: 8_000, output: 2_000, cacheRead: 4_000 },
    cost: 0.04,
    outcome: "succeeded",
  },
  {
    id: "a_rev_r1",
    taskId: "t_rev0001",
    runId: demoRun.id,
    workerId: "w_review01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "异构审查，避开作者 agent",
    reused: true,
    kind: "review",
    startedAt: T5,
    tokens: { input: 6_000, output: 1_000, cacheRead: 2_000 },
    cost: 0.03,
    reviewRound: {
      round: 1,
      maxRounds: 2,
      baseTreeSha: "base0001aaa",
      treeSha: "tree_rev0001aaa",
      patchArtifactId: "art_rev_r1",
    },
  },
  {
    id: "a_revfail_author",
    taskId: "t_revfail1",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "backend 交给正在写 daemon 的 cursor",
    reused: true,
    kind: "author",
    startedAt: T3,
    endedAt: T3,
    tokens: { input: 11_000, output: 3_000, cacheRead: 4_000 },
    cost: 0.05,
    outcome: "failed",
    failure: { kind: "review_rejected", message: "两轮都没把预算暂停和手动暂停区分开" },
  },
  {
    id: "a_revfail_r1",
    taskId: "t_revfail1",
    runId: demoRun.id,
    workerId: "w_review01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "第一轮只读审查",
    reused: false,
    kind: "review",
    startedAt: T3,
    endedAt: "2026-11-06T05:50:00.000Z",
    tokens: { input: 9_000, output: 2_000, cacheRead: 3_000 },
    cost: 0.06,
    outcome: "succeeded",
    reviewRound: {
      round: 1,
      maxRounds: 2,
      baseTreeSha: "base_revfail",
      treeSha: "tree_revfail_r1",
      patchArtifactId: "art_revfail_r1",
      verdict: "changes_requested",
      summary: "暂停按钮只有一种文案，没有区分预算暂停和手动暂停",
      findings: [{ severity: "error", file: "web/src/App.tsx", message: "暂停按钮仍只有一种文案" }],
    },
  },
  {
    id: "a_revfail_rework",
    taskId: "t_revfail1",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "第一轮要求修改后，唤醒原作者热会话返工",
    reused: true,
    kind: "author",
    startedAt: "2026-11-06T05:50:30.000Z",
    endedAt: "2026-11-06T05:54:00.000Z",
    tokens: { input: 8_000, output: 2_400, cacheRead: 6_000 },
    cost: 0.04,
    outcome: "succeeded",
  },
  {
    id: "a_revfail_r2",
    taskId: "t_revfail1",
    runId: demoRun.id,
    workerId: "w_review01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "复用上轮 Reviewer 热会话",
    reused: true,
    kind: "review",
    startedAt: "2026-11-06T05:55:00.000Z",
    endedAt: T4,
    tokens: { input: 7_000, output: 1_500, cacheRead: 5_000 },
    cost: 0.05,
    outcome: "succeeded",
    reviewRound: {
      round: 2,
      maxRounds: 2,
      baseTreeSha: "tree_revfail_r1",
      treeSha: "tree_revfail_r2",
      patchArtifactId: "art_revfail_r2",
      deltaPatchArtifactId: "art_revfail_r2_delta",
      verdict: "changes_requested",
      summary: "返工后的增量改动仍未区分预算暂停和手动暂停",
      findings: [
        { severity: "error", file: "web/src/App.tsx", message: "暂停按钮仍只有一种文案" },
        { severity: "warning", file: "web/src/i18n.ts", message: "缺少预算暂停的说明" },
      ],
    },
  },
  {
    id: "a_revok_author",
    taskId: "t_revok01",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "前端文案交给正在写界面的 cursor",
    reused: true,
    kind: "author",
    startedAt: "2026-11-06T06:06:00.000Z",
    endedAt: "2026-11-06T06:10:00.000Z",
    tokens: { input: 5_000, output: 1_200, cacheRead: 2_000 },
    cost: 0.03,
    outcome: "succeeded",
  },
  {
    id: "a_revok_r1",
    taskId: "t_revok01",
    runId: demoRun.id,
    workerId: "w_review01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "异构审查，避开作者 cursor",
    reused: true,
    kind: "review",
    startedAt: "2026-11-06T06:10:00.000Z",
    endedAt: "2026-11-06T06:14:00.000Z",
    tokens: { input: 4_000, output: 900, cacheRead: 1_500 },
    cost: 0.02,
    outcome: "succeeded",
    reviewRound: {
      round: 1,
      maxRounds: 2,
      baseTreeSha: "base_ok",
      treeSha: "tree_ok_r1",
      patchArtifactId: "art_revok_r1",
      verdict: "changes_requested",
      summary: "两套暂停文案还共用同一个字符串",
      findings: [{ severity: "error", file: "web/src/i18n.ts", message: "run.pause 同时描述了手动暂停和预算暂停" }],
    },
  },
  {
    id: "a_revok_rework",
    taskId: "t_revok01",
    runId: demoRun.id,
    workerId: "w_busy0001",
    agent: "cursor",
    model: "grok-4.7",
    effort: "high",
    routeReason: "审查打回后由原作者补上预算暂停文案",
    reused: true,
    kind: "author",
    startedAt: "2026-11-06T06:14:30.000Z",
    endedAt: "2026-11-06T06:18:00.000Z",
    tokens: { input: 3_200, output: 800, cacheRead: 2_400 },
    cost: 0.02,
    outcome: "succeeded",
  },
  {
    id: "a_revok_r2",
    taskId: "t_revok01",
    runId: demoRun.id,
    workerId: "w_review01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "high",
    routeReason: "复用上轮 Reviewer 热会话，只看增量 Diff",
    reused: true,
    kind: "review",
    startedAt: "2026-11-06T06:18:00.000Z",
    endedAt: "2026-11-06T06:22:00.000Z",
    tokens: { input: 2_800, output: 700, cacheRead: 3_200 },
    cost: 0.02,
    outcome: "succeeded",
    reviewRound: {
      round: 2,
      maxRounds: 2,
      baseTreeSha: "tree_ok_r1",
      treeSha: "tree_ok_r2",
      patchArtifactId: "art_revok_r2",
      deltaPatchArtifactId: "art_revok_r2_delta",
      verdict: "approved",
      summary: "文案已分开，审查通过",
      findings: [],
    },
  },
  {
    id: "a_vfail",
    taskId: "t_vfail001",
    runId: demoRun.id,
    workerId: "w_closed01",
    agent: "codex",
    model: "gpt-6.1-sol",
    effort: "medium",
    routeReason: "类型检查交给刚改过 store 的 codex",
    reused: true,
    kind: "author",
    startedAt: T4,
    endedAt: T5,
    tokens: { input: 4_000, output: 800, cacheRead: 2_000 },
    cost: 0.02,
    outcome: "failed",
    failure: { kind: "verification_failed", message: "npx tsc 退出码 1：src/cli.ts 仍引用已删除的入口" },
  },
];

const messages: Message[] = [
  {
    id: "m_1",
    seq: 1,
    runId: demoRun.id,
    type: "question",
    from: "worker:w_busy0001",
    to: "coordinator",
    threadId: "m_1",
    taskId: "t_daemon01",
    attemptId: "a_daemon01",
    subject: "SSE 心跳间隔用 15 秒可以吗？",
    body: "规格写了每 15 秒一条注释。确认后我按这个实现。",
    options: ["可以", "改成 30 秒"],
    recommend: "可以",
    why: "与默认反向代理超时兼容且开销极低",
    needsAction: true,
    createdAt: T4,
  },
  {
    id: "m_2",
    seq: 2,
    runId: demoRun.id,
    type: "escalation",
    from: "worker:w_dead0001",
    to: "coordinator",
    taskId: "t_test0001",
    attemptId: "a_test0002",
    subject: "集成测试连续失败",
    body: "两次都没有 worker_done。需要换模型或改验收标准。",
    needsAction: true,
    createdAt: T4,
  },
  {
    id: "m_3",
    seq: 3,
    runId: demoRun.id,
    type: "failed",
    from: "worker:w_dead0001",
    to: "coordinator",
    taskId: "t_test0001",
    attemptId: "a_test0002",
    subject: "任务失败",
    body: "等待 worker_done 超时。连续失败 2 次。",
    needsAction: true,
    createdAt: T4,
  },
  {
    id: "m_5",
    seq: 5,
    runId: demoRun.id,
    type: "done",
    from: "worker:w_idle0001",
    to: "coordinator",
    taskId: "t_spec0001",
    attemptId: "a_spec0001",
    subject: "规格已定稿",
    body: "docs/TECH_SPEC.md 已更新。",
    needsAction: false,
    createdAt: T1,
  },
  {
    id: "m_6",
    seq: 6,
    runId: demoRun.id,
    type: "note",
    from: "worker:w_busy0001",
    to: "coordinator",
    taskId: "t_daemon01",
    subject: "正在接 SSE",
    body: "change 事件会防抖后再拉快照。",
    needsAction: false,
    createdAt: T5,
  },
  {
    id: "m_7",
    seq: 7,
    runId: demoRun.id,
    type: "reply",
    from: "coordinator",
    to: "worker:w_idle0001",
    threadId: "m_1",
    subject: "Re: 规格范围",
    body: "先写契约，不要改旧文件。",
    needsAction: false,
    createdAt: T1,
  },
  {
    id: "m_8",
    seq: 8,
    runId: demoRun.id,
    type: "system",
    from: "coordinator",
    to: "coordinator",
    subject: "调度已恢复",
    body: "daemon 重启后把 busy worker 标成 dead 的恢复逻辑未在本轮触发。",
    needsAction: false,
    createdAt: T2,
  },
  {
    id: "m_9",
    seq: 9,
    runId: demoRun.id,
    type: "budget",
    from: "coordinator",
    to: "coordinator",
    subject: "预算已用约 9%",
    body: "tokens 180000 / 2000000，成本 $1.42 / $20。",
    needsAction: false,
    createdAt: T5,
  },
  {
    id: "m_10",
    seq: 10,
    runId: demoRun.id,
    type: "run_done",
    from: "coordinator",
    to: "coordinator",
    subject: "示例：运行结束",
    body: "这条只为展示 run_done。当前 Run 仍是 active。",
    needsAction: false,
    createdAt: T5,
  },
  {
    id: "m_11",
    seq: 11,
    runId: demoRun.id,
    type: "budget",
    from: "coordinator",
    to: "coordinator",
    subject: "预算已用超过 80%",
    body: "继续跑会在触顶后暂停调度。",
    needsAction: true,
    createdAt: T5,
  },
];

const items: Item[] = [
  {
    id: 1,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: T2,
    kind: "user",
    text: "实现 daemon HTTP：路由、JSON、SSE。只改 src/daemon。",
  },
  {
    id: 2,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: "2026-11-06T05:12:00.000Z",
    kind: "thought",
    text: "先对一下 TECH_SPEC §5 的路径表，写接口才带 token。",
  },
  {
    id: 3,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: "2026-11-06T05:14:00.000Z",
    kind: "plan",
    text: "1. 路由表\n2. 快照\n3. SSE：change 与 item\n4. 静态资源 dist/web",
  },
  {
    id: 4,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: "2026-11-06T05:20:00.000Z",
    kind: "tool",
    tool: {
      id: "call_read_spec",
      title: "读取 TECH_SPEC",
      kind: "read",
      status: "completed",
      locations: ["docs/TECH_SPEC.md"],
      input: { path: "docs/TECH_SPEC.md", offset: 162 },
      output: "GET /api/runs/:id 返回 RunSnapshot。GET /api/events 推送 change 和 item。",
    },
  },
  {
    id: 5,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: "2026-11-06T05:28:00.000Z",
    kind: "assistant",
    text: "快照和 SSE 的骨架已经接上，接下来补鉴权头。",
  },
  {
    id: 6,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: "2026-11-06T05:36:00.000Z",
    kind: "tool",
    tool: {
      id: "call_edit_server",
      title: "修改 server.ts",
      kind: "edit",
      status: "in_progress",
      locations: ["src/daemon/server.ts"],
      input: { path: "src/daemon/server.ts" },
      output: "正在写入路由注册。",
    },
  },
  {
    id: 7,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: T5,
    kind: "usage",
    usage: { used: 155_000, size: 200_000, cost: 0.21, input: 28_000, output: 12_000, cacheRead: 24_000 },
  },
  {
    id: 8,
    workerId: "w_busy0001",
    attemptId: "a_daemon01",
    ts: T5,
    kind: "system",
    text: "turn 仍在进行，尚未收到 worker_done。",
  },
  {
    id: 101,
    workerId: "w_idle0001",
    attemptId: "a_spec0001",
    ts: T0,
    kind: "user",
    text: "把类型、表和 HTTP 契约写进 TECH_SPEC。",
  },
  {
    id: 102,
    workerId: "w_idle0001",
    attemptId: "a_spec0001",
    ts: T1,
    kind: "assistant",
    text: "规格已写完，结果摘要放在任务上。",
  },
  {
    id: 103,
    workerId: "w_idle0001",
    attemptId: "a_spec0001",
    ts: T1,
    kind: "usage",
    usage: { used: 42_000, size: 200_000, cost: 0.62, input: 40_000, output: 18_000, cacheRead: 30_000 },
  },
  {
    id: 201,
    workerId: "w_review01",
    attemptId: "a_revfail_r2",
    ts: "2026-11-06T05:55:10.000Z",
    kind: "user",
    text: "只读审查第 2 轮增量。标准：暂停语义和文案一致，不能把预算暂停写成手动暂停。只读快照，不要改文件。候选树 tree_revfail_r2，增量补丁 art_revfail_r2_delta。",
  },
  {
    id: 202,
    workerId: "w_review01",
    attemptId: "a_revfail_r2",
    ts: "2026-11-06T05:56:00.000Z",
    kind: "thought",
    text: "上一轮已经指出按钮只有一种文案。这轮只看增量，确认 run.pause 有没有拆开预算暂停。",
  },
  {
    id: 203,
    workerId: "w_review01",
    attemptId: "a_revfail_r2",
    ts: "2026-11-06T05:57:30.000Z",
    kind: "tool",
    tool: {
      id: "call_read_snapshot",
      title: "读取只读快照",
      kind: "read",
      status: "completed",
      locations: ["/tmp/co-review-w_review01/web/src/App.tsx", "/tmp/co-review-w_review01/web/src/i18n.ts"],
      input: { path: "/tmp/co-review-w_review01/web/src/i18n.ts", offset: 330 },
      output: "run.pause: 暂停调度。run.pauseUser 与 run.pauseBudget 仍共用按钮上的同一句文案。",
    },
  },
  {
    id: 204,
    workerId: "w_review01",
    attemptId: "a_revfail_r2",
    ts: "2026-11-06T06:04:00.000Z",
    kind: "assistant",
    text: "审查结论：changes_requested。返工后的增量仍未区分预算暂停和手动暂停。web/src/App.tsx 的暂停按钮只有一种文案，web/src/i18n.ts 缺少预算暂停的说明。",
  },
];

let profile: Profile = {
  version: 1,
  updatedAt: "2026-11-08T10:44:44.121Z",
  defaults: { maxInstances: 3 },
  roles: {
    coder: { access: "writable", effort: "high", tags: ["backend", "refactor"], agents: ["antigravity/gemini-4-hydrogen", "codex/gpt-6.1-sol", "claude/zcode/GLM-5.3[1m]"] },
    tester: { access: "scoped_write", effort: "medium", tags: ["tests"], agents: ["dsh", "antigravity/gemini-4-hydrogen", "cursor/grok-4.7", "claude/zcode/GLM-5.3-Flash[1m]"] },
    writer: { access: "scoped_write", effort: "medium", tags: ["docs"], agents: ["antigravity/gemini-4-carbon", "antigravity/gemini-4-argon", "claude"] },
    reviewer: { access: "readonly", effort: "high", tags: ["review"], agents: ["codex/gpt-6.1-sol", "antigravity/gemini-4-hydrogen"] },
    researcher: { access: "readonly", effort: "high", tags: ["research", "data"], agents: ["dsh", "claude/zcode/GLM-5.3-Flash[1m]", "antigravity/gemini-4-carbon"] },
  },
  agents: [
    {
      id: "antigravity",
      launch: { command: "agy acp" },
      modelSelection: "session",
      modelEnv: null,
      effortKey: "thought_level",
      summary: "全部技术文档与专题文章撰写/润色唯一主力，同时负责 coder 默认首选（gemini-4-hydrogen）、Android 系统顶层规划与复杂渲染/时序 Bug 推演（经 agy 运行）。",
      bestFor: ["backend", "refactor", "docs", "planning", "architecture", "debugging", "data", "long-context"],
      avoid: [],
      sessionConfig: { thought_level: "high" },
      isolateSecrets: true,
      models: [
        {
          id: "gemini-4-hydrogen",
          default: true,
          summary: "Gemini 4 Hydrogen（coder 角色默认首选，擅长高质量实现、多模块重构与测试排查）。",
          bestFor: ["backend", "refactor", "tests", "debugging", "review"],
          cost: "medium",
          speed: "fast",
          contextWindow: 1048576,
          price: { input: 2, output: 10, cacheRead: 0.1 },
          sessionConfig: { thought_level: "high" },
          verified: { ok: true, at: "2026-11-12T10:35:00.000Z" },
          quota: { providers: ["antigravity"], pool: "Gemini" },
        },
        {
          id: "gemini-4-carbon",
          summary: "Gemini 4 Carbon（writer / researcher 角色高性价比长上下文主力）。",
          bestFor: ["docs", "research", "data", "long-context"],
          cost: "low",
          speed: "fast",
          contextWindow: 1048576,
          price: { input: 1, output: 5, cacheRead: 0.05 },
          sessionConfig: { thought_level: "high" },
          verified: { ok: true, at: "2026-11-12T10:35:00.000Z" },
          quota: { providers: ["antigravity"], pool: "Gemini" },
        },
        {
          id: "gemini-4-argon",
          summary: "Gemini 4 Argon（默认 high 推理，1M 统一上下文池/长解码至 1M，长提示 82–115 tok/s 不降速反提速，Terminal-Bench 4.0 57.6% 车队第一、LiveBench Reasoning 95.1 & Data 85.2 车队第一、Arena 文本榜首 1524、Vals Index 69.2%；尝鲜价 $2/$10 至 11-30，12-01 起 $4/$20）；唯一负责全部技术文档撰写与润色，兼做 WMS/DMS/SF/图形与 AI 工具的顶层规划、跨层架构设计、疑难根因推演与 Trace/Dump 深度分析",
          bestFor: ["docs", "planning", "architecture", "debugging", "data", "long-context"],
          cost: "medium",
          speed: "medium",
          contextWindow: 1048576,
          price: { input: 2, output: 10, cacheRead: 0.1 },
          sessionConfig: { thought_level: "high" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          quota: { providers: ["antigravity"], pool: "Gemini" },
        },
      ],
    },
    {
      id: "codex",
      launch: { acpx: "codex" },
      modelSelection: "session",
      modelEnv: null,
      effortKey: "reasoning_effort",
      summary: "全部代码审查（review）与架构图/时序图/渲染流水线图绘制（diagrams）唯一主力，同时负责 Android 系统底层（WMS/DMS/SF/图形 C++ & Java）高难攻坚、跨进程/并发锁/内存生命周期疑难排障、复杂构建工程（devops），以及 AI 提效工具的高复杂度核心逻辑与高要求 Web 界面（默认 high 思考强度 + fast-mode=on，SWE-bench Verified 96.2% 车队第一、DeepSWE v1.1 75.2%、SEC-Bench Pro 78.8%、WebDev Arena 1741.8 车队第一、Vals SRE Bench 50.8% 车队第一）。",
      bestFor: ["review", "diagrams", "architecture", "debugging", "backend", "refactor", "devops", "frontend", "ui"],
      avoid: ["docs", "planning", "bulk-mechanical", "quick-edits", "scripting", "research"],
      sessionConfig: { mode: "agent-full-access", reasoning_effort: "high", "fast-mode": "on" },
      isolateSecrets: true,
      models: [
        {
          id: "gpt-6.1-sol",
          default: true,
          summary: "GPT-6.1-Sol（默认 high 推理 + fast-mode=on，DeepSWE 75.2% 最优档，xhigh/max 反降至 71.9% 勿调高；Codex 默认 500k 上下文 / API 1.05M + 128K 输出，SWE-bench Verified 96.2%、SEC-Bench Pro 78.8%、WebDev Arena 1741.8 车队第一、SRE Bench 50.8% 车队第一）；唯一负责全部 review（C++ 生命周期/并发锁/Binder 契约审查）与图表绘制，兼做 WMS/DMS/SF/图形高难攻坚、疑难排障、复杂构建治理及 AI 工具高要求前端/UI",
          bestFor: ["review", "diagrams", "architecture", "debugging", "backend", "refactor", "devops", "frontend", "ui"],
          cost: "medium",
          speed: "fast",
          contextWindow: 512000,
          price: { input: 2, output: 10, cacheRead: 0.1 },
          sessionConfig: { reasoning_effort: "high", "fast-mode": "on" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          quota: { providers: ["codex"] },
        },
      ],
    },
    {
      id: "claude",
      launch: { command: "bin/acp-claude claude-magpie" },
      modelSelection: "session",
      modelEnv: "ACP_CLAUDE_MODEL",
      effortKey: "effort",
      summary: "Claude Code 工具链经 claude-magpie 运行（默认 high 思考强度控本 + 1M 上下文，Coding Plan 额度最充裕且非高峰时段积分半价）：GLM-5.3 作为 Android 系统核心代码（WMS/DMS/SF/图形 Java & C++）与 AI 提效工具后端/CLI 的第一编码主力；GLM-5.3-Flash 与 dsh 共同负责 AOSP 跨仓检索、dumpsys/日志提取、构建/刷机脚本与多模态截图辅助。",
      bestFor: ["long-context"],
      avoid: ["docs", "review", "diagrams", "planning", "architecture", "frontend", "ui"],
      maxInstances: 5,
      sessionConfig: { effort: "high" },
      isolateSecrets: true,
      models: [
        {
          id: "zcode/GLM-5.3[1m]",
          default: true,
          summary: "GLM-5.3（第一编码主力，744B/40B MoE，额度最充裕，固定 high 推理避免 max 档冗长翻倍，1M 上下文 + 128K 输出可完整容纳数千行 WMS/SF 大文件，73–86 tok/s，SWE-bench Verified 95.4%、TB2.1 88.2%、CyberGym 84.5%、DeepSWE 66.9%）；主力负责 Android 系统服务/Native 图形模块（backend）与 AI 工具核心逻辑开发、跨文件重构、日常 Bug 排查与单元测试（AI 工具 Web 前端交由 cursor/dsh/codex）",
          bestFor: ["backend", "refactor", "debugging", "tests", "long-context"],
          cost: "low",
          speed: "medium",
          contextWindow: 1000000,
          maxInstances: 5,
          price: { input: 1.4, output: 4.4, cacheRead: 0.26 },
          sessionConfig: { effort: "high" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          avoidWindows: [{ start: "14:00", end: "18:00", tz: "Asia/Shanghai", reason: "高峰时段价格翻倍" }],
          quota: { providers: ["zcode"] },
        },
        {
          id: "zcode/GLM-5.3-Flash[1m]",
          summary: "GLM-5.3-Flash（默认 high 推理，1M 上下文 + 128K 输出，320B/18B MoE 原生多模态支持图/视频，标准档 ~49 tok/s，TB2.1 84.3%、Toolathlon 78.4%、DeepSWE 63.4%、AutomationBench 48.8%）；Coding Plan 积分消耗仅为 GLM-5.3 的 1/3，负责 AOSP 跨仓检索、dumpsys/logcat 数据清洗、构建/刷机自动化脚本、局部小改及带渲染异常截图/录屏的辅助分析",
          bestFor: ["bulk-mechanical", "quick-edits", "scripting", "research", "data", "devops", "long-context"],
          cost: "low",
          speed: "medium",
          contextWindow: 1000000,
          price: { input: 0.15, output: 0.5, cacheRead: 0.03 },
          sessionConfig: { effort: "high" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          avoidWindows: [{ start: "14:00", end: "18:00", tz: "Asia/Shanghai", reason: "高峰时段价格翻倍" }],
          quota: { providers: ["zcode"] },
        },
      ],
    },
    {
      id: "cursor",
      launch: { command: "bin/acp-cursor" },
      modelSelection: "session",
      modelEnv: null,
      effortKey: "reasoning_effort",
      summary: "第二编码主力（额度第二充裕，经 bin/acp-cursor 开启 Grok 4.7 500k Max 上下文 + 默认 high 推理 + fast 2× 提速档）：与 GLM-5.3、dsh 共同承担 Android 系统模块（WMS/DMS/SF/图形）及 AI 提效工具核心编码、跨文件长程重构、日常排障与单测，同时作为 AI 提效工具 Web 前端与 UI 界面的首选主力（规避 devops 与终端脚本短板）。",
      bestFor: ["backend", "refactor", "debugging", "tests", "frontend", "ui", "long-context"],
      avoid: ["docs", "review", "diagrams", "planning", "architecture", "devops", "scripting", "data", "bulk-mechanical", "quick-edits", "research"],
      maxInstances: 4,
      sessionConfig: { context: "500k", reasoning_effort: "high", fast: "true" },
      isolateSecrets: true,
      models: [
        {
          id: "grok-4.7",
          default: true,
          summary: "Grok 4.7（第二编码主力，额度充裕，500k 上下文/无硬性输出上限、默认 high 推理、Fast 2× 速度档，SWE-bench Verified 95.6%、DeepSWE 71.0%、CursorBench 4.0 46.3% 多文件长程修改专项调优、Vibe Code 86.2%；≥200k 长上下文按 $4/$12 计，SRE Bench 3.82% 垫底且 TB4.0 独立评测 28.8% 偏弱故避开 devops/scripting）；负责 WMS/DMS/SF/图形模块与 AI 工具编码、跨文件重构、排障、单测，以及 AI 提效工具的 Web 前端/UI 实现",
          bestFor: ["backend", "refactor", "debugging", "tests", "frontend", "ui", "long-context"],
          cost: "low",
          speed: "fast",
          contextWindow: 500000,
          maxInstances: 4,
          price: { input: 2, output: 6, cacheRead: 0.5 },
          sessionConfig: { context: "500k", reasoning_effort: "high", fast: "true" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          quota: { providers: ["cursor"], windows: ["Grok Bot", "Total"] },
        },
      ],
    },
    {
      id: "dsh",
      launch: { acpx: "dsh" },
      modelSelection: "session",
      modelEnv: null,
      effortKey: "reasoning_effort",
      summary: "第三编码主力兼 AOSP 检索/测试/自动化脚本主力（DeepSeek-V4.1-Flash-1017 默认 high 推理，1M 上下文 + 384K 超长输出，DeepSWE v1.1 77.0% 全场第一、LiveBench Coding 96.0 并列第一、TB2.1 90.6% 全场第一、WebDev Arena 1665、CyberGym 88.1% 车队第一、AutomationBench 60.5% 车队第一，缓存读取仅 $0.006/Mtok 且谷时半价）：与 GLM-5.3、cursor 共同承担 Android 系统模块（WMS/DMS/SF/图形）及 AI 提效工具的前后端编码、模块重构与单测，并与 GLM-5.3-Flash 共同负责 AOSP 超大源码树检索、日志/Trace 数据清洗、编译/刷机脚本与批量修改。",
      bestFor: ["backend", "refactor", "tests", "frontend", "devops", "scripting", "research", "data", "quick-edits", "bulk-mechanical", "long-context"],
      avoid: ["docs", "review", "diagrams", "planning", "architecture", "debugging", "ui"],
      maxInstances: 4,
      sessionConfig: { reasoning_effort: "high" },
      isolateSecrets: true,
      models: [
        {
          id: '["magpie","group/deepseek-v4.1-flash"]',
          default: true,
          summary: "DeepSeek V4.1 Flash 1017（第三编码主力，magpie 路由组，552B MoE 原生视觉，默认 high 推理，1M 上下文 + 384K 超长输出，DeepSWE 77.0% 全场第一、LiveBench Coding 96.0、TB2.1 90.6% 全场第一、WebDev 1665、CyberGym 88.1%、AutomationBench 60.5%，缓存命中 $0.006/Mtok、谷时半价；SWE-Marathon 40.0% 偏弱故避开跨多层长程疑难 debugging）；负责 Android 系统模块与 AI 工具功能编码、中小型重构、单测编写、AOSP 源码/资料检索、日志数据清洗与编译/刷机自动化脚本",
          bestFor: ["backend", "refactor", "tests", "frontend", "devops", "scripting", "research", "data", "quick-edits", "bulk-mechanical", "long-context"],
          cost: "low",
          speed: "fast",
          contextWindow: 1000000,
          maxInstances: 4,
          price: { input: 0.3, output: 1.2, cacheRead: 0.006 },
          sessionConfig: { reasoning_effort: "high" },
          verified: { ok: true, at: "2026-11-08T10:35:00.000Z" },
          quota: { providers: ["workbuddy", "deepseek"] },
        },
      ],
    },
  ],
};

const stats: StatsTable = {
  "antigravity/gemini-4-argon/docs": { n: 6, success: 6, avgTokens: 90_000, avgCost: 0.42, avgDurationMs: 1_500_000, retryRate: 0 },
  "codex/gpt-6.1-sol/review": { n: 8, success: 7, avgTokens: 110_000, avgCost: 0.45, avgDurationMs: 2_400_000, retryRate: 0.12, estimated: { tokens: true, cost: true } },
  "claude/zcode/GLM-5.3[1m]/backend": { n: 10, success: 9, avgTokens: 85_000, avgCost: 0.18, avgDurationMs: 1_200_000, retryRate: 0.1 },
  "cursor/grok-4.7/frontend": { n: 6, success: 5, avgTokens: 60_000, avgCost: 0.2, avgDurationMs: 1_100_000, retryRate: 0.17 },
  "dsh/[\"magpie\",\"group/deepseek-v4.1-flash\"]/research": { n: 5, success: 5, avgTokens: 40_000, avgCost: 0.03, avgDurationMs: 600_000, retryRate: 0 },
};

interface Db {
  runs: Run[];
  tasks: Task[];
  workers: Worker[];
  attempts: Attempt[];
  messages: Message[];
  items: Item[];
}

const db: Db = { runs: [demoRun, doneRun], tasks, workers, attempts, messages, items };

function clone<T>(v: T): T {
  return JSON.parse(JSON.stringify(v)) as T;
}

function ranked(run: Run): boolean {
  return typeof run.order === "number" && Number.isFinite(run.order);
}

function sortRuns(runs: Run[]): Run[] {
  return [...runs].sort((a, b) => {
    const ar = ranked(a), br = ranked(b);
    if (ar && br && a.order !== b.order) return a.order! - b.order!;
    if (ar && !br) return -1;
    if (!ar && br) return 1;
    return b.createdAt.localeCompare(a.createdAt);
  });
}

function inTrash(run: Run): boolean {
  return typeof run.trashedAt === "string" && run.trashedAt.length > 0;
}

export function mockListRuns(trash = false): RunSummary[] {
  return sortRuns(db.runs.filter((run) => inTrash(run) === trash)).map((run) => {
    const taskCounts: RunSummary["taskCounts"] = { pending: 0, ready: 0, running: 0, succeeded: 0, failed: 0, blocked: 0, cancelled: 0, skipped: 0 };
    for (const task of db.tasks) if (task.runId === run.id) taskCounts[task.status]++;
    return clone({ ...run, taskCounts });
  });
}

function mockRun(id: string): Run {
  const run = db.runs.find((item) => item.id === id);
  if (!run) throw new Error("Run 不存在");
  return run;
}

export function mockPauseRun(id: string): Run {
  const run = mockRun(id);
  run.status = "paused";
  run.pauseReason = "user";
  run.updatedAt = new Date().toISOString();
  return clone(run);
}

export function mockResumeRun(id: string): Run {
  const run = mockRun(id);
  if (run.status !== "paused") throw new Error("Run 没有暂停");
  run.status = "active";
  run.pauseReason = undefined;
  run.updatedAt = new Date().toISOString();
  return clone(run);
}

export function mockRenameRun(id: string, name: string): Run {
  const run = mockRun(id);
  const trimmed = name.trim();
  run.name = trimmed || undefined;
  run.updatedAt = new Date().toISOString();
  return clone(run);
}

export function mockReorderRuns(ids: string[]): void {
  if (ids.some((id) => !id.trim())) throw new Error("ids 必须是非空字符串数组");
  if (new Set(ids).size !== ids.length) throw new Error("ids 不能重复");
  const now = new Date().toISOString();
  ids.forEach((id, order) => {
    const run = mockRun(id);
    run.order = order;
    run.updatedAt = now;
  });
}

export function mockTrashRun(id: string): Run {
  const run = mockRun(id);
  if (inTrash(run)) throw new Error("Run 已在回收站");
  const running = db.tasks.some((task) => task.runId === id && task.status === "running");
  const busy = db.workers.some((worker) => worker.runId === id && worker.state === "busy");
  if (running || busy) throw new Error("先停止运行中的任务");
  run.statusBeforeTrash = run.status;
  run.status = "paused";
  run.trashedAt = new Date().toISOString();
  run.updatedAt = run.trashedAt;
  for (const worker of db.workers) {
    if (worker.runId === id && (worker.state === "idle" || worker.state === "starting")) {
      worker.state = "closed";
      worker.worktree = undefined;
      worker.currentAttemptId = undefined;
      worker.lastActiveAt = run.trashedAt;
    }
  }
  return clone(run);
}

export function mockRestoreRun(id: string): Run {
  const run = mockRun(id);
  if (!inTrash(run)) throw new Error("Run 不在回收站");
  run.status = run.statusBeforeTrash ?? "active";
  run.trashedAt = undefined;
  run.statusBeforeTrash = undefined;
  run.updatedAt = new Date().toISOString();
  return clone(run);
}

export function mockDeleteRun(id: string): void {
  const run = mockRun(id);
  if (!inTrash(run)) throw new Error("未进回收站的 Run 不能彻底删除");
  const workerIds = new Set(db.workers.filter((worker) => worker.runId === id).map((worker) => worker.id));
  db.runs = db.runs.filter((item) => item.id !== id);
  db.tasks = db.tasks.filter((item) => item.runId !== id);
  db.workers = db.workers.filter((item) => item.runId !== id);
  db.attempts = db.attempts.filter((item) => item.runId !== id);
  db.messages = db.messages.filter((item) => item.runId !== id);
  db.items = db.items.filter((item) => !workerIds.has(item.workerId));
}

export function mockSnapshot(runId: string): RunSnapshot {
  const run = db.runs.find((r) => r.id === runId);
  if (!run) throw new Error(`没有这个 Run：${runId}`);
  const tasksOf = db.tasks.filter((t) => t.runId === runId);
  const attemptsOf = db.attempts.filter((a) => a.runId === runId);
  const tokens = attemptsOf.reduce((s, a) => s + a.tokens.input + a.tokens.output + a.tokens.cacheRead, 0);
  const cost = attemptsOf.reduce((s, a) => s + (a.cost ?? 0), 0);
  return {
    run: clone(run),
    tasks: clone(tasksOf),
    workers: clone(db.workers.filter((w) => w.runId === runId)),
    attempts: clone(attemptsOf),
    pendingMessages: clone(db.messages.filter((m) => m.runId === runId && m.needsAction && !m.handledAt)),
    messages: clone(db.messages.filter((m) => m.runId === runId)),
    stats: { tokens, cost },
    stalls: runId === demoRun.id ? clone(demoStalls) : undefined,
  };
}

const demoStalls: StallSignal[] = [
  {
    kind: "unanswered_action",
    summary: "主会话离线，SSE 心跳问题还没有回复",
    since: T4,
    taskIds: ["t_daemon01"],
    messageIds: ["m_1"],
  },
  {
    kind: "capacity_starved",
    summary: "并行已满，就绪任务在等空位",
    since: T5,
    taskIds: ["t_cli00001"],
  },
  {
    kind: "dead_end_deps",
    summary: "文档任务的前置还没完成，依赖走不通",
    since: T3,
    taskIds: ["t_docs0001"],
  },
];

/** Newest first, matching listItems. `before` is an exclusive upper id. `hasMore` matches the daemon. */
export function mockItems(workerId: string, before?: number, limit = 200, attemptId?: string): { items: Item[]; hasMore: boolean } {
  const rows = db.items
    .filter((i) => i.workerId === workerId && (attemptId == null || i.attemptId === attemptId) && (before == null || i.id < before))
    .sort((a, b) => b.id - a.id);
  return { items: clone(rows.slice(0, limit)), hasMore: rows.length > limit };
}

const mockQuotaCards: QuotaCard[] = [
  {
    provider: "antigravity",
    name: "Antigravity",
    kind: "subscription",
    plan: "pro",
    user: "zz****38@gmail.com",
    windows: [
      { name: "5 hours", remaining: 100, pool: "Gemini", resetsAt: "2026-11-13T15:00:00Z" },
      { name: "7 days", remaining: 82.5, pool: "Gemini", resetsAt: "2026-11-18T00:00:00Z" },
      { name: "5 hours", remaining: 100, pool: "Claude & GPT", resetsAt: "2026-11-13T15:00:00Z" },
    ],
  },
  {
    provider: "codex",
    name: "Codex",
    kind: "subscription",
    plan: "team",
    user: "zz****38@gmail.com",
    balance: "51438.92 credits",
    windows: [
      { name: "5 hours", remaining: 94, resetsAt: "2026-11-13T14:30:00Z" },
      { name: "7 days", remaining: 0, resetsAt: "2026-11-15T06:00:00Z" },
      { name: "Image generation", remaining: 100, text: true, display: "included" },
    ],
  },
  {
    provider: "zcode",
    name: "Zhipu GLM",
    kind: "subscription",
    plan: "Max",
    user: "zz****38@163.com",
    windows: [
      { name: "5 hours", remaining: 88, resetsAt: "2026-11-13T15:20:00Z" },
      { name: "Weekly", remaining: 76, resetsAt: "2026-11-17T00:00:00Z" },
    ],
  },
  {
    provider: "cursor",
    name: "Cursor",
    kind: "subscription",
    plan: "pro",
    user: "zz****38@gmail.com",
    windows: [
      { name: "Other Models", remaining: 0, resetsAt: "2026-11-20T00:00:00Z" },
      { name: "Grok Bot", remaining: 99.7, resetsAt: "2026-11-20T00:00:00Z" },
      { name: "Total", remaining: 46.4, resetsAt: "2026-11-20T00:00:00Z" },
    ],
  },
  {
    provider: "workbuddy",
    name: "WorkBuddy",
    kind: "subscription",
    plan: "pro",
    windows: [
      { name: "Total credits", remaining: 72, resetsAt: "2026-11-30T16:00:00Z" },
      { name: "Credits", remaining: 68 },
    ],
  },
  {
    provider: "deepseek",
    name: "DeepSeek",
    kind: "balance",
    balance: "¥18.53",
    windows: [],
  },
];

const mockAvailability: Record<string, ModelAvailability> = {
  "antigravity/gemini-4-hydrogen": { available: true, detail: "Gemini 5 hours 100%, 7 days 82.5%" },
  "antigravity/gemini-4-carbon": { available: true, detail: "Gemini 5 hours 100%, 7 days 82.5%" },
  "antigravity/gemini-4-argon": { available: true, detail: "Gemini 5 hours 100%, 7 days 82.5%" },
  "codex/gpt-6.1-sol": {
    available: true,
    detail: "codex: 7 days 0%，改用积分 51438.92 credits",
  },
  "claude/zcode/GLM-5.3[1m]": { available: true, detail: "zcode 5 hours 88%, Weekly 76%" },
  "claude/zcode/GLM-5.3-Flash[1m]": { available: true, detail: "zcode 5 hours 88%, Weekly 76%" },
  "cursor/grok-4.7": { available: true, detail: "cursor Grok Bot 99.7%, Total 46.4%" },
  'dsh/["magpie","group/deepseek-v4.1-flash"]': {
    available: true,
    detail: "workbuddy Total credits 72%; deepseek ¥18.53",
  },
};

export function mockAgents(): {
  profile: Profile;
  stats: StatsTable;
  availability: Record<string, ModelAvailability>;
  quotas: QuotaCard[];
} {
  return {
    profile: clone(profile),
    stats: clone(stats),
    availability: clone(mockAvailability),
    quotas: clone(mockQuotaCards),
  };
}

export function mockSaveProfile(next: Profile): {
  profile: Profile;
  stats: StatsTable;
  availability: Record<string, ModelAvailability>;
  quotas: QuotaCard[];
} {
  profile = { ...clone(next), version: 1, updatedAt: new Date().toISOString() };
  return mockAgents();
}

export function mockAssign(taskId: string, patch: { agent?: string; model?: string; effort?: Effort; role?: string; worker?: string; fork?: string }): void {
  const task = db.tasks.find((t) => t.id === taskId);
  if (!task) throw new Error(`没有这个任务：${taskId}`);
  if ((task.status !== "pending" && task.status !== "ready") || db.attempts.some((a) => a.taskId === taskId)) {
    throw new Error("任务已开始");
  }
  task.assign = { ...patch, source: "user" };
  task.updatedAt = new Date().toISOString();
  const run = db.runs.find((r) => r.id === task.runId);
  if (run) run.updatedAt = task.updatedAt;
}

export function mockStop(workerId: string): void {
  const worker = db.workers.find((w) => w.id === workerId);
  if (!worker) throw new Error(`没有这个 worker：${workerId}`);
  worker.state = "closed";
  worker.currentAttemptId = undefined;
  worker.lastActiveAt = new Date().toISOString();
}

const REVIEW_DIFFS: Record<string, string> = {
  art_rev_r1: [
    "diff --git a/docs/TECH_SPEC.md b/docs/TECH_SPEC.md",
    "index 1a2b3c4..5d6e7f8 100644",
    "--- a/docs/TECH_SPEC.md",
    "+++ b/docs/TECH_SPEC.md",
    "@@ -40,6 +40,8 @@",
    " ## HTTP",
    " ",
    "-GET /api/runs/:id returns the snapshot.",
    "+GET /api/runs/:id returns the snapshot.",
    "+GET /api/tasks/:id/review-diff returns one patch slice.",
    " ",
  ].join("\n"),
  art_revfail_r1: [
    "diff --git a/web/src/App.tsx b/web/src/App.tsx",
    "index 4f2a1c0..9c0b77d 100644",
    "--- a/web/src/App.tsx",
    "+++ b/web/src/App.tsx",
    "@@ -118,7 +118,7 @@ export function PauseButton() {",
    "   return (",
    "     <button type=\"button\" onClick={onPause}>",
    "-      {running ? \"Resume\" : \"Pause\"}",
    "+      {paused ? \"Resume\" : \"Pause\"}",
    "     </button>",
    "   );",
    " }",
  ].join("\n"),
  art_revfail_r2: [
    "diff --git a/web/src/App.tsx b/web/src/App.tsx",
    "index 4f2a1c0..aabbcc0 100644",
    "--- a/web/src/App.tsx",
    "+++ b/web/src/App.tsx",
    "@@ -118,7 +118,10 @@ export function PauseButton({ reason }: { reason?: string }) {",
    "   return (",
    "     <button type=\"button\" onClick={onPause}>",
    "-      {paused ? \"Resume\" : \"Pause\"}",
    "+      {reason === \"budget\"",
    "+        ? \"Budget paused\"",
    "+        : paused ? \"Resume\" : \"Pause\"}",
    "     </button>",
    "   );",
    " }",
    "diff --git a/web/src/i18n.ts b/web/src/i18n.ts",
    "index 1111111..2222222 100644",
    "--- a/web/src/i18n.ts",
    "+++ b/web/src/i18n.ts",
    "@@ -10,6 +10,7 @@ const zh = {",
    "   \"run.pause\": \"Pause scheduling\",",
    "+  \"run.pauseBudget\": \"Paused by budget\",",
    " };",
  ].join("\n"),
  art_revfail_r2_delta: [
    "diff --git a/web/src/i18n.ts b/web/src/i18n.ts",
    "index 1111111..2222222 100644",
    "--- a/web/src/i18n.ts",
    "+++ b/web/src/i18n.ts",
    "@@ -10,6 +10,7 @@ const zh = {",
    "   \"run.pause\": \"Pause scheduling\",",
    "+  \"run.pauseBudget\": \"Paused by budget\",",
    " };",
  ].join("\n"),
  art_revok_r1: [
    "diff --git a/web/src/i18n.ts b/web/src/i18n.ts",
    "index 0101010..0202020 100644",
    "--- a/web/src/i18n.ts",
    "+++ b/web/src/i18n.ts",
    "@@ -20,7 +20,7 @@ const zh = {",
    "-  \"run.pause\": \"Pause\",",
    "+  \"run.pause\": \"Pause scheduling, including a budget pause\",",
    " };",
  ].join("\n"),
  art_revok_r2: [
    "diff --git a/web/src/i18n.ts b/web/src/i18n.ts",
    "index 0101010..0303030 100644",
    "--- a/web/src/i18n.ts",
    "+++ b/web/src/i18n.ts",
    "@@ -20,7 +20,8 @@ const zh = {",
    "-  \"run.pause\": \"Pause\",",
    "+  \"run.pauseUser\": \"Paused manually. Raising the budget will not resume it\",",
    "+  \"run.pauseBudget\": \"Paused by the budget. Raise the budget to resume\",",
    " };",
  ].join("\n"),
  art_revok_r2_delta: [
    "diff --git a/web/src/i18n.ts b/web/src/i18n.ts",
    "index 0202020..0303030 100644",
    "--- a/web/src/i18n.ts",
    "+++ b/web/src/i18n.ts",
    "@@ -20,7 +20,8 @@ const zh = {",
    "-  \"run.pause\": \"Pause scheduling, including a budget pause\",",
    "+  \"run.pauseUser\": \"Paused manually. Raising the budget will not resume it\",",
    "+  \"run.pauseBudget\": \"Paused by the budget. Raise the budget to resume\",",
    " };",
  ].join("\n"),
};

function fallbackReviewDiff(taskId: string, artifactId: string): string {
  return [
    `diff --git a/web/src/review.ts b/web/src/review.ts`,
    "index 1111111..2222222 100644",
    "--- a/web/src/review.ts",
    "+++ b/web/src/review.ts",
    "@@ -1,3 +1,4 @@",
    `+// artifact ${artifactId} for ${taskId}`,
    " export function reviewBoard() {",
    "-  return null;",
    "+  return board;",
    " }",
  ].join("\n");
}

/** Unified diff slice for `?mock=1`. `offset` and `limit` count UTF-8 bytes; these fixtures are ASCII. */
export function mockReviewDiff(taskId: string, artifactId: string, opts?: { offset?: number; limit?: number }): ReviewDiffSlice {
  const full = REVIEW_DIFFS[artifactId] ?? fallbackReviewDiff(taskId, artifactId);
  const totalBytes = new TextEncoder().encode(full).length;
  const offset = Math.max(0, Math.floor(opts?.offset ?? 0));
  const limit = opts?.limit == null ? Math.max(0, totalBytes - offset) : Math.max(0, Math.floor(opts.limit));
  const content = offset >= full.length ? "" : full.slice(offset, offset + limit);
  return {
    artifactId,
    content,
    totalBytes,
    truncated: offset + content.length < full.length,
    offset,
  };
}

const MOCK_COORD_ITEMS: Record<string, Array<{ kind: "user" | "assistant" | "thought" | "tool"; text: string; status?: string; ts?: string }>> = {
  [demoRun.id]: [
    { kind: "user", text: "请围绕 daemon + DAG Web UI 拆分并行任务，优先保证 store 与路由可测试性。", ts: T0 },
    { kind: "thought", text: "先拆出架构 spec 与 SQLite store 作为关键路径底座，再并行推进 daemon API 和测试。", ts: T0 },
    { kind: "tool", text: "co plan apply --file docs/plan.yaml", status: "ok", ts: T0 },
    { kind: "assistant", text: "已应用编排计划并启动 spec 与 store 任务，进入等待态监听 worker 事件。", ts: T1 },
    { kind: "tool", text: "co wait --run run_demo0001 --timeout 60000", status: "ok", ts: T2 },
    { kind: "tool", text: "co reply --task t_daemon01 --message \"保留 409 状态码以兼容重试\"", status: "ok", ts: T3 },
    { kind: "assistant", text: "已回复 daemon worker 的接口状态码疑问，正在跟进测试超时与终态 web build 修复。", ts: T4 },
  ],
  [doneRun.id]: [
    { kind: "user", text: "整理仓库说明并补充中文与英文入口。", ts: "2026-11-05T02:00:05.000Z" },
    { kind: "tool", text: "co plan apply --file docs/readme-plan.yaml", status: "ok", ts: "2026-11-05T02:01:00.000Z" },
    { kind: "assistant", text: "README 任务已完成且最终验证通过，Run 已收尾。", ts: "2026-11-05T03:12:00.000Z" },
  ],
};

const MOCK_VERIFY_LOGS: Record<string, string> = {
  fin_demo_verify_1: [
    "$ npm run build",
    "> co-agent@0.0.6 build",
    "> tsc -p . && npm --prefix web run build",
    "src/core/scheduler.ts: ok",
    "web/src/components/PauseButton.tsx(118,12): error TS2322: Type 'string | undefined' is not assignable to type 'string'.",
    "[verify] 终态校验失败（exit code 2），已进入第 1 轮修复（repairing）。",
  ].join("\n"),
  v_test_fail_2: [
    "$ node --test test/daemon.test.mjs",
    "TAP version 13",
    "# Subtest: daemon listens on ephemeral port",
    "not ok 1 - 等待 worker_done 超时 (15000ms)",
    "  检查建议: 先确认 server.listen(0) 是否已完成绑定。",
  ].join("\n"),
};

export function mockCoordinatorTranscript(runId: string, opts?: { before?: number; limit?: number }): CoordinatorTranscriptSlice {
  const run = db.runs.find((r) => r.id === runId);
  if (!run) {
    return { available: false, host: "unknown", hostSession: "", items: [], models: [], total: 0, start: 0, hasMore: false };
  }
  const list = MOCK_COORD_ITEMS[runId];
  if (!list) {
    return {
      available: false,
      host: run.coordinator.host,
      hostSession: run.coordinator.hostSession,
      items: [],
      models: [],
      total: 0,
      start: 0,
      hasMore: false,
    };
  }
  const limit = Math.max(1, Math.min(1000, Math.floor(opts?.limit ?? 200)));
  const total = list.length;
  const end = Math.min(opts?.before ?? total, total);
  const start = Math.max(0, end - limit);
  return {
    available: true,
    host: run.coordinator.host,
    hostSession: run.coordinator.hostSession,
    match: run.id === demoRun.id ? "session-id" : "cwd-and-time",
    sourceFile: run.id === demoRun.id ? `${run.coordinator.hostSession}.jsonl` : "rollout-done-0002.jsonl",
    models: [run.id === demoRun.id ? "claude-opus-4-6" : "gpt-6.1-sol"],
    tokens: { input: 64_200, output: 14_800, cacheRead: 112_000, cacheWrite: 8_400, total: 199_400 },
    firstTs: list[0]?.ts,
    lastTs: list[list.length - 1]?.ts,
    total,
    start,
    hasMore: start > 0,
    items: list.slice(start, end).map((item, i) => ({ ...item, index: start + i })),
  };
}

export function mockVerifyLog(runId: string, artifactId: string, opts?: { offset?: number; limit?: number }): VerifyLogSlice {
  const full = MOCK_VERIFY_LOGS[artifactId] ?? `[mock] verify log ${artifactId} (run ${runId})\nok`;
  const totalBytes = new TextEncoder().encode(full).length;
  const offset = Math.max(0, Math.floor(opts?.offset ?? 0));
  const limit = opts?.limit == null ? Math.max(1, full.length - offset) : Math.max(1, Math.floor(opts.limit));
  const content = offset >= full.length ? "" : full.slice(offset, offset + limit);
  return {
    artifact: artifactId,
    content,
    totalBytes,
    truncated: offset + content.length < full.length,
    offset,
  };
}

const mockEvents: DaemonEvent[] = [
  {
    id: 5,
    ts: T5,
    runId: demoRun.id,
    taskId: "t_test0001",
    attemptId: "a_test0002",
    workerId: "w_dead0001",
    category: "ai",
    kind: "deep_diagnose",
    status: "ok",
    title: "AI 深度诊断完成：test（retry_fresh）",
    detail: "根因：server.listen(0) 尚未完成绑定前测试请求挂起导致超时。",
    durationMs: 1420,
    tokens: { input: 1650, output: 320, cacheHit: 900 },
    cost: 0.0005,
  },
  {
    id: 4,
    ts: T4,
    runId: demoRun.id,
    taskId: "t_test0001",
    attemptId: "a_test0002",
    workerId: "w_dead0001",
    category: "gate",
    kind: "verify_gate",
    status: "warn",
    title: "任务 verify 门禁拒绝（1/8）：test",
    detail: "npm test exit=1: 等待 worker_done 超时",
    durationMs: 3210,
  },
  {
    id: 3,
    ts: T3,
    runId: demoRun.id,
    taskId: "t_daemon01",
    category: "schedule",
    kind: "conflict_check",
    status: "ok",
    title: "Plan 并行写冲突已自动隔离（1 个任务）",
    detail: "已为 daemon 启用 isolation: worktree",
  },
  {
    id: 2,
    ts: T1,
    runId: demoRun.id,
    taskId: "t_spec0001",
    attemptId: "a_spec0001",
    workerId: "w_idle0001",
    category: "ai",
    kind: "handover_compact",
    status: "ok",
    title: "交接摘要已压缩：写 TECH_SPEC",
    detail: "186 chars -> 28 chars",
    durationMs: 380,
    tokens: { input: 620, output: 64, cacheHit: 400 },
    cost: 0.0002,
  },
  {
    id: 1,
    ts: T0,
    runId: demoRun.id,
    category: "ai",
    kind: "auto_title",
    status: "ok",
    title: "Run 自动命名：可观察编排 MVP",
    durationMs: 410,
    tokens: { input: 340, output: 24, cacheHit: 0 },
    cost: 0.0001,
  },
];

export function mockDaemonHealth(): DaemonHealth {
  return { ok: true, pid: 42088, version: "0.0.6", startedAt: T0 };
}

export function mockAiStatus(): DaemonAiStatus {
  return {
    enabled: true,
    configured: true,
    keySource: "file",
    keyFilePath: "~/.local/state/herdr2/drove/secrets/ai.key",
    baseUrl: "http://127.0.0.1:11434/v1",
    model: "qwen3-coder",
    effort: "low",
    circuitOpen: false,
    consecutiveFailures: 0,
    switches: {
      verifyDigest: true,
      handoverCompact: true,
      autoTitle: true,
      taskEnrich: true,
      conflictPolicy: "auto_worktree",
      questionMode: "draft",
      loopDetect: "steer_once",
    },
  };
}

export function mockTestAiConnection(): DaemonAiProbeResult {
  return { ok: true, latencyMs: 184, model: "qwen3-coder", reply: "pong" };
}

export function mockSaveAiConfig(patch: import("./types").DaemonAiConfigPatch): DaemonAiStatus {
  const base = mockAiStatus();
  return {
    ...base,
    enabled: patch.enabled ?? base.enabled,
    baseUrl: patch.baseUrl?.trim().replace(/\/+$/, "") || base.baseUrl,
    model: patch.model?.trim() || base.model,
    effort: patch.effort?.trim() || base.effort,
    contextWindow: patch.contextWindow !== undefined ? (patch.contextWindow.trim() || undefined) : base.contextWindow,
    maxTokens: patch.maxTokens !== undefined ? (patch.maxTokens || undefined) : base.maxTokens,
    configured: patch.clearKey ? false : patch.apiKey?.trim() ? true : base.configured,
    keySource: patch.clearKey ? "none" : patch.apiKey?.trim() ? "file" : base.keySource,
    switches: {
      verifyDigest: patch.verifyDigest ?? base.switches.verifyDigest,
      handoverCompact: patch.handoverCompact ?? base.switches.handoverCompact,
      autoTitle: patch.autoTitle ?? base.switches.autoTitle,
      taskEnrich: patch.taskEnrich ?? base.switches.taskEnrich,
      conflictPolicy: patch.conflictPolicy ?? base.switches.conflictPolicy,
      questionMode: patch.questionMode ?? base.switches.questionMode,
      loopDetect: patch.loopDetect ?? base.switches.loopDetect,
    },
  };
}

export function mockDaemonEvents(opts?: {
  runId?: string;
  category?: DaemonEventCategory;
  status?: DaemonEventStatus;
  before?: number;
  limit?: number;
}): { events: DaemonEvent[]; hasMore: boolean } {
  const limit = opts?.limit ?? 100;
  const filtered = mockEvents.filter((ev) => {
    if (opts?.runId && ev.runId !== opts.runId) return false;
    if (opts?.category && ev.category !== opts.category) return false;
    if (opts?.status && ev.status !== opts.status) return false;
    if (opts?.before != null && ev.id >= opts.before) return false;
    return true;
  });
  return { events: filtered.slice(0, limit), hasMore: filtered.length > limit };
}

export function mockInspectTask(id: string, opts?: { ai?: boolean }): Diagnosis {
  const taskObj = db.tasks.find((item) => item.id === id) ?? db.tasks[0]!;
  const taskAttempts = db.attempts.filter((a) => a.taskId === taskObj.id);
  const latest = taskAttempts[taskAttempts.length - 1];
  const fallbackAi = opts?.ai !== false ? {
    at: new Date().toISOString(),
    basedOnUpdatedAt: taskObj.updatedAt,
    rootCause: latest?.failure?.message ?? "Mock AI 深度诊断结果：未发现额外阻塞",
    evidence: latest?.failure ? [`failure: ${latest.failure.kind} - ${latest.failure.message}`] : [],
    recommendedAction: "retry_fresh" as const,
    suggestedCommand: `co retry ${taskObj.key} --fresh`,
  } : undefined;
  return {
    task: taskObj,
    attempt: latest,
    failure: latest?.failure,
    routeReason: latest?.routeReason,
    lastItems: [],
    filesModified: taskObj.result?.filesModified ?? [],
    gitStatus: "",
    suggestion: latest?.aiDiagnosis?.suggestedCommand ?? `co retry ${taskObj.key}`,
    ...(opts?.ai === false ? {} : { ai: latest?.aiDiagnosis ?? fallbackAi }),
  };
}
