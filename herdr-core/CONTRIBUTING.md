# Contributing

Report bugs and request features in [issues](https://github.com/herdrdev/herdr-core/issues). Discuss bigger changes there before writing a PR. Keep PRs small and add tests for behavior changes.

Core is a headless ACP multi-agent orchestration runtime, not a terminal multiplexer. It runs coding agents over stdio NDJSON without PTYs, advertises `terminal: false`, mediates permissions, injects an MCP server (`h2 mcp`), and reports structured status from ACP events.

## Setup

Install Rust (CI pins 1.95.0), [just](https://just.systems/), and a C linker/toolchain for your OS. On Windows, use the MSVC toolchain and Git for Windows' `sh`.

```sh
rustup component add rustfmt clippy
cargo install cargo-deny --locked
```

On Linux and macOS, also install the target used to type-check Windows code:

```sh
rustup target add x86_64-pc-windows-msvc
```

## Checks

```sh
cargo build --locked -p herdr2
just check
just docs-check
```

`just check` runs formatting, cargo-deny, clippy, and the full test suite. `cargo test --locked` runs just the tests. Tests use isolated `H2_SESSION` sessions; don't point them at a running core.

Keep OS-specific code behind `platform/` and its shared interface, without `cfg` branches in shared code. See [AGENTS.md](AGENTS.md) for the project rules.

After changing dependencies, run `just notices` (Unix; needs `jq`) and review the generated licenses. Keep the [non-Cargo notices](distribution/notices/README.md) current too. `h2 --licenses` and every release archive carry these texts.

PRs target `master`. Only the maintainer publishes releases.
At release time, write what users will notice in CHANGELOG.md under the version being released.
