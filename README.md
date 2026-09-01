# udp_prague

`udp_prague` is a near-literal Rust port of the original [C++ UDP Prague example project](https://github.com/L4STeam/udp_prague).

The core Prague congestion-control logic, packet formats, and socket/runtime pieces intentionally stay close to the C++ reference so parity work remains straightforward.

The reusable `congestion::ClassicAqmMonitor` is an RFC 9331 safety monitor
for transport adapters. It consumes validated RTT/min-RTT, CE, ack-extent,
application-limited, and congestion-avoidance observations. Its fixed-point
score follows the L4STeam Linux `testing` branch at revision
`c6c391a4c5b78a1bff5954e7c25406b4964f50f0`; the monitor owns its slower RTT/MDEV
state and never changes ECN marking by itself. A safety-enabled Prague profile
keeps ECT(1) and clamps effective alpha; ECT(0) is reserved for an explicit
Classic controller profile.

As in the Linux proof-of-concept, low-rate or high-RTT paths can be
conservatively classified as Classic-like even when they are L4S. That safe
false-positive direction is intentional and is exposed through the score and
state telemetry; it is not hidden by a throughput-only shortcut.

On top of that port, the crate adds optional Rust-facing session wrappers plus repository tooling for validation, parity checks, and basic measurements.

The reference-style sender runner now feeds validated ACK/RTT extents into the
Classic-AQM monitor automatically. JSON reports include the detector state,
score, alpha floor, and whether the safety response is enabled/active. The
controlled experiment switch `--no-classic-aqm-fallback` disables only the
alpha floor; it leaves detector telemetry available.

This package has a mixed license boundary: the original UDP Prague portions
are Apache-2.0, while the Linux-derived Classic-AQM monitor and the code that
integrates it are identified as GPL-2.0-only. See [NOTICE](NOTICE),
`LICENSE`, and `LICENSES/GPL-2.0-only.txt` before redistributing it.

Use the crate in two main ways:

- as a reusable Rust crate for your own transport, media, or messaging code
- as a demo sender/receiver setup for parity checks, validation, and basic measurements

If you are new to the codebase, enable the `session` feature and start with the session wrappers in `udp_prague::core`. If you need the closest match to the original example project, use the lower-level controller, packet, and runner APIs.

## Documentation

If you are exploring the repository for the first time, read the docs in this order:

- [docs/embedding-guide.md](docs/embedding-guide.md): start here for API selection, library integration, and end-to-end examples
- [docs/benchmarking-and-validation.md](docs/benchmarking-and-validation.md): use this for demo binaries, Rust-vs-C++ checks, and reproducible validation commands
- [docs/compatibility.md](docs/compatibility.md): read this if you need parity details against the original C++ project

## Quick Start

The crate has three public layers:

- base Prague runtime: always available
- `session`: high-level sender/receiver wrapper APIs for embedding applications
- `demo-app`: reference-style CLI, config parsing, reporting, and demo binaries

Pick a Cargo source the same way you would for any Rust dependency: `version`, `git`, or `path`. The feature recipes below stay the same regardless of which source you use.

If you want only the base Prague runtime:

```toml
[dependencies]
udp_prague = { version = "0.2", default-features = false }
```

If you want the wrapper layer without the demo binaries, enable only `session`:

```toml
[dependencies]
udp_prague = { version = "0.2", default-features = false, features = ["session"] }
```

If you want the full feature set, enable both optional layers explicitly:

```toml
[dependencies]
udp_prague = { version = "0.2", default-features = false, features = ["session", "demo-app"] }
```

If you prefer an unreleased checkout, keep the same shape and replace `version = "0.2"` with `git = "https://github.com/mcabla/udp_prague.git"` or a local `path = "..."`.

The default feature set currently enables `session` and `demo-app` for compatibility, but production integrations are usually clearer when they declare the feature set explicitly.

If you are evaluating the source tree itself, these are useful first commands:

```bash
cargo test --no-default-features
cargo test --no-default-features --features session
cargo test --all-features
cargo run --bin udp_prague_sender -- --help
cargo run --bin udp_prague_receiver -- --help
```

For API walkthroughs and embedding examples, continue with [docs/embedding-guide.md](docs/embedding-guide.md).

## C++ Reference Checkout

The upstream C++ tree is only needed for cross-language comparison and benchmarking. The helper scripts can clone or refresh a pinned checkout automatically when you need it. See [docs/benchmarking-and-validation.md](docs/benchmarking-and-validation.md) for the step-by-step workflow.
