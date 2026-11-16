# h2 manual (herdr2)

`h2` (`herdr2`) is a headless multi-agent orchestration runtime speaking the Agent Client Protocol (ACP) over stdio JSON-RPC 2.0. Called agents receive `terminal: false` and `fs: false` capabilities, zero PTYs, and zero terminal environment variables (`TERM`, `COLORTERM`, `TERM_PROGRAM` are omitted).

- Inside an orchestrated agent, `$H2_AGENT_ID` holds your address (`a1`, `a7`, ...) and `$H2_AGENT_SOCKET` points to the daemon socket.
- Agents you launch default to becoming your children (they receive `<h2>` tags on inter-agent messages and exit with you unless `--outlive-parent` is passed).
- Piped output is JSON except `run` (waited), `exec`, `read`, and `file read` (text unless `--json`).
- Run `h2 --help` for commands or `h2 <command> --help` for exact flags.

## Launch an ACP agent

Built-in presets for `--agent` (or positional first argument): `claude`, `codex`, `gemini`, `opencode`, `copilot`, `cursor`, `kimi`, `qwen`, `pi`. Custom command lines work via `--agent "python3 my_acp.py"` or positional argv `h2 run -- python3 my_acp.py`.

```sh
# Check reusable Markdown templates in ~/.config/herdr2/templates/*.md
h2 templates

# Launch with preset + initial prompt in background
h2 run --bg --name reviewer --cwd /path/to/repo --agent claude --prompt "Review src/x.rs only. Report findings via h2_send or h2 send $H2_AGENT_ID."

# Wait for an agent to finish or block
h2 wait reviewer

# Launch a template (instructions from template body + task argument)
h2 run --bg --name worker reviewer "Check src/acp/mod.rs for edge cases"
```

Overrides on `run`: `--model <MODEL>`, `--mode <MODE>`, `--system-prompt <TEXT>`, `--permissions <approve-all|approve-reads|deny-all|ask>`, `--no-mcp`, `--keep`, `--rm`, `--no-notify`, `--outlive-parent`, `--label k=v`, `--on <machine>`.

Never use shell `&` or `sleep` polling for agents: use `h2 run --bg`, `h2 wait <a>`, or `h2 wait --children any|all`.

## Coordinate, steer, and read transcripts

```sh
h2 ls                                   # list live agents (--all includes finished)
h2 get worker                           # full JSON/summary agent state
h2 send worker "Also inspect error.rs"  # queue prompt into serial ACP inbox
h2 send worker --steer "Stop that; inspect foo.rs instead" # cancel active turn & deliver next
h2 read worker                          # rendered JSONL transcript
h2 read worker --last                   # final assistant message from latest turn
h2 read worker --lines 50               # trailing 50 rendered transcript lines
h2 read worker --wait --last            # wait for current turn to settle, then read --last
h2 interrupt worker                     # send ACP session/cancel without killing process
h2 kill worker                          # terminate an ACP agent process
h2 rm worker                            # remove a finished agent record
h2 keep worker [--off]                  # resume crashed/restarted agent via session/load
h2 label worker env=staging             # set or clear labels (`env=` removes)
h2 put worker ./payload.bin             # stage a file beside the agent, print path
h2 events                               # follow ordered lifecycle events as JSON lines
```

Messages sent from one agent to another (when `notes != false` in `~/.config/herdr2/config.toml`) are attributed with `<h2 from="...">...</h2>` tags (and `reach="h2 send <addr> when done or stuck"` on first task launches).

Wait reasons:
- `status`: target is now `idle`, `done`, `blocked`, or `error`.
- `exit`: target process exited (`exit_code` / `signal`).
- `output`: `--output TEXT` matched streamed transcript output.
- `message`: a caller queued a prompt into your inbox while you waited on children (inspect inbox).
- `timeout`: default 180s check-in; call `h2 wait` again to continue waiting.

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

## Built-in MCP server (`h2 mcp`)

By default (`inject_mcp = true`, unless `--no-mcp`), `herdr2` injects `{ "type": "stdio", "name": "herdr2", "command": "<h2-exe>", "args": ["mcp"], "env": [{"name": "H2_AGENT_ID", ...}, {"name": "H2_AGENT_SOCKET", ...}] }` into ACP `session/new` and `session/load` `mcpServers` (falling back automatically without `"type": "stdio"` if an adapter returns `-32602`).

Exposed MCP tools: `h2_run`, `h2_send`, `h2_wait`, `h2_read`, `h2_ls`, `h2_approve`, `h2_deny`.

## Non-agent commands, files, and machines

- `h2 exec [--on box] [--cwd dir] <cmd>`: execute plain non-agent commands directly (no ACP, no PTY); returns exact stdout/stderr and exit code (`-i` feeds stdin).
- `h2 file read|write|edit [--on box] <path>`: read lines (`--offset`/`--limit`), atomically write stdin, or exact-match replace (`--old`/`--new`).
- `h2 machine add|ls|login|update|rename|rm`: orchestrate agents across SSH or custom `--via` hosts (`box/a7` addresses).
- `h2 server start|stop|status|restart`, `h2 update`, `h2 connect`, `h2 schema`: manage daemon, self-updates, stdio JSON-RPC bridge, and OpenRPC schema.

## Persistence, recovery, and safety rules

- State lives in `~/.local/state/herdr2/<session>/` (`agents.json`, `transcripts/<addr>.jsonl`), config in `~/.config/herdr2/config.toml`, runtime sockets in `$XDG_RUNTIME_DIR/herdr2/<session>.sock`.
- Side-by-side coexistence: `herdr2`/`h2` uses completely separate binaries, env vars (`H2_*`), sockets, and state paths from legacy `herdr-core`/`hc`.
- Never call `h2 server stop` or `h2 server restart` on a shared session unless requested; tests must use a dedicated `H2_SESSION` and scratch XDG paths.
- Exit codes: `2` usage, `3` not_found, `4` machine_unknown, `5` unavailable, `6` invalid_params, `7` needs_target, `8` config, `9` blocked, `10` status_mismatch, `11` finished, `124` waited run/exec timeout.
