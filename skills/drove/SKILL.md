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
      isolation: worktree
      review:
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

Roles (`coder`, `tester`, `writer`, `illustrator`, `reviewer`, `researcher`, plus custom roles) and their candidate `Agent + Model + Thinking Effort + Strengths` (work types each candidate excels at) are configured by the user (or auto-configured via AI) in **`Herdr2 App -> Settings -> Drove`**.

- Assign tasks by `role:` in YAML or `--role` in CLI; add concise `tags:` (e.g. `[前端UI]`, `[后端架构]`, `[快速修复]`) or clear `title`/`files` so the Drove router automatically picks the candidate whose configured `strengths` best match the task (with optional AI assistant tie-breaking, Magpie quota skipping, and `#1 -> #2 -> #3` retry rotation).
- You can inspect all roles, candidate models, and their `strengths` at any time via `h2 drove settings`; only override `--agent`/`--model`/`--effort` via `h2 drove route` if explicitly requested.
- Access guardrails are enforced automatically on `drove worker done`:
  - `readonly` (`reviewer`, `researcher`): zero workspace edits permitted (`bwrap --ro-bind` + diff check).
  - `scoped_write` (`tester`, `writer`, `illustrator`): edits restricted to configured `allowedPaths` globs (`422 role_scope_violation` if exceeded).
  - `writable` (`coder`): workspace edits permitted.
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
3. Inspect failures or intervene cleanly:
   ```bash
   h2 drove inspect <task_key_or_id>
   h2 drove send <task_key_or_id> "Check edge case X" [--interrupt]
   h2 drove retry <task_key_or_id> --hint "Fix test assertion" [--role tester] [--fresh]
   ```
4. Finalize the Run (runs Run-level `verify` + `goal.autoRepair` if enabled; healthy workers stay warm `idle` until the Coordinator closes or the Run is deleted in Drove Web):
   ```bash
   h2 drove complete --summary "Implemented and verified storage refactor"
   ```

---

## 4. SSH Remote Transparency

Append `--on <machine>` to operate on a connected remote `herdr2` host (`h2 drove plan --on rohan ...`, `h2 drove wait --on rohan ...`). Local `Drove Web` (`127.0.0.1:7799`) automatically aggregates and proxies `<machine>/<run_id>` Runs and streams remote SSE events.
