# Compatibility Notes

Use this page when you need to compare the Rust port against the original C++ project or depend on older compatibility aliases.

For project overview and usage guidance, see [../README.md](../README.md) and [embedding-guide.md](embedding-guide.md).

Older top-level module paths are still re-exported for compatibility. New code should prefer the structured module paths:

- `udp_prague::core`
- `udp_prague::congestion`
- `udp_prague::protocol`
- `udp_prague::net`
- `udp_prague::demo`

## Remaining Intentional Divergences

The Rust port aims to keep the Prague wire format, congestion-control behavior, and demo reporting surface close to the C++ reference. A small number of deliberate differences still remain.

### Demo reporting and JSON parity

The demo sender/receiver reporting path keeps the reference JSON-lines schema intentionally close to the C++ output:

- All JSON values remain strings, matching the original `json_writer.h` behavior.
- Field names and field order match the reference reporting code.
- Float-valued fields use six fractional digits, matching C++ `std::to_string(float)` output.
- Receiver JSON keeps the original key quirk: classic feedback reports `RTT`, while RFC8888 feedback reports `ATO`.
- RT sender JSON keeps the original extra `frame_inflight` and `frame_window` fields ahead of the packet-window fields.
- The `-j` CLI flag preserves the original filename validation quirk and rejects path separators, so it accepts bare filenames rather than arbitrary paths.

### Rust-side cleanups

These are intentional cleanups rather than bug-for-bug parity:

- String fields are escaped via `serde_json`, so quotes and newlines stay valid JSON even though the C++ writer would emit invalid JSON for those inputs.
- JSON dump failures are latched and warned once instead of being silently ignored.

### Runtime and socket robustness extensions

These are deliberate Rust-side robustness improvements around the runtime and socket boundary:

- The non-connected sender startup wait can optionally be bounded through `RunnerConfig::startup_wait_timeout_us` or the demo CLI flag `--startuptimeout`. Leaving that value unset preserves the reference behavior and waits indefinitely for the trigger packet.
- On Linux, the Unix socket backend intentionally accepts the real 1-byte IPv4 `IP_TOS` ancillary-data payload returned by `recvmsg()`. That is a socket-API robustness improvement rather than a wire-format divergence; the decoded low ECN bits remain semantically aligned with the C++ reference.