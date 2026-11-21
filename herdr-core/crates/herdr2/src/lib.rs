pub mod acp;
pub mod addr;
pub mod agents;
pub mod ai;
pub mod api;
pub mod attach;
pub mod checkpoint;
pub mod checkpoint_safety;
pub mod cli;
pub mod client;
pub mod config;
pub mod drove;
pub mod events;
pub mod exec;
pub mod files;
pub mod foreground;
pub mod handoff;
pub mod holder;
pub mod holder_proto;
pub mod input;
mod kept;
pub mod keys;
pub mod locale;
pub mod machines;
pub mod mcp;
pub mod notes;
pub mod paths;
pub mod permissions;
pub mod picker;
pub mod platform;
pub mod pty_adapters;
pub mod pty_input;
pub mod pty_transcript;
pub mod put;
pub mod relay;
pub mod releases;
pub mod remote;
mod restart;
pub mod review;
pub mod rpc;
pub mod runtime;
pub mod server;
pub mod status;
pub mod stitch;
pub mod sweep;
pub mod templates;
pub mod terminals;
pub mod transcript;
pub mod viewers;
pub mod wait;
pub mod wire;

/// The folder tests put their scratch data in: `H2_TEST_TMPDIR` if set, else
/// /var/tmp, whose short path keeps socket paths under the macOS limit.
#[cfg(test)]
pub(crate) fn test_tmp() -> std::path::PathBuf {
    std::env::var_os("H2_TEST_TMPDIR").map_or_else(|| "/var/tmp".into(), Into::into)
}
