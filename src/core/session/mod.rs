//! Higher-level sender and receiver session helpers for embedded applications.
//!
//! These wrappers sit above `PragueCC` and `UDPSocket` and own the repetitive
//! bookkeeping around sequence numbers, ACK parsing, and Prague header packing
//! for classic bulk traffic. They are intended to make real application
//! integration easier without replacing the application's own event loop,
//! packetization, or adaptation logic.

mod receiver;
mod segment;
mod sender;
mod types;
mod video;

use std::convert::TryInto;
use std::time::Duration;

use crate::congestion::{count_tp, time_tp};
use crate::protocol::pkt_format::FRM_BUFFER_SIZE;

pub use receiver::PragueReceiverSession;
pub use segment::{PragueSegmentReceiverSession, PragueSegmentSenderSession};
pub use sender::PragueSenderSession;
pub use types::*;
pub use video::{PragueVideoReceiverSession, PragueVideoSenderSession};

#[inline]
fn bool_as_count(value: bool) -> count_tp {
    if value {
        1
    } else {
        0
    }
}

#[inline]
fn idx_frm(frame: count_tp) -> usize {
    (frame as u32 % FRM_BUFFER_SIZE as u32) as usize
}

#[inline]
fn boxed_array<T: Clone, const N: usize>(value: T) -> Box<[T; N]> {
    vec![value; N]
        .into_boxed_slice()
        .try_into()
        .ok()
        .expect("boxed array length")
}

#[inline]
fn sleep_delay_us(delay_us: time_tp) {
    if delay_us > 0 {
        std::thread::sleep(Duration::from_micros(delay_us as u64));
    }
}
