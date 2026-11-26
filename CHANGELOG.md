# Changelog

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
