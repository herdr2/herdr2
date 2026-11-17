# Commands

Generated from the built binary's help. Do not edit by hand.
Run `just docs` to refresh it and `just docs-check` to check it.

[Start here](../README.md) for a short tour.

## h2

```text
h2 - hybrid PTY workspace & headless ACP multi-agent runtime

Usage: h2 <command> [options]
       h2               list live agents and terminals

Agents & terminals
  run [--agent A] [--prompt P] [argv|template [task]]
                              start a headless ACP sub-agent (--bg: don't wait)
  ls                          list live terminals and ACP agents (--all: finished too)
  get <a|t>                   show one agent's or terminal's full record
  read <a|t> [--last|--lines N]
                              print rendered transcript (ACP) or screen/history (PTY)
  attach <t>                  follow a live PTY terminal in the shell (Ctrl-\ then d)
  keys <t> <key>...           send named keys (enter, esc, ctrl+c, ...) to a PTY terminal
  kill <a|t>                  terminate an ACP agent or PTY terminal
  rm <a|t>                    remove a finished agent or terminal
  label <a|t> k=v...          set or clear labels (`k=` clears)
  keep <a|t> [--off]          resume crashed/restarted agent or shell

Communicate & control
  send <a|t> <text>           queue ACP prompt or paste text into PTY (--steer: cancel & deliver next)
  approve <a> [--option ID]   approve a blocked ACP agent's pending permission request
  deny <a> [--option ID]      deny a blocked ACP agent's pending permission request
  interrupt <a|t>             send ACP `session/cancel` or PTY interrupt (Esc / Ctrl-C)
  wait <a|t>                  wait until not `working` (--output X, --children any|all)
  put <a|t> <file>            stage a file beside agent or terminal, print path

Non-agent & files
  exec [--on m] <cmd>         run non-agent command directly; exact stdout/stderr & exit code
  file read|write|edit [--on m] <path>
                              read, atomic-write from stdin, or exact-edit files

More
  templates                   list templates (~/.config/herdr2/templates)
  mcp                         run built-in stdio MCP server (`h2 mcp`)
  events                      follow ordered lifecycle events as JSON lines
  server start|stop|status|restart
                              manage background runtime daemon
  update [--version X|--from <file>]
                              update h2 binary and restart core cleanly
  machine add|ls|login|update|rename|rm
                              manage remote machines over SSH or `--via`
  connect                     JSON-RPC API over stdin/stdout
  schema                      print OpenRPC 1.3.2 JSON Schema

<a> / <t> is an agent (`a1`), terminal (`t1`), remote (`machine/a7`), or unique name.
Built-in `--agent` presets: claude, codex, gemini, opencode, copilot, cursor, cursor-agent, kimi, qwen, pi, hermes, glm, zcode.
Options: --json (default when piped except read/file read/exec), -V --version, --licenses.
Per command: h2 <command> --help
Settings: ~/.config/herdr2/config.toml (default_permissions, inject_mcp, [agents]).
Agent manual: h2 --skill
```

## h2 approve

```text
Approve a blocked agent's pending ACP permission request

Usage: h2 approve [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json             JSON output even in a terminal
      --option <OPTION>  Explicit ACP permission `optionId` to choose (default: first matching option)
  -h, --help             Print help
```

## h2 attach

```text
Follow live output of an interactive PTY terminal (Ctrl-\ then d detaches)

Usage: h2 attach [OPTIONS] [TERMINAL]

Arguments:
  [TERMINAL]  The terminal: `t7`, a name, or `machine/t7`

Options:
      --json               JSON output even in a terminal
      --observe            Watch only: never type or resize
      --history <HISTORY>  Scrollback lines to show first (main screen only) [default: 0]
  -h, --help               Print help
```

## h2 connect

```text
Speak the protocol over stdin/stdout (`ssh box herdr2 connect`)

Usage: h2 connect [OPTIONS]

Options:
      --json               JSON output even in a terminal
      --session <SESSION>  The h2 session to reach, like H2_SESSION (one command that reads the same in sh, cmd and PowerShell)
  -h, --help               Print help
```

## h2 deny

```text
Deny a blocked agent's pending ACP permission request

Usage: h2 deny [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json             JSON output even in a terminal
      --option <OPTION>  Explicit ACP permission `optionId` to choose (default: first matching option)
  -h, --help             Print help
```

## h2 events

```text
Follow fact changes as they happen (one JSON line each when piped)

Usage: h2 events [OPTIONS]

Options:
      --agent <AGENT>              Only this agent
      --json                       JSON output even in a terminal
      --terminal <TERMINAL>        Only this terminal
      --children-of <CHILDREN_OF>  Only children of this agent or terminal
      --label <K=V>                Only records carrying all of these labels (repeatable)
      --doc <NAME>                 Documents by name (repeatable); a name ending in `*` is a prefix. Naming narrows, so documents alone hear only them — name an agent or terminal for both
  -h, --help                       Print help
```

## h2 exec

```text
Run a non-agent command, here or on a machine: exact stdout/stderr and exit code

Usage: h2 exec [OPTIONS] <ARGV>...

Arguments:
  <ARGV>...  Program and arguments, run as given (no shell)

Options:
      --json               JSON output even in a terminal
      --on <MACHINE>       Run it on another machine (see `h2 machine ls`)
      --cwd <CWD>          Working directory (default: where h2 runs; through the API, the core's; with --on, that machine's home)
      --env <K=V>          Set on top of the core's environment, key=value (repeatable)
      --timeout <TIMEOUT>  Seconds before it and everything it started are ended (default 60, at most 600)
  -i, --stdin              Feed what is piped into h2 to its stdin
  -h, --help               Print help
```

## h2 file

```text
Read, write or edit a file here or on a machine

Usage: h2 file [OPTIONS] <COMMAND>

Commands:
  read   Print a text file, or lines of it (--offset, --limit)
  write  Write what is piped in as the file, making missing folders
  edit   Replace exact text found once (--old X --new Y, repeatable); all or none
  help   Print this message or the help of the given subcommand(s)

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 file edit

```text
Replace exact text found once (--old X --new Y, repeatable); all or none

Usage: h2 file edit [OPTIONS] --old <OLD> --new <NEW> <PATH>

Arguments:
  <PATH>  The file: absolute, or from `cwd`

Options:
      --json          JSON output even in a terminal
      --on <MACHINE>  On another machine (see `h2 machine ls`)
      --cwd <CWD>     Where a relative path starts (default: where h2 runs; through the API, the core's; with --on, that machine's home)
      --old <OLD>     Exact text found once in the file (repeatable, paired with --new in order)
      --new <NEW>     What replaces it
  -h, --help          Print help
```

## h2 file read

```text
Print a text file, or lines of it (--offset, --limit)

Usage: h2 file read [OPTIONS] <PATH>

Arguments:
  <PATH>  The file: absolute, or from `cwd`

Options:
      --json             JSON output even in a terminal
      --on <MACHINE>     On another machine (see `h2 machine ls`)
      --cwd <CWD>        Where a relative path starts (default: where h2 runs; through the API, the core's; with --on, that machine's home)
      --offset <OFFSET>  The first line to give, from 1
      --limit <LIMIT>    At most this many lines (default: all, up to 8 MB)
  -h, --help             Print help
```

## h2 file write

```text
Write what is piped in as the file, making missing folders

Usage: h2 file write [OPTIONS] <PATH>

Arguments:
  <PATH>  The file: absolute, or from `cwd`

Options:
      --json          JSON output even in a terminal
      --on <MACHINE>  On another machine (see `h2 machine ls`)
      --cwd <CWD>     Where a relative path starts (default: where h2 runs; through the API, the core's; with --on, that machine's home)
  -h, --help          Print help
```

## h2 get

```text
Show one agent

Usage: h2 get [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 interrupt

```text
Send ACP `session/cancel` to stop the current turn cleanly

Usage: h2 interrupt [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json               JSON output even in a terminal
      --timeout <TIMEOUT>  Seconds to wait for cancellation effect (default 10)
  -h, --help               Print help
```

## h2 keep

```text
Keep a live agent recoverable via `session/load` (--off disables)

Usage: h2 keep [OPTIONS] <AGENT>

Arguments:
  <AGENT>  The agent: `a7`, a name, or `machine/a7`

Options:
      --json  JSON output even in a terminal
      --off   Keep this live agent recoverable across crash or planned restart
  -h, --help  Print help
```

## h2 keys

```text
Send named keys (enter, esc, tab, ctrl+c, ...) to a live PTY terminal

Usage: h2 keys [OPTIONS] <TERMINAL> <KEYS>...

Arguments:
  <TERMINAL>  The terminal: `t7`, a name, or `machine/t7`
  <KEYS>...   Key names: enter, esc, tab, up, ctrl+c, alt+x, f5, a, ...

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 kill

```text
End an agent process (HUP, then escalate)

Usage: h2 kill [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 label

```text
Set labels (key=value; key= removes)

Usage: h2 label [OPTIONS] <AGENT> <K=V>...

Arguments:
  <AGENT>   The agent: `a7`, a name, or `machine/a7`
  <K=V>...  Labels to set, key=value; `key=` removes one

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 ls

```text
List live agents

Usage: h2 ls [OPTIONS]

Options:
      --all          Include finished agents
      --json         JSON output even in a terminal
      --label <K=V>  Only agents carrying all of these labels (repeatable)
  -h, --help         Print help
```

## h2 machine

```text
Other machines, reached over ssh

Usage: h2 machine [OPTIONS] <COMMAND>

Commands:
  add     Add a machine you reach with ssh: h2 machine add vps
  ls      List the machines added here
  login   Log in again (a password or code) after the machine says it needs one. On Windows: reconnect once its SSH keys work
  update  Copy this h2 there and restart h2 there
  rename  Give a machine a new name here
  rm      Forget a machine (its agents keep running there)
  help    Print this message or the help of the given subcommand(s)

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 machine add

```text
Add a machine you reach with ssh: h2 machine add vps

Usage: h2 machine add [OPTIONS] [TARGET]

Arguments:
  [TARGET]  How ssh reaches it: `vps`, `can@10.0.0.7`, `can@10.0.0.7:2222`, a Host from ~/.ssh/config

Options:
      --json               JSON output even in a terminal
      --via <COMMAND>      Reach it with this command instead of ssh: it is given a shell line to run there as its last argument, like ssh (`ssh -p 2222 -i key user@ip`, `fly ssh console -a app --pty=false -C`)
      --temp               Mark it short-lived (a sandbox for a task), so clients can show it apart from machines that stay. h2 treats it like any other; remove it with `h2 machine rm`
      --install            Copy this h2 there when it has none or one too old, without asking
      --name <NAME>        What to call it here (default: the target's host)
      --session <SESSION>  The h2 session on that machine (default: its default)
  -h, --help               Print help
```

## h2 machine login

```text
Log in again (a password or code) after the machine says it needs one. On Windows: reconnect once its SSH keys work

Usage: h2 machine login [OPTIONS] <MACHINE>

Arguments:
  <MACHINE>  The machine's name

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 machine ls

```text
List the machines added here

Usage: h2 machine ls [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 machine rename

```text
Give a machine a new name here

Usage: h2 machine rename [OPTIONS] <MACHINE> <NEW_NAME>

Arguments:
  <MACHINE>   The machine's name now
  <NEW_NAME>  Its new name. Addresses with the old one stop working

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 machine rm

```text
Forget a machine (its agents keep running there)

Usage: h2 machine rm [OPTIONS] <MACHINE>

Arguments:
  <MACHINE>  The machine's name

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 machine update

```text
Copy this h2 there and restart h2 there

Usage: h2 machine update [OPTIONS] <MACHINE>

Arguments:
  <MACHINE>  The machine's name

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 mcp

```text
Run the built-in stdio MCP server (`h2 mcp`)

Usage: h2 mcp [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 put

```text
Stage a local file beside a live agent (on its machine); prints its path

Usage: h2 put [OPTIONS] <AGENT> <FILE>

Arguments:
  <AGENT>  The agent: `a7`, a name, or `machine/a7`
  <FILE>   The file's name, kept so `.png` stays `.png` (CLI: the local file to send)

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 read

```text
Print an agent's transcript or final assistant text (`--last`)

Usage: h2 read [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json               JSON output even in a terminal
      --lines <LINES>      Trailing rendered transcript lines (default: full transcript)
      --last               Print only the final assistant text from the latest turn
      --wait               Wait for the agent to finish its current turn or block before reading
      --timeout <TIMEOUT>  With `--wait`: seconds before returning early (default: config, else 180)
  -h, --help               Print help
```

## h2 rm

```text
Remove a finished agent

Usage: h2 rm [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent: `a7`, a name, or `machine/a7`

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 run

```text
Start an ACP agent preset, command, or template: `h2 run claude "fix tests"`

Usage: h2 run [OPTIONS] [ARGV]...

Arguments:
  [ARGV]...  Preset/command and arguments, or a template name and its task

Options:
      --agent <AGENT>         ACP preset name (`claude`, `codex`, `gemini`, ...) or explicit command line
      --json                  JSON output even in a terminal
      --prompt <TEXT>         Initial prompt queued right after ACP session setup
      --system-prompt <TEXT>  System prompt appended via `_meta.systemPrompt` on session creation
      --model <MODEL>         Model override passed to `session/set_config_option` (`model`)
      --mode <MODE>           Session mode override passed to `session/set_mode`
      --permissions <MODE>    Permission policy (`approve-all`, `approve-reads`, `deny-all`, `ask`) [possible values: approve-all, approve-reads, deny-all, ask]
      --no-mcp                Disable injecting the built-in `herdr2` MCP server into `mcpServers`
      --name <NAME>           Name, unique among live agents: 1-32 of a-z 0-9 - _ 
      --cwd <CWD>             Working directory (default: where h2 runs; through the API, the core's; with --on, that machine's home)
      --on <MACHINE>          Run it on another machine (see `h2 machine ls`); its address is `machine/a7`
      --label <K=V>           Labels, key=value (repeatable)
      --parent <PARENT>       Parent agent (default: the agent it is started from)
      --no-parent             No parent, even when started from an agent
      --no-notify             Don't send automated notes about it to its parent
      --outlive-parent        Keep it running when its parent finishes (by default it ends too)
      --rm                    Remove it right after it exits (once a waiting parent has seen it)
      --keep                  Keep recovering via `session/load` (if supported) after crash or restart
      --bg                    Start it and return immediately
      --timeout <TIMEOUT>     Seconds before a wait checks in, leaving it running (default: config `wait_timeout`, else 180)
  -h, --help                  Print help
```

## h2 schema

```text
Every API method with its JSON Schema, for app builders

Usage: h2 schema [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 send

```text
Queue a prompt into an agent's ACP inbox (or --steer to cancel & deliver next)

Usage: h2 send [OPTIONS] <AGENT> <TEXT>

Arguments:
  <AGENT>  The agent: `a7`, a name, or `machine/a7`
  <TEXT>   Prompt text queued for the agent's ACP inbox

Options:
      --json               JSON output even in a terminal
      --steer              Cancel the active ACP turn (`session/cancel`) and deliver this prompt next
      --if <STATUS>        Only enqueue if the status is exactly this [possible values: idle, working]
      --wait               Wait for the turn this send triggers
      --timeout <TIMEOUT>  With wait: seconds before it checks in (default: config, else 180)
      --from <FROM>        Sender address attributed in `<h2 from="...">` tags (default: caller)
  -h, --help               Print help
```

## h2 server

```text
Start, stop or inspect the core

Usage: h2 server [OPTIONS] <COMMAND>

Commands:
  start    
  stop     
  status   
  restart  Restart the core (e.g. after an update); kept agents resume via `session/load`
  help     Print this message or the help of the given subcommand(s)

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 server restart

```text
Restart the core (e.g. after an update); kept agents resume via `session/load`

Usage: h2 server restart [OPTIONS]

Options:
      --fresh  Take this shell's environment for new agents; run from outside an agent
      --json   JSON output even in a terminal
  -h, --help   Print help
```

## h2 server start

```text
Usage: h2 server start [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 server status

```text
Usage: h2 server status [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 server stop

```text
Usage: h2 server stop [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 templates

```text
Templates you can run by name (~/.config/herdr2/templates)

Usage: h2 templates [OPTIONS]

Options:
      --json  JSON output even in a terminal
  -h, --help  Print help
```

## h2 update

```text
Swap this h2 for another build and restart core

Usage: h2 update [OPTIONS]

Options:
      --from <FILE>      A herdr2 binary to use (default: download the latest release)
      --json             JSON output even in a terminal
      --version <X.Y.Z>  At least this release (`0.1.4`): nothing changes when this h2 is already that or newer
      --force            Allow an older version (saved state only moves forward), or a release of another protocol generation
  -h, --help             Print help
```

## h2 wait

```text
Wait for children (--children any) or one agent; a message to you also ends it

Usage: h2 wait [OPTIONS] [AGENT]

Arguments:
  [AGENT]  The agent; with `children`, whose children (default: the caller)

Options:
      --children <CHILDREN>  Wait on children instead: all = until none is working, any = next news or a message to you [possible values: any, all]
      --json                 JSON output even in a terminal
      --output <OUTPUT>      End when streamed transcript output after the wait starts contains this text. Blocked, error or exit end it too
      --timeout <TIMEOUT>    Seconds before it checks in (default: config `wait_timeout`, else 180)
  -h, --help                 Print help
```
