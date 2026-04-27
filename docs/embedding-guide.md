# Embedding Guide

Start here if you want to embed `udp_prague` into a Rust application.

For project overview, feature selection, and validation commands, see [../README.md](../README.md).

## Library-first usage

The crate is split into a small number of public feature layers:

- base Prague runtime: always available
- `session`: higher-level wrapper APIs for bulk, segmented bulk, and video sessions
- `demo-app`: reference-style CLI/config/reporting layer and example binaries

Choose a Cargo source (`version`, `git`, or `path`) and then apply the feature recipe you want.

If you only want the reusable base Prague API inside another project:

```toml
[dependencies]
udp_prague = { version = "0.1.0", default-features = false }
```

If you want the higher-level session wrappers without the demo CLI/reporting layer:

```toml
[dependencies]
udp_prague = { version = "0.1.0", default-features = false, features = ["session"] }
```

If you want the full crate surface including the demo binaries:

```toml
[dependencies]
udp_prague = { version = "0.1.0", default-features = false, features = ["session", "demo-app"] }
```

If you want a git checkout instead of a published version, keep the same inline-table form and replace `version = "0.1.0"` with `git = "https://github.com/mcabla/udp_prague.git"` or a local `path = "..."`.

The default feature set currently enables `session` and `demo-app`, but explicit feature selection is usually easier to maintain in larger applications.

### Primary modules

- `udp_prague::core`: runtime config, reporting hooks, sender/receiver loops, error types
- `udp_prague::congestion`: Prague congestion controller and related types
- `udp_prague::protocol`: packet format helpers
- `udp_prague::net`: UDP socket backend with ECN support
- `udp_prague::core` session wrappers: available with feature `session`
- `udp_prague::demo`: reference-style app/config/reporting layer, only with feature `demo-app`

### Which API Should I Start With?

If you are new to this codebase, this is the shortest decision guide:

- I want the closest thing to the original C++ example.
    Start with `PragueCC`, the packet-format types, and `run_sender_with_reporter(...)` / `run_receiver_with_reporter(...)`.
- I want to send ordinary messages, audio chunks, or app-defined datagrams.
    Start with `PragueSenderSession` and `PragueReceiverSession`.
- I want to send one logical large object and get that same logical object back out on receive.
    Start with `PragueSegmentSenderSession` and `PragueSegmentReceiverSession`.
- I want frame-oriented video transport with Prague's video advice.
    Start with `PragueVideoSenderSession` and `PragueVideoReceiverSession`.

Everything below the next heading assumes the `session` feature is enabled.

### Start Here For A Real Application

If you want to send actual audio, video, game state, messages, or any other application content, start with these low-level pieces:

- `udp_prague::core::{PragueSenderSession, PragueReceiverSession, PragueSessionConfig}`
- `udp_prague::core::{PragueSegmentSenderSession, PragueSegmentReceiverSession}`
- `udp_prague::core::{PragueVideoSenderSession, PragueVideoSessionConfig}`
- `udp_prague::congestion::PragueCC`
- `udp_prague::congestion::{PragueRateAdvice, PragueVideoRateAdvice}`
- `udp_prague::net::UDPSocket`
- `udp_prague::protocol::{DataMessage, FrameMessage, AckMessage}`

The `udp_prague::core::run_sender_with_reporter` and `run_receiver_with_reporter` APIs are compatibility loops for the reference demo application. They currently generate the demo/reference packet flow and fill the payload area with dummy bytes. They are useful for parity tests, not as the primary embedding API for your own media or messaging stack.

For most real applications, the best starting point is now the session wrapper layer:

- `PragueSenderSession` owns sequence numbers, in-flight tracking, packet packing, and classic ACK processing for bulk/message traffic.
- `PragueSegmentSenderSession` / `PragueSegmentReceiverSession` add a Rust-only logical segment layer on top of bulk Prague packets, so large payloads can be chunked and reassembled as one logical unit without changing the underlying Prague transport.
- `PragueVideoSenderSession` owns frame-slot timing, frame fragmentation, frame-aware ACK processing, and Prague RT header packing for video traffic.
- `PragueVideoReceiverSession` reassembles RT frame fragments back into one complete frame payload while still sending classic ACKs per Prague packet.
- `PragueReceiverSession` parses inbound bulk or frame packets, updates Prague receiver state, and can auto-send classic ACKs.

The low-level `PragueCC` + `UDPSocket` + packet-view API is still there when you want full control over framing, delayed ACKs, RFC8888, or custom send scheduling.

### Minimal Embedding Imports

```rust
use udp_prague::core::{
    PragueReceiverSession, PragueSegmentReceiverSession, PragueSegmentSenderSession,
    PragueSenderSession, PragueSessionConfig, PragueVideoReceiverSession,
    PragueVideoSenderSession, PragueVideoSessionConfig,
};
use udp_prague::congestion::{
    PragueBitrateAction, PragueCC, PragueCongestionSignal, PragueRateAdvice,
    PragueVideoRateAdvice, count_tp, ecn_tp, rate_tp, size_tp, time_tp,
};
use udp_prague::net::UDPSocket;
use udp_prague::protocol::{AckMessage, DataMessage, FrameMessage};
```

### High-Level Session Wrapper

The session wrapper is intended as the easiest library API for classic Prague data traffic.

Sender side:

- `PragueSenderSession::connect(...)`
- `session.send_bulk(app_bytes)`
- `session.send_large_bulk_blocking(app_bytes, feedback_timeout_us)`
- `session.receive_feedback(timeout)`
- `session.advice()`
- `session.recommended_bitrate_bits_per_sec()`
- `session.max_configured_bitrate_bits_per_sec()`

Segmented bulk side:

- `PragueSegmentSenderSession::connect(...)`
- `sender.send_segment_blocking(content_tag, payload, feedback_timeout_us)`
- `PragueSegmentReceiverSession::bind(...)`
- `receiver.receive_segment_and_ack(timeout)`

Receiver side:

- `PragueReceiverSession::bind(...)`
- `session.receive(timeout)` or `session.receive_and_ack(timeout)`
- `session.acknowledge(sequence_number)`

Video sender side:

- `PragueVideoSenderSession::connect(...)`
- `session.queue_frame(encoded_frame_bytes)`
- `session.transmit_ready_frame_fragments()`
- `session.receive_feedback(timeout)`
- `session.advice()`

Video receiver side:

- `PragueVideoReceiverSession::bind(...)`
- `receiver.receive_frame_and_ack(timeout)`

Example:

```rust
use udp_prague::core::{
    PragueReceivedPacket, PragueReceiverSession, PragueSenderSession, PragueSessionConfig,
};

fn session_wrapper_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = PragueSenderSession::connect(
        "127.0.0.1",
        8080,
        PragueSessionConfig::default(),
    )?;

    let _recommended_bitrate_bps = sender.recommended_bitrate_bits_per_sec();
    let _max_bitrate_bps = sender.max_configured_bitrate_bits_per_sec();

    if sender.can_send_now() {
        let sent = sender.send_bulk(b"encoded-audio-or-message-bytes")?;

        if let Some(feedback) = sender.receive_feedback(50_000)? {
            let advice = feedback.advice;
            let _target_bitrate_bps = advice.pacing_rate_bits_per_sec();
            let _acked_seq = feedback.acked_sequence_number;
            let _sent_seq = sent.sequence_number;
        }
    }

    let mut receiver = PragueReceiverSession::bind("0.0.0.0", 8080)?;
    if let Some(received) = receiver.receive_and_ack(50_000)? {
        match received.packet {
            PragueReceivedPacket::Bulk(packet) => {
                let _app_bytes = packet.app_data;
            }
            PragueReceivedPacket::Frame(packet) => {
                let _frame_fragment = packet.app_data;
            }
        }
    }

    Ok(())
}
```

You do not need to call `receive_feedback(...)` after every single `send_bulk(...)` call. But you do need to process feedback regularly enough for Prague to:

- reduce the in-flight packet count
- update RTT / ECN / loss state
- refresh the recommended bitrate and other hints

If you keep sending and never process feedback, the sender will eventually fill its Prague window and stall.

For the easiest synchronous path, `send_large_bulk_blocking(...)` handles this loop for you: it splits a large payload across ordinary Prague bulk packets, waits as needed for pacing / ACK progress, and returns when the whole payload has been sent and acknowledged.

This helper does not invent a new wire format or hidden reassembly protocol. The receiver still sees normal bulk packets, so your application remains responsible for any higher-level content boundaries or segment metadata.

If you do want the library to own one logical payload boundary end-to-end, use `PragueSegmentSenderSession` and `PragueSegmentReceiverSession`. They add a Rust-only segment header inside the Prague bulk payload area, then reassemble the full payload for you on the receiver side. This stays compatible with the reference transport because the outer Prague packet format is unchanged, but the segment wrapper itself is only understood by the Rust segmented-bulk API.

Example:

```rust
use udp_prague::core::{
    PragueSegmentReceiverSession, PragueSegmentSenderSession, PragueSessionConfig,
};

fn segmented_bulk_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = PragueSegmentSenderSession::connect(
        "127.0.0.1",
        8080,
        PragueSessionConfig::default(),
    )?;
    let mut receiver = PragueSegmentReceiverSession::bind("0.0.0.0", 8080)?;

    let payload = vec![0u8; 32 * 1024];
    let content_tag = 1u16;

    let _report = sender.send_segment_blocking(content_tag, &payload, 50_000)?;
    if let Some(received) = receiver.receive_segment_and_ack(50_000)? {
        let _tag = received.content_tag;
        let _payload = received.payload;
    }

    Ok(())
}
```

This wrapper currently focuses on classic ACK-based transport bookkeeping. If you want RFC8888 ACK handling, delayed ACK policies, or full RT/frame scheduling in the transport layer itself, use the lower-level Prague pieces directly.

### Video Session Wrapper

The video sender wrapper is the easiest way to drive RT/Prague from an encoder loop.

Typical usage is:

1. ask `session.advice()` for the current target frame size / bitrate guidance
2. encode or pick the next frame
3. `queue_frame(...)` when the next frame slot opens
4. call `transmit_ready_frame_fragments()` whenever the socket is allowed to send
5. call `receive_feedback(...)` to process ACKs and refresh Prague state

Example:

```rust
use udp_prague::core::{
    PragueVideoReceiverSession, PragueVideoSenderSession, PragueVideoSessionConfig,
};

fn video_session_wrapper_example() -> Result<(), Box<dyn std::error::Error>> {
    let mut sender = PragueVideoSenderSession::connect(
        "127.0.0.1",
        8080,
        PragueVideoSessionConfig::default(),
    )?;
    let mut receiver = PragueVideoReceiverSession::bind("0.0.0.0", 8080)?;

    let advice = sender.advice();
    let _target_bitrate_bps = sender.recommended_bitrate_bits_per_sec();
    let _max_bitrate_bps = sender.max_configured_bitrate_bits_per_sec();
    let _target_frame_size = advice.target_frame_size_bytes;

    // Example integration point:
    // let encoded_frame = encoder.encode_next_frame(_target_bitrate_bps, _target_frame_size as usize)?;
    let encoded_frame = vec![0u8; 1200];

    if sender.can_queue_frame_now() {
        let queued = sender.queue_frame(&encoded_frame)?;

        while sender.has_pending_frame() || sender.inflight_packets() > 0 {
            match sender.transmit_ready_frame_fragments() {
                Ok(Some(report)) => {
                    let _frame_complete = report.frame_complete;
                    let _fragments_sent = report.fragments_sent;
                }
                Ok(None) => {}
                Err(udp_prague::SessionError::WouldBlock { .. }) => {}
                Err(err) => return Err(Box::new(err)),
            }

            if let Some(feedback) = sender.receive_feedback(20_000)? {
                let _feedback_advice = feedback.advice;
            }
        }

        let _frame_number = queued.frame_number;
    }

    if let Some(received) = receiver.receive_frame_and_ack(50_000)? {
        let _full_frame = received.payload;
        let _frame_nr = received.frame_number;
        let _frame_size = received.frame_size_bytes;
    }

    Ok(())
}
```

This wrapper uses the classic ACK path, but it already owns the RT-specific transport bookkeeping: frame slot timing, Prague frame headers, fragmentation, in-flight frame accounting, and ACK-driven frame completion/loss tracking.

At the session layer there are now two RT receive choices:

- `PragueReceiverSession` if you want raw frame fragments and will handle reassembly yourself.
- `PragueVideoReceiverSession` if you want the library to give you one reassembled frame payload.

What it still does not own is your media semantics. Your application still decides:

- how to encode the frame
- whether to drop or skip frames
- how to react to `PragueVideoRateAdvice`
- whether to use delayed ACKs or RFC8888 instead of classic ACKs

Choosing between the wrappers:

- Use `PragueSenderSession` when your application already owns datagram boundaries.
- Use `send_large_bulk_blocking(...)` when you want synchronous transport splitting but will reassemble at the app layer yourself.
- Use `PragueSegmentSenderSession` / `PragueSegmentReceiverSession` when you want the Rust library to preserve one logical large-payload boundary for you.
- Use `PragueVideoSenderSession` + `PragueVideoReceiverSession` when the payload is really frame-oriented video and Prague frame advice should drive the encoder.

### Built-In L4S Adaptation Advice

The library now exposes an application-facing advice API directly on `PragueCC`:

- `cc.bulk_advice()` for audio chunks, messages, and other non-frame traffic
- `cc.video_advice()` for frame-oriented video traffic

Each snapshot reports:

- the current target pacing rate
- packet size, window, and burst guidance
- a coarse congestion summary
- whether Prague has fallen back out of L4S marking
- for video, a target frame size and frame window

Typical usage is:

1. process ACK or RFC8888 feedback with `PacketReceived(...)` and `ACKReceived(...)`
2. query fresh advice from `PragueCC`
3. adapt encoder bitrate, frame size, batching, or payload density

Example:

```rust
use udp_prague::congestion::{
    PragueBitrateAction, PragueCC, PragueCongestionSignal, PragueVideoRateAdvice,
};

fn apply_video_adaptation(
    cc: &mut PragueCC,
    previous: Option<PragueVideoRateAdvice>,
) -> PragueVideoRateAdvice {
    let advice = cc.video_advice();

    let target_bitrate_bps = advice.pacing_rate_bits_per_sec();
    let target_frame_size = advice.target_frame_size_bytes;

    // Example integration point:
    // encoder.set_target_bitrate(target_bitrate_bps);
    // encoder.set_target_frame_size(target_frame_size as usize);

    match advice.transport.congestion_signal {
        PragueCongestionSignal::Stable => {
            // no active congestion signal
        }
        PragueCongestionSignal::EcnMarked => {
            // L4S path is asking for a gentle reduction or hold
        }
        PragueCongestionSignal::LossRecovery => {
            // stronger backoff is appropriate
        }
        PragueCongestionSignal::L4sFallback => {
            // the path no longer supports L4S marking; avoid assuming ECT(1)
        }
    }

    if let Some(previous) = previous {
        match advice.bitrate_action_since(&previous, 5) {
            PragueBitrateAction::Increase => {
                // quality may be raised carefully
            }
            PragueBitrateAction::Hold => {
                // stay near the current operating point
            }
            PragueBitrateAction::Decrease => {
                // reduce bitrate, frame complexity, or payload density
            }
        }
    }

    let _ = (target_bitrate_bps, target_frame_size);
    advice
}
```

This is a pull-based reporting API. If your application wants actual notifications, the normal pattern is to compare the newest advice snapshot with the previous one and emit your own app-level event when the bitrate direction or congestion signal changes.

### How Your Content Fits Into The Packet

For bulk mode, audio chunks, control messages, or any other non-frame application traffic, a datagram typically looks like this:

```text
[ DataMessage header | your app header | your payload bytes ]
```

For frame-oriented video mode, a datagram typically looks like this:

```text
[ FrameMessage header | your fragment header | encoded frame bytes ]
```

The Prague header carries congestion-control timing and sequence information.
Your application header carries things such as stream id, codec id, message type, fragment number, frame timestamp, or payload length.
The remaining bytes are your actual content.

This crate does not impose a media format, message format, muxing layer, retransmission policy, or FEC scheme. That part remains your application protocol.

### Sender Sketch With Your Own Payload

```rust
use udp_prague::congestion::{PragueCC, count_tp, ecn_tp, rate_tp, size_tp};
use udp_prague::net::UDPSocket;
use udp_prague::protocol::DataMessage;

fn send_app_chunk(
    socket: &mut UDPSocket,
    cc: &mut PragueCC,
    seqnr: &mut count_tp,
    stream_id: u16,
    kind: u8,
    app_payload: &[u8],
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut pacing_rate, mut packet_window, mut packet_burst, mut packet_size):
        (rate_tp, count_tp, count_tp, size_tp) = (0, 0, 0, 0);
    cc.GetCCInfo(
        &mut pacing_rate,
        &mut packet_window,
        &mut packet_burst,
        &mut packet_size,
    );

    let app_header_len = 1 + 2 + 2; // kind + stream_id + payload_len
    let max_payload = (packet_size as usize).saturating_sub(DataMessage::SIZE + app_header_len);
    let payload = &app_payload[..app_payload.len().min(max_payload)];
    let total_len = DataMessage::SIZE + app_header_len + payload.len();

    let mut packet = vec![0u8; total_len];

    let (mut ts, mut ets, mut ecn) = (0, 0, ecn_tp::ecn_not_ect);
    cc.GetTimeInfo(&mut ts, &mut ets, &mut ecn);

    *seqnr += 1;
    {
        let mut dm = DataMessage::new(&mut packet[..])?;
        dm.set_timestamp(ts);
        dm.set_echoed_timestamp(ets);
        dm.set_seq_nr(*seqnr);
        dm.hton();
    }

    let hdr = DataMessage::SIZE;
    packet[hdr] = kind; // e.g. audio=1, video=2, control=3
    packet[hdr + 1..hdr + 3].copy_from_slice(&stream_id.to_be_bytes());
    packet[hdr + 3..hdr + 5].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    packet[hdr + 5..hdr + 5 + payload.len()].copy_from_slice(payload);

    socket.Send(&packet, total_len as size_tp, ecn)?;
    Ok(())
}
```

This sketch shows where your payload bytes go. A real sender loop also keeps track of in-flight packets, reacts to ACK or RFC8888 feedback, and only transmits when the Prague window and pacing schedule allow it.

In a real sender, `app_payload` would be one of these:

- an encoded audio access unit
- one fragment of an encoded video frame
- a game-state or telemetry message
- a control or chat message

Your send loop decides which bytes to send next. Prague decides when you may send, how large the packet may be, how many packets may be in flight, and which ECN marking to use.

### Receiver Sketch With Payload Extraction

```rust
use udp_prague::congestion::{PragueCC, count_tp, ecn_tp, time_tp};
use udp_prague::net::UDPSocket;
use udp_prague::protocol::DataMessage;

fn recv_app_chunk(
    socket: &mut UDPSocket,
    cc: &mut PragueCC,
    buf: &mut [u8],
) -> Result<Option<(u8, u16, Vec<u8>)>, Box<dyn std::error::Error>> {
    let mut recv_ecn = ecn_tp::ecn_not_ect;
    let bytes_received = socket.Receive(buf, &mut recv_ecn, 50_000)?;
    if bytes_received == 0 {
        return Ok(None);
    }

    let bytes_received = bytes_received as usize;
    let (seqnr, timestamp, echoed_timestamp): (count_tp, time_tp, time_tp);
    {
        let mut dm = DataMessage::new(&mut buf[..bytes_received])?;
        dm.hton();
        seqnr = dm.seq_nr();
        timestamp = dm.timestamp();
        echoed_timestamp = dm.echoed_timestamp();
    }

    cc.PacketReceived(timestamp, echoed_timestamp);
    cc.DataReceivedSequence(recv_ecn, seqnr);

    let hdr = DataMessage::SIZE;
    let kind = buf[hdr];
    let stream_id = u16::from_be_bytes([buf[hdr + 1], buf[hdr + 2]]);
    let payload_len = u16::from_be_bytes([buf[hdr + 3], buf[hdr + 4]]) as usize;
    let payload = buf[hdr + 5..hdr + 5 + payload_len].to_vec();

    Ok(Some((kind, stream_id, payload)))
}
```

In a real receiver, validate your application header and payload length before slicing into the buffer.

At that point, the Prague header has been consumed and the returned payload is entirely yours. You can hand it to an audio decoder, video depacketizer, message parser, or any other application-specific consumer.

### ACK Feedback Loop

The receiver still needs to send Prague feedback back to the sender. For classic ACK mode, that means building an `AckMessage` from the receiver-side `PragueCC` state:

```rust
use udp_prague::congestion::{PragueCC, count_tp, ecn_tp, size_tp};
use udp_prague::net::UDPSocket;
use udp_prague::protocol::AckMessage;

fn send_ack(
    socket: &mut UDPSocket,
    cc: &mut PragueCC,
    last_seq: count_tp,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ack_buf = [0u8; AckMessage::SIZE];

    let (mut ts, mut ets, mut ecn) = (0, 0, ecn_tp::ecn_not_ect);
    cc.GetTimeInfo(&mut ts, &mut ets, &mut ecn);

    let (mut pr, mut pc, mut pl, mut err) = (0, 0, 0, false);
    cc.GetACKInfo(&mut pr, &mut pc, &mut pl, &mut err);

    let mut ack = AckMessage::new(&mut ack_buf)?;
    ack.set_ack_seq(last_seq);
    ack.set_timestamp(ts);
    ack.set_echoed_timestamp(ets);
    ack.set_packets_received(pr);
    ack.set_packets_CE(pc);
    ack.set_packets_lost(pl);
    ack.set_error_L4S(err);
    ack.set_stat();

    socket.Send(&ack_buf, AckMessage::SIZE as size_tp, ecn)?;
    Ok(())
}
```

On the sender side, when an ACK or RFC8888 report comes in, feed it back into `PragueCC` with `PacketReceived(...)` and `ACKReceived(...)`. That is what updates pacing rate, window size, burst size, and CE/loss reaction for the next send opportunity.

### Video Frame Mode

For frame-based video, use `FrameMessage` instead of `DataMessage` and put your own fragment metadata after `FrameMessage::SIZE`. Prague will then give you frame-oriented guidance through `GetCCInfoVideo(...)`, including a target frame size and a frame window.

### When To Use `core::run_*`

Use the `udp_prague::core::run_sender_with_reporter` and `run_receiver_with_reporter` helpers only if you want the reference/demo packet loop.
For a real embedded application, the normal path is:

- keep your own queue of encoded media or messages
- ask `PragueCC` when and how much you may send
- write a Prague header into the front of the datagram
- append your own header and payload bytes after it
- parse the datagram on receive and hand the remaining bytes to your application
- feed Prague ACK information back into the sender loop

## Demo application usage

The reference-style CLI/config/reporting layer is behind the `demo-app` feature.

```toml
[dependencies]
udp_prague = { version = "0.1.0", default-features = false, features = ["session", "demo-app"] }
```

That enables:

- `udp_prague::demo::AppStuff`
- `udp_prague_sender`
- `udp_prague_receiver`