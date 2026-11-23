# 将 zcode 配置与 agy 对齐 — 实施计划

## Context

用户要求把 zcode 的 skill / MCP / 插件配置与 agy（Antigravity CLI）对齐。完整对比后结论：

- **MCP**：已对齐。agy 的 13 个服务（实际上游为 `~/.claude.json`，经 `mcp-bridge.py` 桥接）在 zcode 全部存在且语义一致；zcode 额外多 codesearch、codeup（保留，不做删除）；buildlink/jira 仅有益的 timeoutMs 差异。
- **Skills**：已对齐。agy 的 android-game-autoplay、android-use 在 zcode 均存在且内容逐字一致。
- **Plugins**：pyright-lsp、typescript-lsp 两边均已安装。**唯一缺口：herdr2-checkpoints**（在 agy 侧以 hook 插件形式存在，见 `~/.gemini/config/plugins/herdr2-checkpoints/hooks.json`）。
- 其他（statusline、权限、信任目录）两边语义不同，不对齐。

用户已确认：**仅做配置对齐**，hook 复用 `--source claude`（zcode 的 hook 协议与 Claude Code 兼容：相同事件名、stdin JSON 含 session_id/transcript_path/prompt），不改 herdr2 项目代码。

## 改动内容

### 1. 备份
`cp ~/.zcode/cli/setting.json ~/.zcode/cli/setting.json.bak-agy-align-<YYYYMMDD-HHMMSS>`

### 2. 修改 `~/.zcode/cli/setting.json` 的 `hooks.events`

合并以下三条 hook（模板对照 herdr2 源码 `herdr-core/crates/herdr2/src/pty_adapters.rs:444` 的 `ensure_claude_hooks_file`；timeout 参照 agy 版插件的 10/10/15 秒）。**保留现有 orca hooks（SessionStart/UserPromptSubmit/PreToolUse/PermissionRequest）不动，只做追加**：

- `PreToolUse`：追加条目
  ```json
  { "matcher": "Read|Edit|Write|MultiEdit|NotebookEdit|Bash|Glob|Grep",
    "hooks": [{ "type": "command",
                "command": "/home/zzh0838/.local/bin/herdr2 checkpoint hook --source claude --phase guard",
                "timeout": 10 }] }
  ```
- `UserPromptSubmit`：追加
  ```json
  { "hooks": [{ "type": "command",
                "command": "/home/zzh0838/.local/bin/herdr2 checkpoint hook --source claude --phase pre",
                "timeout": 10 }] }
  ```
- `Stop`：新增事件（若该 key 不存在）
  ```json
  [{ "hooks": [{ "type": "command",
                "command": "/home/zzh0838/.local/bin/herdr2 checkpoint hook --source claude --phase post",
                "timeout": 15 }] }]
  ```

### 3. 不改动
- `~/.zcode/cli/config.json`（MCP）、skills 目录、plugins —— 已对齐。

## 验证

1. `python3 -m json.tool ~/.zcode/cli/setting.json` 确认 JSON 合法
2. 手动执行 `echo '{}' | herdr2 checkpoint hook --source claude --phase pre`，确认命令可运行、无崩溃（herdr2 daemon 未跑时输出 `{}` 即正常）
3. 用户重启 zcode 后通过 hooks 诊断（/hooks 或 zcode-guide:diagnosing-hooks）确认 hook 已注册；herdr2 面板中出现该会话的 checkpoint

## 风险说明

- zcode 若不支持 `Stop` 事件，该条会被忽略（无害）
- setting.json 由运行中的 zcode 进程持有，改动在下次启动 zcode 时生效
- 已知局限（用户接受）：herdr2 侧会把 zcode 会话的 cli_kind 标记为 claude