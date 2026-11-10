//! Receiver side: UDP in, jitter buffer out.
//!
//! Packets go straight into the lock-free ring that the render thread drains.
//! Loss is detected from the sequence numbers and counted, never waited for.
//! Malformed or foreign datagrams on the port are discarded silently.

use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::BLOCK_SAMPLES;
use crate::net::protocol::{PACKET_LEN, decode_audio, payload_into_block};
use crate::ring::BlockRing;
use crate::state::{Shared, now_ms};

/// Receive buffer. Slightly larger than a packet so an oversized datagram is
/// read and rejected rather than silently truncated into something plausible.
const RECV_BUF: usize = PACKET_LEN + 64;

pub fn spawn(
    port: u16,
    ring: Arc<BlockRing>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("audiopush-udp-recv".into())
        .spawn(move || {
            let socket = match UdpSocket::bind(("0.0.0.0", port)) {
                Ok(s) => s,
                Err(e) => {
                    shared.set_net_error(Some(format!(
                        "could not listen on UDP port {port}: {e}. \
                         Another copy of AudioPush may already be running."
                    )));
                    return;
                }
            };
            // A read timeout is what lets us notice the stop flag.
            let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));
            shared.set_net_error(None);

            let mut buf = [0u8; RECV_BUF];
            let mut block = [0i16; BLOCK_SAMPLES];
            let mut expected_seq: Option<u32> = None;
            let mut current_peer: Option<String> = None;

            while !stop.load(Ordering::Relaxed) {
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(_) => {
                        // Timeout, or an ICMP-driven error from a previous
                        // send. Neither is fatal.
                        continue;
                    }
                };

                let Some((header, payload)) = decode_audio(&buf[..len]) else {
                    continue;
                };

                // Sequence gaps mean lost or reordered packets. We never wait
                // for them; the gap simply shortens the buffer by that much.
                if let Some(expected) = expected_seq {
                    let gap = header.seq.wrapping_sub(expected);
                    // A huge gap means the sender restarted, not 4 billion
                    // lost packets, so only count plausible losses.
                    if gap > 0 && gap < 1000 {
                        shared.lost.fetch_add(gap as u64, Ordering::Relaxed);
                    }
                    // A gap of 1000+ means the sender restarted rather than
                    // that we lost that many packets, so it is not counted.
                    // The render thread trims any resulting backlog; only it
                    // may move the read side of the ring.
                }
                expected_seq = Some(header.seq.wrapping_add(1));

                let peer_text = from.to_string();
                if current_peer.as_deref() != Some(peer_text.as_str()) {
                    shared.set_peer(Some(peer_text.clone()));
                    current_peer = Some(peer_text);
                }

                payload_into_block(payload, &mut block);
                if !ring.push(&block) {
                    // Ring full: playback has stalled. Drop the newest block
                    // rather than grow unbounded latency.
                    shared.dropped_late.fetch_add(1, Ordering::Relaxed);
                }

                shared.blocks.fetch_add(1, Ordering::Relaxed);
                shared.last_packet_ms.store(now_ms(), Ordering::Relaxed);
                shared
                    .buffer_blocks
                    .store(ring.len() as u64, Ordering::Relaxed);
            }
        })
        .expect("failed to spawn udp receiver thread")
}
