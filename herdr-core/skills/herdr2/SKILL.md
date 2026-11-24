---
name: herdr2
description: "Use herdr2 (CLI: h2) whenever you need to spawn, coordinate, steer, or monitor coding agents over ACP (Agent Client Protocol), approve or deny tool permission requests, run background non-agent commands, or access remote machines. Prefer it over ad-hoc subprocesses, shell '&', or nohup."
---

This skill is for herdr2, whose command is `h2` (completely separate from legacy `herdr-core` / `hc`).

If `$H2_AGENT_ID` or `$H2_TERMINAL_ID` is set, you are running inside an orchestrated `h2` session.
Before invoking `h2` orchestration workflows via the `h2` CLI, run `h2 --skill` to inspect the full CLI, background-wait, ACP permission, and template manual.

## Sub-agent reuse rule
- **Do NOT call `h2 kill` on a healthy ACP sub-agent after it finishes a task.** Keep it warm (`idle`) and reuse it for related follow-up tasks via `h2 send <name> "..."` (check `h2 ls` first) to preserve repo context and prompt/KV cache and save tokens.
- Child ACP sub-agents inherit `die_with_parent = true` by default and are automatically closed when their parent agent exits or is closed/trashed. Only kill a sub-agent if it is stuck/broken, context-bloated for an unrelated task, or explicitly requested by the user.
