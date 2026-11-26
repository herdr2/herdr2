# h2 manual (herdr2)

`h2` (`herdr2`) combines real Ghostty/PTY workspace terminals (`t1`, `t2`, ...) for human interactive sessions with a headless ACP (Agent Client Protocol) orchestration runtime (`a1`, `a2`, ...) for subordinate agents. Called ACP sub-agents receive `terminal: false` and `fs: false` capabilities, zero PTYs, and zero terminal environment variables (`TERM`, `COLORTERM`, `TERM_PROGRAM` are omitted), and open no terminal panes in `Herdr2`.

- Inside an ACP sub-agent, `$H2_AGENT_ID` holds your address (`a1`, `a7`, ...) and `$H2_AGENT_SOCKET` points to the daemon socket. Inside a workspace PTY terminal, `$H2_TERMINAL_ID` holds `t1`, ... and `$H2_TERMINAL_SOCKET` points to the same socket, so launching `h2 run` or `h2 mcp` from a workspace automatically parents child ACP sub-agents to that terminal.
- Agents you launch default to becoming your children (they receive `<h2>` tags on inter-agent messages and exit automatically when you exit or are closed unless `--outlive-parent` is passed).
- **Keep & reuse warm sub-agents (do NOT `h2 kill` after a task)**: Healthy ACP sub-agents stay idle after completing a turn and preserve their loaded repo context and prompt/KV cache. Before calling `h2 run`, check `h2 ls` and reuse an existing idle child via `h2 send <name> "..."` for related follow-up tasks (saves input tokens and cold-start overhead). Never `h2 kill` a healthy sub-agent just because a turn finished — let it stay warm until you (its parent) close, and only `h2 kill` if a child is stuck/broken, context-bloated on an unrelated task, or explicitly requested by the user.
- Piped output is JSON except `run` (waited), `exec`, `read`, and `file read` (text unless `--json`).
- Run `h2 --help` for commands or `h2 <command> --help` for exact flags.

## Launch an ACP agent

Built-in presets for `--agent` (or positional first argument): `claude`, `codex`, `gemini`, `opencode`, `copilot`, `cursor`, `kimi`, `qwen`, `pi`. Custom command lines work via `--agent "python3 my_acp.py"` or positional argv `h2 run -- python3 my_acp.py`.

```sh
# Check reusable Markdown templates in ~/.config/herdr2/templates/*.md
h2 templates

# Launch with preset + initial prompt in background
h2 run --bg --name reviewer --cwd /path/to/repo --agent claude --prompt "Review src/x.rs only. Report findings via h2 send $H2_AGENT_ID."

# Wait for an agent to finish or block (or pass --no-timeout inside a background command runner)
h2 wait reviewer
h2 wait reviewer --no-timeout

# Launch a template (instructions from template body + task argument)
h2 run --bg --name worker reviewer "Check src/acp/mod.rs for edge cases"
```

Overrides on `run`: `--model <MODEL>`, `--mode <MODE>`, `--system-prompt <TEXT>`, `--permissions <approve-all|approve-reads|deny-all|ask>`, `--no-mcp`, `--keep`, `--rm`, `--no-notify`, `--outlive-parent`, `--label k=v`, `--on <machine>`.

Never use shell `&` or `sleep` polling for agents: use `h2 run --bg` with `h2 wait` (or your host's background command runner).

## Non-blocking waiting (for hosts with background command execution)

If your host supports running shell commands in the background, do not block your active turn waiting synchronously:

1. Start child agents with `h2 run --bg --name <name> ...`.
2. Run `h2 wait <name> --no-timeout` (or `h2 wait --children all --no-timeout`, or `--children any --no-timeout`) in **your host's background command runner**:
   - **Claude Code / `dsh`**: call `Bash` with `run_in_background: true` (wakes you via task notification on exit).
   - **Antigravity (`agy`)**: call `run_command` with short `WaitMsBeforeAsync` (e.g. `500`), then end your turn; reactive wakeup resumes you when `h2 wait` exits.
   - **Codex CLI**: call `exec_command` (yields to a background `session_id` after ~10s) and read completion via `write_stdin`.
   - **Cursor**: call `run_terminal_cmd` with `is_background: true`.
3. Once `h2 wait` finishes (`status`, `exit`, `output`, or `message`), inspect with `h2 read <name> --last` or resolve `blocked` permissions with `h2 approve` / `h2 deny`.
4. If your host does not support background commands, run `h2 wait <name>` synchronously (default 180s check-in; re-run on `timeout`) or omit `wait` and let `notes` / `h2 send` deliver updates.

## Coordinate, steer, and read transcripts

```sh
h2 ls                                   # list live agents (--all includes finished)
h2 get worker                           # full JSON/summary agent state
h2 send worker "Also inspect error.rs"  # queue prompt into serial ACP inbox
h2 send worker --image shot.png "Fix this layout bug" # attach native ACP image/resource blocks (--resource, --resource-link)
h2 inbox ls|edit|rm worker ...          # inspect, edit, or remove queued prompts before turn starts
h2 send worker --steer "Stop that; inspect foo.rs instead" # cancel active turn & deliver next
h2 read worker                          # rendered JSONL transcript
h2 read worker --last                   # final assistant message from latest turn
h2 read worker --lines 50               # trailing 50 rendered transcript lines
h2 read worker --from-turn 2 --to-turn 5 # slice specific turn range for handoff lookup
h2 read worker --wait --last            # wait for current turn to settle, then read --last
h2 handoff worker --kind handoff --preset codex --prompt "Finish tests" # cross-provider context handoff
h2 handoff worker --kind side-chat --into main --prompt "Merge back"    # merge/deliver context into existing target
h2 interrupt worker                     # send ACP session/cancel without killing process
h2 kill worker                          # ONLY when stuck/broken or requested; do NOT kill healthy idle sub-agents after a task (reuse via `h2 send`; they auto-exit when parent closes)
h2 rm worker                            # remove a finished agent record
h2 keep worker [--off]                  # resume crashed/restarted agent via session/load
h2 label worker env=staging             # set or clear labels (`env=` removes)
h2 put worker ./payload.bin             # stage a file beside the agent or terminal
h2 attach t1                            # inspect a live PTY workspace terminal (Ctrl-\ then d)
h2 keys t1 enter                        # send named keys (enter, esc, ctrl+c, ...) to a PTY
h2 events                               # follow ordered lifecycle events as JSON lines
```

Messages sent from one ACP agent to another (when `notes != false` in `~/.config/herdr2/config.toml`) are attributed with `<h2 from="...">...</h2>` tags (and `reach="h2 send <addr> when done or stuck"` for ACP parents). ACP agents cannot inject terminal input into PTY terminals (`t*`); terminal coordinators receive results exclusively via `h2 wait` and `h2 read`.

Wait reasons:
- `status`: target is now `idle`, `done`, `blocked`, or `error`.
- `exit`: target process exited (`exit_code` / `signal`).
- `output`: `--output TEXT` matched streamed transcript output.
- `message`: a caller queued a prompt into your inbox while you waited on children (inspect inbox).
- `timeout`: default 180s check-in (unless `--no-timeout`); call `h2 wait` again to continue waiting.

## Permission policy (`approve-reads` default)

`session/request_permission` is evaluated against `default_permissions` (or `--permissions` / template `permissions`):
- `approve-reads` (default): automatically approves read-only tool kinds (`read`, `search`, `think`, `fetch`, `list`, `glob`, `grep`); transitions to `blocked` on mutating/execution requests (`edit`, `write`, `execute`, `delete`, etc.), sets `pending_permission`, and notifies parent with `<h2>...</h2>`.
- `approve-all`: auto-approves every permission request (when an allow option is offered).
- `deny-all`: auto-denies/cancels every permission request.
- `ask`: holds every permission request in `blocked`.

Resolve a `blocked` agent:
```sh
h2 get worker                  # inspect pending_permission.{request_id, title, kind, options}
h2 approve worker              # approve (or --option <option_id>)
h2 deny worker                 # deny/cancel (or --option <option_id>)
```

## CLI-first orchestration (no MCP required)

Use the `h2` CLI directly together with your host's shell / background command runner. Set `inject_mcp = false` in `~/.config/herdr2/config.toml` if you also want ACP child agents to use `h2` CLI + `herdr2` skill without auto-injecting `h2 mcp`.

## Non-agent commands, files, and machines

- `h2 exec [--on box] [--cwd dir] <cmd>`: execute plain non-agent commands directly (no ACP, no PTY); returns exact stdout/stderr and exit code (`-i` feeds stdin).
- `h2 file read|write|edit [--on box] <path>`: read lines (`--offset`/`--limit`), atomically write stdin, or exact-match replace (`--old`/`--new`).
- `h2 machine add|ls|login|update|rename|rm`: orchestrate agents across SSH or custom `--via` hosts (`box/a7` addresses).
- `h2 server start|stop|status|restart`, `h2 update`, `h2 connect`, `h2 schema`: manage daemon, self-updates, stdio JSON-RPC bridge, and OpenRPC schema.
- `h2 adapter status|install|update [--all|--detected] [--on <machine>]`: inspect, auto-install, or update managed ACP adapters (`claude`, `codex`, `pi`, `glm`) from the official ACP Registry into `~/.local/share/herdr2/adapters/`.
- `h2 checkpoint ls|snap|diff|rollback <target>`: inspect hidden Git ref snapshots (`refs/herdr2/checkpoints/<session>/<addr>/<turn>`), view pure authored turn diffs (`--from`/`--to`), or roll back conversation + files (`--to <turn> [--no-files] [--confirm]`).

## Persistence, recovery, and safety rules

- State lives in `~/.local/state/herdr2/<session>/` (`agents.json`, `transcripts/<addr>.jsonl`), config in `~/.config/herdr2/config.toml`, runtime sockets in `$XDG_RUNTIME_DIR/herdr2/<session>.sock`.
- Side-by-side coexistence: `herdr2`/`h2` uses completely separate binaries, env vars (`H2_*`), sockets, and state paths from legacy `herdr-core`/`hc`.
- Never call `h2 server stop` or `h2 server restart` on a shared session unless requested; tests must use a dedicated `H2_SESSION` and scratch XDG paths.
- `h2 drove status|web|probe` manages native Drove role-based multi-agent orchestration (Workers appear as `a*` sub-agents).
- `h2 ai status|settings-set|test` inspects, updates, or tests the shared Core AI assistant (KV-cache metrics, terminal state arbitration, and Drove/notes helpers).
- Exit codes: `2` usage, `3` not_found, `4` machine_unknown, `5` unavailable, `6` invalid_params, `7` needs_target, `8` config, `9` blocked, `10` status_mismatch, `11` finished, `124` waited run/exec timeout.
