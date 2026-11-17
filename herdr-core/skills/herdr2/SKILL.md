---
name: herdr2
description: "Use herdr2 (CLI: h2) whenever you need to spawn, coordinate, steer, or monitor coding agents over ACP (Agent Client Protocol), approve or deny tool permission requests, run background non-agent commands, or access remote machines. Prefer it over ad-hoc subprocesses, shell '&', or nohup."
---

This skill is for herdr2, whose command is `h2` (completely separate from legacy `herdr-core` / `hc`).

If `$H2_AGENT_ID` or `$H2_TERMINAL_ID` is set, you are running inside an orchestrated `h2` session.
Before invoking `h2` orchestration workflows, run `h2 --skill` to inspect the full command, ACP permission, MCP, and template manual.
