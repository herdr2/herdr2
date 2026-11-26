# Changelog

## v2.0.12

### Features
- **AI Probe Diagnostics (`drove.probe.ai_diagnose`)**: Add `h2 drove probe-ai-diagnose` RPC/CLI and Settings UI actions (**AI Diagnose**, **Fix with Agent**, and safe **Run in Terminal**) to explain ACP agent probe failures, distinguish daemon environment false positives from real setup problems, and launch remediation commands.
- **Windows Live Terminal CWD & Active-Tab Workspace Naming**: Track live working directories via OSC 7 (Git Bash/MSYS2/Cygwin) and OSC 9;9 (`cmd.exe` prompt injection) in `ghostty-vt` and `herdr-core`, walk nested Git Bash wrapper shells for foreground detection, and let unnamed workspaces follow the active tab's focused terminal folder.

### Bug Fixes
- **Windows ACP Agents & Shell Discovery**: Spawn Windows ACP agents suspended inside kill-on-close Job Objects with proper `signal_agent` termination, `COMSPEC` batch execution, GBK/CP_ACP output decoding fallback, user CLI `PATH` augmentation, Claude `CLAUDE_CODE_GIT_BASH_PATH` & `bypassPermissions` fixes, and PowerShell 7 / Store App Execution Alias discovery.
- **Windows Icon Embedding & Packaging**: Embed `herdr-app.ico` during Linux-to-Windows cross-builds (`llvm-rc`/`windres`) and ship `herdr2.ico` across Windows release archives, Inno Setup installers, OTA updates, and local hot-sync.

## v2.0.11

### Features
- **Agents Sidebar Device Grouping**: Group workspaces by device (`This computer` / `本机` first, then remote machines alphabetically) in the left Agents sidebar with clear device headers, icons, connection dots, dividers, and per-device fold/unfold (including urgent status rollup when folded and compact rail headers when collapsed).
- **Device Badges in Priority & Pinned Views**: Show explicit device tags (`Local` / `本机` and remote machine names) on rows in Priority and Pinned views when remote devices are present, making identically named workspaces across devices easy to tell apart.
- **Automated Release Notes**: Generate GitHub Release notes and concise in-app OTA update summaries automatically from `CHANGELOG.md`, annotated tag messages, or categorized commit history.
