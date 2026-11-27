# Changelog

## v2.0.20

#### 中文

**新增功能**
- **更新操作自动同步已安装 Skill (`h2 skill sync`)**：
  - 新增 `h2 skill sync` 命令（支持 `--on <MACHINE>` 远程同步）及自定义安装目录持久化记录（`skill_targets.json`），仅原地更新已存在且内容发生变化的内置 Skill，不创建未安装 Agent 目录、不补写未安装子集，重复执行完全幂等。
  - `h2 update`、`h2 machine update` / `machine add --install`、桌面端全渠道更新（Core、App、UI 热更新、外部安装包升级重启、SSH 远端同步）以及 `install.sh` / `install.ps1` 安装脚本现均自动同步已安装 Skill，无需手动进入设置页更新。
- **托管 ACP 适配器版本检测与设置页交互重设计**：
  - `adapter.status` 新增 `--refresh` 参数（轻量拉取 ~75 KB Registry 清单并返回 `latest_version` / `update_available`）；`adapter.update` 先比对本地 `meta.json` 版本号，已是最新时直接返回 `up_to_date`，避免重复下载 ~235 MB 大包或将健康独立二进制误降级为 npm。
  - 「设置 → 智能体与 ACP」彻底拆分「检查 ACP 更新」与「发现新版本才展示更新按钮」：单行按实际状态区分绿色 **已是最新** / 橙色 **可更新**（仅此时出现主按钮 **更新至 vX.Y.Z**）/ 红色 **更新失败 + 重试更新**；顶部批量按钮区分 **检查 ACP 更新**、**更新可用 ACP (N)**、**安装缺失 ACP (N)** 与 **按需下载其余 ACP (N)**，所有结果均提供清晰 Toast 反馈。

**问题修复**
- **彻底修复右上角按钮点击更新报 `checksum mismatch`**：
  - 桌面端触发更新（`start_update` / `start_ui_update`）在下载前强制拉取带 `Cache-Control: no-cache` 的实时 `*-latest.json` 清单，若遇远端滚动发布导致校验不匹配则自动等待 1 秒重拉清单并重试一次（覆盖 App、内置 Core 与 UI Bundle）。
  - `h2 update` 与 `h2 machine update` 在 GitHub 扁平滚动 Release（`ota-latest`）上始终以 `core-latest.json` 为准，避免读取已被新归档覆盖的历史 `core-<v>-manifest.json` 并在校验不匹配时自动重试。
  - CI 发布工作流新增共享 `herdr2-ota-publish` 并发串行锁，正式版 Tag 提交自动跳过重复的 `ota-update.yml` 发布，并将 `gh release upload` 调整为先上传压缩包与安装脚本、最后上传 `*-latest.json` 清单，消除发布窗口期的半更新竞态。
- **对齐 ACP 适配器前端超时并修复错误行重装参数**：前端 `adapter.install` / `adapter.update` 超时由 240s 提升至 600s，并修复异常行点击「重装」未传递 `--force` 的问题。

---

#### English

**New Features**
- **Automatic Skill Sync During Updates (`h2 skill sync`)**:
  - Added `h2 skill sync` (with `--on <MACHINE>` remote support) and persistent custom target tracking (`skill_targets.json`) to idempotently update only already-installed bundled Skills whose contents changed, without creating directories for uninstalled agents or adding uninstalled subsets.
  - Wired automatic Skill sync into `h2 update`, `h2 machine update` / `machine add --install`, all desktop app update channels (Core, App, UI hot-update, post-installer relaunch, SSH remote sync), and `install.sh` / `install.ps1` installers so users no longer need to update Skills manually in Settings.
- **ACP Adapter Version Checking & Settings UI Redesign**:
  - Added `adapter.status --refresh` to fetch the lightweight ~75 KB Registry manifest (`latest_version`, `update_available`) without downloading archives, and updated `adapter.update` to compare local `meta.json` versions first—returning `up_to_date` when current instead of re-downloading ~235 MB packages or downgrading healthy standalone binaries to npm.
  - Redesigned **Settings → Agents & ACP** to separate **Check ACP Updates** from actual upgrade actions: rows now show **Up to date** (no misleading update button), **Update available** (with **Update to vX.Y.Z**), or **Update failed** (inline error + **Retry**); top batch actions distinguish **Check ACP Updates**, **Update Available ACP (N)**, **Install Missing ACP (N)**, and optional **Download Remaining ACP (N)** with clear toast feedback.

**Bug Fixes**
- **Fixed `checksum mismatch` When Updating from the Top-Right Update Button**:
  - Forced desktop updates (`start_update` / `start_ui_update`) to fetch fresh `*-latest.json` manifests with `Cache-Control: no-cache` before downloading, with an automatic 1-second refresh-and-retry on transient rolling-release checksum mismatches across App, bundled Core, and UI Bundle downloads.
  - Fixed `h2 update` and `h2 machine update` on flat rolling GitHub `ota-latest` releases to always resolve from `core-latest.json` (preventing stale historical `core-<v>-manifest.json` hashes from mismatching rolled archives) and retry once on transient checksum mismatches.
  - Added a shared `herdr2-ota-publish` CI concurrency lock across `ota-update.yml` and `release.yml`, skipped duplicate `ota-update.yml` runs on version-tagged release commits, and ordered `gh release upload` to publish archives and scripts before `*-latest.json` manifests.
- **Aligned ACP Adapter UI Timeouts & Reinstall Flag**: Raised frontend `adapter.install` / `adapter.update` timeouts from 240s to 600s and fixed missing `--force` when clicking **Reinstall** on failed adapter rows.

## v2.0.19

#### 中文

**新增功能**
- **ACP 未登录状态统一识别与一键「终端登录」**：
  - ACP 传输层自动解析 `-32000` 鉴权错误中的 `data.authMethods` 并过滤 `@agentclientprotocol/sdk` 冗余 `stderr` 回显；探测层新增 `pi` 本地模型解析（`~/.pi/agent/settings.json` 与 `models.json`）及 Windows `USERPROFILE` 目录优先级对齐。
  - 设置页、欢迎页与 ACP 会话页统一区分「需登录」与真实适配器故障，为未登录智能体提供一键 **终端登录** 入口（支持 `pi`、`claude`、`cursor-agent`、`codex` 等），不再误导重装适配器。
- **AI 异常排查体验增强**：按模型能力自动解析思考强度变体（不向无档位模型拼接非法后缀），AI 排查启动的 Agent 独立归属目标主机的 Home（`~`）工作区，跨平台远程主机亦正确显示为 `~`。

**问题修复**
- **修复 Windows 下 Cursor CLI 多行提示词截断**：Git Bash 包装脚本在无原生 `.exe` 时优先调用 `.ps1`（`pwsh`/`powershell`），`cmd.exe` 检测到换行符时自动改走 PowerShell 包装，避免批处理参数在首个换行处截断多行 UTF-8 提示词。
- **Drove 编排引擎多项实测核心修复**：
  - 静态 `foreach` 与动态 `foreachFrom` 现递归替换 `exec`（`cmd`/`program`/`args`/`env`）、`spec`（含 `verify`）、`files` 与 `cache` 全字段中的 `{{item}}`。
  - 过滤并发 Worktree 合并导致的 Reviewer / Breaker 越权误判，并为 Breaker 补齐作者私有 Worktree 只读基线校验与时段/备注偏好筛选。
  - `isolation: worktree` 触发 `handoff_to_coordinator` 或等待用户裁决时自动安全同步候选改动至 `run.cwd` 并止损收敛，防止代码丢失与无效重试 `drove_finish`。
  - 修复 Linux `bwrap` 沙箱 NVM 多版本 `PATH` 注入（遵循 `~/.nvm/alias/default` 并补齐其余 Node 版本路径）。
  - 完善 Run 自动收口守卫与 `retry` / `edit` 自动重激活；支持跨 `foreachFrom` 模板节点级联重开与 `h2 drove retry` 遵循上游依赖就绪度。
  - 收紧 MCP `drove_ask` 单次轮询至 45s（兼容 `cursor-agent` 60s 硬超时）并按 `(attempt_id, question)` 幂等去重；`task_enrich` / `run_briefing` 切至 `Nano` 低思考档位并支持空正文截断自动扩容重试；修复 `h2 drove settings --compact` 列表展示与 `h2 ls` 的 `drove.display` 展示。
- **修复托管 ACP 适配器优先级、远程安装超时与待确认对话状态**：
  - 托管适配器目录现优先于 `PATH` 解析以确保 `h2 adapter update` 立即生效；远程 `adapter.install` / `adapter.update` RPC 超时延长至 `+900s` 以兼容大体积独立适配器包下载。
  - 修复 PTY 提交非空提示词后从 `blocked` 立即切回 `working`、允许直接回复 `blocked (question)` 状态的 ACP/PTY 智能体而无需 `--force`/`--steer`，并跳过已有待处理队列消息的回合后提问仲裁。

---

#### English

**New Features**
- **Unified ACP Login-Required Detection & One-Click "Sign In"**:
  - Parsed `data.authMethods` from ACP `-32000` authentication errors in the transport layer while suppressing noisy `@agentclientprotocol/sdk` stderr echoes; added `pi` local model discovery (`~/.pi/agent/settings.json` and `models.json`) and aligned Windows `USERPROFILE` home directory precedence.
  - Settings, Welcome, and ACP session views now distinguish **Login Required** (amber) from true adapter failures and offer a one-click **Sign In** terminal action (`pi`, `claude`, `cursor-agent`, `codex`, etc.) instead of misleading adapter reinstall buttons.
- **Improved AI Troubleshooting & Home (`~`) Workspace Isolation**: Automatically resolves supported reasoning-effort model variants without appending invalid suffixes, isolates AI troubleshooting sessions into the target host's dedicated Home (`~`) workspace, and displays remote Unix home paths consistently as `~`.

**Bug Fixes**
- **Fixed Multiline Prompt Truncation for Cursor CLI on Windows**: Updated Git Bash command wrappers to prefer `.ps1` (`pwsh`/`powershell`) over `.cmd`/`.bat` when no native `.exe` exists, and routed `cmd.exe` multiline arguments through PowerShell so multiline UTF-8 prompts are delivered intact.
- **Drove Orchestration Reliability Fixes**:
  - Recursively expanded `{{item}}` across `exec` (`cmd`/`program`/`args`/`env`), `spec` (including `verify`), `files`, and `cache` for both static `foreach` and dynamic `foreachFrom`.
  - Filtered concurrent sibling worktree merges out of Reviewer/Breaker scope checks, added read-only baseline validation on author worktrees for Breakers, and aligned Breaker candidate routing preferences.
  - Preserved worktree candidate edits into `run.cwd` when `handoff_to_coordinator` or user-decision gates fire, preventing lost work and redundant `drove_finish` retries.
  - Fixed Linux `bwrap` sandbox `PATH` injection for multi-version NVM installs (respecting `~/.nvm/alias/default` plus installed Node version paths).
  - Hardened Run auto-completion guards and automatic reactivation on `retry`/`edit`, supported cascading reopens across `foreachFrom` template barriers, and made `h2 drove retry` respect upstream dependency readiness.
  - Tightened MCP `drove_ask` polling to 45s (avoiding `cursor-agent` 60s timeouts) with `(attempt_id, question)` deduplication, moved `task_enrich`/`run_briefing` to `AiTaskTier::Nano` with empty-body length-truncation retries, and fixed `h2 drove settings --compact` & `h2 ls` detail display.
- **Fixed Managed ACP Adapter Precedence, Remote Install Timeout & Blocked-Question Replies**:
  - Prioritized managed adapter binaries over `PATH` so `h2 adapter update` takes effect immediately, and extended remote `adapter.install`/`adapter.update` RPC deadlines to `+900s` for large standalone adapter archives.
  - Transitioned PTY agents immediately from `blocked` to `working` upon non-empty prompt submission, allowed replying directly to `blocked (question)` ACP/PTY agents without `--force`/`--steer`, and skipped post-turn question arbitration when inbox messages are already queued.

## v2.0.18

#### 中文

**新增功能**
- **设置页「智能体 (Agents)」全生命周期管理升级**：
  - 新增分类筛选视图（**全部 / 已安装 / 未安装 / 需处理**），统一展示每个智能体的 CLI 安装状态、ACP 协议状态（隔离托管 / CLI 内置 / 自定义 `config.toml` / 不支持）、可用模型数以及安装/探测异常状态。
  - 新增 **一键安装 CLI**（在所选本机或远程主机终端执行官方安装命令）、**AI 安装**（针对无静态单行命令的智能体由已装 AI 协助安装）、逐行 **一键安装 / 更新 / 重装 ACP 适配器** 以及按单个异常智能体聚焦排查的 **AI 处理异常** 入口。
- **`knownAgents({ on, refresh: true })` 强制实时刷新**：新增 `refresh` 参数支持跳过 60 秒安装缓存，在新装 CLI 或修复环境后立即识别最新状态。

**问题修复**
- **优化异常排查与状态刷新体验**：支持针对单个异常智能体聚焦启动 AI 排查，并确保点击刷新或完成安装后立即同步最新 CLI 与 ACP 适配器状态。

---

#### English

**New Features**
- **Settings → Agents Full-Lifecycle Management Overhaul**:
  - Added filter tabs (**All / Installed / Not Installed / Action Needed**) showing live CLI availability, ACP status (`Managed` / `Built-in` / `Custom config.toml` / `Not supported`), model counts, and install/probe error states for every agent.
  - Added one-click **Install CLI** (runs the official installer in a terminal on the selected local or SSH remote host), **AI Install** (launches an installed AI agent when no static one-liner exists), per-agent **Install / Update / Reinstall ACP** actions, and focused single-agent **Fix with AI** troubleshooting.
- **Force-Refresh for `knownAgents({ on, refresh: true })`**: Added an optional `refresh` flag in `herdr-app` (`mods.rs` / `herdr.d.ts`) to bypass the 60-second installation cache and detect newly installed CLIs immediately.

**Bug Fixes**
- **Improved Agent Troubleshooting & Refresh Reliability**: Added single-agent focus mode for AI troubleshooting and ensured manual refresh / post-install checks immediately reflect newly installed CLIs and ACP adapters.

## v2.0.17

#### 中文

**新增功能**
- **Drove 编排引擎七项核心升级**：
  - **极简强 Schema Worker MCP (`h2 mcp --profile drove-worker`)**：为 Drove Worker 自动注入仅含 `drove_finish`、`drove_ask`、`drove_context` 的轻量 MCP 服务，动态嵌入任务 `outputSchema` 并保留 CLI 兜底。
  - **双层角色权限护栏**：`readonly` / `scoped_write` Worker 新增毫秒级 Pre-Tool 事前拦截（`decide_drove_worker_permission` + 零 RPC 的 `h2 drove worker check-path` / `guard-hook`）与事后基线复核，不阻塞在人工审批。
  - **热上下文分叉 (`forkFrom`) 与原地会话切模容灾 (`h2 drove route --in-place`)**：支持从协调器或上游任务热分叉上下文（同家族原生 JSONL 分叉 + 跨厂商自动提炼）；遇到 429/限流/配额耗尽时可在同适配器内原地热切模型，保留 Worktree 改动与已读上下文。
  - **`#ref` 指针按需读取与共享契约板**：大体积上游 `result.data` 自动折叠为 `@task:<key>#data` 指针与摘要，新增 `#contract:<seq>` 频道契约发布/订阅与 `jsonPath` 局部切片读取。
  - **对抗式破坏性证伪审查 (`review.mode: breaker | hybrid`)**：在临时隔离 Worktree 中锁定生产代码只读、仅允许编写破坏性测试，经 Daemon 独立重跑 `verifyCmd` 复现失败才触发打回，支持 `keepRegressionTests` 保留回归测试。
  - **可溯源态势简报与 Drove Web 顶部简报栏 (`RunBriefing`)**：3 秒防抖自动刷新全局进度与阻塞风险，带确定性引用校验器剥离无据断言，Web UI 顶部提供可点击跳转的引用徽章。
  - **动态改图与无级联取消**：新增 `h2 drove edit`（`PATCH /api/tasks/<id>`，含 DAG 环检测）与 `h2 drove cancel <task> --skip-ok`（剥离下游依赖而不触发级联跳过）。
- **TRAE CLI 预设、桌面端 Agents 设置与自定义适配器配置**：新增 `trae`（TRAE CLI）预设与图标、设置页独立 `agents` 分区、`adapter.config.set` RPC/CLI 以及多项输入/菜单/工作区体验优化。

**问题修复**
- **Drove 调度、存储与并发集成十项可靠性修复**：
  - 完成门禁阶段豁免 `TurnFinished` / Stall 误杀，落地 `task.timeout_sec` 墙钟超时熔断并先结算 `FailureKind::Timeout` 再停机以支持自动重试；剔除 `worker ask` 挂起与只读独立 Reviewer 避免卡死并发槽位。
  - `state.json` 改为 200ms 防抖合并落盘并为非活跃 Worker 内存 Transcript 增加 LRU 淘汰；引入隔离临时 `GIT_INDEX_FILE` 快照避免污染主仓索引；Worktree 基线漂移无冲突合入后自动复跑 `verify` 并在语义冲突时回滚刷新；新增 Reviewer 并发限流与只读防篡改校验、Warm Worker 上下文膨胀自动退役与 `RunBudget` 预算熔断恢复，并修复流式估算输出 Token 统计为 0 的缺陷。
- **修复 macOS 应用包内置 `h2` 与 OTA 升级同步**：macOS 打包与 OTA 流程现统一将 `h2` 嵌入 `Herdr2.app/Contents/Helpers/h2` 并独立签名，应用内 OTA 升级自动保留/同步更新内置 `h2` 避免断链。

---

#### English

**New Features**
- **Seven Major Drove Orchestration Upgrades**:
  - **Minimal Schema-Driven Worker MCP (`h2 mcp --profile drove-worker`)**: Automatically injects a lightweight 3-tool MCP (`drove_finish`, `drove_ask`, `drove_context`) with dynamic task `outputSchema` embedding while keeping CLI fallback intact.
  - **Two-Layer Role Permission Guardrails**: Enforces millisecond-level pre-tool permission filtering (`decide_drove_worker_permission` plus zero-RPC `h2 drove worker check-path` / `guard-hook`) for `readonly` / `scoped_write` workers alongside post-hoc baseline verification.
  - **Hot Context Forking (`forkFrom`) & In-Place Model Failover (`h2 drove route --in-place`)**: Forks warm context from the coordinator or upstream tasks (native JSONL fork or cross-provider distilled handoff) and hot-swaps models in-place on 429/quota exhaustion without losing worktree edits or context.
  - **`#ref` On-Demand Data Pointers & Shared Contract Board**: Collapses oversized upstream `result.data` into `@task:<key>#data` pointers with `jsonPath` slicing and adds `#contract:<seq>` pub/sub contract channels.
  - **Adversarial Falsification Review (`review.mode: breaker | hybrid`)**: Runs breaker reviews in a temporary isolated worktree (production code read-only, tests writable) and only rejects when the daemon independently reproduces a non-zero `verifyCmd` exit, with optional `keepRegressionTests`.
  - **Grounded RunBriefing & Drove Web Overview Bar**: Debounced AI status briefings with deterministic citation verification (stripping ungrounded claims) rendered in the Drove Web UI overview bar with clickable jump badges.
  - **Live DAG Editing & Non-Cascading Cancel**: Added `h2 drove edit` (`PATCH /api/tasks/<id>` with cycle detection) and `h2 drove cancel <task> --skip-ok`.
- **TRAE CLI Preset, Settings Agents Section & Adapter Configuration**: Added the `trae` (TRAE CLI) preset and icon, dedicated `agents` Settings section, `adapter.config.set` RPC/CLI, and QuickJS input/menu/workspace UX improvements.

**Bug Fixes**
- **Ten Drove Reliability, Storage & Concurrency Fixes**:
  - Exempted synchronous completion gates from premature `TurnFinished`/stall kills, enforced `task.timeout_sec` wall-clock timeouts (recording `FailureKind::Timeout` before stopping so auto-retry works), and excluded `worker ask` waiters and standalone reviewers from `max_concurrency` slot counts.
  - Added 200ms debounced `state.json` flushing with LRU worker transcript eviction, isolated temporary `GIT_INDEX_FILE` snapshots, post-merge semantic `verify` with automatic rollback/refresh on diverged worktrees, per-run reviewer concurrency limits with read-only tamper checks, warm-worker context bloat retirement, `RunBudget` soft/hard circuit breakers, and fixed streaming output token estimation.
- **Fixed macOS Bundled `h2` Packaging & OTA Preservation**: Embedded and signed `Herdr2.app/Contents/Helpers/h2` across macOS release and OTA packages and preserved/upgraded bundled `h2` cleanly during in-app macOS OTA swaps.

## v2.0.16

#### 中文

**新增功能**
- **托管式 ACP 适配器注册表与自动安装 (`h2 adapter status|install|update`)**：新增 `AdapterManager`、`adapter.status` / `adapter.install` / `adapter.update` RPC 与 `h2 adapter` CLI（支持 `config.toml` 配置 `auto_install_adapters` 与 `acp_registry_url`）；从官方 ACP Registry 下载独立二进制（校验 SHA-256）隔离安装至 `~/.local/share/herdr2/adapters/`（Windows 为 `%LOCALAPPDATA%\herdr2\adapters\`），不可达时自动回退隔离 `npm install --prefix`，检测到配套 CLI（`claude`、`codex`、`pi`、`glm`）支持后台自动安装，并在设置页 Drove 面板新增一键安装/更新适配器状态栏。
- **ACP 与 PTY 提问/待确认状态智能识别与区分**：结合确定性规则与 Core AI 仲裁（`status.rs`、`ai/terminal_state.rs`、`terminal.rs`），自动识别 ACP 回合结束或静默 PTY 终端输出中向用户提问、给出选项或等待确认工具调用的场景，准确标记为 `blocked`（「待确认 / Needs you」），并在 Peek 视图中与权限审批（「待审批 / Needs approval」）清晰区分。

**问题修复**
- **修复 Changes / Checkpoints Diff 视图删除行前缀显示误导性 `+` 号**：Diff 视图现按标准 Git 风格清晰展示删除行 `-` 与新增行 `+`，不再在行号槽位显示易混淆的 `+` 评论提示符号。
- **修复非 OSC 7501 PTY 终端跳过「已完成 (done)」状态的问题**：修复 `turn == 0` 且带 `herdr.seen="0"` 标签的 PTY 终端在任务结束后直接从 `working` 跳到 `idle` 的问题，确保在用户查看前始终保持 `done` 状态并支持 AI 快速完成补标（`mark_unseen`）。
- **修复后台刷新误将未读「已完成」状态清为空闲**：窗口后台状态同步改用 `sync_without_marking_seen`，仅在用户主动切换、点击或聚焦对应 Agent 时才将 `done` 标记为已读（`idle`）。

---

#### English

**New Features**
- **Managed ACP Adapter Registry & Auto-Installer (`h2 adapter status|install|update`)**: Added `AdapterManager`, `adapter.status` / `adapter.install` / `adapter.update` RPCs, the `h2 adapter` CLI, and `auto_install_adapters` / `acp_registry_url` settings in `config.toml`. Downloads SHA-256 verified standalone binaries from the official ACP Registry into isolated `~/.local/share/herdr2/adapters/` (`%LOCALAPPDATA%\herdr2\adapters\` on Windows) with isolated `npm install --prefix` fallback, supports background auto-install for detected companion CLIs (`claude`, `codex`, `pi`, `glm`), and adds a one-click **Managed ACP Adapters** bar in Settings → Drove.
- **Smarter ACP & PTY User-Question / Confirmation Detection**: Added deterministic + Core AI-assisted arbitration (`status.rs`, `ai/terminal_state.rs`, `terminal.rs`) to detect when a completed ACP turn or quiet PTY screen stops to ask a question, present choices, or await a question tool, surfacing it as `blocked` (`Needs you`) distinct from permission approvals (`Needs approval`) in Peek.

**Bug Fixes**
- **Fixed Misleading `+` Gutter Marker on Removed Lines in Changes / Checkpoints Diffs**: Diff views now render standard git-style `-` for removed lines and `+` for added lines without a misleading `+` comment-gutter marker.
- **Fixed Non-7501 PTY Terminals Skipping `done` Status**: Fixed PTY terminals with `turn == 0` and `herdr.seen="0"` jumping directly from `working` to `idle` upon completion, keeping them in `done` until viewed and supporting AI-detected fast completion via `mark_unseen`.
- **Fixed Background UI Syncs Prematurely Clearing Unread `done` Status**: Background window syncs now use `sync_without_marking_seen` so `done` only clears to `idle` when the user actively views or interacts with the agent.

## v2.0.15

#### 中文

**问题修复**
- **修复设置页 Drove 等滚动视图下方出现跟随滚动的 1px 白色横线**：修复 GPUI `Window::cover_bounds` 在处理空裁剪遮罩时对小数设备像素坐标同时执行 `floor` 与 `ceil` 导致零高度遮罩被扩展成 1dp 可见条带的问题，并在文本绘制阶段跳过空 `content_mask`；彻底消除 `overflow_hidden` 圆角卡片滚出弹窗视口后在设置页下方泄漏横线的视觉异常，同时新增 GPU 像素级回归测试。
- **修复 Windows 下 Drove 探测继承 WSL UNC 目录导致异常与 ACP 报错信息不全**：`drove.probe` 现优先选择本地非 UNC 的主目录/临时目录作为无头探测工作目录，避免 Windows `.cmd` 包装器与 ACP 适配器在 `\\wsl.localhost\...` 路径下启动失败；同时扩展 ACP JSON-RPC 错误解析以提取 `details` / `error` / `reason` 及结构化错误字段，并精简设置页探测异常卡片（移除冗余 AI 诊断流程，保留直接用 Agent 修复入口）。

---

#### English

**Bug Fixes**
- **Fixed 1px White Horizontal Line Leaking Below Settings → Drove While Scrolling**: Fixed GPUI's `Window::cover_bounds` expanding collapsed zero-height/zero-width intersected content masks at fractional device-pixel coordinates into a 1dp visible strip via `floor`/`ceil`, and skipped empty `content_mask` regions during text line painting. Nested `overflow_hidden` cards scrolled past modal/scroll viewports no longer bleed outside the clip, verified with a GPU pixel readback regression test.
- **Fixed Windows UNC Probe CWD & Enriched ACP Error Details**: Selected a stable local non-UNC home/temp working directory for headless `drove.probe` runs so Windows `.cmd` shims and ACP adapters no longer fail when `herdr2 serve` starts from a `\\wsl.localhost\...` share; expanded ACP JSON-RPC error formatting to surface `details` / `error` / `reason` and structured payloads; and streamlined Drove probe error cards by removing the redundant AI diagnose flow in favor of direct **Fix with Agent**.

## v2.0.14

#### 中文

**新增功能**
- **设置页「AI 辅助更新」支持自选 Agent / 模型 / 推理强度**：点击设置页更新区块或弹窗中的 **AI 辅助更新** 时，现可展开面板自由选择本机 Agent、模型及推理强度后再启动更新任务，并自动探测本机可用模型与能力选项。

**问题修复**
- **修复 Windows 后台进程/打开浏览器导致的终端窗口闪现**：Windows 下打开 Drove Web UI 改为直接调用 Win32 `ShellExecuteW`（替代 `cmd /C start`），同时为 Windows 子进程与 ACP Agent 默认注入 `AGY_CLI_DISABLE_AUTO_UPDATE=true`（尊重用户显式配置），彻底消除后台自动更新器或命令壳导致的 Windows Terminal / conhost 闪窗，并将 `herdr-app` 的无窗调用审计测试扩展至全部子目录。
- **修复 Windows 下 Codex PTY 启动因 `-c notify=[...]` 转义导致失败**：`pty_adapters` 将 Windows 路径（含 `\\?\` 与 `\\?\UNC\`）规范化为正斜杠并使用 TOML 单引号字面量数组，同时 `platform::windows::quote` 对包含单引号的参数自动加双引号保护，避免 PowerShell 5.1、`cmd.exe` 与 Git Bash / MSYS2 吞掉引号或折叠反斜杠。
- **修复 Windows 已保存默认 Shell 未安装导致新建标签页/工作区失败**：当配置中保存的 Windows 默认 Shell（如 `pwsh`、`git-bash`）在当前环境未安装时，自动刷新检测并优雅回退到 `auto`，设置页同时明确标注 `未安装 · 自动回退`。
- **修复 `~` 主目录展开与远程 SSH 终端默认工作目录问题**：`herdr-core` 支持自动展开 `~` / `~/` / `~\` 并在远程 `terminal.run` 时正确传递 `from_afar`；桌面端在新建本地/远程工作区、启动 AI 更新或 Drove 异常修复终端时规范化主目录请求，不再误用错误目录。

---

#### English

**New Features**
- **Agent / Model / Effort Picker for AI-Assisted Updates**: Clicking **AI Update** in Settings or the Update modal now opens an inline configuration panel to pick the local agent, model, and reasoning effort (with automatic local agent/model discovery) before launching the update session.

**Bug Fixes**
- **Fixed Transient Windows Console / Terminal Window Flashes**: Switched Windows browser launches in Drove from `cmd /C start` to native Win32 `ShellExecuteW`, defaulted `AGY_CLI_DISABLE_AUTO_UPDATE=true` (unless explicitly overridden by the user) on Windows child processes and ACP agents to stop background auto-updaters from flashing detached consoles, and extended `herdr-app`'s `CREATE_NO_WINDOW` spawn audit recursively across all subdirectories.
- **Fixed Windows Codex PTY Launch Failure from `-c notify=[...]` Quoting**: Normalized Windows executable paths (including `\\?\` and `\\?\UNC\`) to forward slashes using single-quoted TOML literal strings in `codex_notify_override`, and updated `platform::windows::quote` to double-quote arguments containing `'` so PowerShell 5.1, `cmd.exe`, and MSYS2/Git Bash preserve the notify array intact.
- **Fixed Missing Configured Windows Default Shell Fallback**: Gracefully fall back from an uninstalled saved Windows default shell (e.g. `pwsh` or `git-bash`) to `"auto"` (after refreshing live PATH), while Settings clearly marks the saved choice as `missing (auto)`.
- **Fixed `~` Home Directory Expansion & Remote SSH Terminal CWD**: Expanded `~` / `~/` / `~\` to the user's home directory in `herdr-core`, propagated `from_afar` in `terminal_run`, and normalized explicit `~` / empty folder requests in the desktop app so local and remote workspaces/fix terminals always start in the right home directory.

## v2.0.13

#### 中文

**新增功能**
- **本机与 SSH 远程主机一键同步更新**：`h2 update` 与桌面端右上角更新按钮现已支持同步升级本机（App / UI / Core）与所有已配置的 SSH 远程主机 `herdr2-core`（远程主机并发升级、互不阻塞、保持运行中终端存活）；设置页「更新」面板新增本机与全部 SSH 远程主机状态列表（`Up to date` / `needs_update` / `Updating…`）。
- **设置页 AI 辅助从 GitHub 更新**：设置页更新区块与更新弹窗新增 **AI 辅助更新** 入口；当手动/自动更新因代理、TLS 证书、权限或网络受限失败时，可一键拉起本地 Coding Agent 自动诊断并从 GitHub Releases 安全下载安装最新版本。
- **Drove 异常排查弹窗支持自选 Agent / 模型 / 推理强度**：在 Drove 探测异常卡片点击 **让 Agent 排查修复** 时，可自由选择用于排查的 Agent、模型及推理强度后再启动排查标签页。
- **Windows 本地 Shell 实时检测与按需展示**：桌面端实时读取 Windows 注册表系统/用户 `PATH` 并通过 `window().shells` 暴露本机实际已安装的 Shell（PowerShell / pwsh / Git Bash / CMD），新建菜单自动过滤未安装的本地 Shell，新安装 Shell 无需重启应用即可识别。

**问题修复**
- **修复 Drove ACP 并发探测超时与错误缓存滞留**：`drove.probe` 改为两阶段受控并发探测（首轮并发 4 / 45s 超时，少量超时 Agent 自动以并发 2 重试，总预算 135s），避免冷启动与缓存争抢导致假超时并保留底层 `setup_session` 报错细节；此外，当某个 ACP Agent 正常启动或恢复成功时，自动清除并持久化移除该 Agent 的历史探测报错。
- **修复 Agents 侧边栏重复显示设备名**：移除行尾在已有设备分组/标签情况下重复展示的机器名文本，界面更简洁。

---

#### English

**New Features**
- **Unified Local + SSH Remote Auto-Sync Updates**: `h2 update` and the desktop top-right Update button now automatically sync updates across this computer (App / UI / Core) and all configured non-temporary SSH remote hosts in parallel (keeping running terminals alive and reporting unreachable hosts independently). Settings → Updates now displays live status rows (`Up to date` / `needs_update` / `Updating…`) for local and all SSH remote hosts.
- **AI-Assisted GitHub Update Fallback**: Added an **AI Update** action in Settings and the Update modal so if automatic/manual updates fail due to proxy, TLS, permission, or network issues, a local coding agent can be launched with full diagnostic context to fetch, verify (SHA-256), and install the latest GitHub release safely.
- **Agent / Model / Effort Picker for Drove Probe Remediation**: Launching **Fix with Agent** from Drove probe error cards now lets you select the remediation agent, model, and reasoning effort before opening the fix tab.
- **Live Windows Installed Shell Discovery**: Reads the live Windows System + User registry `PATH` (merged with process `PATH`) to expose installed local shells via `window().shells`, filtering the New Menu to shells actually installed on the machine without requiring an app restart.

**Bug Fixes**
- **Fixed Drove ACP Probe Contention Timeouts & Stale Error Caches**: Reworked `drove.probe` into a two-phase bounded-concurrency pipeline (wave 1: concurrency 4, 45s timeout; automatic wave 2 retry at concurrency 2 for small timeout batches within a 135s budget) while preserving inner `setup_session` diagnostics (`handshake_timeout`). Successfully starting or recovering an ACP agent now automatically clears and persists removal of its cached probe error.
- **Fixed Duplicate Machine Label in Agents Sidebar**: Removed the redundant trailing machine name on agent rows where device headers/badges already identify the host.

## v2.0.12

#### 中文

**新增功能**
- **Drove ACP 探测异常 AI 诊断 (`drove.probe.ai_diagnose`)**：新增 `drove.probe.ai_diagnose` RPC 与 `h2 drove probe-ai-diagnose` CLI，调用已配置的 Core AI 助手分析 ACP Agent 探测失败原因，区分「守护进程缺少代理/环境导致的误报」与真实安装/认证问题，并给出可执行修复命令。
- **设置页一键排查与终端执行**：Drove 探测异常卡片新增 **AI 诊断 (AI Diagnose)**、**让 Agent 排查修复 (Fix with Agent)** 以及带命令安全校验的 **在终端运行 (Run in Terminal)** 按钮，支持远程机器异常自动回退本机 AI 诊断并自动定位目标机器工作区。
- **Windows 终端实时工作目录同步 (OSC 7 / OSC 9;9)**：`ghostty-vt` 与 `herdr-core` 新增 `OPT_PWD_CHANGED` / `DATA_PWD` 钩子与路径解析，自动为交互式 `cmd.exe`（OSC 9;9）和 Git Bash（OSC 7）注入实时目录上报。
- **未命名工作区跟随活跃标签页目录**：未手动命名的工作区（Spaces 侧边栏与窗口标题）现在优先跟随当前活跃标签页中聚焦终端的实时目录，不再固定于首个终端。

**问题修复**
- **修复 Windows 下 Git Bash 嵌套包装层前景检测失真**：前景进程探测现可穿透 `Git\bin\bash.exe -> Git\usr\bin\bash.exe` 包装层，空闲时准确保持在 Shell 根状态，运行子程序（如 `claude.exe`）时准确识别前台程序。
- **修复 Windows ACP Agent 子进程隔离与终止**：Windows ACP 进程改为挂起启动（`CREATE_SUSPENDED`）并先绑定关闭即清理的 Job Object 再恢复执行，补齐 `signal_agent` 强制终止，避免子进程残留。
- **修复 Windows `.cmd`/`.bat` 执行与中文 GBK 输出截断**：批处理 Shim 优先通过 `COMSPEC` / `SystemRoot` 解析 `cmd.exe`，子进程 stdout/stderr 补充 Windows ANSI 代码页（`CP_ACP` / GBK）回退解码，避免非 UTF-8 诊断输出中断 ACP 通信。
- **修复 Windows 守护进程 PATH、PowerShell 7 与 Claude ACP 兼容问题**：自动补齐常见用户 CLI 目录（`.local\bin`、`.bun\bin`、`.cargo\bin`、`scoop\shims`、`npm`、`Volta\bin`、`pnpm`），支持 Microsoft Store / MSIX App Execution Alias（如 `WindowsApps\pwsh.exe`），优化终端启动报错提示，并为 Windows Claude ACP 自动注入 `CLAUDE_CODE_GIT_BASH_PATH` 与 `bypassPermissions` 默认回退。
- **修复 Windows 跨平台构建图标缺失**：Linux 交叉编译 Windows 桌面端（`herdr-app/build.rs`）通过 `llvm-rc` / `windres` 嵌入 `herdr-app.ico`，并在 Release 归档、Inno Setup 安装包、OTA 升级及本机热同步中统一携带 `herdr2.ico`。

---

#### English

**New Features**
- **AI Diagnostics for Drove ACP Probe Errors (`drove.probe.ai_diagnose`)**: Added the `drove.probe.ai_diagnose` RPC and `h2 drove probe-ai-diagnose` CLI to analyze ACP agent probe failures with Core AI, distinguishing daemon environment/proxy false positives from real setup or auth issues and proposing actionable remediation commands.
- **Guided Remediation Actions in Settings**: Added **AI Diagnose**, **Fix with Agent**, and safety-validated **Run in Terminal** actions on Drove probe error cards, with automatic local-AI fallback for older/unconfigured remote daemons and automatic targeting of the selected machine's workspace.
- **Windows Live Terminal CWD Tracking (OSC 7 / OSC 9;9)**: Added `OPT_PWD_CHANGED` / `DATA_PWD` hooks in `ghostty-vt` and OSC 7 / OSC 9;9 directory parsing in `herdr-core`, automatically injecting live CWD reporting for interactive Windows `cmd.exe` and Git Bash shells.
- **Active-Tab Workspace Naming**: Unnamed workspaces (`Model::space_name` and the Spaces sidebar) now follow the active tab's focused terminal folder instead of staying pinned to the first terminal.

**Bug Fixes**
- **Fixed Windows Git Bash Nested Wrapper Foreground Detection**: Foreground process resolution now walks through `Git\bin\bash.exe -> Git\usr\bin\bash.exe` wrappers so idle prompts remain at shell root while inner programs (e.g., `claude.exe`) are detected accurately.
- **Fixed Windows ACP Subprocess Containment & Termination**: Spawn Windows ACP agents suspended (`CREATE_SUSPENDED`), attach a kill-on-close Job Object before resuming, and implement `signal_agent` termination to prevent orphaned subprocesses.
- **Fixed Windows Batch Shim Resolution & Non-UTF-8 Output Decoding**: Resolve `cmd.exe` via `COMSPEC` / `SystemRoot` for `.cmd`/`.bat` shims and add Windows active code page (`CP_ACP`, e.g., GBK/CP936) fallback decoding so child stdout/stderr never truncates or breaks ACP framing.
- **Fixed Windows Daemon PATH, PowerShell 7 & Claude ACP Quirks**: Augment Windows daemon `PATH` with common user CLI directories (`.local\bin`, `.bun\bin`, `.cargo\bin`, `scoop\shims`, `npm`, `Volta\bin`, `pnpm`), recognize Windows Store / MSIX App Execution Aliases (e.g., `pwsh.exe`), improve terminal spawn error messages, and auto-populate `CLAUDE_CODE_GIT_BASH_PATH` plus `bypassPermissions` fallback for Claude ACP.
- **Fixed Missing Windows App Icon on Cross-Builds & Packaging**: Embed `herdr-app.ico` during Linux-to-Windows cross-builds (`llvm-rc` / `windres`) and bundle `herdr2.ico` across Windows release archives, Inno Setup installers, OTA updates, and local hot-sync.

## v2.0.11

### Features
- **Agents Sidebar Device Grouping**: Group workspaces by device (`This computer` / `本机` first, then remote machines alphabetically) in the left Agents sidebar with clear device headers, icons, connection dots, dividers, and per-device fold/unfold (including urgent status rollup when folded and compact rail headers when collapsed).
- **Device Badges in Priority & Pinned Views**: Show explicit device tags (`Local` / `本机` and remote machine names) on rows in Priority and Pinned views when remote devices are present, making identically named workspaces across devices easy to tell apart.
- **Automated Release Notes**: Generate GitHub Release notes and concise in-app OTA update summaries automatically from `CHANGELOG.md`, annotated tag messages, or categorized commit history.
