---
name: drove
description: Multi-agent DAG orchestration inside herdr2. Coordinates specialized Role workers (`coder`, `tester`, `writer`, `illustrator`, `reviewer`, `researcher`) spawned as native headless ACP `a*` sub-agents under your session (`parent`), handles blocking worker questions (`inbox` / `reply`), verify gates, git/AOSP worktree merges, and adversarial reviews.
disable-model-invocation: true
---

# `/drove`: Multi-Agent Task Dispatch & DAG Orchestration

When invoked, you act as the **Coordinator** for a single Drove Run inside `herdr2`. Drove spawns workers as real `herdr2` headless ACP `a*` sub-agents parented to your session (`$H2_AGENT_ID` / `$H2_TERMINAL_ID`), so users can monitor every worker live in the bottom-left **Agents** tree and view the macro DAG in **Drove Web** (`h2 drove web`).

---

## 1. Choose the Right Execution Size

- **Simple inline edit (1-2 small files)**: Handle directly in the current conversation.
- **Single delegated task**: Use `h2 drove quick`!
  ```bash
  h2 drove quick "Implement auth middleware" \
    --role coder \
    --cwd /path/to/repo \
    --verify "cargo test" \
    --worktree \
    --wait
  ```
- **Multi-task DAG (sequential, parallel, `foreach`, `when`, adversarial `review`)**: Write `/tmp/plan.yaml` and run `h2 drove plan`:
  ```yaml
  objective: Refactor storage layer and add tests
  maxConcurrency: 3
  verify: "cargo test"
  goal:
    autoRepair: true
    maxRounds: 2
  tasks:
    - key: impl
      title: Implement storage engine
      role: coder
      forkFrom: coordinator     # optional: inherit distilled/native context from coordinator or upstream task key
      channels: [storage-api]   # optional: subscribe to Run-scoped shared contract board (#contract:<seq>)
      isolation: worktree
      review:
        mode: hybrid            # static (default) | breaker (throwaway adversarial test worktree) | hybrid
        maxRounds: 2
      spec:
        target: src/storage.rs
        change: |
          Implement indexed lookup and atomic persistence.
        acceptance: Clean unit tests and API compatibility
        verify: "cargo test storage"
    - key: tests
      title: Add integration tests
      role: tester
      deps: [impl]
      channels: [storage-api]
      spec:
        target: tests/storage_test.rs
        change: Write regression tests covering concurrent reads/writes.
        acceptance: Integration suite passes
        verify: "cargo test --test storage_test"
  ```
  Then submit and monitor:
  ```bash
  h2 drove plan --file /tmp/plan.yaml --cwd /path/to/repo
  h2 drove web   # optional browser DAG view (opens automatically on local SSH host when remote)
  h2 drove wait --timeout 300
  ```

---

## 2. Role Routing (`Settings -> Drove`)

Roles (`coder`, `tester`, `writer`, `illustrator`, `reviewer`, `researcher`, plus custom roles) and their candidate `Agent + Model + Thinking Effort + Strengths + Note` plus optional global routing `note` (e.g., time-window or quota-cost policies) are configured by the user (or auto-configured via AI) in **`Herdr2 App -> Settings -> Drove`**.

- Assign tasks by `role:` in YAML or `--role` in CLI; add concise `tags:` (e.g. `[前端UI]`, `[后端架构]`, `[快速修复]`) or clear `title`/`files` so the Drove router automatically picks the candidate whose configured `strengths` and `note` best fit the task and current local time (with AI routing evaluation, Magpie quota skipping, and `#1 -> #2 -> #3` retry rotation).
- You can inspect all roles, candidate models, their `strengths`, and global/per-candidate `note` at any time via `h2 drove settings`; if you ever choose or override `--agent`/`--model` manually when spawning sub-agents, always read `config.note` and each candidate's `note` first and follow those constraints (e.g., avoiding double-quota models during restricted hours unless no alternative is available).
- Access guardrails are enforced in **two layers** (pre-tool ACP permission interception / `h2 drove worker guard-hook` before disk writes, plus post-turn baseline diff verification on completion):
  - `readonly` (`reviewer`, `researcher`): zero workspace edits permitted (ACP pre-tool auto-deny + OS sandbox: Linux `bwrap --ro-bind` / macOS `sandbox-exec` / Windows `restricted-token` + diff check).
  - `scoped_write` (`tester`, `writer`, `illustrator`): edits restricted to configured `allowedPaths` globs (auto-denied before write and verified on completion with `422 role_scope_violation` if exceeded).
  - `writable` (`coder`): workspace edits permitted.
- **Minimal Strong-Schema Worker MCP & `#ref` Pointers**:
  - Every spawned ACP worker automatically receives a lightweight `drove` MCP server (`h2 mcp --profile drove-worker`) exposing only 3 tools: `drove_finish` (with the task's `outputSchema` dynamically embedded into `data`), `drove_ask`, and `drove_context` (supporting `target: "@task:<key>#data"` + `json_path` slicing and `#contract:<seq>` reads/publishes). CLI `h2 drove worker` remains available as a 100% compatible fallback.
  - Upstream `result.data` payloads larger than `512 B` are collapsed into `@task:<key>#data` pointers with JSON shape summaries so fan-in tasks stay lean.
- **In-Place Model Failover**:
  - On 429/rate-limit/quota errors, Drove automatically switches a healthy live worker session (`< 75%` context) to the next fallback model on the **same ACP adapter** in place without losing uncommitted worktree edits or session history. You can also switch a running worker's model manually via `h2 drove route <task> --model <model> --in-place`.
- **Worktree Preflight Guard**: Before starting a Run that uses `--worktree` / `isolation: worktree` or concurrent writable tasks under `conflictPolicy=auto_worktree`, Drove verifies the workspace is ready for Git worktrees. If it returns `worktree_not_ready` (e.g., not a Git repo, no initial commit yet, in-progress merge/rebase, lock file, or nested unborn `.git`), immediately show the user the reported reason and fix command and wait for them to resolve it before retrying.

---

## 3. Event Loop & Handling Worker Messages

1. Run `h2 drove wait` to block efficiently until a worker asks a question, fails, or all tasks settle.
2. Check unread coordinator mail at any time:
   ```bash
   h2 drove inbox
   h2 drove reply <msg_id> "Use port 8080"
   h2 drove ack <msg_id>
   ```
3. Inspect failures, dynamically reshape pending tasks, or intervene cleanly:
   ```bash
   h2 drove inspect <task_key_or_id>
   h2 drove send <task_key_or_id> "Check edge case X" [--interrupt]
   h2 drove edit <task_key_or_id> [--change "..."] [--deps a,b | --clear-deps] [--worktree]
   h2 drove route <task_key_or_id> --model <model> --in-place
   h2 drove cancel <task_key_or_id> [--skip-ok]
   h2 drove retry <task_key_or_id> --hint "Fix test assertion" [--role tester] [--fresh]
   ```
4. Finalize the Run (runs Run-level `verify` + `goal.autoRepair` if enabled; healthy workers stay warm `idle` until the Coordinator closes or the Run is deleted in Drove Web):
   ```bash
   h2 drove complete --summary "Implemented and verified storage refactor"
   ```

---

## 4. Review 熔断与主 Agent 接管协议（强制规范 / Review Circuit-Breaker）

为避免对抗式 Review 陷入无限打补丁与二阶故障注入的收益递减循环，Drove 对**所有 Review 流程**（含 `task.review` 对抗审查门禁与独立 `role: reviewer` 任务，覆盖自动修复与手动 `h2 drove retry`）强制执行三段式熔断状态机：

1. **连续 2 轮 Review 未通过 → 熔断并转交主 Agent 亲自接管（`handoff_to_coordinator`）**：
   - 当某个审查任务或被审查节点连续 **2 轮** 未通过时，Drove 立即触发熔断并向 Coordinator 发送高优 `escalation` 通知，汇总前两轮的关键未通过项。
   - 此时**停止**常规 worker 修复-review 循环，**禁止**通过 `h2 drove add` 新建重复 review 任务绕过熔断，必须由 **主 Agent（Coordinator）亲自阅读代码与分歧点并直接修复、完成本地验证**。
2. **主 Agent 接管处理完成后 → 仅允许再 Review 1 次（`post_handoff_review`, 1/1）**：
   - 主 Agent 完成直接修改并确认测试通过后，执行 **1 次** `h2 drove retry <task>` 触发唯一一次终审复核（post-handoff review）。
3. **终审复核仍不通过 → 硬熔断并立即上报用户（`awaiting_user_decision`）**：
   - 若这一次复审**仍未通过**，Drove 进入硬熔断状态 `awaiting_user_decision`，默认拦截所有后续 `h2 drove retry`（返回 `409 review_circuit_open`）和无 `--force` 的 `h2 drove complete`。
   - 主 Agent **必须立即停手向用户汇报**：
     - 当前状态与历轮 review 的关键未通过项；
     - 仍未通过的核心分歧点及其实际影响；
     - **主 Agent 自己的明确处理建议**（例如：接受现状直接收口 / 简化设计规避过度复杂 / 定向继续修某项）。
   - **必须等待用户明确拍板后再行动**：
     - 用户批准定向继续修并再审：`h2 drove retry <task> --user-override --override-reason "<用户决策说明>"`（每次 override 仅放行 1 轮受控复审）；
     - 用户批准接受现状直接收口：`h2 drove cancel <task>` 后 `h2 drove complete`，或直接 `h2 drove complete --force`。

---

## 5. SSH Remote Transparency

Append `--on <machine>` to operate on a connected remote `herdr2` host (`h2 drove plan --on rohan ...`, `h2 drove wait --on rohan ...`). Local `Drove Web` (`127.0.0.1:7799`) automatically aggregates and proxies `<machine>/<run_id>` Runs and streams remote SSE events.
