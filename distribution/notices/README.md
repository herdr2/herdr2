# Shipped notices

`just notices` combines these texts with every registry package in `Cargo.lock`, including target-specific and build dependencies. It reads full LICENSE/COPYING/NOTICE files from Cargo's downloaded sources and fails if none exist. It needs Cargo, jq, and Unix tools. The result is embedded in `h2 --licenses` and shipped in release archives.

Non-Cargo inputs are checked in so regeneration doesn't depend on a license server:

- `musl.txt`: [musl v1.2.5 COPYRIGHT](https://github.com/ifduyue/musl/blob/v1.2.5/COPYRIGHT). Rust 1.95.0's `src/ci/docker/scripts/musl.sh` and `musl-toolchain.sh` select 1.2.5 with security patches for the release Linux targets. The patches do not replace its license.
- `rust.txt`: Rust 1.95.0's installed `share/doc/rust/COPYRIGHT-library.html`, converted to plain text, followed by `library/compiler-builtins/LICENSE.txt` from the same toolchain's rust-src. These cover the standard library and compiler runtime, which Cargo.lock does not list.

`herdr2` runs ACP agents strictly headless over stdio JSON-RPC and ships no GUI bundle, terminal emulator, PTY/ConPTY binaries, icons, fonts, artwork, or sounds.
