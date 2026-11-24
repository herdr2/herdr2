# Herdr2

**面向 AI 辅助软件工程的双平面工作台与多智能体编排运行时**

Herdr2 将 **Ghostty 真终端（`t*`）**、**无头结构化 ACP 智能体（`a*`）**、**上下文接力与隐藏 Git Ref 快照** 以及 **Drove 多智能体 DAG 编排引擎** 融合到同一个原生 GPU 桌面窗口（`herdr2`）与后台持久守护进程（`h2`）之中。关闭窗口、更新界面甚至断开 SSH，所有终端与智能体会话依然在后台持续运行。

![Herdr2 主工作台全景](./docs/user-guide/images/02-workspace-overview.png)

---

## 核心特性

- **双平面解耦运行时**：交互式 PTY 真终端（`t1, t2...`，`libghostty-vt` GPU 渲染）与无头 ACP v1 智能体（`a1, a2...`，纯 stdio NDJSON、零 PTY / 零 `TERM*` 污染）并存，全部持久化托管在 `h2` 守护进程中。
- **14 种内置智能体预设与行内审批**：开箱支持 `claude`、`codex`、`agy`、`gemini`、`cursor`、`opencode`、`copilot`、`kimi`、`qwen`、`pi`、`hermes`、`dsh`、`glm`、`zcode`；内置串行收件箱（Inbox）、`--steer` 实时插话、四档权限仲裁（`approve-reads` / `ask` / `approve-all` / `deny-all`）、运行时热切模型/思考强度，以及内置 MCP 服务器（`h2 mcp`）支撑父子智能体树。
- **上下文接力、分叉与无污染快照**：支持跨 Agent / 跨模型的 **上下文接力（Handoff）**、隔离 Git Worktree **会话分叉（Fork）** 与 **旁路讨论**；基于隐藏 Git Ref 提供逐轮纯净 Diff、一键回滚与自然语言故障定位（`h2 checkpoint bisect`），不污染正常 Git 分支历史。
- **Drove 原生 DAG 编排**：内置单守护进程多智能体编排器，按角色隔离权限（`writable` / `scoped_write` / `readonly` + OS 沙盒）、支持多候选模型故障顺延、自动 Verify 验收门禁、异构模型交叉审查与只读 Web 实时 DAG 监控工作台（`127.0.0.1:7799`）。
- **多机合一与文件级热定制**：通过 SSH 将本机、WSL 与远程 Linux/macOS 服务器统一汇入同一窗口；窗口布局（`layouts/*.json`）、快捷键（`keys.json`）与 JavaScript 面板模块（`mods/*`）均支持秒级热重载。
- **与原版 Herdr 零冲突共存**：使用独立二进制名（`h2` / `herdr2`）、环境变量（`H2_*` / `HERDR2_APP_HOME`）与配置/套接字目录，可与原版 `hc` / `herdr-app` 长期并行运行。

![Herdr2 双平面解耦架构](./docs/user-guide/images/diagram-dual-plane-architecture.svg)

---

## 安装方式

推荐直接通过仓库内置的 **`herdr2-install` Skill**（[skills/herdr2-install/SKILL.md](./skills/herdr2-install/SKILL.md)），交给你常用的 AI 编程助手（Antigravity、Claude Code、Codex、Cursor、OpenCode 等）一站式自动安装。

只需将下面这段话直接发给你的 AI Agent：

```text
请阅读 https://raw.githubusercontent.com/herdr2/herdr2/main/skills/herdr2-install/SKILL.md 并按步骤在本机安装和配置 Herdr2。
```

AI Agent 会自动完成全流程：
1. **自动识别系统与架构**：适配 macOS（Apple Silicon）、Linux、WSL 及 Windows（x64）；
2. **安装桌面端与 `h2` 守护进程**：从 [GitHub Releases](https://github.com/herdr2/herdr2/releases/latest) 拉取对应平台的官方安装包；
3. **启动并校验环境**：验证 `PATH`、`h2 --version`、`herdr2 --version` 与 `h2 server status`；
4. **注入配套 Agent Skills**：执行 `h2 skill install --all`，自动为本机编码 Agent 安装 `herdr2`、`drove`、`drove-add`、`drove-setup`；
5. **引导首次启动与 Core AI 配置**：完成后即可启用应用内 OTA 一键热更新。

---

## 快速导航与文档

### 用户使用手册（推荐从这里开始）

完整的分主题图文教程见 **[docs/user-guide/README.md](./docs/user-guide/README.md)**（所有流程均经沙盒实操复现并附真实界面截图与矢量图）：

1. [01. 快速上手与界面导览](./docs/user-guide/01-getting-started.md) — 双平面架构、欢迎向导、主界面 5 大区域、命令面板
2. [02. 工作区、终端与代码面板](./docs/user-guide/02-workspaces-and-terminals.md) — 工作区/标签页/分栏、快捷键、文件与 Git 变更面板、闪念清单
3. [03. 无头 ACP 智能体与权限控制](./docs/user-guide/03-headless-acp-agents.md) — 14 种预设、结构化对话、Inbox/`--steer`、行内审批与内置 MCP
4. [04. 上下文接力、快照回滚与全局 AI 助手](./docs/user-guide/04-handoff-checkpoints-and-ai.md) — Handoff / Fork / 旁路、隐藏 Git Ref 快照、全局 Core AI
5. [05. Drove 多智能体 DAG 编排](./docs/user-guide/05-drove-orchestration.md) — 角色护栏与顺延、`h2 drove` CLI、Drove Web 实时编排画布
6. [06. 远程主机、深度定制与自动化自检](./docs/user-guide/06-remote-customization-and-qa.md) — SSH 多机管理、Layouts/Mods 热定制、隔离沙盒 QA 框架

### 架构与参考文档

- [Herdr2 架构与实现原理](./docs/herdr2-architecture-and-principles.md)
- [Herdr2 使用与共存指南](./docs/herdr2-usage-guide.md)
- [Drove 编排架构说明](./docs/drove-architecture.md)
- [Core 运行时说明](./herdr-core/README.md) · [桌面端说明](./herdr-app/README.md) · [Drove Web 说明](./drove-web/README.md) · [QA 测试目录](./qa/CATALOG.md)

---

## 仓库结构

```text
herdr2/
├── herdr-core/       # Rust 后台守护进程与 CLI（h2 / herdr2-core）：ACP、PTY holder、快照、Core AI、Drove 引擎
├── herdr-app/        # GPUI + libghostty-vt 原生桌面端与热重载 Layouts / Mods / Frame
├── drove-web/        # Drove 只读实时 DAG 可视化前端（Vite + TypeScript）
├── docs/             # 用户使用手册（docs/user-guide/）与架构设计文档
├── qa/               # 活体功能清单、分层测试用例与自动生成的 CATALOG.md
└── scripts/          # 本地开发热同步（dev.sh / dev.ps1）、发布打包与隔离沙盒 QA 运行器（scripts/qa/）
```

---

## 从源码构建与开发

环境要求：Rust（workspace edition 2024）、Zig（用于 `herdr-app` 构建 `ghostty-sys`）、[just](https://github.com/casey/just)、Node.js / [Bun](https://bun.sh)（前端与 Mod 单测）。

```bash
# 1. 构建后台守护进程与 CLI（herdr-core）
cd herdr-core && cargo build --locked -p herdr2

# 2. 构建 Drove Web 静态产物
cd ../drove-web && npm install && npm run build

# 3. 构建原生桌面应用（herdr-app）
cd ../herdr-app && cargo build --locked
```

仓库根目录的 [justfile](./justfile) 提供了完整的增量开发、热重载与测试命令：

```bash
just dev-ui                  # 零编译热重载 JS Mods / Layouts / 主题
just dev-core                # 增量编译 herdr-core 并零中断重启 h2 server
just dev-app                 # 增量编译 herdr-app 并重启窗口（后台终端不中断）
just test                    # 运行脚本、herdr-core 与 herdr-app 全量测试
just qa-drift                # 校验代码契约与 QA 用例目录同步（0 漂移）
just qa-run --tier L0,L1,L2,L3  # 在完全隔离沙盒中运行分层自动化 QA 回归与截图
```

---

## 致谢（Acknowledgments）

Herdr2 站在巨人的肩膀上成长，在此向以下开源项目与社区致以最诚挚的感谢：

### 特别鸣谢：原版 **Herdr** 项目

衷心感谢原版 **Herdr**（[`herdrdev/herdr-app`](https://github.com/herdrdev/herdr-app) 与 [`herdrdev/herdr-core`](https://github.com/herdrdev/herdr-core)）的作者与全体贡献者！

Herdr 在设计理念与工程实现上为 Herdr2 奠定了极其坚实而优雅的基石，包括但不限于：
- **「桌面窗口与核心守护进程解耦」** 的核心架构哲学——让关闭窗口不再意味着中断工作流；
- 基于 **GPUI + `libghostty-vt`** 精心打磨的高性能原生跨平台终端桌面体验，以及 **Space / Tab / Pane** 清晰的工作区组织模型；
- 极具创造力的 **「一切界面皆本地可热重载文件」**（Layouts、Mods、Keys 与 Themes）定制体系，以及简洁直观的 **SSH 多机联邦** 设计。

Herdr2 在此基础上进一步探索了无头 ACP v1 智能体平面、行内权限仲裁、跨模型上下文接力/分叉、隐藏 Git Ref 轮次快照、全局 Core AI 控制面与原生 Drove 多智能体 DAG 编排等方向，并刻意保持与原版 Herdr 的命名与运行环境完全隔离，以示对上游项目的尊重并方便用户并行体验。如果你喜欢 Herdr2 的原生桌面与终端设计，请一定去关注并支持原版 **[Herdr](https://github.com/herdrdev/herdr-app)** 项目！

### 其他关键开源项目

- **[GPUI](https://www.gpui.rs/)** & **[gpui-kit](https://gpui-kit.com/)** — 高性能 Rust GPU UI 框架与脚本化外壳
- **[Ghostty](https://ghostty.org/)**（`libghostty-vt`）— 现代、高速、规范兼容的终端仿真核心
- **[Agent Client Protocol (ACP)](https://agentclientprotocol.com/)** — 开放的结构化编程智能体通信协议

---

## 许可证

本项目遵循 [Apache-2.0](./herdr-core/LICENSE) 开源许可证。
