# herdr2

If AGENTS.local.md exists, read it too: the maintainer's local rules.

`herdr2` (CLI command `h2`) is a headless ACP (Agent Client Protocol) multi-agent
orchestration runtime: it spawns and drives coding agents over stdio NDJSON without
PTYs or terminal emulation, tracks structured agent state, queues prompts, mediates
tool permissions, injects an MCP coordination server, and relays events/files across
local and SSH machines.

Read the code. This file only holds what reading won't tell you: the rules,
the traps, and the handful of decisions that look arbitrary until you know
why.

`h2 --skill` prints the manual in `crates/herdr2/src/SKILL.md`. The
installable skill is the short stub in `skills/herdr2/SKILL.md`: it says
when to use h2 and points to `h2 --skill`, so it rarely changes.
Keep the manual current when changing agent-facing behavior.

## The rule

Core is a headless orchestration runtime, not a terminal multiplexer. Never
reintroduce PTYs, terminal emulators, or screen scraping. Advertise `terminal: false`
and `fs: { readTextFile: false, writeTextFile: false }` in ACP `initialize`.

## Rules that aren't in the code

- Coexists cleanly alongside legacy `herdr-core` (`hc`): distinct binary (`herdr2`/`h2`),
  env vars (`H2_*`, `HERDR2_*`), config (`~/.config/herdr2`), state (`~/.local/state/herdr2`),
  and runtime sockets (`$XDG_RUNTIME_DIR/herdr2`).
- Every action reports what was observed, never what was assumed.
- One API; the CLI is one call per command. The schema is generated from one
  method table, so params and results in `api/types.rs` are also CLI args and
  the published schema. Change them and regenerate:
  `H2_UPDATE_SCHEMA=1 cargo test -p herdr2 --test m0`.
- Lean, KISS, always. Fewer lines, fewer files, fewer dependencies.
- Deciding code stays separate from IO, so it tests without processes.
- Gate per OS only where the OS really behaves differently, and keep it in
  `platform/` (`unix.rs`, `windows.rs`) behind one interface: shared code
  calls `platform::…` and has no `#[cfg]` of its own.
- Aim for source files under ~32 KB, hard cap 48 KB because read tools stop
  near 50. Split along a real seam when you next touch one that's over.

## Traps

- Tests and smokes run in isolated sessions (`H2_SESSION`) with scratch XDG
  paths under `/var/tmp`. Never touch the machine's real `herdr-core` (`hc`)
  or its running sessions. Clean up scratch data.
- `just install` replaces the installed binary and restarts core. Keeping `--keep`
  agents resumes them via ACP `session/load`.
- A dependency change needs `just notices`; a test fails if a crate is missing
  from the notices.
- `rustfmt.toml` width 120 is the format and CI checks it: `just fmt`.
  `just lint` is fmt, clippy, and clippy for Windows from Linux. `just check`
  is lint plus the whole suite.

## Contributing

Work on a branch and open a pull request to `master`. CI must pass and Can
approves; merges are squashed. Commit messages: lowercase conventional
commits, single line, no AI co-author lines. Only Can releases (a `v*` tag).
CHANGELOG.md: every release gets a `## [x.y.z] - YYYY-MM-DD` section
(`### New` / `### Fixed`), written at release time from the commits since the
last tag; `just release-prepare` refuses without it. One line per change, in
user words (what a person notices, not code), with the issue or PR number and
`thanks @name` for contributors. Leave out internal changes and fixes for bugs
that never shipped.
