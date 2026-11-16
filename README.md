# herdr2 (`h2`)

`herdr2` (`h2`) is a **headless multi-agent orchestration runtime** communicating exclusively via **ACP (Agent Client Protocol) JSON-RPC 2.0 over stdio**.

Called agents receive no PTY, no terminal window, and no terminal identity environment variables (`TERM`, `COLORTERM`, `TERM_PROGRAM` are not set). Client capabilities explicitly advertise `fs: { readTextFile: false, writeTextFile: false }` and `terminal: false` so child agents perform file/tool operations internally according to their own sandbox and policy.

## Quick start

```sh
h2 run --bg --name lead --agent claude --prompt "Inspect README.md and summarize"
h2 ls
h2 wait lead
h2 read lead --last
```

Without `--bg`, `h2 run` waits for the first ACP turn to settle (default 180s check-in, exit 124 if still running while leaving the agent live).

Built-in `--agent` presets: `claude`, `codex`, `gemini`, `opencode`, `copilot`, `cursor`, `kimi`, `qwen`, `pi`, custom `[agents]` tables in `~/.config/herdr2/config.toml`, or explicit command lines (`h2 run --agent "python3 my_acp.py"`).

Non-agent commands run via `exec`:

```sh
h2 exec cargo test
```

## Permissions (`approve-reads` default)

By default (`default_permissions = "approve-reads"`), read-only ACP tool requests (`read`, `search`, `think`, `fetch`, `list`, `glob`, `grep`) are auto-approved. Mutating or command-execution permission prompts pause the agent in state `blocked`:

```sh
h2 get lead
h2 approve lead
h2 deny lead
```

Other modes (`--permissions` or config `default_permissions`): `approve-all`, `approve-reads`, `deny-all`, `ask`.

## Inbox & steering

Prompts sent to a working agent enqueue serially into its per-agent inbox:

```sh
h2 send lead "Also inspect src/mcp.rs"
h2 send lead --steer "Abort current work and summarize now"
h2 interrupt lead
```

`--steer` sends `session/cancel` to abort the running ACP turn cleanly and promotes the steered message ahead of queued inbox items.

## Built-in MCP (`h2 mcp`)

By default (`inject_mcp = true`, disable per-run with `--no-mcp`), `herdr2` injects its built-in stdio MCP server (`h2 mcp`) into ACP `session/new` and `session/load` `mcpServers` with `$H2_AGENT_ID` and `$H2_AGENT_SOCKET` set, exposing `h2_run`, `h2_send`, `h2_wait`, `h2_read`, `h2_ls`, `h2_approve`, and `h2_deny`.

## Templates, remote machines, and files

- [Templates](docs/templates.md): reusable agent specs in `~/.config/herdr2/templates/<name>.md`.
- [Machines](docs/machines.md): orchestrate agents, `exec`, and `file` across SSH or `--via` targets (`box/a3`).
- [Security](docs/security.md): isolated Unix socket / named pipe endpoints, sessions (`H2_SESSION`), and state layout.
- [Commands](docs/commands.md): generated CLI help reference.
- [API](schema/README.md): JSON-RPC 2.0 / OpenRPC protocol (`h2 schema`), events, documents, and `h2 connect`.

Give an agent `h2 --skill` to teach it the full orchestration workflow in one read.

## Side-by-side coexistence with `herdr-core` (`hc`)

`herdr2` (`h2`) uses distinct binary/package names (`herdr2`, `h2`), environment variables (`H2_SESSION`, `H2_SOCKET`, `H2_AGENT_ID`, `H2_AGENT_SOCKET`), config/state directories (`~/.config/herdr2/`, `~/.local/state/herdr2/`), and runtime sockets (`$XDG_RUNTIME_DIR/herdr2/`). It runs alongside an active `herdr-core` (`hc`) daemon without conflict.

## Build and test from source

```sh
cargo build --locked -p herdr2
cargo test --locked
```

License: [Apache-2.0](LICENSE). Run `h2 --licenses` for bundled third-party license notices.
