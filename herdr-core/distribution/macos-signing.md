# macOS release signing and notarization

In herdr2's GitHub **release** environment, add these secrets:

- `APPLE_CERTIFICATE_P12`: base64-encoded Developer ID Application certificate
  exported as a `.p12`, including its private key.
- `APPLE_CERTIFICATE_PASSWORD`: the password for that `.p12`.
- `APPLE_API_KEY`: App Store Connect API private key (the contents of the `.p8`).
- `APPLE_API_KEY_ID`: that API key's ID.
- `APPLE_API_ISSUER`: that API key's issuer ID.

Tag pushes require all five secrets. Both macOS architectures are signed with
hardened runtime and notarized before packaging; rejection fails the release.
Manual/untagged release builds use ad-hoc signing without production credentials.

The signature uses the stable identifier `dev.herdr2.core`. The executable has
a bound Info.plist naming **Herdr2** and explaining privacy requests made
by agents. Existing Herdr Core grants do not transfer. Grants to
ad-hoc builds may need to be granted again on the first Developer ID release.
Keep the signing team and identifier stable across updates. This does not
guarantee that macOS's session-scoped permissions stop prompting.

The hardened-runtime entitlement
`com.apple.security.cs.allow-dyld-environment-variables` preserves `DYLD_*`
for child agents. It does not disable library validation for core itself,
nor override restrictions in Apple-protected child programs. Core needs no JIT,
debugging or unsigned-memory exceptions. Camera, audio-input and Apple Events
entitlements permit user-approved agent workloads; they do not grant access
without consent. There is no App Sandbox.

`scripts/mac-bundle` assembles **Herdr2.app** with the same Info.plist as
its executable. `scripts/mac-sign` signs the executable and then the bundle.
`scripts/mac-notarize` submits the bundle zip, waits for acceptance, and staples
and validates its ticket. The macOS archive contains the complete
bundle plus a root executable for released updaters. The root and bundled
executables are independently signed and notarized.

The installer and `h2 update` stage and verify the complete bundle, check
that it runs, then atomically exchange it with the installed bundle. They do
not re-sign, strip, or add quarantine. Entry points in `H2_INSTALL_DIR` (default
`~/.local/bin`) resolve to the newest bundle at the fixed physical path
`~/.local/share/herdr2/active/Herdr2.app`, via the `current` symlink.
Previous bundles are kept aside while old cores run; later installs
reclaim unused previous bundles.
Run `h2 update` to complete a bare install with the bundle, even if the version is already
current. Startup never downloads updates. Updates keep `--keep` agents across restarts via ACP `session/load`.

After launchd detachment the core uses a SETEXEC responsibility disclaimer.
Missing SPI falls back to inherited responsibility with a diagnostic.

## Verification

The tag release is the Developer ID/notarization check. If signing,
notarization or Gatekeeper assessment fails, the build fails and nothing
publishes. No separate manual workflow receives Apple credentials.

For native scratch checks, `tests/macos.rs` verifies launchd isolation and the
embedded identity. Compile `crates/herdr2/tests/macos-dyld.c` with `cc`
for a non-Apple probe: with no arguments it checks that DYLD_LIBRARY_PATH equals
`/herdr-identity-test`; with core PID and caller PID arguments it checks their
coalitions differ. Start only an isolated core with scratch XDG paths and
`H2_SESSION`, set that DYLD value on startup, then run the probe using
`h2 exec`. Reject a smoke if its core.log contains
`launchd startup failed`, even if child commands succeed.

These checks do not attempt LAN access or trigger privacy prompts. A GUI Mac
still needs a separately approved Local Network test; CI success cannot prove
prompt wording, System Settings visibility or user-grant persistence.
