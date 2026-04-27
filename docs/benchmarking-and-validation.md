# Benchmarking And Validation

Use this guide when you want to run the demo binaries, compare the Rust port against the upstream C++ reference, or reproduce the validation checks used for this crate.

For API walkthroughs and embedding examples, start with [embedding-guide.md](embedding-guide.md).

## Typical Workflow

1. Validate the Rust crate on its own.
2. Fetch the pinned C++ reference checkout if you want cross-language comparisons.
3. Run the localhost comparison or throughput scripts.
4. Move to a real L4S setup only when you need bottleneck behavior instead of local socket validation.

## Validate The Rust Crate

Start here if you only want to check the Rust port:

```bash
cargo test --no-default-features
cargo test --no-default-features --features session
cargo test --all-features
```

On Linux, the Unix socket backend also has a kernel-backed regression test for the IPv4 `IP_TOS` ancillary-data path. That check verifies the local ECN socket API shape; it is intentionally narrower than a full L4S bottleneck or `dualpi2` testbed.

## Prepare The C++ Reference Checkout

The upstream C++ repository is optional. You only need it for Rust-vs-C++ comparison work.

The helper scripts use a pinned upstream commit so repeated measurements stay reproducible:

```text
e8e343533a7cc39b40e357b3975a557a081bf6ec
```

To clone or refresh that checkout, run:

```bash
bash scripts/fetch_cpp_reference.sh
```

By default this prepares a checkout under the repository's `udp_prague/` subdirectory. The scripts also accept:

- `UDP_PRAGUE_CPP_DIR=/path/to/udp_prague` for an existing git checkout elsewhere

Useful overrides:

- `UDP_PRAGUE_CPP_COMMIT=<commit>` to compare against a different upstream revision
- `UDP_PRAGUE_CPP_REPO_URL=<url>` to clone from SSH or a mirror instead of the default HTTPS remote
- `UDP_PRAGUE_AUTO_CLONE_CPP=0` to make the comparison scripts fail instead of cloning automatically

## Performance

Release-mode localhost measurements show that the Rust port is performance-comparable to the C++ reference implementation.

As always, localhost throughput measurements are noisy and should be treated as an approximation rather than a fixed guarantee.

## Helper Scripts

Once the optional C++ checkout is ready, the repository includes this measurement and comparison toolkit under `scripts/`:

- `scripts/fetch_cpp_reference.sh` clones or refreshes the pinned upstream C++ reference checkout used by the comparison scripts.
- `scripts/compare_release_localhost.sh` runs aligned Rust/C++ sender/receiver pairings and reports both the final summary line and a trailing summary window.
- `scripts/measure_release_localhost_perf.sh` measures quiet-mode localhost throughput from loopback byte counters.
- `scripts/measure_release_localhost_perf_batches.sh` repeats the quiet-mode measurement harness in batches and aggregates the per-run results.

These scripts resolve the Rust and C++ tree paths relative to their own location, so they can be invoked from any current working directory. Running them through `bash` is sufficient; they do not rely on the executable bit being set.

By default the measurement scripts rebuild both the Rust and C++ release binaries before running. For follow-up runs against an unchanged build, set `SKIP_BUILD=1`.

### Common Commands

```bash
bash scripts/compare_release_localhost.sh
bash scripts/measure_release_localhost_perf.sh
bash scripts/measure_release_localhost_perf_batches.sh
```

## L4S Test Setup Notes

Not every verification task needs a full L4S bottleneck setup.

- Socket-level ECN verification, localhost regression tests, and the Linux `IP_TOS` ancillary-data check do not require `dualpi2`, a custom `qdisc`, or a separate L4S queue.
- Those checks validate the local send/receive socket path: the sender writes ECN bits, the receiver gets them back from `recvmsg()` ancillary data, and Prague can decode the low ECN bits correctly.
- Real end-to-end L4S behavior validation is different. If you want to study CE marking under queue pressure, Prague adaptation against an L4S AQM, or path behavior beyond localhost, you should use an L4S-capable bottleneck such as `dualpi2` or an equivalent setup.

For background on L4S, deployment context, and related tooling, see https://l4steam.github.io/.