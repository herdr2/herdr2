---
name: drove-add
description: Append new tasks dynamically to an active or existing Drove Run without restarting or losing warm workers. Use when adding follow-up implementation, verification, or review tasks to the current Run.
disable-model-invocation: true
---

# `/drove-add`: Append Tasks to the Current Drove Run

Use `h2 drove add` to hot-plug new tasks into the current Run (recorded by `h2 drove use` / latest `h2 drove plan`). Workers remain warm (`idle`) after tasks finish and after `h2 drove complete` until the Coordinator closes or the Run is deleted in Drove Web, so appending tasks to a settled or `done` Run automatically reactivates it and reuses those warm workers:

## Single Task (Quick Flags)

```bash
h2 drove add \
  --key fix-lint \
  --title "Fix clippy warnings" \
  --role coder \
  --change "Resolve clippy warnings in src/drove/" \
  --deps impl \
  --verify "cargo clippy --all-targets"
```

Pass `--worktree` to isolate the task in a detached Git / AOSP harness worktree merged back automatically on completion. If Drove returns `worktree_not_ready`, report the blocker reason and fix hint to the user before retrying.

## Batch Tasks (YAML/JSON)

```bash
h2 drove add --file /tmp/more-tasks.yaml
# or `--run <run_id>` and `--on <machine>` for remote SSH targets
```

After appending tasks:
- Run `h2 drove wait` (if not already waiting) to block until the newly added tasks settle or ask questions.
- Run `h2 drove show` to see current task statuses, review circuit-breaker stages, and warm worker states.
- **Review Circuit-Breaker Guard**: Do NOT use `h2 drove add` to spawn duplicate `role: reviewer` tasks in order to bypass a tripped `handoff_to_coordinator` or `awaiting_user_decision` review circuit breaker; follow the Coordinator takeover (1 post-handoff review) -> user decision escalation protocol instead.
