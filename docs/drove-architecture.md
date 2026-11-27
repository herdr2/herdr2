# Drove Architecture Inside `herdr2`

Drove is the multi-agent DAG orchestrator built natively inside `herdr2` (`herdr-core/crates/herdr2/src/drove/`), combining single-daemon Rust execution with the Herdr2 Desktop UI (`herdr-app`) and the read-only macro observability workbench (`drove-web/`).

---

## 1. Single-Daemon Design (Zero External Node/acpx Processes)

All orchestration runs inside the existing `herdr2` daemon (`DroveRuntime`):

- **State & Secrets (`~/.local/state/herdr2/<session>/drove/`)**:
  - `state.json`: atomic persisted state for Runs, Tasks, Workers, Attempts, Messages, and DaemonEvents (`store.rs`).
  - `items/<worker_id>.jsonl`: append-only worker transcript items streamed over SSE.
  - `secrets/` (`0700`): `ai.key` (`0600`) and `coordinator.token` (`0600`), plus `artifacts/` (`verify-*` logs and `review-*.patch` diffs).
- **Config (`~/.config/herdr2/drove.json`)**:
  - Stores role bindings (`coder`, `tester`, `writer`, `reviewer`, `researcher`, custom roles), minimal AI Assistant settings (`enabled`, `baseUrl`, `model`, switches — raw API key never written here), and optional `magpie` quota toggle (default `enabled: false`).
- **Worker Pool & Bottom-Left Agents Tree (`pool.rs`, `api/run.rs`, `mcp.rs`)**:
  - Every Drove worker spawns directly via `Core` as a headless ACP `a*` sub-agent (`name: None`, `notify: false`, `mcp_profile: Some("drove-worker")`, `parent` = coordinator `t*` or `a*`), injecting a minimal 3-tool MCP surface (`drove_finish` with dynamic `outputSchema`, `drove_ask`, `drove_context`) while keeping `h2 drove worker` CLI fallback.
  - `dangerously_skip_permissions` is enabled only for `writable` roles; `readonly` and `scoped_write` workers route permission requests through `decide_drove_worker_permission` (`acp/mod.rs`) to auto-approve reads/`drove_*` tools and pre-emptively deny out-of-scope edits in `<0.1ms` before disk mutation.
  - Warm Context Fork (`forkFrom: coordinator | <task_key>`) reuses `api/handoff.rs` for same-provider native JSONL fork + `session/load` or cross-provider path-rewritten `<herdr2-handoff kind="fork">` packages. On 429/quota errors, `try_in_place_model_failover` switches healthy live sessions (`< 75%` context) to a same-adapter fallback model via `apply_warm_config` without discarding uncommitted worktree changes.
  - Live display text uses `a.title` + `labels["drove.display"]` (`"<role> · <task_key>"` while busy, `"<role> · idle"` when warm idle) so the bottom-left **Agents** tree in `herdr-app` displays worker teams natively without running into `Agent.name` uniqueness/charset rules.
  - Completed author, worktree, and reviewer workers stay `idle` (and Git worktrees are merged and reset clean) across task completion and `h2 drove complete` for warm reuse via `h2 drove add`. Workers are closed only when the Coordinator session exits/is trashed, the user deletes/trashes the Run in Drove Web, or explicitly stopped.
  - `Core` -> `DroveRuntime` lifecycle events flow lock-free over `mpsc::UnboundedSender<PoolSignal>`, preventing lock-order deadlocks and keeping external `h2 kill <a*>` and coordinator close/trash hooks synchronized with Drove worker/attempt lifecycle.
- **Process Sandboxing (`sandbox.rs`)**:
  - **Linux (`bwrap`)**: When `bwrap` user namespaces are available, worker launches bind `<state>/drove` read-only, shadow `<state>/drove/secrets` and `<state>/secrets` with `--tmpfs`, re-bind `<cwd>` writable when it is a worktree under `<state>/drove/worktrees`, and `--ro-bind <cwd>` for `readonly` roles.
  - **macOS (`sandbox-exec`)**: When `/usr/bin/sandbox-exec` (Seatbelt) is available, worker launches run with an inline SBPL profile that denies writes to `<state>/drove`, `drove.json`, and `ai.json` (re-allowing writable worktrees under `<state>/drove/worktrees`), denies writes to `<cwd>` for `readonly` roles, and denies all reads & writes to `<state>/drove/secrets` and `<state>/secrets`.
  - **Windows (`restricted-token`)**: Worker launches run through `herdr2 __drove-sandbox -- <argv>` using Win32 `CreateRestrictedToken` (`DISABLE_MAX_PRIVILEGE` + `NT AUTHORITY\RESTRICTED` `S-1-5-12` in `SidsToRestrict`) + `CreateProcessAsUserW` + `KILL_ON_JOB_CLOSE` Job Object containment, paired with idempotent NTFS Deny ACEs for `S-1-5-12` blocking read/write on `<state>/drove/secrets` & `<state>/secrets` and blocking write on `<state>/drove`, `drove.json`, and `ai.json` (with a protected DACL on `<state>/drove/worktrees` so worktrees stay writable without mutating user repo ACLs; `readonly` workspace edits continue to be caught by `roles.rs` diff guardrails).

---

## 2. UI Split: Herdr2 App (`Settings -> Drove`) vs Read-Only `drove-web`

- **`herdr-app` (`defaults/frame/settings/main.js` -> `Drove` tab)**:
  - Single control plane for configuring Drove across `This computer` and SSH-paired machines:
    - **Roles ONLY** (`Agent`, `Model`, `Thinking Effort`, optional `Fallback`, `+ Add role`).
    - **AI Assistant** (`enabled`, `Service URL`, masked `API Key`, `Save`, `Test Connection` — no vendor presets).
    - **Magpie Quota (Optional)** (`enabled` default OFF, optional gateway URL + `Test`).
  - Secrets are written to temporary local JSON files and immediately deleted via `--file <tmp> --consume` so API keys never appear on CLI `argv`.
- **`drove-web/` (`127.0.0.1:7799` / `H2_DROVE_PORT`)**:
  - Served directly by `herdr2`'s localhost HTTP/1.1 + SSE server (`http.rs`, `routes.rs`) with injected `window.__DROVE_TOKEN__`.
  - Shows DAG visualization, live worker item streams, review diff patches, verify logs, and read-only Role matrix + stats (`PUT /api/agents` & `PUT /api/ai/config` return `405 configured_in_app`).

---

## 3. Execution, Guardrails & SSH Federation

- **DAG Scheduler & Router (`scheduler.rs`, `router.rs`, `protocol.rs`)**:
  - Supports static `foreach`, dynamic `foreachFrom` JSON artifact expansion, `when` conditions, `triggerRule` (`all_success` / `any_success`), native `kind: exec` shell steps, warm same-role worker reuse, and silence stall watchdogs.
  - Coordinator wakeups (`notify.rs`) deliver terse summaries to `t*` via `Core::pty_send` (when idle) or `a*` via `Core::send`.
- **Quality Gates & Grounded Narrator (`roles.rs`, `verify.rs`, `integrate.rs`, `review.rs`, `ai.rs`)**:
  - **Two-Layer Role Scope**: Intercepts out-of-scope writes pre-tool (`acp/mod.rs` + `h2 drove worker guard-hook`) and diffs touched workspace paths against an attempt baseline on completion (`422 role_scope_violation`).
  - **Pointer-First `#ref` & Shared Contract Board (`protocol.rs`, `store.rs`)**: Collapses upstream `dep.result.data > 512 B` into `@task:<key>#data` pointers with JSON shape summaries + `jsonPath` slicing, and maintains Run-scoped sequenced `ContractPost` entries (`#contract:<seq>`) filtered by `channels`.
  - **Verify Gates**: Executes task `spec.verify` and Run `verify` (`goal.autoRepair`), storing artifact logs and tracking stagnant output hashes.
  - **Worktrees**: Creates isolated detached Git / AOSP `./bin/harness` worktrees and serializes merge-back under a per-run async mutex.
  - **Adversarial Review (`static | breaker | hybrid`)**: Supports read-only diff review (`static`), throwaway Git worktree falsification where a `ScopedWrite` Breaker writes edge-case tests inside `testPaths` and the daemon independently reproduces `verifyCmd` failures (`breaker`), or `hybrid` (Breaker followed by Static review), all governed by the 3-stage `ReviewCircuit`.
  - **Grounded Run Narrator (`RunBriefing`)**: Event-driven debounced Core AI (`run_briefing`) synthesis verified deterministically against real `Store` IDs/keys (`sanitize_and_verify_run_briefing`), surfaced in `Drove Web` (`OverviewBar`) and `h2 drove show`.
- **Multi-Machine SSH Federation (`http.rs`, `remote.rs`, `mod.rs`)**:
  - Local `h2` (`:7799`) aggregates `<machine>/<run_id>` from connected SSH daemons, proxies `<machine>/*` and `?on=<machine>` API calls via `drove.http_proxy`, relays SSE events via framed `drove.subscribe` streams, and handles `$/drove_open_web` notifications so `h2 drove web` inside SSH sessions opens the user's local browser cleanly.
