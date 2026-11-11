//! Receiver side: UDP in, jitter buffer out.
//!
//! Packets go straight into the lock-free ring that the render thread drains.
//! Loss is detected from the sequence numbers and counted, never waited for.
//! Malformed or foreign datagrams on the port are discarded silently.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::{BLOCK_SAMPLES, MIC_PORT};
use crate::net::protocol::{PACKET_LEN, decode_audio, payload_into_block, verify_audio};
use crate::net::sender::Target;
use crate::pairing::{TokenSlot, TokenStore};
use crate::ring::BlockRing;
use crate::state::{Shared, now_ms};

/// Receive buffer. Slightly larger than a packet so an oversized datagram is
/// read and rejected rather than silently truncated into something plausible.
const RECV_BUF: usize = PACKET_LEN + 64;

pub fn spawn(
    port: u16,
    magic: [u8; 4],
    tokens: Arc<TokenStore>,
    ring: Arc<BlockRing>,
    shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    // Filled in with the senders address so the return microphone stream
    // knows where to go. The receiver cannot know this in advance: it learns
    // who is streaming to it only when the first packet arrives.
    mic_target: Option<Target>,
    // The key the return microphone stream should sign with: whichever
    // pairing the accepted sender used.
    mic_token: Option<TokenSlot>,
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
            let mut unauthorised: u64 = 0;

            while !stop.load(Ordering::Relaxed) {
                let (len, from) = match socket.recv_from(&mut buf) {
                    Ok(v) => v,
                    Err(_) => {
                        // Timeout, or an ICMP-driven error from a previous
                        // send. Neither is fatal.
                        continue;
                    }
                };

                let Some((header, payload)) = decode_audio(&buf[..len], magic) else {
                    continue;
                };

                // Authenticate before anything else looks at the packet.
                // Without this an unpaired machine on the network could
                // play audio through these speakers, and -- far worse --
                // become the destination of this machine's microphone.
                let Some(token) = tokens.find(header.key_id) else {
                    unauthorised = unauthorised.saturating_add(1);
                    if unauthorised == 1 || unauthorised % 500 == 0 {
                        shared.set_net_error(Some(format!(
                            "Ignoring audio from an unpaired machine ({}). \
                             Pair it to allow it.",
                            from.ip()
                        )));
                    }
                    continue;
                };
                if !verify_audio(&buf[..len], &token) {
                    unauthorised = unauthorised.saturating_add(1);
                    continue;
                }

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
                    // Point the return microphone stream at whoever started
                    // streaming to us -- once, and only once.
                    //
                    // Following the peer here would be an eavesdropping hole:
                    // anyone on the network could send a single packet to the
                    // audio port and this machine would redirect its live
                    // microphone to them, silently. The destination is latched
                    // for the session instead, and shown in the UI, so a later
                    // arrival cannot steal the microphone. Stopping and
                    // starting the stream is what re-pairs it.
                    if let Some(target) = &mic_target {
                        if let Ok(mut slot) = target.lock() {
                            if slot.is_none() {
                                *slot = Some(SocketAddr::new(from.ip(), MIC_PORT));
                                // Sign the microphone with the same
                                // pairing this sender authenticated with.
                                if let Some(key) = &mic_token {
                                    if let Ok(mut key) = key.lock() {
                                        *key = Some(token.clone());
                                    }
                                }
                            }
                        }
                    }
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
