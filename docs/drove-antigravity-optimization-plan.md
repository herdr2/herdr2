# Drove 借鉴 Antigravity 多智能体机制改进与优化计划

## 1. 背景与总体设计原则

`drove` 作为内置于 `herdr2` 单守护进程的原生多智能体 DAG 编排引擎（见 `docs/drove-architecture.md`），在**跨厂商异构 ACP 混编**、**守护进程级确定性门禁与三段式 Review 硬熔断**、**无损临时索引 Git Worktree 自动合入**以及 **OS 级进程沙盒**四个硬核工程维度上具备明显优势。

对标 Antigravity 的五层协同体系（`Subagents`、`run_workflow`、`/swarm`、`/boost`、`/teamwork` 及底层 `PreToolUse` / `Stop` Hooks），本计划围绕**冷启动上下文继承、强 Schema 交单工具、限流原地保会话恢复、双层权限护栏前移、指针优先上下文与共享契约槽、破坏性证伪审查及带引用溯源的全局叙事简报**七大方向进行系统升级：

1. **保留确定性守护进程状态机，不退化为纯自由聊天黑板**：继续以 DAG 拓扑、自动 Worktree 合入与门禁硬拦截为主轴，仅用有序共享契约槽（`ContractPost`）补充并发节点间的横向契约对齐。
2. **复用低成本控制面 Core AI，不常驻高价辅助 Worker**：通过 `drove/ai.rs` 的 `Fast` 档控制面 AI 按关键事件防抖刷新 `RunBriefing`，并由服务端对每条引用（`task` / `attempt` / `message` / `event` / `contract`）执行确定性验真，空闲期 0 Token 消耗。
3. **最大化复用 `herdr2` 底座**：复用 `handoff.rs` / `api/handoff.rs`（会话分叉与路径改写）、`mcp.rs`（内置 stdio MCP）与 `acp/mod.rs`（在线 `session/set_config_option` / `session/set_model`）。

---

## 2. 七大核心改进项

1. **极简强 Schema Worker MCP（`h2 mcp --profile drove-worker`）**：
   - 为 Drove Worker 自动注入轻量 `drove` MCP 服务，仅暴露 `drove_finish`、`drove_ask`、`drove_context` 三个正交工具，并在 `tools/list` 时动态把活跃任务的 `outputSchema` 内联进 `drove_finish.inputSchema.properties.data`，消除复杂 JSON 在 Shell 参数中的转义错误与漏敲 `worker done` 空转，同时保留 `h2 drove worker` CLI 兼容回退。
2. **`forkFrom` 热上下文分叉（`coordinator` | `<task_key>`）**：
   - 支持在 `plan.yaml`、`h2 drove add` 与 `h2 drove quick` 中声明 `forkFrom`（及 `forkMode: auto | native | package`）。同 CLI 家族（Claude/Codex）优先克隆会话 JSONL 并重写工作树路径以命中 KV Cache；异构 Provider 自动生成有界交接包（配合 Core AI 折叠轮次蒸馏与 `rewrite_origin_paths` Worktree 路径改写）。
3. **同 Adapter 原地保会话热切模型（In-Place Model Failover）**：
   - 当运行中的 Worker 遇到 429 速率限制、配额耗尽或模型过载，且下一顺位候选同属一个 ACP Adapter、进程存活且上下文水位健康（`< 75%`）时，直接通过 `apply_warm_config` 原地切换模型并在原会话、原未提交 Worktree 上续跑，避免丢失已完成工作进度。
4. **双层角色权限护栏前移**：
   - **启动期收敛（`readonly`）**：对只读角色关闭无条件 `dangerously_skip_permissions`，在 ACP 权限层自动放行只读工具与 `drove_*` 工具、自动拒绝写操作。
   - **事前路径守卫（`scoped_write` / `readonly`）**：在 ACP `handle_permission` 与 CLI `PreToolUse` 钩子（`h2 drove worker guard-hook` / `check-path`）中毫秒级校验目标路径是否命中 `allowedPaths`，越界当场阻断并提示允许范围，配合 OS 沙盒与 `worker done` 基线树比对形成三层纵深防御。
5. **指针优先上下文（`#ref` Pointer-First）与共享契约槽（`ContractBoard`）**：
   - 当上游 `result.data` 超出内联预算（单节点 `> 512 B` 或总计 `> 2 KiB`）时，自动折叠为 `compactSummary` + 结构预览 + `@task:<key>#data` 指针，支持按 `jsonPath` 精准读取；新增按 `channels` 订阅的有序共享契约槽（`#contract:<seq>`），解耦并行任务的接口对齐。
6. **`breaker` 破坏性证伪审查模式（`review.mode: static | breaker | hybrid`）**：
   - 在一次性隔离 Worktree 中派生 `ScopedWrite` 受限的异构 Breaker Worker（业务源码只读锁死，仅允许修改 `testPaths`），专门构造边界/并发单测运行验证命令尝试证伪候选补丁；跑出真实编译通过但断言失败的用例即附带复现补丁打回，纳入三段式 Review 熔断管理。
7. **带强制溯源引用的全局叙事简报（`RunBriefing`）**：
   - 在关键状态变更时防抖调用控制面 Core AI 生成 `RunBriefing`（`headline`、`runningSummary`、`threads`、`attention`、`risks`），服务端强制校验每条 `refs` 指向真实存在的实体（无效引用自动剔除），并在 `Drove Web` 顶部 `OverviewBar` 与 `h2 drove show` 同步呈现。
